//! Closed request/response protocol for EchoWall's independent Qwen3-ASR and
//! forced-alignment worker.
//!
//! The boundary carries only App-owned relative paths, exact public model
//! identities, bounded audio metadata, and word timestamps. It has no Tauri,
//! provider, credential, network, queue, or archive dependency.

use std::collections::HashSet;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const LOCAL_QWEN_PROTOCOL_VERSION: u32 = 1;
pub const LOCAL_QWEN_RUNTIME_ID: &str = "qwen-asr-rust-0.11.0";
pub const LOCAL_QWEN_ASR_MODEL_ID: &str = "qwen3-asr-1.7b";
pub const LOCAL_QWEN_ASR_REVISION: &str = "7278e1e70fe206f11671096ffdd38061171dd6e5";
pub const LOCAL_QWEN_ALIGNER_MODEL_ID: &str = "qwen3-forced-aligner-0.6b";
pub const LOCAL_QWEN_ALIGNER_REVISION: &str = "c7cbfc2048c462b0d63a45797104fc9db3ad62b7";
pub const LOCAL_QWEN_LEGACY_CHUNK_POLICY: &str = "quiet-split-30s-language-per-chunk-v1";
pub const LOCAL_QWEN_CHUNK_POLICY: &str = "vad-utterance-30s-language-per-chunk-v2";
pub const LOCAL_QWEN_CHUNK_DURATION_MS: u64 = 30_000;
pub const LOCAL_QWEN_SPLIT_SEARCH_MS: u64 = 3_000;
pub const MAX_LOCAL_QWEN_REQUEST_BYTES: usize = 128 * 1024;
pub const MAX_LOCAL_QWEN_RESPONSE_BYTES: usize = 24 * 1024 * 1024;
pub const MAX_LOCAL_QWEN_AUDIO_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_LOCAL_QWEN_AUDIO_DURATION_MS: u64 = 5 * 60 * 60 * 1_000;
pub const MAX_LOCAL_QWEN_MODEL_FILES: usize = 8;
pub const MAX_LOCAL_QWEN_MODEL_BYTES: u64 = 8 * 1024 * 1024 * 1024;
pub const MAX_LOCAL_QWEN_SEGMENTS: usize = 1_000;
pub const MAX_LOCAL_QWEN_ALIGNED_WORDS: usize = 200_000;
pub const MAX_LOCAL_QWEN_SEGMENT_TEXT_BYTES: usize = 128 * 1024;
pub const MAX_LOCAL_QWEN_WORD_TEXT_BYTES: usize = 4 * 1024;
pub const MAX_LOCAL_QWEN_TRANSCRIPT_TEXT_BYTES: usize = 12 * 1024 * 1024;

