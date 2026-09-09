//! Closed one-shot MOSS boundary. Rust retains the recording, queue, and archive.
//! Joint output preserves cross-speaker overlap instead of weakening the
//! existing Whisper protocol's non-overlap invariant.

use std::collections::BTreeMap;
use std::path::{Component, Path};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod chronology;
mod raw;
pub mod speakers;
pub mod windows;
pub use chronology::ChronologicalProvenance;
pub use raw::parse_raw_segments;
pub const RAW_TIMING_POLICY: &str = "explicit-model-boundaries-v1";

pub const PROTOCOL_VERSION: u32 = 1;
pub const RUNTIME_ID: &str = "transcribe-cpp-0.2.3-moss-metal";
pub const MODEL_ID: &str = "moss-transcribe-diarize-0.9b-q8_0";
pub const MODEL_REVISION: &str = "6fdfa33aed776bbb0ac11a1a9835634fe6d75dd7";
pub const MODEL_SHA256: &str = "64ec654dc6ffcfdfe180422dffce1d33422b0c30959b7edfd131bad77ee35039";
pub const MODEL_SIZE_BYTES: u64 = 986_899_616;
pub const TIMING_POLICY: &str = "joint-overlap-unknown-tail100-v1";
pub const COALESCING_TIMING_POLICY_V2: &str = "joint-adjacent-union-unknown-tail100-v2";
pub const CHRONOLOGICAL_TIMING_POLICY_V3: &str =
    "joint-stable-chronological-adjacent-union-unknown-tail100-v3";
pub const MAX_REQUEST_BYTES: usize = 64 * 1024;
pub const MAX_RESPONSE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_TEXT_BYTES: usize = 12 * 1024 * 1024;
pub const MAX_SEGMENTS: usize = 10_000;
pub const MAX_SPEAKERS: u32 = 16;
/// Only the final interval may exceed the decoded duration, by at most 100 ms.
/// This bounds the observed 84 ms tail anomaly; other timing errors fail closed.
pub const MAX_TAIL_OVERRUN_MS: u64 = 100;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossRequest {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub runtime_id: String,
    pub model_id: String,
    pub model_revision: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub timing_policy: String,
    pub audio_relative_path: String,
    pub audio_sha256: String,
    pub audio_size_bytes: u64,
    pub audio_duration_ms: u64,
    pub language: Option<String>,
}

