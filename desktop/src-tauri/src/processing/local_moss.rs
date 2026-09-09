//! Pure App boundary for immutable MOSS window requests and canonical output.
//!
//! The caller verifies source/prepared audio and model bytes, dispatches workers,
//! and durably stores response sidecars. This module binds their declarations
//! and receipt hashes; it performs no I/O, launching, routing, or model attestation.
//! MOSS uses its own overlapping-segment protocol, never LocalWhisperResponse.

use std::io::Write;

use echowall_local_moss_protocol::{self as moss, speakers, ValidatedSegment};
use serde::Serialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

mod bundle;
mod mapping;
mod plan;
#[cfg(test)]
mod tests;

pub use bundle::{
    ChronologicalWindowEvidence, CompleteMossResponses, ResponseBinding, ResponseReceipt,
    ValidatedMossWindowResponse, ValidatedSpeakerKitResponse,
};
pub use mapping::{MossMappingEvidence, NativeSlotMappingEvidence};
pub use plan::{
    LocalMossPlan, LocalMossPlanSpec, MossWindowRequestSpec, SourceAudioIdentity,
    ValidatedMossWindowRequest,
};

// v3 pins the App mapping policy. v2 retains its implicit graph-v3 policy;
// v1 remains readable for retained evidence but is not executable.
pub const LOCAL_MOSS_PLAN_VERSION: u32 = 3;
pub const COMPOSED_MAPPING_POLICY: &str = "single-window-native-multi-window-graph3-v1";
pub const MAX_PLAN_BYTES: usize = 1024 * 1024;
pub const MAX_ARCHIVE_SEGMENTS: usize = 10_000;
pub const MAX_ARCHIVE_JSON_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_BUNDLE_RESPONSE_BYTES: usize = 32 * 1024 * 1024;
/// Matches the decoder's permitted container/decoded-duration disagreement.
pub const MAX_SOURCE_DURATION_DRIFT_MS: u64 = 100;
pub const PCM_TIMESTAMP_POLICY: &str = "frame-exact-final-ceil-ms-v1";
pub const BACKEND: &str = "moss_local";
const FRAMES_PER_MS: u64 = moss::windows::SAMPLE_RATE / 1000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalMossError {
    pub code: &'static str,
}

impl std::fmt::Display for LocalMossError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local MOSS data failed validation")
    }
}

impl std::error::Error for LocalMossError {}

fn error(code: &'static str) -> LocalMossError {
    LocalMossError { code }
}
fn digest(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}
fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// Missing legacy fields remain absent; an explicitly present null is not a
/// policy pin and must not be accepted as a legacy omission.
pub(in crate::processing) fn deserialize_mapping_policy<'de, D>(
    deserializer: D,
) -> Result<Option<String>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    <String as serde::Deserialize>::deserialize(deserializer).map(Some)
}