pub const LOCAL_QWEN_ASR_FILES: [&str; 3] = [
    "model-00001-of-00002.safetensors",
    "model-00002-of-00002.safetensors",
    "vocab.json",
];
pub const LOCAL_QWEN_ALIGNER_FILES: [&str; 2] = ["model.safetensors", "vocab.json"];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenModelFileIdentity {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenRequest {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub runtime_id: String,
    pub asr_model_id: String,
    pub asr_model_revision: String,
    pub asr_model_files: Vec<LocalQwenModelFileIdentity>,
    pub aligner_model_id: String,
    pub aligner_model_revision: String,
    pub aligner_model_files: Vec<LocalQwenModelFileIdentity>,
    pub audio_relative_path: String,
    pub audio_sha256: String,
    pub audio_size_bytes: u64,
    pub audio_duration_ms: u64,
    pub language: Option<String>,
    pub chunk_policy: String,
    pub chunk_duration_ms: u64,
    pub split_search_ms: u64,
}

impl LocalQwenRequest {
    pub fn validate(&self) -> Result<(), LocalQwenProtocolError> {
        if self.schema_version != LOCAL_QWEN_PROTOCOL_VERSION {
            return Err(LocalQwenProtocolError::new("unsupported_protocol"));
        }
        if self.runtime_id != LOCAL_QWEN_RUNTIME_ID
            || self.asr_model_id != LOCAL_QWEN_ASR_MODEL_ID
            || self.asr_model_revision != LOCAL_QWEN_ASR_REVISION
            || self.aligner_model_id != LOCAL_QWEN_ALIGNER_MODEL_ID
            || self.aligner_model_revision != LOCAL_QWEN_ALIGNER_REVISION
            || !matches!(
                self.chunk_policy.as_str(),
                LOCAL_QWEN_LEGACY_CHUNK_POLICY | LOCAL_QWEN_CHUNK_POLICY
            )
            || self.chunk_duration_ms != LOCAL_QWEN_CHUNK_DURATION_MS
            || self.split_search_ms != LOCAL_QWEN_SPLIT_SEARCH_MS
            || !valid_relative_audio(&self.audio_relative_path)
            || !valid_sha256(&self.audio_sha256)
            || self.audio_size_bytes == 0
            || self.audio_size_bytes >= MAX_LOCAL_QWEN_AUDIO_BYTES
            || self.audio_duration_ms == 0
            || self.audio_duration_ms >= MAX_LOCAL_QWEN_AUDIO_DURATION_MS
            || self
                .language
                .as_deref()
                .is_some_and(|value| !valid_language(value))
        {
            return Err(LocalQwenProtocolError::new("invalid_request"));
        }
        validate_model_files(&self.asr_model_files, &LOCAL_QWEN_ASR_FILES)?;
        validate_model_files(&self.aligner_model_files, &LOCAL_QWEN_ALIGNER_FILES)?;
        let total = self
            .asr_model_files
            .iter()
            .chain(&self.aligner_model_files)
            .try_fold(0_u64, |total, file| total.checked_add(file.size_bytes))
            .ok_or_else(|| LocalQwenProtocolError::new("request_too_large"))?;
        if total > MAX_LOCAL_QWEN_MODEL_BYTES {
            return Err(LocalQwenProtocolError::new("request_too_large"));
        }
        Ok(())
    }

    pub fn model_set_sha256(&self) -> Result<String, LocalQwenProtocolError> {
        self.validate()?;
        let mut digest = Sha256::new();
        for value in [
            self.runtime_id.as_str(),
            self.asr_model_id.as_str(),
            self.asr_model_revision.as_str(),
            self.aligner_model_id.as_str(),
            self.aligner_model_revision.as_str(),
        ] {
            update_digest_field(&mut digest, value.as_bytes())?;
        }
        for file in self.asr_model_files.iter().chain(&self.aligner_model_files) {
            update_digest_field(&mut digest, file.relative_path.as_bytes())?;
            update_digest_field(&mut digest, file.sha256.as_bytes())?;
            digest.update(file.size_bytes.to_be_bytes());
        }
        Ok(hex::encode(digest.finalize()))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenAlignedWord {
    pub start_ms: u64,
    pub end_ms: u64,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub language: Option<String>,
    pub text: String,
    pub words: Vec<LocalQwenAlignedWord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenResponse {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub runtime_id: String,
    pub asr_model_id: String,
    pub asr_model_revision: String,
    pub aligner_model_id: String,
    pub aligner_model_revision: String,
    pub audio_sha256: String,
    pub chunk_policy: String,
    pub segments: Vec<LocalQwenSegment>,
}

impl LocalQwenResponse {
    pub fn validate_against(
        &self,
        request: &LocalQwenRequest,
    ) -> Result<(), LocalQwenProtocolError> {
        request.validate()?;
        if self.schema_version != LOCAL_QWEN_PROTOCOL_VERSION
            || self.recording_id != request.recording_id
            || self.runtime_id != request.runtime_id
            || self.asr_model_id != request.asr_model_id
            || self.asr_model_revision != request.asr_model_revision
            || self.aligner_model_id != request.aligner_model_id
            || self.aligner_model_revision != request.aligner_model_revision
            || self.audio_sha256 != request.audio_sha256
            || self.chunk_policy != request.chunk_policy
            || self.segments.is_empty()
            || self.segments.len() > MAX_LOCAL_QWEN_SEGMENTS
        {
            return Err(LocalQwenProtocolError::new("invalid_response"));
        }

        let mut prior_segment_end = 0_u64;
        let mut transcript_bytes = 0_usize;
        let mut word_count = 0_usize;
        for segment in &self.segments {
            if segment.start_ms < prior_segment_end
                || segment.end_ms <= segment.start_ms
                || segment.end_ms > request.audio_duration_ms
                || segment
                    .language
                    .as_deref()
                    .is_some_and(|language| !valid_language(language))
                || request
                    .language
                    .as_deref()
                    .is_some_and(|language| segment.language.as_deref() != Some(language))
                || segment.text.trim().is_empty()
                || segment.text.len() > MAX_LOCAL_QWEN_SEGMENT_TEXT_BYTES
                || segment.text.contains('\0')
                || segment.words.is_empty()
            {
                return Err(LocalQwenProtocolError::new("invalid_response"));
            }
            prior_segment_end = segment.end_ms;
            transcript_bytes = transcript_bytes
                .checked_add(segment.text.len())
                .ok_or_else(|| LocalQwenProtocolError::new("response_too_large"))?;
            if transcript_bytes > MAX_LOCAL_QWEN_TRANSCRIPT_TEXT_BYTES {
                return Err(LocalQwenProtocolError::new("response_too_large"));
            }

            let mut prior_word_start = segment.start_ms;
            for word in &segment.words {
                if word.start_ms < prior_word_start
                    || word.start_ms < segment.start_ms
                    || word.end_ms < word.start_ms
                    || word.end_ms > segment.end_ms
                    || word.text.is_empty()
                    || word.text.len() > MAX_LOCAL_QWEN_WORD_TEXT_BYTES
                    || word.text.contains('\0')
                {
                    return Err(LocalQwenProtocolError::new("invalid_response"));
                }
                prior_word_start = word.start_ms;
                word_count = word_count
                    .checked_add(1)
                    .ok_or_else(|| LocalQwenProtocolError::new("response_too_large"))?;
                if word_count > MAX_LOCAL_QWEN_ALIGNED_WORDS {
                    return Err(LocalQwenProtocolError::new("response_too_large"));
                }
            }
        }
        Ok(())
    }
}

pub fn encode_request(request: &LocalQwenRequest) -> Result<Vec<u8>, LocalQwenProtocolError> {
    request.validate()?;
    encode_bounded(request, MAX_LOCAL_QWEN_REQUEST_BYTES, "request_too_large")
}

pub fn decode_request(bytes: &[u8]) -> Result<LocalQwenRequest, LocalQwenProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_QWEN_REQUEST_BYTES {
        return Err(LocalQwenProtocolError::new("request_too_large"));
    }
    let request: LocalQwenRequest =
        serde_json::from_slice(bytes).map_err(|_| LocalQwenProtocolError::new("invalid_json"))?;
    request.validate()?;
    Ok(request)
}

pub fn encode_response(
    response: &LocalQwenResponse,
    request: &LocalQwenRequest,
) -> Result<Vec<u8>, LocalQwenProtocolError> {
    response.validate_against(request)?;
    encode_bounded(
        response,
        MAX_LOCAL_QWEN_RESPONSE_BYTES,
        "response_too_large",
    )
}

pub fn decode_response(
    bytes: &[u8],
    request: &LocalQwenRequest,
) -> Result<LocalQwenResponse, LocalQwenProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_LOCAL_QWEN_RESPONSE_BYTES {
        return Err(LocalQwenProtocolError::new("response_too_large"));
    }
    let response: LocalQwenResponse =
        serde_json::from_slice(bytes).map_err(|_| LocalQwenProtocolError::new("invalid_json"))?;
    response.validate_against(request)?;
    Ok(response)
}

fn validate_model_files(
    files: &[LocalQwenModelFileIdentity],
    exact_paths: &[&str],
) -> Result<(), LocalQwenProtocolError> {
    if files.len() != exact_paths.len() || files.len() > MAX_LOCAL_QWEN_MODEL_FILES {
        return Err(LocalQwenProtocolError::new("invalid_request"));
    }
    let mut paths = HashSet::new();
    for (file, exact_path) in files.iter().zip(exact_paths) {
        if file.relative_path != *exact_path
            || !paths.insert(file.relative_path.as_str())
            || !valid_relative_path(&file.relative_path)
            || !valid_sha256(&file.sha256)
            || file.size_bytes == 0
        {
            return Err(LocalQwenProtocolError::new("invalid_request"));
        }
    }
    Ok(())
}

fn encode_bounded(
    value: &impl Serialize,
    maximum_bytes: usize,
    error_code: &'static str,
) -> Result<Vec<u8>, LocalQwenProtocolError> {
    let mut bytes =
        serde_json::to_vec(value).map_err(|_| LocalQwenProtocolError::new("invalid_json"))?;
    bytes.push(b'\n');
    if bytes.len() > maximum_bytes {
        Err(LocalQwenProtocolError::new(error_code))
    } else {
        Ok(bytes)
    }
}

fn update_digest_field(digest: &mut Sha256, value: &[u8]) -> Result<(), LocalQwenProtocolError> {
    let length =
        u64::try_from(value.len()).map_err(|_| LocalQwenProtocolError::new("request_too_large"))?;
    digest.update(length.to_be_bytes());
    digest.update(value);
    Ok(())
}

fn valid_language(value: &str) -> bool {
    (2..=16).contains(&value.len())
        && value.split('-').all(|part| {
            (2..=8).contains(&part.len()) && part.bytes().all(|byte| byte.is_ascii_lowercase())
        })
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn valid_relative_path(value: &str) -> bool {
    let path = Path::new(value);
    !value.is_empty()
        && value.len() <= 2_048
        && !value.contains('\\')
        && !path.is_absolute()
        && path.components().all(|component| {
            matches!(component, Component::Normal(_)) && component.as_os_str().to_str().is_some()
        })
}

fn valid_relative_audio(value: &str) -> bool {
    valid_relative_path(value)
        && matches!(
            value
                .rsplit_once('.')
                .map(|(_, extension)| extension.to_ascii_lowercase()),
            Some(extension) if matches!(extension.as_str(), "wav" | "m4a" | "mp3")
        )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalQwenProtocolError {
    code: &'static str,
}

impl LocalQwenProtocolError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }

    pub const fn code(&self) -> &'static str {
        self.code
    }
}

impl std::fmt::Display for LocalQwenProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local Qwen protocol validation failed")
    }
}

