use std::{
    collections::HashSet,
    path::{Component, Path},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
    bounded_json, digest, error, moss, speakers, valid_hash, LocalMossError,
    COMPOSED_MAPPING_POLICY, FRAMES_PER_MS, LOCAL_MOSS_PLAN_VERSION, MAX_PLAN_BYTES,
    MAX_SOURCE_DURATION_DRIFT_MS,
};
use crate::processing::local_whisper::{
    encode_diarization_request, LocalDiarizationRequest, LOCAL_DIARIZATION_SPEAKERKIT_FILES,
    LOCAL_DIARIZATION_SPEAKERKIT_PRESET, LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAudioIdentity {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub duration_ms: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossWindowRequestSpec {
    pub index: usize,
    pub start_frame: u64,
    pub end_frame: u64,
    pub request: moss::MossRequest,
}

/// Persist this DTO, then re-enter through LocalMossPlan::new/from_json.
/// Original source identity remains distinct from each prepared PCM request.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalMossPlanSpec {
    pub schema_version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::deserialize_mapping_policy"
    )]
    pub mapping_policy: Option<String>,
    pub recording_id: Uuid,
    pub source: SourceAudioIdentity,
    pub pcm_sample_rate: u64,
    pub pcm_source_frames: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pcm_quantization_policy: Option<String>,
    pub window_policy: String,
    pub windows: Vec<MossWindowRequestSpec>,
    pub diarization_request: LocalDiarizationRequest,
}

#[derive(Clone, Debug)]
pub struct ValidatedMossWindowRequest {
    spec: MossWindowRequestSpec,
    request_bytes: Vec<u8>,
    request_sha256: String,
    start_ms: u64,
    end_ms: u64,
}

impl ValidatedMossWindowRequest {
    pub fn index(&self) -> usize {
        self.spec.index
    }
    pub fn start_frame(&self) -> u64 {
        self.spec.start_frame
    }
    pub fn end_frame(&self) -> u64 {
        self.spec.end_frame
    }
    pub fn start_ms(&self) -> u64 {
        self.start_ms
    }
    pub fn end_ms(&self) -> u64 {
        self.end_ms
    }
    pub fn request(&self) -> &moss::MossRequest {
        &self.spec.request
    }
    /// Dispatch these exact bytes and retain this hash in the durable receipt.
    pub fn request_bytes(&self) -> &[u8] {
        &self.request_bytes
    }
    pub fn request_sha256(&self) -> &str {
        &self.request_sha256
    }
}

#[derive(Clone, Debug)]
pub struct LocalMossPlan {
    spec: LocalMossPlanSpec,
    plan_bytes: Vec<u8>,
    plan_sha256: String,
    windows: Vec<ValidatedMossWindowRequest>,
    diarization_request_bytes: Vec<u8>,
    diarization_request_sha256: String,
    requested_language: Option<String>,
}

impl LocalMossPlan {
    pub fn new(spec: LocalMossPlanSpec) -> Result<Self, LocalMossError> {
        if spec.schema_version != LOCAL_MOSS_PLAN_VERSION {
            return Err(error("legacy_moss_plan_requires_reprepare"));
        }
        Self::validate_spec(spec)
    }

    /// A retained preparation checkpoint may explicitly pin schema 2. Never
    /// replace that pin with the current creation defaults after a restart.
    pub(in crate::processing) fn new_for_preparation(
        spec: LocalMossPlanSpec,
    ) -> Result<Self, LocalMossError> {
        let plan = Self::validate_spec(spec)?;
        plan.require_executable()?;
        Ok(plan)
    }