impl MossRequest {
    pub fn validate(&self) -> Result<(), ProtocolError> {
        let expected_prefix = format!("inbox/{}/", self.recording_id);
        if self.schema_version != PROTOCOL_VERSION
            || self.recording_id.is_nil()
            || self.runtime_id != RUNTIME_ID
            || self.model_id != MODEL_ID
            || self.model_revision != MODEL_REVISION
            || self.model_sha256 != MODEL_SHA256
            || self.model_size_bytes != MODEL_SIZE_BYTES
            || !matches!(
                self.timing_policy.as_str(),
                TIMING_POLICY | COALESCING_TIMING_POLICY_V2 | CHRONOLOGICAL_TIMING_POLICY_V3
            )
            || !self.audio_relative_path.starts_with(&expected_prefix)
            || !safe_audio_path(&self.audio_relative_path)
            || !valid_sha256(&self.audio_sha256)
            || self.audio_size_bytes == 0
            || self.audio_size_bytes >= 512 * 1024 * 1024
            || self.audio_duration_ms == 0
            || self.audio_duration_ms >= 5 * 60 * 60 * 1_000
            || self
                .language
                .as_deref()
                .is_some_and(|value| !matches!(value, "en" | "zh"))
        {
            return Err(ProtocolError("invalid_request"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossSegment {
    pub start_ms: i64,
    pub end_ms: i64,
    /// Anonymous model slot; zero means no attribution. Never a confidence.
    pub speaker_id: u32,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossResponse {
    pub schema_version: u32,
    pub recording_id: Uuid,
    pub runtime_id: String,
    pub model_id: String,
    pub model_sha256: String,
    pub audio_sha256: String,
    pub timing_policy: String,
    /// The native worker must reject aborted or token-truncated generation.
    pub complete: bool,
    pub segments: Vec<MossSegment>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ValidatedSegment {
    pub start_ms: u64,
    pub end_ms: u64,
    pub speaker_id: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdaptedTranscript {
    pub segments: Vec<ValidatedSegment>,
    pub unknown_segments: usize,
    pub conflicting_speaker_segments: usize,
    pub clipped_tail_ms: u64,
    pub coalesced_source_segments: usize,
    /// Present only for the explicit chronological policy. Retained v1/v2
    /// adaptation serialization keeps its original fields and order.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chronology: Option<ChronologicalProvenance>,
}

impl MossResponse {
    pub fn adapt(&self, request: &MossRequest) -> Result<AdaptedTranscript, ProtocolError> {
        request.validate()?;
        if self.schema_version != PROTOCOL_VERSION
            || self.recording_id != request.recording_id
            || self.runtime_id != request.runtime_id
            || self.model_id != request.model_id
            || self.model_sha256 != request.model_sha256
            || self.audio_sha256 != request.audio_sha256
            || self.timing_policy != request.timing_policy
            || !self.complete
            || self.segments.is_empty()
            || self.segments.len() > MAX_SEGMENTS
        {
            return Err(ProtocolError("invalid_response"));
        }
        let permutation = if request.timing_policy == CHRONOLOGICAL_TIMING_POLICY_V3 {
            Some(chronology::source_index_permutation(
                &self.segments,
                request,
            )?)
        } else {
            None
        };
        let mut output = AdaptedTranscript {
            segments: Vec::with_capacity(self.segments.len()),
            unknown_segments: 0,
            conflicting_speaker_segments: 0,
            clipped_tail_ms: 0,
            coalesced_source_segments: 0,
            chronology: permutation.as_ref().map(|indices| ChronologicalProvenance {
                source_index_permutation: indices.clone(),
                output_source_indices: Vec::with_capacity(indices.len()),
            }),
        };
        let mut last_start = 0;
        let mut last_end = BTreeMap::<u32, u64>::new();
        let mut last_unknown_end = 0;
        let mut text_bytes = 0_usize;
        for index in 0..self.segments.len() {
            let source_index = permutation.as_ref().map_or(index, |indices| indices[index]);
            let segment = &self.segments[source_index];
            let start =
                u64::try_from(segment.start_ms).map_err(|_| ProtocolError("invalid_timing"))?;
            let mut end =
                u64::try_from(segment.end_ms).map_err(|_| ProtocolError("invalid_timing"))?;
            if start < last_start || end <= start || start >= request.audio_duration_ms {
                return Err(ProtocolError("invalid_timing"));
            }
            if end > request.audio_duration_ms {
                let overrun = end - request.audio_duration_ms;
                if index + 1 != self.segments.len() || overrun > MAX_TAIL_OVERRUN_MS {
                    return Err(ProtocolError("invalid_timing"));
                }
                output.clipped_tail_ms = overrun;
                end = request.audio_duration_ms;
            }
            if segment.speaker_id > MAX_SPEAKERS
                || segment.text.trim().is_empty()
                || segment.text.contains('\0')
                || segment.text.len() > 64 * 1024
            {
                return Err(ProtocolError("invalid_segment"));
            }
            text_bytes = text_bytes
                .checked_add(segment.text.len())
                .ok_or(ProtocolError("response_too_large"))?;
            if text_bytes > MAX_TEXT_BYTES {
                return Err(ProtocolError("response_too_large"));
            }
            let mut speaker = (segment.speaker_id != 0).then_some(segment.speaker_id);
            if matches!(
                request.timing_policy.as_str(),
                COALESCING_TIMING_POLICY_V2 | CHRONOLOGICAL_TIMING_POLICY_V3
            ) {
                if let Some(previous) = output.segments.last_mut() {
                    let joined_bytes = previous.text.len() + 1 + segment.text.len();
                    if speaker.is_some()
                        && previous.speaker_id == speaker
                        && start < previous.end_ms
                        && joined_bytes <= 64 * 1024
                    {
                        // Only adjacent turns in the selected policy's order
                        // with the SAME model-supplied identity can coalesce.
                        // Preserve joined transcript
                        // bytes/order and the exact interval union; never cross
                        // another speaker's turn or guess an unknown identity.
                        text_bytes = text_bytes
                            .checked_add(1)
                            .ok_or(ProtocolError("response_too_large"))?;
                        if text_bytes > MAX_TEXT_BYTES {
                            return Err(ProtocolError("response_too_large"));
                        }
                        previous.text.push(' ');
                        previous.text.push_str(&segment.text);
                        previous.end_ms = previous.end_ms.max(end);
                        last_end.insert(segment.speaker_id, previous.end_ms);
                        last_start = start;
                        output.coalesced_source_segments += 1;
                        if let Some(chronology) = &mut output.chronology {
                            chronology
                                .output_source_indices
                                .last_mut()
                                .ok_or(ProtocolError("invalid_timing"))?
                                .push(source_index);
                        }
                        continue;
                    }
                }
            }
            if let Some(id) = speaker {
                if last_end.get(&id).is_some_and(|previous| start < *previous) {
                    speaker = None;
                    output.conflicting_speaker_segments += 1;
                } else {
                    last_end.insert(id, end);
                }
            }
            if speaker.is_none() {
                // Do not fabricate extra slots to make overlapping unknowns
                // fit the canonical transcript. Fail adaptation explicitly;
                // the App retains the original recording for recovery.
                if start < last_unknown_end {
                    return Err(ProtocolError("ambiguous_unknown_timing"));
                }
                last_unknown_end = end;
                output.unknown_segments += 1;
            }
            last_start = start;
            output.segments.push(ValidatedSegment {
                start_ms: start,
                end_ms: end,
                speaker_id: speaker,
                text: segment.text.clone(),
            });
            if let Some(chronology) = &mut output.chronology {
                chronology.output_source_indices.push(vec![source_index]);
            }
        }
        Ok(output)
    }
}

pub fn decode_request(bytes: &[u8]) -> Result<MossRequest, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_REQUEST_BYTES {
        return Err(ProtocolError("request_too_large"));
    }
    let request: MossRequest =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError("invalid_json"))?;
    request.validate()?;
    Ok(request)
}

pub fn decode_response(bytes: &[u8], request: &MossRequest) -> Result<MossResponse, ProtocolError> {
    if bytes.is_empty() || bytes.len() > MAX_RESPONSE_BYTES {
        return Err(ProtocolError("response_too_large"));
    }
    let response: MossResponse =
        serde_json::from_slice(bytes).map_err(|_| ProtocolError("invalid_json"))?;
    response.adapt(request)?;
    Ok(response)
}

pub fn encode_request(request: &MossRequest) -> Result<Vec<u8>, ProtocolError> {
    request.validate()?;
    encode_bounded(request, MAX_REQUEST_BYTES)
}

pub fn encode_response(
    response: &MossResponse,
    request: &MossRequest,
) -> Result<Vec<u8>, ProtocolError> {
    response.adapt(request)?;
    encode_bounded(response, MAX_RESPONSE_BYTES)
}

fn encode_bounded(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, ProtocolError> {
    let mut output = serde_json::to_vec(value).map_err(|_| ProtocolError("invalid_json"))?;
    output.push(b'\n');
    if output.len() > limit {
        return Err(ProtocolError("frame_too_large"));
    }
    Ok(output)
}

fn valid_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_audio_path(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 2048
        && !value.contains(['\\', '\0'])
        && !value.starts_with('/')
        && value
            .split('/')
            .all(|part| !part.is_empty() && part != "." && part != "..")
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
        && Path::new(value)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| matches!(ext.to_ascii_lowercase().as_str(), "wav" | "m4a" | "mp3"))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProtocolError(pub &'static str);

impl std::fmt::Display for ProtocolError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.0)
    }
}

impl std::error::Error for ProtocolError {}

#[cfg(test)]
mod tests;