impl std::error::Error for LocalQwenProtocolError {}

#[cfg(test)]
mod tests {
    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    const SHA_B: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

    fn files(paths: &[&str]) -> Vec<LocalQwenModelFileIdentity> {
        paths
            .iter()
            .enumerate()
            .map(|(index, path)| LocalQwenModelFileIdentity {
                relative_path: (*path).to_owned(),
                sha256: if index.is_multiple_of(2) {
                    SHA_A
                } else {
                    SHA_B
                }
                .to_owned(),
                size_bytes: 1_024 + index as u64,
            })
            .collect()
    }

    fn request() -> LocalQwenRequest {
        LocalQwenRequest {
            schema_version: LOCAL_QWEN_PROTOCOL_VERSION,
            recording_id: Uuid::nil(),
            runtime_id: LOCAL_QWEN_RUNTIME_ID.to_owned(),
            asr_model_id: LOCAL_QWEN_ASR_MODEL_ID.to_owned(),
            asr_model_revision: LOCAL_QWEN_ASR_REVISION.to_owned(),
            asr_model_files: files(&LOCAL_QWEN_ASR_FILES),
            aligner_model_id: LOCAL_QWEN_ALIGNER_MODEL_ID.to_owned(),
            aligner_model_revision: LOCAL_QWEN_ALIGNER_REVISION.to_owned(),
            aligner_model_files: files(&LOCAL_QWEN_ALIGNER_FILES),
            audio_relative_path: "derived/input.wav".to_owned(),
            audio_sha256: SHA_A.to_owned(),
            audio_size_bytes: 64_000,
            audio_duration_ms: 4_000,
            language: None,
            chunk_policy: LOCAL_QWEN_CHUNK_POLICY.to_owned(),
            chunk_duration_ms: LOCAL_QWEN_CHUNK_DURATION_MS,
            split_search_ms: LOCAL_QWEN_SPLIT_SEARCH_MS,
        }
    }