    fn validate_spec(spec: LocalMossPlanSpec) -> Result<Self, LocalMossError> {
        if !match (spec.schema_version, spec.mapping_policy.as_deref()) {
            (1 | 2, None) => true,
            (LOCAL_MOSS_PLAN_VERSION, Some(policy)) => {
                matches!(policy, speakers::POLICY | COMPOSED_MAPPING_POLICY)
            }
            _ => false,
        } || spec.recording_id.is_nil()
            || spec.pcm_sample_rate != moss::windows::SAMPLE_RATE
            || spec.pcm_source_frames == 0
            || spec.pcm_source_frames >= moss::windows::MAX_SOURCE_FRAMES
            || spec
                .pcm_quantization_policy
                .as_deref()
                .is_some_and(|policy| policy != echowall_local_audio::PCM_QUANTIZATION_POLICY)
            || spec.window_policy != moss::windows::QUIET_WINDOW_POLICY
            || spec.windows.is_empty()
            || spec.windows.len() > moss::windows::MAX_WINDOWS
            || spec
                .pcm_source_frames
                .div_ceil(FRAMES_PER_MS)
                .abs_diff(spec.source.duration_ms)
                > MAX_SOURCE_DURATION_DRIFT_MS
            || !owned_audio_path(&spec.source.relative_path, spec.recording_id)
            || !valid_hash(&spec.source.sha256)
        {
            return Err(error("local_moss_plan_invalid"));
        }
        let diarization = &spec.diarization_request;
        diarization
            .validate()
            .map_err(|_| error("local_moss_diarization_request_invalid"))?;
        if diarization.recording_id != spec.recording_id
            || diarization.pack_id != "speakerkit-v1"
            || !(diarization.quality_preset == LOCAL_DIARIZATION_SPEAKERKIT_PRESET
                || (spec.schema_version >= 3
                    && diarization.quality_preset
                        == LOCAL_DIARIZATION_SPEAKERKIT_TAIL_CONTEXT_PRESET))
            || if spec.schema_version == 1 {
                diarization.audio_relative_path != spec.source.relative_path
            } else {
                format!(
                    "inbox/{}/{}",
                    spec.recording_id, diarization.audio_relative_path
                ) != spec.source.relative_path
            }
            || diarization.audio_sha256 != spec.source.sha256
            || diarization.audio_size_bytes != spec.source.size_bytes
            || diarization.audio_duration_ms != spec.source.duration_ms
            || diarization.model_files.len() != LOCAL_DIARIZATION_SPEAKERKIT_FILES.len()
            || diarization
                .model_files
                .iter()
                .zip(LOCAL_DIARIZATION_SPEAKERKIT_FILES)
                .any(|(file, expected)| file.relative_path != expected)
        {
            return Err(error("local_moss_diarization_identity_mismatch"));
        }
        let diarization_request_bytes = encode_diarization_request(diarization)
            .map_err(|_| error("local_moss_diarization_request_invalid"))?;
        let requested_language = spec.windows[0].request.language.clone();
        let mut paths = HashSet::new();
        let mut windows = Vec::with_capacity(spec.windows.len());
        let mut last_frame = 0;
        for (index, window) in spec.windows.iter().enumerate() {
            let frames = window
                .end_frame
                .checked_sub(window.start_frame)
                .ok_or_else(|| error("local_moss_window_invalid"))?;
            if window.index != index
                || window.start_frame != last_frame
                || frames == 0
                || frames > moss::windows::MAX_WINDOW_FRAMES
                || window.end_frame > spec.pcm_source_frames
                || !window
                    .start_frame
                    .is_multiple_of(moss::windows::QUIET_CUT_GRID_FRAMES as u64)
                || (index + 1 != spec.windows.len()
                    && (frames
                        < moss::windows::MAX_WINDOW_FRAMES - moss::windows::QUIET_LOOKBACK_FRAMES
                        || !window
                            .end_frame
                            .is_multiple_of(moss::windows::QUIET_CUT_GRID_FRAMES as u64)))
            {
                return Err(error("local_moss_window_invalid"));
            }
            let request = &window.request;
            request
                .validate()
                .map_err(|_| error("local_moss_window_request_invalid"))?;
            if request.recording_id != spec.recording_id
                || !(request.timing_policy == moss::COALESCING_TIMING_POLICY_V2
                    || (spec.schema_version == LOCAL_MOSS_PLAN_VERSION
                        && request.timing_policy == moss::CHRONOLOGICAL_TIMING_POLICY_V3))
                || request.timing_policy != spec.windows[0].request.timing_policy
                || request.audio_duration_ms != frames.div_ceil(FRAMES_PER_MS)
                || request.language != requested_language
                || request
                    .audio_relative_path
                    .eq_ignore_ascii_case(&spec.source.relative_path)
                || !Path::new(&request.audio_relative_path)
                    .extension()
                    .and_then(|value| value.to_str())
                    .is_some_and(|value| value.eq_ignore_ascii_case("wav"))
                || !paths.insert(request.audio_relative_path.to_ascii_lowercase())
            {
                return Err(error("local_moss_window_request_mismatch"));
            }
            let request_bytes = moss::encode_request(request)
                .map_err(|_| error("local_moss_window_request_invalid"))?;
            windows.push(ValidatedMossWindowRequest {
                spec: window.clone(),
                request_sha256: digest(&request_bytes),
                request_bytes,
                start_ms: window.start_frame / FRAMES_PER_MS,
                end_ms: window.end_frame.div_ceil(FRAMES_PER_MS),
            });
            last_frame = window.end_frame;
        }
        if last_frame != spec.pcm_source_frames {
            return Err(error("local_moss_window_plan_incomplete"));
        }
        let plan_bytes = bounded_json(&spec, MAX_PLAN_BYTES)?;
        Ok(Self {
            plan_sha256: digest(&plan_bytes),
            plan_bytes,
            windows,
            diarization_request_sha256: digest(&diarization_request_bytes),
            diarization_request_bytes,
            requested_language,
            spec,
        })
    }

