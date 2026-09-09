//! Closed protocol shared by EchoWall's Rust job owner and one-shot local
//! summary worker. The protocol carries one bounded transcript plus exact
//! model/prompt identities and returns only the existing archive summary shape.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const LOCAL_SUMMARY_PROTOCOL_VERSION: u32 = 1;
pub const LOCAL_SUMMARY_PROMPT_VERSION: &str = "echowall-summary-v1";
pub const MAX_LOCAL_SUMMARY_REQUEST_BYTES: usize = 2 * 1024 * 1024 + 64 * 1024;
pub const MAX_LOCAL_SUMMARY_RESPONSE_BYTES: usize = 256 * 1024;
pub const MAX_TRANSCRIPT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_TITLE_CHARS: usize = 200;
pub const MAX_SUMMARY_CHARS: usize = 16_000;
pub const MAX_LIST_ITEMS: usize = 12;
pub const MAX_LIST_ITEM_CHARS: usize = 2_000;

pub const CATEGORIES: [&str; 6] = [
    "亲密关系",
    "自我成长",
    "学习认知",
    "工作商务",
    "生活日常",
    "其他",
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSummaryRequest {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub model_id: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub prompt_version: String,
    pub transcript_sha256: String,
    pub transcript: String,
}

impl LocalSummaryRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if self.schema_version != LOCAL_SUMMARY_PROTOCOL_VERSION
            || self.prompt_version != LOCAL_SUMMARY_PROMPT_VERSION
            || !valid_identifier(&self.model_id)
            || !valid_sha256(&self.model_sha256)
            || !valid_sha256(&self.transcript_sha256)
            || self.model_size_bytes == 0
            || self.transcript.is_empty()
            || self.transcript.len() > MAX_TRANSCRIPT_BYTES
            || self
                .transcript
                .chars()
                .any(|character| character.is_control() && !matches!(character, '\n' | '\r' | '\t'))
        {
            return Err(ProtocolError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSummaryDocument {
    pub title: String,
    pub category: String,
    pub summary_en: String,
    pub summary_zh: String,
    pub key_points_en: Vec<String>,
    pub key_points_zh: Vec<String>,
    pub action_items: Vec<String>,
}

impl LocalSummaryDocument {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        if !valid_text(&self.title, 1, MAX_TITLE_CHARS)
            || !CATEGORIES.contains(&self.category.as_str())
            || !valid_text(&self.summary_en, 0, MAX_SUMMARY_CHARS)
            || !valid_text(&self.summary_zh, 0, MAX_SUMMARY_CHARS)
            || !valid_list(&self.key_points_en)
            || !valid_list(&self.key_points_zh)
            || !valid_list(&self.action_items)
        {
            return Err(ProtocolError);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSummaryResponse {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub model_id: String,
    pub model_sha256: String,
    pub prompt_version: String,
    pub transcript_sha256: String,
    pub summary: LocalSummaryDocument,
}

impl LocalSummaryResponse {
    pub fn validate_for(&self, request: &LocalSummaryRequest) -> Result<(), ProtocolError> {
        request.validate()?;
        if self.schema_version != LOCAL_SUMMARY_PROTOCOL_VERSION
            || self.recording_id != request.recording_id
            || self.model_id != request.model_id
            || self.model_sha256 != request.model_sha256
            || self.prompt_version != request.prompt_version
            || self.transcript_sha256 != request.transcript_sha256
        {
            return Err(ProtocolError);
        }
        self.summary.validate()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolError;

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("invalid local summary protocol value")
    }
}

impl std::error::Error for ProtocolError {}

pub fn decode_request(bytes: &[u8]) -> Result<LocalSummaryRequest, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_SUMMARY_REQUEST_BYTES {
        return Err(ProtocolError);
    }
    let request: LocalSummaryRequest = serde_json::from_slice(bytes).map_err(|_| ProtocolError)?;
    request.validate()?;
    Ok(request)
}

pub fn encode_request(request: &LocalSummaryRequest) -> Result<Vec<u8>, ProtocolError> {
    request.validate()?;
    let bytes = serde_json::to_vec(request).map_err(|_| ProtocolError)?;
    if bytes.len() > MAX_LOCAL_SUMMARY_REQUEST_BYTES {
        return Err(ProtocolError);
    }
    Ok(bytes)
}

pub fn decode_response(
    bytes: &[u8],
    request: &LocalSummaryRequest,
) -> Result<LocalSummaryResponse, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_SUMMARY_RESPONSE_BYTES {
        return Err(ProtocolError);
    }
    let response: LocalSummaryResponse =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError)?;
    response.validate_for(request)?;
    Ok(response)
}

pub fn encode_response(
    response: &LocalSummaryResponse,
    request: &LocalSummaryRequest,
) -> Result<Vec<u8>, ProtocolError> {
    response.validate_for(request)?;
    let bytes = serde_json::to_vec(response).map_err(|_| ProtocolError)?;
    if bytes.len() > MAX_LOCAL_SUMMARY_RESPONSE_BYTES {
        return Err(ProtocolError);
    }
    Ok(bytes)
}

fn valid_identifier(value: &str) -> bool {
    let bytes = value.as_bytes();
    (2..=128).contains(&bytes.len())
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"._-".contains(byte))
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_text(value: &str, minimum_chars: usize, maximum_chars: usize) -> bool {
    let trimmed = value.trim();
    let chars = trimmed.chars().count();
    chars >= minimum_chars && chars <= maximum_chars && !trimmed.chars().any(char::is_control)
}

fn valid_list(values: &[String]) -> bool {
    values.len() <= MAX_LIST_ITEMS
        && values
            .iter()
            .all(|value| valid_text(value, 1, MAX_LIST_ITEM_CHARS))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request() -> LocalSummaryRequest {
        LocalSummaryRequest {
            schema_version: LOCAL_SUMMARY_PROTOCOL_VERSION,
            recording_id: Uuid::nil(),
            model_id: "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            model_sha256: "a".repeat(64),
            model_size_bytes: 17_559_178_144,
            prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
            transcript_sha256: "b".repeat(64),
            transcript: "SPEAKER_01: fabricated meeting\n".to_owned(),
        }
    }

    fn response(request: &LocalSummaryRequest) -> LocalSummaryResponse {
        LocalSummaryResponse {
            schema_version: LOCAL_SUMMARY_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            model_id: request.model_id.clone(),
            model_sha256: request.model_sha256.clone(),
            prompt_version: request.prompt_version.clone(),
            transcript_sha256: request.transcript_sha256.clone(),
            summary: LocalSummaryDocument {
                title: "Fabricated meeting".to_owned(),
                category: "工作商务".to_owned(),
                summary_en: "A fabricated summary.".to_owned(),
                summary_zh: "一段合成摘要。".to_owned(),
                key_points_en: vec!["Synthetic point".to_owned()],
                key_points_zh: vec!["合成要点".to_owned()],
                action_items: Vec::new(),
            },
        }
    }

    #[test]
    fn roundtrip_binds_every_identity() {
        let request = request();
        let decoded = decode_request(&encode_request(&request).unwrap()).unwrap();
        assert_eq!(decoded, request);
        let response = response(&request);
        let decoded =
            decode_response(&encode_response(&response, &request).unwrap(), &request).unwrap();
        assert_eq!(decoded, response);
    }

    #[test]
    fn unknown_fields_and_identity_substitution_fail_closed() {
        let request = request();
        let mut request_json = serde_json::to_value(&request).unwrap();
        request_json["url"] = serde_json::json!("https://example.invalid");
        assert!(decode_request(&serde_json::to_vec(&request_json).unwrap()).is_err());

        let mut response = response(&request);
        response.transcript_sha256 = "c".repeat(64);
        assert!(encode_response(&response, &request).is_err());
    }

    #[test]
    fn output_shape_and_bounds_fail_closed() {
        let request = request();
        let mut response = response(&request);
        response.summary.category = "made-up".to_owned();
        assert!(response.validate_for(&request).is_err());
        response.summary.category = "其他".to_owned();
        response.summary.key_points_en = vec!["x".to_owned(); MAX_LIST_ITEMS + 1];
        assert!(response.validate_for(&request).is_err());

        let mut oversized = request;
        oversized.transcript = "x".repeat(MAX_TRANSCRIPT_BYTES + 1);
        assert!(oversized.validate().is_err());
    }
}