    #[test]
    fn legacy_chunk_policy_remains_replayable() {
        let mut legacy = request();
        legacy.chunk_policy = LOCAL_QWEN_LEGACY_CHUNK_POLICY.to_owned();
        legacy.validate().unwrap();
        let encoded = encode_request(&legacy).unwrap();
        assert_eq!(decode_request(&encoded).unwrap(), legacy);
    }

    fn response(request: &LocalQwenRequest) -> LocalQwenResponse {
        LocalQwenResponse {
            schema_version: request.schema_version,
            recording_id: request.recording_id,
            runtime_id: request.runtime_id.clone(),
            asr_model_id: request.asr_model_id.clone(),
            asr_model_revision: request.asr_model_revision.clone(),
            aligner_model_id: request.aligner_model_id.clone(),
            aligner_model_revision: request.aligner_model_revision.clone(),
            audio_sha256: request.audio_sha256.clone(),
            chunk_policy: request.chunk_policy.clone(),
            segments: vec![LocalQwenSegment {
                start_ms: 0,
                end_ms: 4_000,
                language: Some("en".to_owned()),
                text: "hello world".to_owned(),
                words: vec![
                    LocalQwenAlignedWord {
                        start_ms: 100,
                        end_ms: 500,
                        text: "hello".to_owned(),
                    },
                    LocalQwenAlignedWord {
                        start_ms: 600,
                        end_ms: 1_000,
                        text: "world".to_owned(),
                    },
                ],
            }],
        }
    }