fn bounded_json(value: &impl Serialize, limit: usize) -> Result<Vec<u8>, LocalMossError> {
    struct Limited {
        bytes: Vec<u8>,
        limit: usize,
    }
    impl Write for Limited {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            if bytes.len() > self.limit.saturating_sub(self.bytes.len()) {
                return Err(std::io::Error::other("json_limit"));
            }
            self.bytes.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut output = Limited {
        bytes: Vec::new(),
        limit,
    };
    serde_json::to_writer(&mut output, value).map_err(|_| error("local_moss_json_limit"))?;
    Ok(output.bytes)
}

#[derive(Clone, Debug, Serialize)]
pub struct MossFinalizationEvidence {
    pub plan_sha256: String,
    pub adaptation_policy: &'static str,
    pub mapping_policy: &'static str,
    pub pcm_timestamp_policy: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pcm_quantization_policy: Option<String>,
    pub pcm_source_frames: u64,
    pub pcm_duration_ms_ceil: u64,
    pub source_container_duration_ms: u64,
    pub validated_timeline_duration_ms: u64,
    /// Timestamp representation only; no PCM samples are inserted or removed.
    pub final_timestamp_round_up_frames: u64,
    pub requested_language: Option<String>,
    pub source_segments: usize,
    pub output_segments: usize,
    pub coalesced_source_segments: usize,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub chronological_windows: Vec<ChronologicalWindowEvidence>,
    pub clipped_tail_ms_sum: u64,
    pub window_receipts: Vec<ResponseReceipt>,
    pub diarization_receipt: ResponseReceipt,
    pub mapping: MossMappingEvidence,
}

#[derive(Clone, Debug)]
pub struct FinalizedMossTranscript {
    transcript: Value,
    json_bytes: Vec<u8>,
    evidence: MossFinalizationEvidence,
}

impl FinalizedMossTranscript {
    pub fn transcript_json(&self) -> &Value {
        &self.transcript
    }
    pub fn json_bytes(&self) -> &[u8] {
        &self.json_bytes
    }
    pub fn evidence(&self) -> &MossFinalizationEvidence {
        &self.evidence
    }
    pub fn into_transcript_json(self) -> Value {
        self.transcript
    }
}

pub fn finalize(
    plan: &LocalMossPlan,
    responses: &CompleteMossResponses,
    anchors: &ValidatedSpeakerKitResponse,
) -> Result<FinalizedMossTranscript, LocalMossError> {
    finalize_with_cancel(plan, responses, anchors, || false)
}

pub fn finalize_with_cancel(
    plan: &LocalMossPlan,
    responses: &CompleteMossResponses,
    anchors: &ValidatedSpeakerKitResponse,
    mut cancelled: impl FnMut() -> bool,
) -> Result<FinalizedMossTranscript, LocalMossError> {
    if cancelled() {
        return Err(error("mapping_cancelled"));
    }
    plan.require_executable()?;
    if responses.plan_sha256 != plan.plan_sha256()
        || anchors.receipt.binding.plan_sha256 != plan.plan_sha256()
        || anchors.receipt.binding.request_sha256 != plan.diarization_request_sha256()
        || responses.windows.len() != plan.windows().len()
    {
        return Err(error("local_moss_bundle_identity_mismatch"));
    }
    let mut assembled = Vec::with_capacity(responses.output_segments);
    let mut mapping_windows = Vec::with_capacity(plan.windows().len());
    let mut previous_start = 0;
    for (window, response) in plan.windows().iter().zip(&responses.windows) {
        if cancelled() {
            return Err(error("mapping_cancelled"));
        }
        if response.receipt.window_index != Some(window.index())
            || response.receipt.binding.request_sha256 != window.request_sha256()
        {
            return Err(error("local_moss_window_identity_mismatch"));
        }
        mapping_windows.push(speakers::Window {
            index: window.index(),
            start_ms: window.start_ms(),
            end_ms: window.end_ms(),
        });
        for segment in &response.adapted.segments {
            let start = window
                .start_ms()
                .checked_add(segment.start_ms)
                .ok_or_else(|| error("local_moss_timing_overflow"))?;
            let end = window
                .start_ms()
                .checked_add(segment.end_ms)
                .ok_or_else(|| error("local_moss_timing_overflow"))?;
            if start < previous_start || start >= end || end > window.end_ms() {
                return Err(error("local_moss_assembled_timing_invalid"));
            }
            previous_start = start;
            assembled.push(ValidatedSegment {
                start_ms: start,
                end_ms: end,
                speaker_id: segment.speaker_id,
                text: segment.text.clone(),
            });
        }
    }
    if assembled.is_empty() || assembled.len() > MAX_ARCHIVE_SEGMENTS {
        return Err(error("local_moss_segment_limit"));
    }
    let timing: Vec<_> = assembled
        .iter()
        .map(|segment| speakers::TimingSegment {
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            speaker: segment
                .speaker_id
                .map(|slot| format!("moss_speaker_{slot}")),
        })
        .collect();
    // Confidence is validated by the diarization protocol but is deliberately
    // absent from the fixed v3 mapping inputs; no new confidence threshold.
    let anchor_timing: Vec<_> = anchors
        .response
        .segments
        .iter()
        .map(|segment| speakers::TimingSegment {
            start_ms: segment.start_ms,
            end_ms: segment.end_ms,
            speaker: Some(format!("local_speaker_{:02}", segment.speaker_slot)),
        })
        .collect();
    let mapping = mapping::reconcile_with_cancel(
        plan,
        &timing,
        &anchor_timing,
        &mapping_windows,
        &mut cancelled,
    )?;
    let mut output = Vec::with_capacity(assembled.len());
    for (segment, assignment) in assembled.iter().zip(mapping.assignments()) {
        let speaker = assignment.as_deref().unwrap_or("local_unknown");
        if speaker.is_empty()
            || speaker.len() > 32
            || !speaker
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
        {
            return Err(error("local_moss_archive_speaker_invalid"));
        }
        output.push(json!({"start_time":segment.start_ms,"end_time":segment.end_ms,
            "speaker":{"id":speaker},"content":segment.text,"stt_backend":BACKEND,"model_id":moss::MODEL_ID,
            "language":plan.requested_language().unwrap_or("auto")}));
    }
    let transcript = Value::Array(output);
    let json_bytes = bounded_json(&transcript, MAX_ARCHIVE_JSON_BYTES)?;
    if cancelled() {
        return Err(error("mapping_cancelled"));
    }
    let evidence = MossFinalizationEvidence {
        plan_sha256: plan.plan_sha256().into(),
        adaptation_policy: plan.adaptation_policy(),
        mapping_policy: plan.mapping_policy(),
        pcm_timestamp_policy: PCM_TIMESTAMP_POLICY,
        pcm_quantization_policy: plan.spec().pcm_quantization_policy.clone(),
        pcm_source_frames: plan.spec().pcm_source_frames,
        pcm_duration_ms_ceil: plan.pcm_duration_ms_ceil(),
        source_container_duration_ms: plan.spec().source.duration_ms,
        validated_timeline_duration_ms: plan.timeline_duration_ms(),
        final_timestamp_round_up_frames: plan.final_timestamp_round_up_frames(),
        requested_language: plan.requested_language().map(str::to_owned),
        source_segments: responses.source_segments,
        output_segments: assembled.len(),
        coalesced_source_segments: responses.coalesced_source_segments,
        chronological_windows: responses
            .windows
            .iter()
            .filter_map(|window| window.chronology().cloned())
            .collect(),
        clipped_tail_ms_sum: responses.clipped_tail_ms_sum,
        window_receipts: responses
            .windows
            .iter()
            .map(|response| response.receipt.clone())
            .collect(),
        diarization_receipt: anchors.receipt.clone(),
        mapping,
    };
    Ok(FinalizedMossTranscript {
        transcript,
        json_bytes,
        evidence,
    })
}
