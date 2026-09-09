//! Durable local selection precedes decoding or derived-file publication.
//! A preparation claim is separate from ready-plan inference claims, so a
//! crashed preparation can resume without ever entering the remote route.

use std::sync::atomic::AtomicBool;

use echowall_local_moss_protocol as moss;
use serde::{Deserialize, Deserializer, Serialize};
use uuid::Uuid;

use super::local_models::{MossCandidateModelPackProof, SpeakerKitCandidateModelPackProof};
use super::local_moss::{LocalMossPlan, SourceAudioIdentity, COMPOSED_MAPPING_POLICY};
use super::local_whisper::{
    LocalDiarizationRequest, LocalModelFileIdentity, LOCAL_DIARIZATION_PROTOCOL_VERSION,
};
use super::moss_artifacts::{ArtifactKind, ArtifactRef, MossArtifacts, OwnerLease};
use super::moss_ledger::MossCheckpoint;
use super::{
    require_state, transition, validate_normalized, LocalSummaryCheckpoint,
    NormalizedArtifactCheckpoint, ProcessingError, ProcessingLedger, ProcessingState,
    ProcessingStore, PublicationBackend, SummaryBackend, TranscriptionBackend,
};

mod driver;
mod files;
#[cfg(test)]
mod tests;

pub use driver::prepare_moss_source;