    #[test]
    fn roundtrip_binds_runtime_models_audio_and_chunk_policy() {
        let request = request();
        let request_bytes = encode_request(&request).unwrap();
        assert_eq!(decode_request(&request_bytes).unwrap(), request);
        let response = response(&request);
        let bytes = encode_response(&response, &request).unwrap();
        assert_eq!(decode_response(&bytes, &request).unwrap(), response);
        let digest = request.model_set_sha256().unwrap();
        assert_eq!(digest.len(), 64);
        let mut changed = request.clone();
        changed.asr_model_files[0].sha256 = SHA_B.to_owned();
        assert_ne!(changed.model_set_sha256().unwrap(), digest);
    }

    #[test]
    fn unknown_fields_substitution_and_unpinned_files_fail_closed() {
        let request = request();
        let mut value = serde_json::to_value(&request).unwrap();
        value["surprise"] = serde_json::json!(true);
        assert_eq!(
            decode_request(&serde_json::to_vec(&value).unwrap())
                .unwrap_err()
                .code(),
            "invalid_json"
        );

        let mut changed = request.clone();
        changed.runtime_id = "qwen-cli".to_owned();
        assert_eq!(changed.validate().unwrap_err().code(), "invalid_request");
        let mut changed = request.clone();
        changed.asr_model_files.swap(0, 1);
        assert_eq!(changed.validate().unwrap_err().code(), "invalid_request");
        let mut changed = request.clone();
        changed.asr_model_files[0].relative_path = "../model.safetensors".to_owned();
        assert_eq!(changed.validate().unwrap_err().code(), "invalid_request");
    }

    #[test]
    fn response_rejects_identity_timestamp_language_and_size_drift() {
        let request = request();
        let mut changed = response(&request);
        changed.audio_sha256 = SHA_B.to_owned();
        assert_eq!(
            changed.validate_against(&request).unwrap_err().code(),
            "invalid_response"
        );

        let mut changed = response(&request);
        changed.segments[0].words[1].start_ms = 50;
        assert_eq!(
            changed.validate_against(&request).unwrap_err().code(),
            "invalid_response"
        );

        let mut forced = request.clone();
        forced.language = Some("zh".to_owned());
        assert_eq!(
            response(&forced)
                .validate_against(&forced)
                .unwrap_err()
                .code(),
            "invalid_response"
        );

        let mut changed = response(&request);
        changed.segments[0].text = "x".repeat(MAX_LOCAL_QWEN_SEGMENT_TEXT_BYTES + 1);
        assert_eq!(
            changed.validate_against(&request).unwrap_err().code(),
            "invalid_response"
        );
    }
}