    pub fn from_json(bytes: &[u8]) -> Result<Self, LocalMossError> {
        if bytes.is_empty() || bytes.len() > MAX_PLAN_BYTES {
            return Err(error("local_moss_plan_bytes_invalid"));
        }
        let spec =
            serde_json::from_slice(bytes).map_err(|_| error("local_moss_plan_json_invalid"))?;
        let mut plan = Self::validate_spec(spec)?;
        // Retained evidence is byte identity, not merely equivalent JSON.
        // Never rewrite retained bodies through current creation defaults.
        plan.plan_bytes = bytes.to_vec();
        plan.plan_sha256 = digest(bytes);
        Ok(plan)
    }
    pub fn require_executable(&self) -> Result<(), LocalMossError> {
        if !matches!(self.spec.schema_version, 2 | LOCAL_MOSS_PLAN_VERSION) {
            return Err(error("legacy_moss_plan_requires_reprepare"));
        }
        Ok(())
    }
    pub fn mapping_policy(&self) -> &'static str {
        match self.spec.mapping_policy.as_deref() {
            Some(COMPOSED_MAPPING_POLICY) => COMPOSED_MAPPING_POLICY,
            // Validation restricts legacy plans to an absent field and v3
            // plans to one of the two recognized explicit policies.
            _ => speakers::POLICY,
        }
    }
    pub fn adaptation_policy(&self) -> &'static str {
        // Validation fixes one recognized policy for the entire immutable plan.
        match self.spec.windows[0].request.timing_policy.as_str() {
            moss::CHRONOLOGICAL_TIMING_POLICY_V3 => moss::CHRONOLOGICAL_TIMING_POLICY_V3,
            _ => moss::COALESCING_TIMING_POLICY_V2,
        }
    }
    pub fn spec(&self) -> &LocalMossPlanSpec {
        &self.spec
    }
    pub fn plan_bytes(&self) -> &[u8] {
        &self.plan_bytes
    }
    pub fn plan_sha256(&self) -> &str {
        &self.plan_sha256
    }
    pub fn windows(&self) -> &[ValidatedMossWindowRequest] {
        &self.windows
    }
    pub fn diarization_request(&self) -> &LocalDiarizationRequest {
        &self.spec.diarization_request
    }
    pub fn diarization_request_bytes(&self) -> &[u8] {
        &self.diarization_request_bytes
    }
    pub fn diarization_request_sha256(&self) -> &str {
        &self.diarization_request_sha256
    }
    pub fn requested_language(&self) -> Option<&str> {
        self.requested_language.as_deref()
    }
    pub fn pcm_duration_ms_ceil(&self) -> u64 {
        self.spec.pcm_source_frames.div_ceil(FRAMES_PER_MS)
    }
    /// Both identities must be within100ms; this bound does not extend any
    /// anchor or response interval. It accommodates a floored container end.
    pub fn timeline_duration_ms(&self) -> u64 {
        self.spec
            .source
            .duration_ms
            .max(self.pcm_duration_ms_ceil())
    }
    pub fn final_timestamp_round_up_frames(&self) -> u64 {
        (FRAMES_PER_MS - self.spec.pcm_source_frames % FRAMES_PER_MS) % FRAMES_PER_MS
    }
}

fn owned_audio_path(value: &str, recording_id: Uuid) -> bool {
    value.len() <= 2048
        && value.starts_with(&format!("inbox/{recording_id}/"))
        && !value.contains(['\\', '\0'])
        && value
            .split('/')
            .all(|part| !matches!(part, "" | "." | ".."))
        && Path::new(value)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
}