pub use echowall_local_audio::PCM_QUANTIZATION_POLICY;
pub const PREPARATION_POLICY: &str = "verified-effective-decode-quiet-wav-v1";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ModelPins {
    pack_id: String,
    runtime_id: String,
    model_id: String,
    model_revision: String,
    model_sha256: String,
    model_size_bytes: u64,
    speakerkit_pack_id: String,
    speakerkit_quality_preset: String,
    speakerkit_model_revision: String,
    speakerkit_model_files: Vec<LocalModelFileIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MossPreparationClaim {
    token: Uuid,
    owner_session: Uuid,
    recording_id: Uuid,
    generation: Uuid,
}

impl MossPreparationClaim {
    pub fn token(&self) -> Uuid {
        self.token
    }
    pub fn owner_session(&self) -> Uuid {
        self.owner_session
    }
    pub fn recording_id(&self) -> Uuid {
        self.recording_id
    }
    pub fn generation(&self) -> Uuid {
        self.generation
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreparedMossWindow {
    index: usize,
    start_frame: u64,
    end_frame: u64,
    relative_path: String,
    sha256: String,
    size_bytes: u64,
}

impl PreparedMossWindow {
    pub fn index(&self) -> usize {
        self.index
    }
    pub fn relative_path(&self) -> &str {
        &self.relative_path
    }
    pub fn sha256(&self) -> &str {
        &self.sha256
    }
    pub fn size_bytes(&self) -> u64 {
        self.size_bytes
    }
    pub fn start_frame(&self) -> u64 {
        self.start_frame
    }
    pub fn end_frame(&self) -> u64 {
        self.end_frame
    }

    fn validate(&self, recording: Uuid, generation: Uuid) -> Result<(), ProcessingError> {
        let frames = self
            .end_frame
            .checked_sub(self.start_frame)
            .ok_or_else(|| fail("invalid_moss_preparation"))?;
        if self.index >= moss::windows::MAX_WINDOWS
            || frames == 0
            || frames > moss::windows::MAX_WINDOW_FRAMES
            || self.end_frame >= moss::windows::MAX_SOURCE_FRAMES
            || self.relative_path != window_path(recording, generation, self.index)
            || self.size_bytes != frames * 2 + 44
            || !self
                .start_frame
                .is_multiple_of(moss::windows::QUIET_CUT_GRID_FRAMES as u64)
        {
            return Err(fail("invalid_moss_preparation"));
        }
        validate_normalized(&NormalizedArtifactCheckpoint {
            relative_path: self.relative_path.clone(),
            sha256: self.sha256.clone(),
            size_bytes: self.size_bytes,
        })
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PreparationData {
    schema_version: u32,
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        deserialize_with = "super::local_moss::deserialize_mapping_policy"
    )]
    mapping_policy: Option<String>,
    recording_id: Uuid,
    generation: Uuid,
    source: SourceAudioIdentity,
    language: Option<String>,
    preparation_policy: String,
    window_policy: String,
    pcm_quantization_policy: String,
    timing_policy: String,
    model_pins: ModelPins,
    summary: LocalSummaryCheckpoint,
    prepared_windows: Vec<PreparedMossWindow>,
    active_claim: Option<MossPreparationClaim>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct MossPreparationCheckpoint(PreparationData);

impl<'de> Deserialize<'de> for MossPreparationCheckpoint {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let checkpoint = Self(PreparationData::deserialize(deserializer)?);
        checkpoint
            .validate_internal()
            .map_err(|error| serde::de::Error::custom(error.code))?;
        Ok(checkpoint)
    }
}

impl MossPreparationCheckpoint {
    pub fn new(
        recording_id: Uuid,
        generation: Uuid,
        source: SourceAudioIdentity,
        language: Option<String>,
        model: &MossCandidateModelPackProof,
        speakerkit: &SpeakerKitCandidateModelPackProof,
        summary: LocalSummaryCheckpoint,
    ) -> Result<Self, ProcessingError> {
        let checkpoint = Self(PreparationData {
            schema_version: 2,
            mapping_policy: Some(COMPOSED_MAPPING_POLICY.into()),
            recording_id,
            generation,
            source,
            language,
            preparation_policy: PREPARATION_POLICY.into(),
            window_policy: moss::windows::QUIET_WINDOW_POLICY.into(),
            pcm_quantization_policy: PCM_QUANTIZATION_POLICY.into(),
            timing_policy: moss::CHRONOLOGICAL_TIMING_POLICY_V3.into(),
            model_pins: ModelPins {
                pack_id: model.pack_id.clone(),
                runtime_id: model.runtime_id.clone(),
                model_id: model.model_id.clone(),
                model_revision: model.model_revision.clone(),
                model_sha256: model.model_sha256.clone(),
                model_size_bytes: model.model_size_bytes,
                speakerkit_pack_id: speakerkit.pack_id.clone(),
                speakerkit_quality_preset: speakerkit.quality_preset.clone(),
                speakerkit_model_revision: speakerkit.model_revision.clone(),
                speakerkit_model_files: speakerkit.model_files.clone(),
            },
            summary,
            prepared_windows: Vec::new(),
            active_claim: None,
        });
        checkpoint.validate_internal()?;
        Ok(checkpoint)
    }

    pub fn generation(&self) -> Uuid {
        self.0.generation
    }
    pub fn recording_id(&self) -> Uuid {
        self.0.recording_id
    }
    pub fn source(&self) -> &SourceAudioIdentity {
        &self.0.source
    }
    pub fn language(&self) -> Option<&str> {
        self.0.language.as_deref()
    }
    pub fn summary(&self) -> &LocalSummaryCheckpoint {
        &self.0.summary
    }
    pub fn prepared_windows(&self) -> &[PreparedMossWindow] {
        &self.0.prepared_windows
    }
    pub fn active_claim(&self) -> Option<&MossPreparationClaim> {
        self.0.active_claim.as_ref()
    }

    /// Preparation schema fixes the resulting plan schema before decoding.
    /// Legacy checkpoints must finish as plan 2 even after an App upgrade.
    pub fn plan_schema_version(&self) -> u32 {
        match self.0.schema_version {
            1 => 2,
            _ => 3,
        }
    }

    pub fn mapping_policy(&self) -> &str {
        self.0
            .mapping_policy
            .as_deref()
            .unwrap_or(moss::speakers::POLICY)
    }

    pub fn timing_policy(&self) -> &str {
        &self.0.timing_policy
    }

    fn validate_internal(&self) -> Result<(), ProcessingError> {
        let data = &self.0;
        if !match (data.schema_version, data.mapping_policy.as_deref()) {
            (1, None) => {
                data.model_pins.speakerkit_quality_preset
                    == super::local_whisper::LOCAL_DIARIZATION_SPEAKERKIT_PRESET
                    && data.timing_policy == moss::COALESCING_TIMING_POLICY_V2
            }
            (2, Some(policy)) => {
                matches!(policy, moss::speakers::POLICY | COMPOSED_MAPPING_POLICY)
            }
            _ => false,
        } || data.recording_id.is_nil()
            || data.generation.is_nil()
            || data.source.duration_ms == 0
            || data.source.duration_ms >= 18_000_000
            || data.source.size_bytes == 0
            || data.source.size_bytes >= 512 * 1024 * 1024
            || !source_path(&data.source.relative_path, data.recording_id)
            || (0..moss::windows::MAX_WINDOWS).any(|index| {
                data.source.relative_path.eq_ignore_ascii_case(&window_path(
                    data.recording_id,
                    data.generation,
                    index,
                ))
            })
            || data
                .language
                .as_deref()
                .is_some_and(|language| !matches!(language, "en" | "zh"))
            || data.preparation_policy != PREPARATION_POLICY
            || data.window_policy != moss::windows::QUIET_WINDOW_POLICY
            || data.pcm_quantization_policy != PCM_QUANTIZATION_POLICY
            || !matches!(
                data.timing_policy.as_str(),
                moss::COALESCING_TIMING_POLICY_V2 | moss::CHRONOLOGICAL_TIMING_POLICY_V3
            )
            || data.prepared_windows.len() > moss::windows::MAX_WINDOWS
        {
            return Err(fail("invalid_moss_preparation"));
        }
        validate_normalized(&NormalizedArtifactCheckpoint {
            relative_path: data.source.relative_path.clone(),
            sha256: data.source.sha256.clone(),
            size_bytes: data.source.size_bytes,
        })?;
        self.validate_model_pins()?;
        let mut end = 0;
        for (index, window) in data.prepared_windows.iter().enumerate() {
            window.validate(data.recording_id, data.generation)?;
            if window.index != index || window.start_frame != end {
                return Err(fail("invalid_moss_preparation"));
            }
            end = window.end_frame;
        }
        if let Some(claim) = &data.active_claim {
            if claim.token.is_nil()
                || claim.owner_session.is_nil()
                || claim.recording_id != data.recording_id
                || claim.generation != data.generation
            {
                return Err(fail("invalid_moss_preparation"));
            }
        }
        Ok(())
    }

    fn validate_model_pins(&self) -> Result<(), ProcessingError> {
        let pins = &self.0.model_pins;
        super::local_models::moss::validate_preparation_model_pins(
            &MossCandidateModelPackProof {
                pack_id: pins.pack_id.clone(),
                runtime_id: pins.runtime_id.clone(),
                model_id: pins.model_id.clone(),
                model_revision: pins.model_revision.clone(),
                model_sha256: pins.model_sha256.clone(),
                model_size_bytes: pins.model_size_bytes,
            },
            &SpeakerKitCandidateModelPackProof {
                pack_id: pins.speakerkit_pack_id.clone(),
                quality_preset: pins.speakerkit_quality_preset.clone(),
                model_revision: pins.speakerkit_model_revision.clone(),
                model_files: pins.speakerkit_model_files.clone(),
            },
            &self.0.summary,
        )
        .map_err(|_| fail("invalid_moss_preparation_models"))
    }

    pub fn validate_against(
        &self,
        recording_id: Uuid,
        normalized: &NormalizedArtifactCheckpoint,
    ) -> Result<(), ProcessingError> {
        self.validate_internal()?;
        if self.0.recording_id != recording_id
            || self.0.source.relative_path
                != format!("inbox/{recording_id}/{}", normalized.relative_path)
            || self.0.source.sha256 != normalized.sha256
            || self.0.source.size_bytes != normalized.size_bytes
        {
            return Err(fail("moss_source_identity_mismatch"));
        }
        Ok(())
    }

    fn require_claim(
        &self,
        owner: &OwnerLease,
        claim: &MossPreparationClaim,
    ) -> Result<(), ProcessingError> {
        if self.0.active_claim.as_ref() != Some(claim)
            || claim.owner_session != owner.session_id()
            || claim.generation != self.0.generation
            || claim.recording_id != self.0.recording_id
        {
            return Err(fail("stale_moss_preparation_claim"));
        }
        Ok(())
    }

    fn diarization_request(&self, source: &SourceAudioIdentity) -> LocalDiarizationRequest {
        LocalDiarizationRequest {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: self.0.recording_id,
            pack_id: self.0.model_pins.speakerkit_pack_id.clone(),
            quality_preset: self.0.model_pins.speakerkit_quality_preset.clone(),
            model_files: self.0.model_pins.speakerkit_model_files.clone(),
            audio_relative_path: source
                .relative_path
                .strip_prefix(&format!("inbox/{}/", self.0.recording_id))
                .expect("validated preparation source is recording-owned")
                .to_owned(),
            audio_sha256: source.sha256.clone(),
            audio_size_bytes: source.size_bytes,
            audio_duration_ms: source.duration_ms,
            expected_speaker_count: None,
        }
    }
}

impl ProcessingStore {
    pub fn begin_moss_preparation(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        checkpoint: MossPreparationCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            // Even Uploading without a recorded object may have emitted bytes.
            if ledger.state != ProcessingState::Queued {
                return Err(fail(
                    if ledger.tos_object.is_some()
                        || ledger.miaoji.is_some()
                        || matches!(
                            ledger.state,
                            ProcessingState::Uploading
                                | ProcessingState::Transcribing
                                | ProcessingState::Submitting
                                | ProcessingState::Polling
                                | ProcessingState::SubmitAmbiguous
                                | ProcessingState::CancelingUpload
                                | ProcessingState::CanceledAfterUpload
                        )
                    {
                        "remote_effect_already_started"
                    } else {
                        "invalid_state"
                    },
                ));
            }
            if ledger.tos_object.is_some()
                || ledger.miaoji.is_some()
                || ledger.transcript_json.is_some()
                || ledger.local_moss.is_some()
                || ledger.local_moss_preparation.is_some()
            {
                return Err(fail("checkpoint_conflict"));
            }
            checkpoint.validate_against(recording_id, &ledger.normalized)?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            ledger.transcription_backend = TranscriptionBackend::MossLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = None;
            ledger.local_qwen = None;
            ledger.local_diarization = None;
            ledger.local_summary = Some(checkpoint.summary().clone());
            ledger.local_moss_preparation = Some(checkpoint);
            ledger.transcript_only_accepted = false;
            transition(ledger, ProcessingState::PreparingLocalMoss)?;
            Ok(true)
        })
    }

    pub fn claim_moss_preparation(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
    ) -> Result<MossPreparationClaim, ProcessingError> {
        let ledger = self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::PreparingLocalMoss)?;
            let checkpoint = ledger
                .local_moss_preparation
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            if checkpoint
                .active_claim()
                .is_some_and(|claim| claim.owner_session == owner.session_id())
            {
                return Err(fail("moss_preparation_in_progress"));
            }
            checkpoint.0.active_claim = Some(MossPreparationClaim {
                token: Uuid::new_v4(),
                owner_session: owner.session_id(),
                recording_id,
                generation: checkpoint.generation(),
            });
            Ok(true)
        })?;
        ledger
            .local_moss_preparation
            .and_then(|checkpoint| checkpoint.0.active_claim)
            .ok_or_else(|| fail("missing_checkpoint"))
    }

    pub fn check_moss_preparation_claim(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossPreparationClaim,
    ) -> Result<MossPreparationCheckpoint, ProcessingError> {
        let ledger = self.load(recording_id)?;
        require_state(&ledger, ProcessingState::PreparingLocalMoss)?;
        let checkpoint = ledger
            .local_moss_preparation
            .ok_or_else(|| fail("missing_checkpoint"))?;
        self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
        checkpoint.require_claim(owner, claim)?;
        Ok(checkpoint)
    }

    pub fn record_moss_prepared_window(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossPreparationClaim,
        window: PreparedMossWindow,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::PreparingLocalMoss)?;
            let checkpoint = ledger
                .local_moss_preparation
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim)?;
            window.validate(recording_id, checkpoint.generation())?;
            let root = self
                .root
                .parent()
                .ok_or_else(|| fail("unsafe_storage_layout"))?;
            files::verify_window(root, &window, &AtomicBool::new(false))?;
            if let Some(existing) = checkpoint.0.prepared_windows.get(window.index) {
                return if existing == &window {
                    Ok(false)
                } else {
                    Err(fail("moss_preparation_conflict"))
                };
            }
            if window.index != checkpoint.0.prepared_windows.len()
                || window.start_frame
                    != checkpoint
                        .0
                        .prepared_windows
                        .last()
                        .map_or(0, |previous| previous.end_frame)
            {
                return Err(fail("moss_preparation_conflict"));
            }
            checkpoint.0.prepared_windows.push(window);
            Ok(true)
        })
    }

    pub fn complete_moss_preparation(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossPreparationClaim,
        plan: &LocalMossPlan,
        reference: ArtifactRef,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::PreparingLocalMoss)?;
            let preparation = ledger
                .local_moss_preparation
                .as_ref()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            if plan.adaptation_policy() != preparation.timing_policy() {
                return Err(fail("moss_preparation_conflict"));
            }
            let artifacts =
                self.moss_owned_artifacts(recording_id, preparation.generation(), owner)?;
            preparation.require_claim(owner, claim)?;
            let source = &plan.spec().source;
            if ledger.local_moss.is_some()
                || plan.require_executable().is_err()
                || plan.spec().schema_version != preparation.plan_schema_version()
                || plan.mapping_policy() != preparation.mapping_policy()
                || plan.spec().pcm_quantization_policy.as_deref() != Some(PCM_QUANTIZATION_POLICY)
                || reference.generation != preparation.generation()
                || plan.spec().recording_id != recording_id
                || source.relative_path != preparation.source().relative_path
                || source.sha256 != preparation.source().sha256
                || source.size_bytes != preparation.source().size_bytes
                || plan.windows().len() != preparation.prepared_windows().len()
                || plan.diarization_request() != &preparation.diarization_request(source)
                || ledger.local_summary.as_ref() != Some(preparation.summary())
            {
                return Err(fail("moss_preparation_conflict"));
            }
            let root = self
                .root
                .parent()
                .ok_or_else(|| fail("unsafe_storage_layout"))?;
            files::VerifiedSource::open(root, source, &AtomicBool::new(false))?;
            for (window, prepared) in plan.windows().iter().zip(preparation.prepared_windows()) {
                let request = window.request();
                if window.index() != prepared.index
                    || window.start_frame() != prepared.start_frame
                    || window.end_frame() != prepared.end_frame
                    || request.audio_relative_path != prepared.relative_path
                    || request.audio_sha256 != prepared.sha256
                    || request.audio_size_bytes != prepared.size_bytes
                    || request.language.as_deref() != preparation.language()
                {
                    return Err(fail("moss_preparation_conflict"));
                }
                files::verify_window(root, prepared, &AtomicBool::new(false))?;
            }
            if artifacts
                .read(&reference)
                .map_err(|error| fail(error.code()))?
                != plan.plan_bytes()
            {
                return Err(fail("moss_plan_reference_mismatch"));
            }
            let ready = MossCheckpoint::new(plan, reference)?;
            ready.validate_against(recording_id, &ledger.normalized)?;
            ledger.local_moss = Some(ready);
            ledger.local_moss_preparation = None;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn fail_moss_preparation(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
        claim: &MossPreparationClaim,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::PreparingLocalMoss)?;
            let checkpoint = ledger
                .local_moss_preparation
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.require_claim(owner, claim)?;
            checkpoint.0.active_claim = None;
            transition(ledger, ProcessingState::ProviderFailed)?;
            Ok(true)
        })
    }

    pub fn retry_moss_preparation(
        &self,
        recording_id: Uuid,
        owner: &OwnerLease,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::ProviderFailed)?;
            let checkpoint = ledger
                .local_moss_preparation
                .as_mut()
                .ok_or_else(|| fail("missing_checkpoint"))?;
            self.moss_owned_artifacts(recording_id, checkpoint.generation(), owner)?;
            checkpoint.0.active_claim = None;
            transition(ledger, ProcessingState::PreparingLocalMoss)?;
            Ok(true)
        })
    }
}

fn source_path(path: &str, id: Uuid) -> bool {
    let parts: Vec<_> = path.split('/').collect();
    parts.len() == 4
        && parts[0] == "inbox"
        && parts[1] == id.to_string()
        && matches!(parts[2], "tracks" | "derived")
        && !parts[3].is_empty()
        && !parts[3].contains(['\\', '\0'])
        && !matches!(parts[3], "." | "..")
}
fn window_path(id: Uuid, generation: Uuid, index: usize) -> String {
    format!("inbox/{id}/derived/moss_{generation}_{index}.wav")
}
fn fail(code: &'static str) -> ProcessingError {
    ProcessingError::new(code)
}
