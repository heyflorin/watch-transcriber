//! Durable, provider-agnostic processing effect ledger.
//!
//! Network adapters must persist the corresponding `begin_*` checkpoint before
//! dispatch and persist the result before advancing. This module deliberately
//! owns no credentials or network clients.

pub mod archive;
pub mod commands;
pub mod direct;
pub mod engine;
pub mod local_models;
pub mod local_moss;
pub mod local_whisper;
pub mod local_worker;
pub mod moss_artifacts;
pub mod moss_ledger;
pub mod moss_preparation;
pub mod moss_worker;
pub mod preference;
pub mod providers;
pub mod tos;

#[cfg(test)]
mod quality_matrix;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock, Weak};

use chrono::{DateTime, TimeDelta, Utc};
use echowall_local_qwen_protocol::{
    LocalQwenModelFileIdentity, LocalQwenRequest, LOCAL_QWEN_CHUNK_DURATION_MS,
    LOCAL_QWEN_LEGACY_CHUNK_POLICY, LOCAL_QWEN_PROTOCOL_VERSION, LOCAL_QWEN_SPLIT_SEARCH_MS,
};
use echowall_local_summary_protocol::{
    LocalSummaryRequest, LOCAL_SUMMARY_PROMPT_VERSION, LOCAL_SUMMARY_PROTOCOL_VERSION,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::ingest::envelope::validate_sha256;
use local_whisper::{
    LocalDiarizationRequest, LocalModelFileIdentity, LocalTranscriptBackend, LocalWhisperRequest,
    LocalWhisperResponse, LOCAL_DIARIZATION_LEGACY_PRESET, LOCAL_DIARIZATION_PROTOCOL_VERSION,
    LOCAL_DIARIZATION_QUALITY_PRESET, LOCAL_WHISPER_PROTOCOL_VERSION,
};

const SCHEMA_VERSION: u32 = 1;
const MAX_LEDGER_BYTES: u64 = 32 * 1024 * 1024;
const MAX_JSON_BYTES: usize = 16 * 1024 * 1024;
const MAX_JSON_DEPTH: usize = 64;
const MAX_JSON_ARRAY_ITEMS: usize = 10_000;
const MAX_TARGETS: usize = 32;
const MAX_TEXT: usize = 2_048;

static WRITER_LOCKS: OnceLock<Mutex<HashMap<PathBuf, Weak<Mutex<()>>>>> = OnceLock::new();

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProcessingState {
    Queued,
    Uploading,
    Transcribing,
    LocalTranscribing,
    PreparingLocalMoss,
    Submitting,
    Polling,
    Summarizing,
    SummarySubmitting,
    Publishing,
    Complete,
    SubmitAmbiguous,
    SummaryAmbiguous,
    ProviderFailed,
    PublishFailed,
    PublishAmbiguous,
    PublishConflict,
    CancelingUpload,
    CanceledBeforeUpload,
    CanceledAfterUpload,
    Discarding,
    Discarded,
}

impl ProcessingState {
    pub const fn can_transition_to(self, next: Self) -> bool {
        // Full-local work has no TOS object even after ASR/summary.
        if matches!(next, Self::CanceledBeforeUpload)
            && matches!(
                self,
                Self::Summarizing
                    | Self::SummarySubmitting
                    | Self::SummaryAmbiguous
                    | Self::Publishing
                    | Self::PublishFailed
                    | Self::PublishAmbiguous
                    | Self::PublishConflict
            )
        {
            return true;
        }
        match self {
            Self::Queued => matches!(
                next,
                Self::Uploading
                    | Self::PreparingLocalMoss
                    | Self::LocalTranscribing
                    | Self::CanceledBeforeUpload
                    | Self::Discarding
            ),
            Self::Uploading => matches!(
                next,
                Self::Transcribing
                    | Self::LocalTranscribing
                    | Self::ProviderFailed
                    | Self::CancelingUpload
                    | Self::CanceledAfterUpload
            ),
            Self::Transcribing => matches!(
                next,
                Self::Submitting
                    | Self::LocalTranscribing
                    | Self::ProviderFailed
                    | Self::CanceledAfterUpload
            ),
            Self::PreparingLocalMoss => matches!(
                next,
                Self::LocalTranscribing | Self::ProviderFailed | Self::CanceledBeforeUpload
            ),
            Self::LocalTranscribing => matches!(
                next,
                Self::Summarizing
                    | Self::ProviderFailed
                    | Self::CanceledBeforeUpload
                    | Self::CanceledAfterUpload
            ),
            Self::Submitting => matches!(
                next,
                Self::Polling
                    | Self::SubmitAmbiguous
                    | Self::ProviderFailed
                    | Self::CanceledAfterUpload
            ),
            Self::Polling => matches!(
                next,
                Self::LocalTranscribing
                    | Self::Summarizing
                    | Self::ProviderFailed
                    | Self::CanceledAfterUpload
            ),
            Self::Summarizing => matches!(
                next,
                Self::SummarySubmitting | Self::Publishing | Self::CanceledAfterUpload
            ),
            Self::SummarySubmitting => matches!(
                next,
                Self::Summarizing | Self::SummaryAmbiguous | Self::CanceledAfterUpload
            ),
            Self::Publishing => matches!(
                next,
                Self::Complete
                    | Self::PublishFailed
                    | Self::PublishAmbiguous
                    | Self::PublishConflict
                    | Self::CanceledAfterUpload
            ),
            Self::SubmitAmbiguous => matches!(
                next,
                Self::Transcribing | Self::ProviderFailed | Self::CanceledAfterUpload
            ),
            Self::SummaryAmbiguous => {
                matches!(next, Self::Summarizing | Self::CanceledAfterUpload)
            }
            Self::ProviderFailed => matches!(
                next,
                Self::Uploading
                    | Self::PreparingLocalMoss
                    | Self::Transcribing
                    | Self::LocalTranscribing
                    | Self::Summarizing
                    | Self::CanceledBeforeUpload
                    | Self::CanceledAfterUpload
            ),
            Self::PublishFailed => {
                matches!(next, Self::Publishing | Self::CanceledAfterUpload)
            }
            Self::PublishAmbiguous | Self::PublishConflict => {
                matches!(next, Self::Publishing | Self::CanceledAfterUpload)
            }
            Self::Complete => matches!(next, Self::Summarizing | Self::Publishing),
            Self::CancelingUpload => {
                matches!(next, Self::CanceledBeforeUpload | Self::CanceledAfterUpload)
            }
            Self::CanceledBeforeUpload | Self::CanceledAfterUpload => {
                matches!(next, Self::Discarding)
            }
            Self::Discarding => matches!(next, Self::Discarded),
            Self::Discarded => false,
        }
    }

    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete
                | Self::CanceledBeforeUpload
                | Self::CanceledAfterUpload
                | Self::Discarded
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormalizedArtifactCheckpoint {
    pub relative_path: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TosObjectCheckpoint {
    pub bucket: String,
    pub key: String,
    pub version_id: String,
    pub etag: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MiaojiCheckpoint {
    pub request_id: String,
    pub task_id: Option<String>,
    #[serde(default)]
    pub superseded: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranscriptionBackend {
    #[default]
    MiaojiRemote,
    WhisperLocal,
    QwenLocal,
    MossLocal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SummaryBackend {
    #[default]
    GeminiRemote,
    QwenLocal,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationBackend {
    #[default]
    RemoteArchive,
    LocalArchive,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalWhisperCheckpoint {
    pub model_id: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub audio_duration_ms: u64,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalQwenCheckpoint {
    pub runtime_id: String,
    pub asr_model_id: String,
    pub asr_model_revision: String,
    pub asr_model_files: Vec<LocalQwenModelFileIdentity>,
    pub aligner_model_id: String,
    pub aligner_model_revision: String,
    pub aligner_model_files: Vec<LocalQwenModelFileIdentity>,
    pub audio_duration_ms: u64,
    pub language: Option<String>,
    #[serde(default = "legacy_qwen_chunk_policy")]
    pub chunk_policy: String,
    #[serde(default = "default_qwen_chunk_duration_ms")]
    pub chunk_duration_ms: u64,
    #[serde(default = "default_qwen_split_search_ms")]
    pub split_search_ms: u64,
}

fn legacy_qwen_chunk_policy() -> String {
    LOCAL_QWEN_LEGACY_CHUNK_POLICY.to_owned()
}

const fn default_qwen_chunk_duration_ms() -> u64 {
    LOCAL_QWEN_CHUNK_DURATION_MS
}

const fn default_qwen_split_search_ms() -> u64 {
    LOCAL_QWEN_SPLIT_SEARCH_MS
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalDiarizationCheckpoint {
    pub pack_id: String,
    #[serde(default)]
    pub quality_preset: Option<String>,
    pub model_files: Vec<LocalModelFileIdentity>,
    pub expected_speaker_count: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalSummaryCheckpoint {
    pub model_id: String,
    pub model_sha256: String,
    pub model_size_bytes: u64,
    pub prompt_version: String,
    pub transcript_sha256: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalTranscriptionRequest {
    pub whisper: Option<LocalWhisperRequest>,
    pub qwen: Option<LocalQwenRequest>,
    pub diarization: Option<LocalDiarizationRequest>,
    pub transcript_only_accepted: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryMode {
    Idempotent,
    ReconcileBeforeRetry,
    AtMostOnce,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationTargetState {
    Pending,
    Started,
    Verified,
    Failed,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationProof {
    pub locator: String,
    pub version: String,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationTarget {
    pub id: String,
    pub retry_mode: RetryMode,
    pub required: bool,
    pub state: PublicationTargetState,
    pub proof: Option<PublicationProof>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublicationTargetPlan {
    pub id: String,
    pub retry_mode: RetryMode,
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PublicationCheckpoint {
    pub generation: u64,
    pub targets: Vec<PublicationTarget>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalBackupCheckpoint {
    pub locator: String,
    pub version_id: String,
    pub sha256: String,
    pub size_bytes: u64,
    pub proof_json: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CleanupCheckpoint {
    pub temporary_tos_deleted: bool,
    #[serde(default)]
    pub source_tracks_delete_after: Option<DateTime<Utc>>,
    #[serde(default)]
    pub source_tracks_deleted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProcessingLedger {
    pub schema_version: u32,
    pub revision: u64,
    pub recording_id: Uuid,
    pub state: ProcessingState,
    pub normalized: NormalizedArtifactCheckpoint,
    #[serde(default)]
    pub transcription_backend: TranscriptionBackend,
    #[serde(default)]
    pub summary_backend: SummaryBackend,
    #[serde(default)]
    pub publication_backend: PublicationBackend,
    #[serde(default)]
    pub local_whisper: Option<LocalWhisperCheckpoint>,
    #[serde(default)]
    pub local_qwen: Option<LocalQwenCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_moss: Option<moss_ledger::MossCheckpoint>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub local_moss_preparation: Option<moss_preparation::MossPreparationCheckpoint>,
    #[serde(default)]
    pub local_diarization: Option<LocalDiarizationCheckpoint>,
    #[serde(default)]
    pub local_summary: Option<LocalSummaryCheckpoint>,
    #[serde(default)]
    pub transcript_only_accepted: bool,
    pub tos_object: Option<TosObjectCheckpoint>,
    pub miaoji: Option<MiaojiCheckpoint>,
    pub transcript_json: Option<Value>,
    pub summary_json: Option<Value>,
    pub publication: Option<PublicationCheckpoint>,
    pub canonical_backup: Option<CanonicalBackupCheckpoint>,
    pub cleanup: CleanupCheckpoint,
}

impl ProcessingLedger {
    pub fn object_key(&self) -> String {
        deterministic_object_key(self.recording_id, &self.normalized)
    }

    pub fn local_whisper_request(&self) -> Result<LocalWhisperRequest, ProcessingError> {
        if self.transcription_backend != TranscriptionBackend::WhisperLocal {
            return Err(ProcessingError::new("invalid_state"));
        }
        let checkpoint = self
            .local_whisper
            .as_ref()
            .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
        let request = LocalWhisperRequest {
            schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
            recording_id: self.recording_id,
            model_id: checkpoint.model_id.clone(),
            model_sha256: checkpoint.model_sha256.clone(),
            model_size_bytes: checkpoint.model_size_bytes,
            audio_relative_path: self.normalized.relative_path.clone(),
            audio_sha256: self.normalized.sha256.clone(),
            audio_size_bytes: self.normalized.size_bytes,
            audio_duration_ms: checkpoint.audio_duration_ms,
            language: checkpoint.language.clone(),
        };
        request
            .validate()
            .map_err(|_| ProcessingError::new("invalid_ledger"))?;
        Ok(request)
    }

    pub fn local_qwen_request(&self) -> Result<LocalQwenRequest, ProcessingError> {
        if self.transcription_backend != TranscriptionBackend::QwenLocal {
            return Err(ProcessingError::new("invalid_state"));
        }
        let checkpoint = self
            .local_qwen
            .as_ref()
            .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
        let request = LocalQwenRequest {
            schema_version: LOCAL_QWEN_PROTOCOL_VERSION,
            recording_id: self.recording_id,
            runtime_id: checkpoint.runtime_id.clone(),
            asr_model_id: checkpoint.asr_model_id.clone(),
            asr_model_revision: checkpoint.asr_model_revision.clone(),
            asr_model_files: checkpoint.asr_model_files.clone(),
            aligner_model_id: checkpoint.aligner_model_id.clone(),
            aligner_model_revision: checkpoint.aligner_model_revision.clone(),
            aligner_model_files: checkpoint.aligner_model_files.clone(),
            audio_relative_path: self.normalized.relative_path.clone(),
            audio_sha256: self.normalized.sha256.clone(),
            audio_size_bytes: self.normalized.size_bytes,
            audio_duration_ms: checkpoint.audio_duration_ms,
            language: checkpoint.language.clone(),
            chunk_policy: checkpoint.chunk_policy.clone(),
            chunk_duration_ms: checkpoint.chunk_duration_ms,
            split_search_ms: checkpoint.split_search_ms,
        };
        request
            .validate()
            .map_err(|_| ProcessingError::new("invalid_ledger"))?;
        Ok(request)
    }

    pub fn canonical_local_transcript_request(
        &self,
    ) -> Result<LocalWhisperRequest, ProcessingError> {
        match self.transcription_backend {
            TranscriptionBackend::WhisperLocal => self.local_whisper_request(),
            TranscriptionBackend::QwenLocal => {
                let qwen = self.local_qwen_request()?;
                let model_size_bytes = qwen
                    .asr_model_files
                    .iter()
                    .chain(&qwen.aligner_model_files)
                    .try_fold(0_u64, |total, file| total.checked_add(file.size_bytes))
                    .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
                let request = LocalWhisperRequest {
                    schema_version: LOCAL_WHISPER_PROTOCOL_VERSION,
                    recording_id: self.recording_id,
                    model_id: qwen.asr_model_id.clone(),
                    model_sha256: qwen
                        .model_set_sha256()
                        .map_err(|_| ProcessingError::new("invalid_ledger"))?,
                    model_size_bytes,
                    audio_relative_path: qwen.audio_relative_path.clone(),
                    audio_sha256: qwen.audio_sha256.clone(),
                    audio_size_bytes: qwen.audio_size_bytes,
                    audio_duration_ms: qwen.audio_duration_ms,
                    language: qwen.language.clone(),
                };
                request
                    .validate()
                    .map_err(|_| ProcessingError::new("invalid_ledger"))?;
                Ok(request)
            }
            TranscriptionBackend::MiaojiRemote | TranscriptionBackend::MossLocal => {
                Err(ProcessingError::new("invalid_state"))
            }
        }
    }

    pub fn local_summary_request(
        &self,
        transcript: String,
    ) -> Result<LocalSummaryRequest, ProcessingError> {
        if self.summary_backend != SummaryBackend::QwenLocal {
            return Err(ProcessingError::new("invalid_state"));
        }
        let checkpoint = self
            .local_summary
            .as_ref()
            .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
        let transcript_sha256 = hex::encode(Sha256::digest(transcript.as_bytes()));
        if checkpoint.transcript_sha256.as_deref() != Some(transcript_sha256.as_str()) {
            return Err(ProcessingError::new("checkpoint_conflict"));
        }
        let request = LocalSummaryRequest {
            schema_version: LOCAL_SUMMARY_PROTOCOL_VERSION,
            recording_id: self.recording_id,
            model_id: checkpoint.model_id.clone(),
            model_sha256: checkpoint.model_sha256.clone(),
            model_size_bytes: checkpoint.model_size_bytes,
            prompt_version: checkpoint.prompt_version.clone(),
            transcript_sha256,
            transcript,
        };
        request
            .validate()
            .map_err(|_| ProcessingError::new("invalid_checkpoint"))?;
        Ok(request)
    }

    pub fn local_transcription_request(
        &self,
    ) -> Result<LocalTranscriptionRequest, ProcessingError> {
        let (whisper, qwen, audio_duration_ms) = match self.transcription_backend {
            TranscriptionBackend::WhisperLocal => {
                let request = self.local_whisper_request()?;
                let duration = request.audio_duration_ms;
                (Some(request), None, duration)
            }
            TranscriptionBackend::QwenLocal => {
                let request = self.local_qwen_request()?;
                let duration = request.audio_duration_ms;
                (None, Some(request), duration)
            }
            TranscriptionBackend::MiaojiRemote | TranscriptionBackend::MossLocal => {
                return Err(ProcessingError::new("invalid_state"));
            }
        };
        let diarization =
            self.local_diarization
                .as_ref()
                .map(|checkpoint| LocalDiarizationRequest {
                    schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
                    recording_id: self.recording_id,
                    pack_id: checkpoint.pack_id.clone(),
                    quality_preset: checkpoint
                        .quality_preset
                        .clone()
                        .unwrap_or_else(|| LOCAL_DIARIZATION_LEGACY_PRESET.to_owned()),
                    model_files: checkpoint.model_files.clone(),
                    audio_relative_path: self.normalized.relative_path.clone(),
                    audio_sha256: self.normalized.sha256.clone(),
                    audio_size_bytes: self.normalized.size_bytes,
                    audio_duration_ms,
                    expected_speaker_count: checkpoint.expected_speaker_count,
                });
        if let Some(request) = &diarization {
            request
                .validate()
                .map_err(|_| ProcessingError::new("invalid_ledger"))?;
        }
        if diarization.is_none() && !self.transcript_only_accepted {
            return Err(ProcessingError::new("invalid_ledger"));
        }
        Ok(LocalTranscriptionRequest {
            whisper,
            qwen,
            diarization,
            transcript_only_accepted: self.transcript_only_accepted,
        })
    }

    fn validate(&self) -> Result<(), ProcessingError> {
        if self.schema_version != SCHEMA_VERSION || self.revision == 0 {
            return Err(ProcessingError::new("invalid_ledger"));
        }
        validate_normalized(&self.normalized)?;
        if self.transcription_backend != TranscriptionBackend::MossLocal
            && (self.local_moss.is_some() || self.local_moss_preparation.is_some())
        {
            return Err(ProcessingError::new("invalid_ledger"));
        }
        match self.transcription_backend {
            TranscriptionBackend::MiaojiRemote
                if self.local_whisper.is_some()
                    || self.local_qwen.is_some()
                    || self.local_diarization.is_some()
                    || self.transcript_only_accepted =>
            {
                return Err(ProcessingError::new("invalid_ledger"));
            }
            TranscriptionBackend::WhisperLocal => {
                if self.local_qwen.is_some() {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
                self.local_transcription_request()?;
                if self
                    .miaoji
                    .as_ref()
                    .is_some_and(|miaoji| !miaoji.superseded || miaoji.task_id.is_none())
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
            }
            TranscriptionBackend::QwenLocal => {
                if self.local_whisper.is_some() {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
                self.local_transcription_request()?;
                if self
                    .miaoji
                    .as_ref()
                    .is_some_and(|miaoji| !miaoji.superseded || miaoji.task_id.is_none())
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
            }
            TranscriptionBackend::MossLocal => {
                if self.local_whisper.is_some()
                    || self.local_qwen.is_some()
                    || self.local_diarization.is_some()
                    || self.transcript_only_accepted
                    || self.summary_backend != SummaryBackend::QwenLocal
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
                if let Some(preparation) = &self.local_moss_preparation {
                    if self.local_moss.is_some()
                        || self.tos_object.is_some()
                        || self.miaoji.is_some()
                        || self.transcript_json.is_some()
                        || self.summary_json.is_some()
                        || !matches!(
                            self.state,
                            ProcessingState::PreparingLocalMoss
                                | ProcessingState::ProviderFailed
                                | ProcessingState::CanceledBeforeUpload
                                | ProcessingState::Discarding
                                | ProcessingState::Discarded
                        )
                        || self.local_summary.as_ref() != Some(preparation.summary())
                    {
                        return Err(ProcessingError::new("invalid_ledger"));
                    }
                    preparation.validate_against(self.recording_id, &self.normalized)?;
                } else {
                    if self.state == ProcessingState::PreparingLocalMoss {
                        return Err(ProcessingError::new("invalid_ledger"));
                    }
                    let moss = self
                        .local_moss
                        .as_ref()
                        .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
                    moss.validate_against(self.recording_id, &self.normalized)?;
                    moss.validate_transcript(self.transcript_json.as_ref())?;
                }
                if self
                    .miaoji
                    .as_ref()
                    .is_some_and(|miaoji| !miaoji.superseded || miaoji.task_id.is_none())
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
            }
            TranscriptionBackend::MiaojiRemote => {
                if self.miaoji.as_ref().is_some_and(|miaoji| miaoji.superseded) {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
            }
        }
        match self.summary_backend {
            SummaryBackend::GeminiRemote if self.local_summary.is_some() => {
                return Err(ProcessingError::new("invalid_ledger"));
            }
            SummaryBackend::QwenLocal => {
                let checkpoint = self
                    .local_summary
                    .as_ref()
                    .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
                validate_text(&checkpoint.model_id)?;
                validate_sha256(&checkpoint.model_sha256, "summary.model_sha256")
                    .map_err(|_| ProcessingError::new("invalid_ledger"))?;
                if checkpoint.model_size_bytes == 0
                    || checkpoint.prompt_version != LOCAL_SUMMARY_PROMPT_VERSION
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
                if let Some(digest) = &checkpoint.transcript_sha256 {
                    validate_sha256(digest, "summary.transcript_sha256")
                        .map_err(|_| ProcessingError::new("invalid_ledger"))?;
                }
                if let Some(transcript) = &self.transcript_json {
                    let text = transcript_for_summary(transcript)?;
                    let digest = hex::encode(Sha256::digest(text.as_bytes()));
                    if checkpoint.transcript_sha256.as_deref() != Some(digest.as_str()) {
                        return Err(ProcessingError::new("invalid_ledger"));
                    }
                } else if checkpoint.transcript_sha256.is_some() {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
            }
            SummaryBackend::GeminiRemote => {}
        }
        if self.publication_backend == PublicationBackend::LocalArchive
            && (!matches!(
                self.transcription_backend,
                TranscriptionBackend::WhisperLocal
                    | TranscriptionBackend::QwenLocal
                    | TranscriptionBackend::MossLocal
            ) || self.summary_backend != SummaryBackend::QwenLocal)
        {
            return Err(ProcessingError::new("invalid_ledger"));
        }
        if let Some(object) = &self.tos_object {
            validate_text(&object.bucket)?;
            validate_text(&object.version_id)?;
            validate_text(&object.etag)?;
            validate_sha256(&object.sha256, "tos.sha256")
                .map_err(|_| ProcessingError::new("invalid_ledger"))?;
            if object.key != self.object_key()
                || object.sha256 != self.normalized.sha256
                || object.size_bytes != self.normalized.size_bytes
            {
                return Err(ProcessingError::new("invalid_ledger"));
            }
        }
        if let Some(miaoji) = &self.miaoji {
            validate_text(&miaoji.request_id)?;
            if miaoji.request_id != deterministic_request_id(self.recording_id) {
                return Err(ProcessingError::new("invalid_ledger"));
            }
            if let Some(task_id) = &miaoji.task_id {
                validate_text(task_id)?;
            }
        }
        validate_optional_json(&self.transcript_json)?;
        validate_optional_json(&self.summary_json)?;
        if let Some(publication) = &self.publication {
            validate_publication(publication, &self.normalized)?;
        }
        if let Some(backup) = &self.canonical_backup {
            validate_canonical_backup(backup, &self.normalized)?;
        }
        validate_checkpoint_consistency(self)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResumeAction {
    Upload,
    ReconcileCanceledUpload,
    TranscribeLocal {
        request: Box<LocalTranscriptionRequest>,
    },
    /// A lease and generation-bound claim must be acquired before dispatch.
    TranscribeMoss {
        generation: Uuid,
    },
    PrepareMoss {
        generation: Uuid,
    },
    /// The submit fence and request id are already durable when returned.
    DispatchMiaoji {
        request_id: String,
    },
    PollMiaoji {
        task_id: String,
    },
    Summarize,
    AwaitPublicationPlan,
    PublishTarget {
        generation: u64,
        target_id: String,
    },
    VerifyCanonicalBackup {
        generation: u64,
    },
    Cleanup {
        generation: u64,
    },
    ManualSubmitResolution,
    ManualLocalResolution,
    ManualSummaryResolution,
    ManualPublicationResolution,
    Canceled,
    Done,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetOutcome {
    ExplicitFailure,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessingError {
    pub code: &'static str,
    message: &'static str,
}

impl ProcessingError {
    fn new(code: &'static str) -> Self {
        Self {
            code,
            message: public_error(code),
        }
    }
}

impl std::fmt::Display for ProcessingError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for ProcessingError {}

#[derive(Debug, Clone)]
pub struct ProcessingStore {
    root: PathBuf,
    jobs: PathBuf,
    writer: Arc<Mutex<()>>,
    process_lock: Arc<File>,
}

struct FileUnlockGuard<'a> {
    file: &'a File,
    locked: bool,
}

impl<'a> FileUnlockGuard<'a> {
    fn acquire(file: &'a File) -> Result<Self, ProcessingError> {
        file.try_lock_exclusive()
            .map_err(|_| ProcessingError::new("processing_busy"))?;
        Ok(Self { file, locked: true })
    }

    fn finish(mut self) -> Result<(), ProcessingError> {
        FileExt::unlock(self.file).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        self.locked = false;
        Ok(())
    }
}

impl Drop for FileUnlockGuard<'_> {
    fn drop(&mut self) {
        if self.locked {
            let _ = FileExt::unlock(self.file);
        }
    }
}

impl ProcessingStore {
    /// Opens `<app_data>/processing`. `inbox_root` and `archive_root` are
    /// boundary assertions, not storage inputs.
    pub fn open(
        app_data: &Path,
        inbox_root: &Path,
        archive_root: &Path,
    ) -> Result<Self, ProcessingError> {
        fs::create_dir_all(app_data).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        let app_data =
            fs::canonicalize(app_data).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        let root = app_data.join("processing");
        fs::create_dir_all(&root).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        let root =
            fs::canonicalize(root).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        for forbidden in [inbox_root, archive_root] {
            let forbidden = absolute_path(forbidden)?;
            if root.starts_with(&forbidden) || forbidden.starts_with(&root) {
                return Err(ProcessingError::new("unsafe_storage_layout"));
            }
        }
        let jobs = root.join("jobs");
        fs::create_dir_all(&jobs).map_err(|_| ProcessingError::new("storage_unavailable"))?;
        let process_lock_path = root.join("writer.lock");
        if fs::symlink_metadata(&process_lock_path)
            .is_ok_and(|metadata| metadata.file_type().is_symlink())
        {
            return Err(ProcessingError::new("unsafe_storage_layout"));
        }
        let process_lock = Arc::new(
            OpenOptions::new()
                .create(true)
                .truncate(false)
                .read(true)
                .write(true)
                .open(process_lock_path)
                .map_err(|_| ProcessingError::new("storage_unavailable"))?,
        );
        let writer = process_writer_lock(&root)?;
        Ok(Self {
            root,
            jobs,
            writer,
            process_lock,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn enqueue(
        &self,
        recording_id: Uuid,
        normalized: NormalizedArtifactCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_normalized(&normalized)?;
        self.with_writer(|| match self.load_locked(recording_id)? {
            Some(existing) => {
                if existing.normalized == normalized {
                    Ok(existing)
                } else {
                    Err(ProcessingError::new("recording_collision"))
                }
            }
            None => {
                let ledger = ProcessingLedger {
                    schema_version: SCHEMA_VERSION,
                    revision: 1,
                    recording_id,
                    state: ProcessingState::Queued,
                    normalized,
                    transcription_backend: TranscriptionBackend::MiaojiRemote,
                    summary_backend: SummaryBackend::GeminiRemote,
                    publication_backend: PublicationBackend::RemoteArchive,
                    local_whisper: None,
                    local_qwen: None,
                    local_moss: None,
                    local_moss_preparation: None,
                    local_diarization: None,
                    local_summary: None,
                    transcript_only_accepted: false,
                    tos_object: None,
                    miaoji: None,
                    transcript_json: None,
                    summary_json: None,
                    publication: None,
                    canonical_backup: None,
                    cleanup: CleanupCheckpoint::default(),
                };
                self.persist_locked(&ledger)?;
                Ok(ledger)
            }
        })
    }

    pub fn load(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.with_writer(|| {
            self.load_locked(recording_id)?
                .ok_or_else(|| ProcessingError::new("recording_not_found"))
        })
    }

    pub fn list_recording_ids(&self) -> Result<Vec<Uuid>, ProcessingError> {
        self.with_writer(|| {
            let mut ids = HashSet::new();
            for entry in
                fs::read_dir(&self.jobs).map_err(|_| ProcessingError::new("storage_unavailable"))?
            {
                let entry = entry.map_err(|_| ProcessingError::new("storage_unavailable"))?;
                if entry
                    .file_type()
                    .map_err(|_| ProcessingError::new("storage_unavailable"))?
                    .is_symlink()
                {
                    return Err(ProcessingError::new("invalid_ledger"));
                }
                let name = entry.file_name();
                let Some(name) = name.to_str() else { continue };
                let candidate = if let Some(stem) = name.strip_suffix(".json") {
                    Some(stem)
                } else if name.starts_with('.') && name.ends_with(".tmp") {
                    name[1..].split('.').next()
                } else {
                    None
                };
                if let Some(id) = candidate.and_then(|value| Uuid::parse_str(value).ok()) {
                    ids.insert(id);
                }
            }
            let mut ids: Vec<_> = ids.into_iter().collect();
            ids.sort();
            for id in &ids {
                self.load_locked(*id)?
                    .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
            }
            Ok(ids)
        })
    }

    pub fn select_local_whisper(
        &self,
        recording_id: Uuid,
        model_id: String,
        model_sha256: String,
        model_size_bytes: u64,
        audio_duration_ms: u64,
        language: Option<String>,
    ) -> Result<ProcessingLedger, ProcessingError> {
        let checkpoint = LocalWhisperCheckpoint {
            model_id,
            model_sha256,
            model_size_bytes,
            audio_duration_ms,
            language,
        };
        self.mutate(recording_id, |ledger| {
            if ledger.transcription_backend == TranscriptionBackend::WhisperLocal {
                return if ledger.local_whisper.as_ref() == Some(&checkpoint)
                    && ledger.state == ProcessingState::LocalTranscribing
                {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            if ledger
                .miaoji
                .as_ref()
                .and_then(|miaoji| miaoji.task_id.as_ref())
                .is_some()
                || matches!(
                    ledger.state,
                    ProcessingState::Submitting
                        | ProcessingState::SubmitAmbiguous
                        | ProcessingState::Polling
                )
            {
                return Err(ProcessingError::new("manual_resolution_required"));
            }
            if !matches!(
                ledger.state,
                ProcessingState::Queued
                    | ProcessingState::Uploading
                    | ProcessingState::Transcribing
                    | ProcessingState::ProviderFailed
            ) || ledger.transcript_json.is_some()
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.transcription_backend = TranscriptionBackend::WhisperLocal;
            ledger.summary_backend = SummaryBackend::GeminiRemote;
            ledger.publication_backend = PublicationBackend::RemoteArchive;
            ledger.local_whisper = Some(checkpoint);
            ledger.local_qwen = None;
            ledger.local_diarization = None;
            ledger.local_summary = None;
            ledger.transcript_only_accepted = true;
            ledger.miaoji = None;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn select_full_local(
        &self,
        recording_id: Uuid,
        model_id: String,
        model_sha256: String,
        model_size_bytes: u64,
        audio_duration_ms: u64,
        language: Option<String>,
        diarization_pack_id: String,
        diarization_files: Vec<LocalModelFileIdentity>,
        expected_speaker_count: Option<u32>,
        summary_model_id: String,
        summary_model_sha256: String,
        summary_model_size_bytes: u64,
    ) -> Result<ProcessingLedger, ProcessingError> {
        let whisper = LocalWhisperCheckpoint {
            model_id,
            model_sha256,
            model_size_bytes,
            audio_duration_ms,
            language,
        };
        let diarization = LocalDiarizationCheckpoint {
            pack_id: diarization_pack_id,
            quality_preset: Some(LOCAL_DIARIZATION_QUALITY_PRESET.to_owned()),
            model_files: diarization_files,
            expected_speaker_count,
        };
        let summary = LocalSummaryCheckpoint {
            model_id: summary_model_id,
            model_sha256: summary_model_sha256,
            model_size_bytes: summary_model_size_bytes,
            prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
            transcript_sha256: None,
        };
        self.mutate(recording_id, |ledger| {
            if ledger
                .miaoji
                .as_ref()
                .and_then(|miaoji| miaoji.task_id.as_ref())
                .is_some()
                || matches!(
                    ledger.state,
                    ProcessingState::Submitting
                        | ProcessingState::SubmitAmbiguous
                        | ProcessingState::Polling
                )
            {
                return Err(ProcessingError::new("manual_resolution_required"));
            }
            if ledger.transcription_backend == TranscriptionBackend::WhisperLocal {
                return if ledger.local_whisper.as_ref() == Some(&whisper)
                    && ledger.local_diarization.as_ref() == Some(&diarization)
                    && ledger.local_summary.as_ref() == Some(&summary)
                    && ledger.summary_backend == SummaryBackend::QwenLocal
                    && ledger.publication_backend == PublicationBackend::LocalArchive
                    && !ledger.transcript_only_accepted
                    && ledger.state == ProcessingState::LocalTranscribing
                {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            if !matches!(
                ledger.state,
                ProcessingState::Queued
                    | ProcessingState::Uploading
                    | ProcessingState::Transcribing
                    | ProcessingState::ProviderFailed
            ) || ledger.transcript_json.is_some()
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.transcription_backend = TranscriptionBackend::WhisperLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = Some(whisper);
            ledger.local_qwen = None;
            ledger.local_diarization = Some(diarization);
            ledger.local_summary = Some(summary);
            ledger.transcript_only_accepted = false;
            ledger.miaoji = None;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn select_full_local_qwen(
        &self,
        recording_id: Uuid,
        qwen: LocalQwenCheckpoint,
        diarization: LocalDiarizationCheckpoint,
        summary: LocalSummaryCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger
                .miaoji
                .as_ref()
                .and_then(|miaoji| miaoji.task_id.as_ref())
                .is_some()
                || matches!(
                    ledger.state,
                    ProcessingState::Submitting
                        | ProcessingState::SubmitAmbiguous
                        | ProcessingState::Polling
                )
            {
                return Err(ProcessingError::new("manual_resolution_required"));
            }
            if ledger.transcription_backend == TranscriptionBackend::QwenLocal {
                return if ledger.local_qwen.as_ref() == Some(&qwen)
                    && ledger.local_diarization.as_ref() == Some(&diarization)
                    && ledger.local_summary.as_ref() == Some(&summary)
                    && ledger.summary_backend == SummaryBackend::QwenLocal
                    && ledger.publication_backend == PublicationBackend::LocalArchive
                    && !ledger.transcript_only_accepted
                    && ledger.state == ProcessingState::LocalTranscribing
                {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            if !matches!(
                ledger.state,
                ProcessingState::Queued
                    | ProcessingState::Uploading
                    | ProcessingState::Transcribing
                    | ProcessingState::ProviderFailed
            ) || ledger.transcript_json.is_some()
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.transcription_backend = TranscriptionBackend::QwenLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = None;
            ledger.local_qwen = Some(qwen);
            ledger.local_diarization = Some(diarization);
            ledger.local_summary = Some(summary);
            ledger.transcript_only_accepted = false;
            ledger.miaoji = None;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn take_over_with_full_local(
        &self,
        recording_id: Uuid,
        model_id: String,
        model_sha256: String,
        model_size_bytes: u64,
        audio_duration_ms: u64,
        language: Option<String>,
        diarization_pack_id: String,
        diarization_files: Vec<LocalModelFileIdentity>,
        expected_speaker_count: Option<u32>,
        summary_model_id: String,
        summary_model_sha256: String,
        summary_model_size_bytes: u64,
    ) -> Result<ProcessingLedger, ProcessingError> {
        let whisper = LocalWhisperCheckpoint {
            model_id,
            model_sha256,
            model_size_bytes,
            audio_duration_ms,
            language,
        };
        let diarization = LocalDiarizationCheckpoint {
            pack_id: diarization_pack_id,
            quality_preset: Some(LOCAL_DIARIZATION_QUALITY_PRESET.to_owned()),
            model_files: diarization_files,
            expected_speaker_count,
        };
        let summary = LocalSummaryCheckpoint {
            model_id: summary_model_id,
            model_sha256: summary_model_sha256,
            model_size_bytes: summary_model_size_bytes,
            prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
            transcript_sha256: None,
        };
        self.mutate(recording_id, |ledger| {
            if ledger.transcription_backend == TranscriptionBackend::WhisperLocal {
                return if ledger.local_whisper.as_ref() == Some(&whisper)
                    && ledger.local_diarization.as_ref() == Some(&diarization)
                    && ledger.local_summary.as_ref() == Some(&summary)
                    && ledger.summary_backend == SummaryBackend::QwenLocal
                    && ledger.publication_backend == PublicationBackend::LocalArchive
                    && !ledger.transcript_only_accepted
                    && ledger
                        .miaoji
                        .as_ref()
                        .is_some_and(|miaoji| miaoji.superseded)
                    && ledger.state == ProcessingState::LocalTranscribing
                {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            require_state(ledger, ProcessingState::Polling)?;
            if ledger.transcript_json.is_some()
                || ledger
                    .miaoji
                    .as_ref()
                    .is_none_or(|miaoji| miaoji.task_id.is_none() || miaoji.superseded)
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.miaoji.as_mut().expect("validated above").superseded = true;
            ledger.transcription_backend = TranscriptionBackend::WhisperLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = Some(whisper);
            ledger.local_qwen = None;
            ledger.local_diarization = Some(diarization);
            ledger.local_summary = Some(summary);
            ledger.transcript_only_accepted = false;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn take_over_with_full_local_qwen(
        &self,
        recording_id: Uuid,
        qwen: LocalQwenCheckpoint,
        diarization: LocalDiarizationCheckpoint,
        summary: LocalSummaryCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.transcription_backend == TranscriptionBackend::QwenLocal {
                return if ledger.local_qwen.as_ref() == Some(&qwen)
                    && ledger.local_diarization.as_ref() == Some(&diarization)
                    && ledger.local_summary.as_ref() == Some(&summary)
                    && ledger.summary_backend == SummaryBackend::QwenLocal
                    && ledger.publication_backend == PublicationBackend::LocalArchive
                    && !ledger.transcript_only_accepted
                    && ledger
                        .miaoji
                        .as_ref()
                        .is_some_and(|miaoji| miaoji.superseded)
                    && ledger.state == ProcessingState::LocalTranscribing
                {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            require_state(ledger, ProcessingState::Polling)?;
            if ledger.transcript_json.is_some()
                || ledger
                    .miaoji
                    .as_ref()
                    .is_none_or(|miaoji| miaoji.task_id.is_none() || miaoji.superseded)
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.miaoji.as_mut().expect("validated above").superseded = true;
            ledger.transcription_backend = TranscriptionBackend::QwenLocal;
            ledger.summary_backend = SummaryBackend::QwenLocal;
            ledger.publication_backend = PublicationBackend::LocalArchive;
            ledger.local_whisper = None;
            ledger.local_qwen = Some(qwen);
            ledger.local_diarization = Some(diarization);
            ledger.local_summary = Some(summary);
            ledger.transcript_only_accepted = false;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn accept_local_transcript_only(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::ProviderFailed)?;
            if !matches!(
                ledger.transcription_backend,
                TranscriptionBackend::WhisperLocal | TranscriptionBackend::QwenLocal
            ) || ledger.local_whisper.is_none() && ledger.local_qwen.is_none()
                || ledger.local_diarization.is_none()
                || ledger.transcript_only_accepted
                || ledger.transcript_json.is_some()
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.local_diarization = None;
            ledger.transcript_only_accepted = true;
            ledger.local_transcription_request()?;
            transition(ledger, ProcessingState::LocalTranscribing)?;
            Ok(true)
        })
    }

    pub fn checkpoint_local_whisper_transcript(
        &self,
        recording_id: Uuid,
        response: LocalWhisperResponse,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            let local_request = ledger.local_transcription_request()?;
            let request = ledger.canonical_local_transcript_request()?;
            if ledger.transcription_backend == TranscriptionBackend::WhisperLocal
                && local_request.diarization.is_some()
                && !local_request.transcript_only_accepted
                && response
                    .segments
                    .iter()
                    .filter(|segment| {
                        segment
                            .speaker_id
                            .as_deref()
                            .is_some_and(|speaker| speaker != "local_unknown")
                    })
                    .count()
                    .saturating_mul(100)
                    < response.segments.len().saturating_mul(95)
            {
                return Err(ProcessingError::new("diarization_incomplete"));
            }
            let transcript_backend = match ledger.transcription_backend {
                TranscriptionBackend::WhisperLocal => LocalTranscriptBackend::WhisperLocal,
                TranscriptionBackend::QwenLocal => LocalTranscriptBackend::QwenLocal,
                TranscriptionBackend::MiaojiRemote | TranscriptionBackend::MossLocal => {
                    return Err(ProcessingError::new("invalid_checkpoint"));
                }
            };
            let transcript = response
                .transcript_json_for_backend(&request, transcript_backend)
                .map_err(|_| ProcessingError::new("invalid_checkpoint"))?;
            validate_json(&transcript)?;
            if ledger.summary_backend == SummaryBackend::QwenLocal {
                let text = transcript_for_summary(&transcript)?;
                let digest = hex::encode(Sha256::digest(text.as_bytes()));
                let checkpoint = ledger
                    .local_summary
                    .as_mut()
                    .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
                match checkpoint.transcript_sha256.as_deref() {
                    Some(existing) if existing != digest => {
                        return Err(ProcessingError::new("checkpoint_conflict"));
                    }
                    Some(_) => {}
                    None => checkpoint.transcript_sha256 = Some(digest),
                }
            }
            if let Some(existing) = &ledger.transcript_json {
                return if existing == &transcript {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            require_state(ledger, ProcessingState::LocalTranscribing)?;
            ledger.transcript_json = Some(transcript);
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn mark_local_whisper_failed(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::ProviderFailed {
                return Ok(false);
            }
            if !matches!(
                ledger.transcription_backend,
                TranscriptionBackend::WhisperLocal | TranscriptionBackend::QwenLocal
            ) {
                return Err(ProcessingError::new("invalid_state"));
            }
            require_state(ledger, ProcessingState::LocalTranscribing)?;
            transition(ledger, ProcessingState::ProviderFailed)?;
            Ok(true)
        })
    }

    pub fn begin_upload(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::Uploading {
                return Ok(false);
            }
            transition(ledger, ProcessingState::Uploading)?;
            Ok(true)
        })
    }

    pub fn checkpoint_tos_object(
        &self,
        recording_id: Uuid,
        bucket: String,
        version_id: String,
        etag: String,
        sha256: String,
        size_bytes: u64,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_text(&bucket)?;
        validate_text(&version_id)?;
        validate_text(&etag)?;
        self.mutate(recording_id, |ledger| {
            let canceling_upload = ledger.state == ProcessingState::CancelingUpload;
            let checkpoint = TosObjectCheckpoint {
                bucket,
                key: ledger.object_key(),
                version_id,
                etag,
                sha256,
                size_bytes,
            };
            if checkpoint.sha256 != ledger.normalized.sha256
                || checkpoint.size_bytes != ledger.normalized.size_bytes
            {
                return Err(ProcessingError::new("artifact_mismatch"));
            }
            if let Some(existing) = &ledger.tos_object {
                return if existing == &checkpoint {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            if !matches!(
                ledger.state,
                ProcessingState::Uploading | ProcessingState::CancelingUpload
            ) {
                return Err(ProcessingError::new("invalid_state"));
            }
            ledger.tos_object = Some(checkpoint);
            transition(
                ledger,
                if canceling_upload {
                    ProcessingState::CanceledAfterUpload
                } else {
                    ProcessingState::Transcribing
                },
            )?;
            Ok(true)
        })
    }

    pub fn mark_upload_rejected(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::ProviderFailed {
                return Ok(false);
            }
            if ledger.state == ProcessingState::CancelingUpload {
                if ledger.tos_object.is_some() {
                    return Err(ProcessingError::new("checkpoint_conflict"));
                }
                transition(ledger, ProcessingState::CanceledBeforeUpload)?;
                return Ok(true);
            }
            require_state(ledger, ProcessingState::Uploading)?;
            if ledger.tos_object.is_some() {
                return Err(ProcessingError::new("checkpoint_conflict"));
            }
            transition(ledger, ProcessingState::ProviderFailed)?;
            Ok(true)
        })
    }

    /// Persists an internally-derived request id and the `Submitting` fence.
    /// The caller may dispatch only after this returns successfully.
    pub fn begin_miaoji_submit(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::Submitting && ledger.miaoji.is_some() {
                return Ok(false);
            }
            require_state(ledger, ProcessingState::Transcribing)?;
            ledger.miaoji = Some(MiaojiCheckpoint {
                request_id: deterministic_request_id(recording_id),
                task_id: None,
                superseded: false,
            });
            transition(ledger, ProcessingState::Submitting)?;
            Ok(true)
        })
    }

    pub fn checkpoint_miaoji_task(
        &self,
        recording_id: Uuid,
        task_id: String,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_text(&task_id)?;
        self.mutate(recording_id, |ledger| {
            let existing_task = ledger
                .miaoji
                .as_ref()
                .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?
                .task_id
                .as_ref();
            if let Some(existing) = existing_task {
                return if existing == &task_id
                    && matches!(
                        ledger.state,
                        ProcessingState::Polling
                            | ProcessingState::Summarizing
                            | ProcessingState::Publishing
                            | ProcessingState::Complete
                            | ProcessingState::PublishFailed
                            | ProcessingState::PublishAmbiguous
                            | ProcessingState::PublishConflict
                    ) {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            require_state(ledger, ProcessingState::Submitting)?;
            ledger.miaoji.as_mut().expect("checked above").task_id = Some(task_id);
            transition(ledger, ProcessingState::Polling)?;
            Ok(true)
        })
    }

    pub fn mark_submit_ambiguous(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::SubmitAmbiguous {
                return Ok(false);
            }
            transition(ledger, ProcessingState::SubmitAmbiguous)?;
            Ok(true)
        })
    }

    pub fn mark_submit_rejected(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::ProviderFailed {
                return Ok(false);
            }
            require_state(ledger, ProcessingState::Submitting)?;
            transition(ledger, ProcessingState::ProviderFailed)?;
            Ok(true)
        })
    }

    pub fn retry_provider(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::ProviderFailed)?;
            if ledger.transcription_backend == TranscriptionBackend::MossLocal {
                return Err(ProcessingError::new("moss_owner_required"));
            }
            if ledger.transcript_json.is_some() {
                transition(ledger, ProcessingState::Summarizing)?;
            } else if matches!(
                ledger.transcription_backend,
                TranscriptionBackend::WhisperLocal | TranscriptionBackend::QwenLocal
            ) {
                transition(ledger, ProcessingState::LocalTranscribing)?;
            } else if ledger.tos_object.is_none() {
                transition(ledger, ProcessingState::Uploading)?;
            } else {
                ledger.miaoji = None;
                transition(ledger, ProcessingState::Transcribing)?;
            }
            Ok(true)
        })
    }

    /// Explicit operator/provider reconciliation gate. Never called by resume.
    pub fn resolve_submit_for_retry(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::SubmitAmbiguous)?;
            ledger.miaoji = None;
            transition(ledger, ProcessingState::Transcribing)?;
            Ok(true)
        })
    }

    pub fn checkpoint_transcript(
        &self,
        recording_id: Uuid,
        transcript: Value,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_json(&transcript)?;
        self.mutate(recording_id, |ledger| {
            if let Some(existing) = &ledger.transcript_json {
                return if existing == &transcript {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            require_state(ledger, ProcessingState::Polling)?;
            ledger.transcript_json = Some(transcript);
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn checkpoint_summary(
        &self,
        recording_id: Uuid,
        summary: Value,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_json(&summary)?;
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::SummarySubmitting)?;
            if let Some(existing) = &ledger.summary_json {
                return if existing == &summary {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            ledger.summary_json = Some(summary);
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn abort_summary_before_dispatch(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::SummarySubmitting)?;
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn resolve_summary_for_retry(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::SummaryAmbiguous)?;
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn plan_publication(
        &self,
        recording_id: Uuid,
        targets: Vec<PublicationTargetPlan>,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_target_plan(&targets)?;
        self.mutate(recording_id, |ledger| {
            if ledger.summary_json.is_none() {
                return Err(ProcessingError::new("missing_checkpoint"));
            }
            if let Some(existing) = &ledger.publication {
                let requested: Vec<_> = targets
                    .iter()
                    .map(|target| (target.id.as_str(), target.retry_mode, target.required))
                    .collect();
                let persisted: Vec<_> = existing
                    .targets
                    .iter()
                    .map(|target| (target.id.as_str(), target.retry_mode, target.required))
                    .collect();
                if ledger.state == ProcessingState::Summarizing
                    && ledger.cleanup.temporary_tos_deleted
                {
                    if requested != persisted {
                        return Err(ProcessingError::new("publication_plan_immutable"));
                    }
                    let generation = existing
                        .generation
                        .checked_add(1)
                        .ok_or_else(|| ProcessingError::new("generation_exhausted"))?;
                    ledger.publication = Some(PublicationCheckpoint {
                        generation,
                        targets: targets
                            .into_iter()
                            .map(|target| PublicationTarget {
                                id: target.id,
                                retry_mode: target.retry_mode,
                                required: target.required,
                                state: PublicationTargetState::Pending,
                                proof: None,
                            })
                            .collect(),
                    });
                    transition(ledger, ProcessingState::Publishing)?;
                    return Ok(true);
                }
                return if requested == persisted {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("publication_plan_immutable"))
                };
            }
            require_state(ledger, ProcessingState::Summarizing)?;
            ledger.publication = Some(PublicationCheckpoint {
                generation: 1,
                targets: targets
                    .into_iter()
                    .map(|target| PublicationTarget {
                        id: target.id,
                        retry_mode: target.retry_mode,
                        required: target.required,
                        state: PublicationTargetState::Pending,
                        proof: None,
                    })
                    .collect(),
            });
            transition(ledger, ProcessingState::Publishing)?;
            Ok(true)
        })
    }

    pub fn start_publication_target(
        &self,
        recording_id: Uuid,
        generation: u64,
        target_id: &str,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_text(target_id)?;
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Publishing)?;
            let target = current_target_mut(ledger, generation, target_id)?;
            match target.state {
                PublicationTargetState::Pending | PublicationTargetState::Failed => {
                    target.state = PublicationTargetState::Started;
                    Ok(true)
                }
                PublicationTargetState::Started => Ok(false),
                PublicationTargetState::Verified | PublicationTargetState::Ambiguous => {
                    Err(ProcessingError::new("invalid_effect_state"))
                }
            }
        })
    }

    pub fn checkpoint_publication_target(
        &self,
        recording_id: Uuid,
        generation: u64,
        target_id: &str,
        proof: PublicationProof,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_proof(&proof)?;
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Publishing)?;
            let target = current_target_mut(ledger, generation, target_id)?;
            if target.state == PublicationTargetState::Verified {
                return if target.proof.as_ref() == Some(&proof) {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            if target.state != PublicationTargetState::Started {
                return Err(ProcessingError::new("invalid_effect_state"));
            }
            target.state = PublicationTargetState::Verified;
            target.proof = Some(proof);
            Ok(true)
        })
    }

    pub fn fail_publication_target(
        &self,
        recording_id: Uuid,
        generation: u64,
        target_id: &str,
        outcome: TargetOutcome,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Publishing)?;
            let target = current_target_mut(ledger, generation, target_id)?;
            if target.state != PublicationTargetState::Started {
                return Err(ProcessingError::new("invalid_effect_state"));
            }
            let next = match outcome {
                TargetOutcome::ExplicitFailure => {
                    target.state = PublicationTargetState::Failed;
                    ProcessingState::PublishFailed
                }
                TargetOutcome::Ambiguous => {
                    target.state = PublicationTargetState::Ambiguous;
                    ProcessingState::PublishAmbiguous
                }
            };
            transition(ledger, next)?;
            Ok(true)
        })
    }

    pub fn mark_publication_conflict(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::PublishConflict {
                return Ok(false);
            }
            transition(ledger, ProcessingState::PublishConflict)?;
            Ok(true)
        })
    }

    pub fn checkpoint_canonical_backup(
        &self,
        recording_id: Uuid,
        generation: u64,
        backup: CanonicalBackupCheckpoint,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Publishing)?;
            validate_canonical_backup(&backup, &ledger.normalized)?;
            let publication = ledger
                .publication
                .as_ref()
                .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
            if publication.generation != generation {
                return Err(ProcessingError::new("stale_generation"));
            }
            if publication
                .targets
                .iter()
                .any(|target| target.required && target.state != PublicationTargetState::Verified)
            {
                return Err(ProcessingError::new("backup_not_safe"));
            }
            if let Some(existing) = &ledger.canonical_backup {
                return if existing == &backup {
                    Ok(false)
                } else {
                    Err(ProcessingError::new("checkpoint_conflict"))
                };
            }
            ledger.canonical_backup = Some(backup);
            Ok(true)
        })
    }

    pub fn retry_publication(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if !matches!(
                ledger.state,
                ProcessingState::PublishFailed
                    | ProcessingState::PublishAmbiguous
                    | ProcessingState::PublishConflict
            ) {
                return Err(ProcessingError::new("invalid_state"));
            }
            let publication = ledger
                .publication
                .as_mut()
                .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
            let target = publication
                .targets
                .iter_mut()
                .find(|target| target.state != PublicationTargetState::Verified)
                .ok_or_else(|| ProcessingError::new("invalid_effect_state"))?;
            if target.retry_mode == RetryMode::AtMostOnce
                || target.state == PublicationTargetState::Ambiguous
            {
                return Err(ProcessingError::new("manual_resolution_required"));
            }
            target.state = PublicationTargetState::Pending;
            transition(ledger, ProcessingState::Publishing)?;
            Ok(true)
        })
    }

    /// Starts an explicit new summary/publication generation while preserving
    /// the verified upload and transcript checkpoints. A repeated request
    /// while that generation is in flight is idempotent.
    pub fn begin_reprocess(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state != ProcessingState::Complete {
                if ledger.cleanup.temporary_tos_deleted
                    && matches!(
                        ledger.state,
                        ProcessingState::Summarizing
                            | ProcessingState::SummarySubmitting
                            | ProcessingState::SummaryAmbiguous
                            | ProcessingState::Publishing
                            | ProcessingState::PublishFailed
                            | ProcessingState::PublishAmbiguous
                            | ProcessingState::PublishConflict
                    )
                {
                    return Ok(false);
                }
                return Err(ProcessingError::new("invalid_state"));
            }
            if ledger.transcript_json.is_none() || ledger.publication.is_none() {
                return Err(ProcessingError::new("missing_checkpoint"));
            }
            ledger
                .publication
                .as_ref()
                .and_then(|publication| publication.generation.checked_add(1))
                .ok_or_else(|| ProcessingError::new("generation_exhausted"))?;
            ledger.summary_json = None;
            ledger.canonical_backup = None;
            transition(ledger, ProcessingState::Summarizing)?;
            Ok(true)
        })
    }

    pub fn begin_remote_backup(
        &self,
        recording_id: Uuid,
        targets: Vec<PublicationTargetPlan>,
    ) -> Result<ProcessingLedger, ProcessingError> {
        validate_target_plan(&targets)?;
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Complete)?;
            if ledger.publication_backend == PublicationBackend::RemoteArchive {
                return Ok(false);
            }
            if !matches!(
                ledger.transcription_backend,
                TranscriptionBackend::WhisperLocal
                    | TranscriptionBackend::QwenLocal
                    | TranscriptionBackend::MossLocal
            ) || ledger.summary_backend != SummaryBackend::QwenLocal
                || ledger.publication_backend != PublicationBackend::LocalArchive
                || ledger.transcript_json.is_none()
                || ledger.summary_json.is_none()
            {
                return Err(ProcessingError::new("invalid_state"));
            }
            let generation = ledger
                .publication
                .as_ref()
                .and_then(|publication| publication.generation.checked_add(1))
                .ok_or_else(|| ProcessingError::new("generation_exhausted"))?;
            ledger.publication_backend = PublicationBackend::RemoteArchive;
            ledger.publication = Some(PublicationCheckpoint {
                generation,
                targets: targets
                    .into_iter()
                    .map(|target| PublicationTarget {
                        id: target.id,
                        retry_mode: target.retry_mode,
                        required: target.required,
                        state: PublicationTargetState::Pending,
                        proof: None,
                    })
                    .collect(),
            });
            ledger.canonical_backup = None;
            transition(ledger, ProcessingState::Publishing)?;
            Ok(true)
        })
    }

    pub fn checkpoint_cleanup(
        &self,
        recording_id: Uuid,
        generation: u64,
    ) -> Result<ProcessingLedger, ProcessingError> {
        let delete_after = Utc::now() + TimeDelta::days(30);
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::Complete && ledger.cleanup.temporary_tos_deleted {
                if ledger.cleanup.source_tracks_delete_after.is_none() {
                    ledger.cleanup.source_tracks_delete_after = Some(delete_after);
                    return Ok(true);
                }
                return Ok(false);
            }
            require_state(ledger, ProcessingState::Publishing)?;
            let publication = ledger
                .publication
                .as_ref()
                .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
            if publication.generation != generation
                || publication.targets.iter().any(|target| {
                    target.required
                        && (target.state != PublicationTargetState::Verified
                            || target.proof.is_none())
                })
                || ledger.canonical_backup.is_none()
            {
                return Err(ProcessingError::new("cleanup_not_safe"));
            }
            ledger.cleanup.temporary_tos_deleted = true;
            if ledger.cleanup.source_tracks_delete_after.is_none() {
                ledger.cleanup.source_tracks_delete_after = Some(delete_after);
            }
            transition(ledger, ProcessingState::Complete)?;
            Ok(true)
        })
    }

    pub fn checkpoint_source_tracks_deleted(
        &self,
        recording_id: Uuid,
        now: DateTime<Utc>,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::Complete)?;
            if ledger.cleanup.source_tracks_deleted {
                return Ok(false);
            }
            let eligible = ledger
                .cleanup
                .source_tracks_delete_after
                .ok_or_else(|| ProcessingError::new("retention_not_scheduled"))?;
            if now < eligible {
                return Err(ProcessingError::new("retention_not_due"));
            }
            ledger.cleanup.source_tracks_deleted = true;
            Ok(true)
        })
    }

    pub fn checkpoint_canceled_tos_cleanup(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::CanceledAfterUpload)?;
            if ledger.tos_object.is_none() {
                return Err(ProcessingError::new("missing_checkpoint"));
            }
            if ledger.cleanup.temporary_tos_deleted {
                return Ok(false);
            }
            ledger.cleanup.temporary_tos_deleted = true;
            Ok(true)
        })
    }

    pub fn checkpoint_canceled_upload_absent(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            require_state(ledger, ProcessingState::CancelingUpload)?;
            if ledger.tos_object.is_some() {
                return Err(ProcessingError::new("checkpoint_conflict"));
            }
            transition(ledger, ProcessingState::CanceledBeforeUpload)?;
            Ok(true)
        })
    }

    pub fn begin_discard(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::Discarding {
                return Ok(false);
            }
            if ledger.state == ProcessingState::CanceledAfterUpload
                && !ledger.cleanup.temporary_tos_deleted
            {
                return Err(ProcessingError::new("cleanup_not_safe"));
            }
            if !matches!(
                ledger.state,
                ProcessingState::Queued
                    | ProcessingState::CanceledBeforeUpload
                    | ProcessingState::CanceledAfterUpload
            ) {
                return Err(ProcessingError::new("invalid_state"));
            }
            transition(ledger, ProcessingState::Discarding)?;
            Ok(true)
        })
    }

    pub fn checkpoint_discarded(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if ledger.state == ProcessingState::Discarded {
                return Ok(false);
            }
            require_state(ledger, ProcessingState::Discarding)?;
            transition(ledger, ProcessingState::Discarded)?;
            Ok(true)
        })
    }

    pub fn cancel(&self, recording_id: Uuid) -> Result<ProcessingLedger, ProcessingError> {
        self.mutate(recording_id, |ledger| {
            if matches!(
                ledger.state,
                ProcessingState::CancelingUpload
                    | ProcessingState::CanceledBeforeUpload
                    | ProcessingState::CanceledAfterUpload
            ) {
                return Ok(false);
            }
            if ledger.state == ProcessingState::Complete {
                return Err(ProcessingError::new("invalid_state"));
            }
            // Cancellation and target Started arbitrate under the same writer
            // lock. Once publication may have committed, finish/reconcile it;
            // never report a terminal cancellation with an untracked archive.
            // A previous generation's archive does not fence summary rework.
            if ledger.transcription_backend == TranscriptionBackend::MossLocal
                && matches!(
                    ledger.state,
                    ProcessingState::Publishing
                        | ProcessingState::PublishFailed
                        | ProcessingState::PublishAmbiguous
                        | ProcessingState::PublishConflict
                )
                && ledger.publication.as_ref().is_some_and(|publication| {
                    publication
                        .targets
                        .iter()
                        .any(|target| target.state != PublicationTargetState::Pending)
                })
            {
                return Err(ProcessingError::new("publication_commit_in_progress"));
            }
            let next = if ledger.state == ProcessingState::Uploading && ledger.tos_object.is_none()
            {
                ProcessingState::CancelingUpload
            } else if ledger.tos_object.is_some() {
                ProcessingState::CanceledAfterUpload
            } else {
                ProcessingState::CanceledBeforeUpload
            };
            transition(ledger, next)?;
            Ok(true)
        })
    }

    /// Returns the first safe effect. Ambiguous pre-dispatch checkpoints are
    /// atomically fenced into manual-resolution states before returning.
    pub fn resume(&self, recording_id: Uuid) -> Result<ResumeAction, ProcessingError> {
        self.with_writer(|| {
            let mut ledger = self
                .load_locked(recording_id)?
                .ok_or_else(|| ProcessingError::new("recording_not_found"))?;
            if ledger.state == ProcessingState::Submitting {
                ledger.state = ProcessingState::SubmitAmbiguous;
                bump_revision(&mut ledger)?;
                self.persist_locked(&ledger)?;
                return Ok(ResumeAction::ManualSubmitResolution);
            }
            let action = match ledger.state {
                ProcessingState::Queued => {
                    ledger.state = ProcessingState::Uploading;
                    bump_revision(&mut ledger)?;
                    self.persist_locked(&ledger)?;
                    ResumeAction::Upload
                }
                ProcessingState::Uploading => ResumeAction::Upload,
                ProcessingState::CancelingUpload => ResumeAction::ReconcileCanceledUpload,
                ProcessingState::PreparingLocalMoss => ResumeAction::PrepareMoss {
                    generation: ledger
                        .local_moss_preparation
                        .as_ref()
                        .ok_or_else(|| ProcessingError::new("invalid_ledger"))?
                        .generation(),
                },
                ProcessingState::LocalTranscribing
                    if ledger.transcription_backend == TranscriptionBackend::MossLocal =>
                {
                    ResumeAction::TranscribeMoss {
                        generation: ledger
                            .local_moss
                            .as_ref()
                            .ok_or_else(|| ProcessingError::new("invalid_ledger"))?
                            .generation(),
                    }
                }
                ProcessingState::LocalTranscribing => ResumeAction::TranscribeLocal {
                    request: Box::new(ledger.local_transcription_request()?),
                },
                ProcessingState::Transcribing => {
                    let request_id = deterministic_request_id(recording_id);
                    ledger.miaoji = Some(MiaojiCheckpoint {
                        request_id: request_id.clone(),
                        task_id: None,
                        superseded: false,
                    });
                    ledger.state = ProcessingState::Submitting;
                    bump_revision(&mut ledger)?;
                    self.persist_locked(&ledger)?;
                    ResumeAction::DispatchMiaoji { request_id }
                }
                ProcessingState::Polling => ResumeAction::PollMiaoji {
                    task_id: ledger
                        .miaoji
                        .as_ref()
                        .and_then(|checkpoint| checkpoint.task_id.clone())
                        .ok_or_else(|| ProcessingError::new("invalid_ledger"))?,
                },
                ProcessingState::Summarizing if ledger.summary_json.is_none() => {
                    ledger.state = ProcessingState::SummarySubmitting;
                    bump_revision(&mut ledger)?;
                    self.persist_locked(&ledger)?;
                    ResumeAction::Summarize
                }
                ProcessingState::SummarySubmitting => {
                    ledger.state = ProcessingState::SummaryAmbiguous;
                    bump_revision(&mut ledger)?;
                    self.persist_locked(&ledger)?;
                    ResumeAction::ManualSummaryResolution
                }
                ProcessingState::Summarizing => ResumeAction::AwaitPublicationPlan,
                ProcessingState::Publishing => publication_resume_action(&mut ledger)?,
                ProcessingState::SubmitAmbiguous => ResumeAction::ManualSubmitResolution,
                ProcessingState::ProviderFailed
                    if matches!(
                        ledger.transcription_backend,
                        TranscriptionBackend::WhisperLocal
                            | TranscriptionBackend::QwenLocal
                            | TranscriptionBackend::MossLocal
                    ) =>
                {
                    ResumeAction::ManualLocalResolution
                }
                ProcessingState::ProviderFailed => ResumeAction::ManualSubmitResolution,
                ProcessingState::SummaryAmbiguous => ResumeAction::ManualSummaryResolution,
                ProcessingState::PublishFailed
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict => ResumeAction::ManualPublicationResolution,
                ProcessingState::CanceledBeforeUpload | ProcessingState::CanceledAfterUpload => {
                    ResumeAction::Canceled
                }
                ProcessingState::Discarding | ProcessingState::Discarded => ResumeAction::Canceled,
                ProcessingState::Complete => ResumeAction::Done,
                ProcessingState::Submitting => unreachable!("handled above"),
            };
            if ledger.state == ProcessingState::PublishAmbiguous {
                bump_revision(&mut ledger)?;
                self.persist_locked(&ledger)?;
            }
            Ok(action)
        })
    }

    fn mutate<F>(
        &self,
        recording_id: Uuid,
        mutation: F,
    ) -> Result<ProcessingLedger, ProcessingError>
    where
        F: FnOnce(&mut ProcessingLedger) -> Result<bool, ProcessingError>,
    {
        self.with_writer(|| {
            let mut ledger = self
                .load_locked(recording_id)?
                .ok_or_else(|| ProcessingError::new("recording_not_found"))?;
            if mutation(&mut ledger)? {
                bump_revision(&mut ledger)?;
                ledger.validate()?;
                self.persist_locked(&ledger)?;
            }
            Ok(ledger)
        })
    }

    fn with_writer<T>(
        &self,
        operation: impl FnOnce() -> Result<T, ProcessingError>,
    ) -> Result<T, ProcessingError> {
        let _guard = self
            .writer
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let process_guard = FileUnlockGuard::acquire(&self.process_lock)?;
        let result = operation();
        process_guard.finish()?;
        result
    }

    fn load_locked(&self, recording_id: Uuid) -> Result<Option<ProcessingLedger>, ProcessingError> {
        let destination = self.job_path(recording_id);
        let mut candidates = Vec::new();
        if destination.exists() {
            candidates.push(destination.clone());
        }
        let prefix = format!(".{recording_id}.");
        for entry in
            fs::read_dir(&self.jobs).map_err(|_| ProcessingError::new("storage_unavailable"))?
        {
            let entry = entry.map_err(|_| ProcessingError::new("storage_unavailable"))?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name.starts_with(&prefix) && name.ends_with(".tmp") {
                candidates.push(entry.path());
            }
        }
        let mut valid = Vec::new();
        for path in &candidates {
            if fs::symlink_metadata(path)
                .map(|metadata| metadata.file_type().is_symlink() || !metadata.is_file())
                .unwrap_or(true)
            {
                continue;
            }
            let file = File::open(path).map_err(|_| ProcessingError::new("storage_unavailable"))?;
            if file
                .metadata()
                .map_err(|_| ProcessingError::new("storage_unavailable"))?
                .len()
                > MAX_LEDGER_BYTES
            {
                continue;
            }
            if let Ok(ledger) = serde_json::from_reader::<_, ProcessingLedger>(file) {
                if ledger.recording_id == recording_id && ledger.validate().is_ok() {
                    valid.push((ledger.revision, path.clone(), ledger));
                }
            }
        }
        valid.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
        let Some((_, selected_path, ledger)) = valid.pop() else {
            return if candidates.is_empty() {
                Ok(None)
            } else {
                Err(ProcessingError::new("invalid_ledger"))
            };
        };
        if selected_path != destination {
            replace_file(&selected_path, &destination)?;
            sync_directory(&self.jobs)?;
        }
        for (_, path, _) in valid {
            if path != destination {
                let _ = fs::remove_file(path);
            }
        }
        Ok(Some(ledger))
    }

    fn persist_locked(&self, ledger: &ProcessingLedger) -> Result<(), ProcessingError> {
        ledger.validate()?;
        let bytes =
            serde_json::to_vec(ledger).map_err(|_| ProcessingError::new("invalid_ledger"))?;
        if bytes.len() as u64 > MAX_LEDGER_BYTES {
            return Err(ProcessingError::new("checkpoint_too_large"));
        }
        let temporary = self.jobs.join(format!(
            ".{}.{}.{}.tmp",
            ledger.recording_id,
            ledger.revision,
            Uuid::new_v4()
        ));
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(&temporary)
            .map_err(|_| ProcessingError::new("storage_unavailable"))?;
        file.write_all(&bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ProcessingError::new("storage_unavailable"))?;
        drop(file);
        replace_file(&temporary, &self.job_path(ledger.recording_id))?;
        sync_directory(&self.jobs)
    }

    fn job_path(&self, recording_id: Uuid) -> PathBuf {
        self.jobs.join(format!("{recording_id}.json"))
    }
}

fn publication_resume_action(
    ledger: &mut ProcessingLedger,
) -> Result<ResumeAction, ProcessingError> {
    let publication = ledger
        .publication
        .as_mut()
        .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
    let Some(target) = publication
        .targets
        .iter_mut()
        .find(|target| target.state != PublicationTargetState::Verified)
    else {
        return Ok(if ledger.canonical_backup.is_some() {
            ResumeAction::Cleanup {
                generation: publication.generation,
            }
        } else {
            ResumeAction::VerifyCanonicalBackup {
                generation: publication.generation,
            }
        });
    };
    match target.state {
        PublicationTargetState::Pending | PublicationTargetState::Failed => {
            Ok(ResumeAction::PublishTarget {
                generation: publication.generation,
                target_id: target.id.clone(),
            })
        }
        PublicationTargetState::Started if target.retry_mode != RetryMode::AtMostOnce => {
            Ok(ResumeAction::PublishTarget {
                generation: publication.generation,
                target_id: target.id.clone(),
            })
        }
        PublicationTargetState::Started | PublicationTargetState::Ambiguous => {
            target.state = PublicationTargetState::Ambiguous;
            ledger.state = ProcessingState::PublishAmbiguous;
            Ok(ResumeAction::ManualPublicationResolution)
        }
        PublicationTargetState::Verified => unreachable!("filtered above"),
    }
}

fn current_target_mut<'a>(
    ledger: &'a mut ProcessingLedger,
    generation: u64,
    target_id: &str,
) -> Result<&'a mut PublicationTarget, ProcessingError> {
    let publication = ledger
        .publication
        .as_mut()
        .ok_or_else(|| ProcessingError::new("missing_checkpoint"))?;
    if publication.generation != generation {
        return Err(ProcessingError::new("stale_generation"));
    }
    let index = publication
        .targets
        .iter()
        .position(|target| target.id == target_id)
        .ok_or_else(|| ProcessingError::new("unknown_target"))?;
    if publication.targets[..index]
        .iter()
        .any(|target| target.state != PublicationTargetState::Verified)
    {
        return Err(ProcessingError::new("target_out_of_order"));
    }
    Ok(&mut publication.targets[index])
}

fn validate_checkpoint_consistency(ledger: &ProcessingLedger) -> Result<(), ProcessingError> {
    let remote_transcription = ledger.transcription_backend == TranscriptionBackend::MiaojiRemote;
    if remote_transcription
        && matches!(
            ledger.state,
            ProcessingState::LocalTranscribing | ProcessingState::PreparingLocalMoss
        )
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if !remote_transcription
        && !matches!(
            ledger.state,
            ProcessingState::LocalTranscribing
                | ProcessingState::PreparingLocalMoss
                | ProcessingState::ProviderFailed
                | ProcessingState::Summarizing
                | ProcessingState::SummarySubmitting
                | ProcessingState::SummaryAmbiguous
                | ProcessingState::Publishing
                | ProcessingState::Complete
                | ProcessingState::PublishFailed
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict
                | ProcessingState::CanceledBeforeUpload
                | ProcessingState::CanceledAfterUpload
                | ProcessingState::Discarding
                | ProcessingState::Discarded
        )
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    let reprocess_active = ledger.cleanup.temporary_tos_deleted
        && matches!(
            ledger.state,
            ProcessingState::Summarizing
                | ProcessingState::SummarySubmitting
                | ProcessingState::SummaryAmbiguous
                | ProcessingState::Publishing
                | ProcessingState::PublishFailed
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict
        )
        && ledger.publication.is_some();
    let discard_active = matches!(
        ledger.state,
        ProcessingState::Discarding | ProcessingState::Discarded
    );
    // Reprocessing retains the previously verified publication and its cleanup
    // history. Canceling the new summary must preserve that history, not erase
    // the existing archive or make a valid canceled job unreadable. These are
    // receipts only: retention still runs exclusively for Complete jobs.
    let canceled_reprocess_history = ledger.cleanup.temporary_tos_deleted
        && matches!(
            ledger.state,
            ProcessingState::CanceledBeforeUpload
                | ProcessingState::CanceledAfterUpload
                | ProcessingState::Discarding
                | ProcessingState::Discarded
        )
        && ledger.transcript_json.is_some()
        && ledger.publication.as_ref().is_some_and(|publication| {
            publication
                .targets
                .iter()
                .all(|target| target.state == PublicationTargetState::Verified)
        });
    let needs_object = remote_transcription
        && !matches!(
            ledger.state,
            ProcessingState::Queued
                | ProcessingState::Uploading
                | ProcessingState::CancelingUpload
                | ProcessingState::ProviderFailed
                | ProcessingState::CanceledBeforeUpload
                | ProcessingState::Discarding
                | ProcessingState::Discarded
        );
    if needs_object && ledger.tos_object.is_none() {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    let needs_miaoji = remote_transcription
        && matches!(
            ledger.state,
            ProcessingState::Submitting
                | ProcessingState::Polling
                | ProcessingState::Summarizing
                | ProcessingState::SummarySubmitting
                | ProcessingState::SummaryAmbiguous
                | ProcessingState::Publishing
                | ProcessingState::Complete
                | ProcessingState::SubmitAmbiguous
                | ProcessingState::PublishFailed
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict
        );
    if needs_miaoji && ledger.miaoji.is_none() {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if remote_transcription
        && matches!(
            ledger.state,
            ProcessingState::Polling
                | ProcessingState::Summarizing
                | ProcessingState::SummarySubmitting
                | ProcessingState::SummaryAmbiguous
                | ProcessingState::Publishing
                | ProcessingState::Complete
                | ProcessingState::PublishFailed
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict
        )
        && ledger
            .miaoji
            .as_ref()
            .and_then(|value| value.task_id.as_ref())
            .is_none()
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if matches!(
        ledger.state,
        ProcessingState::Summarizing
            | ProcessingState::SummarySubmitting
            | ProcessingState::SummaryAmbiguous
            | ProcessingState::Publishing
            | ProcessingState::Complete
            | ProcessingState::PublishFailed
            | ProcessingState::PublishAmbiguous
            | ProcessingState::PublishConflict
    ) && ledger.transcript_json.is_none()
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if matches!(
        ledger.state,
        ProcessingState::Publishing
            | ProcessingState::Complete
            | ProcessingState::PublishFailed
            | ProcessingState::PublishAmbiguous
            | ProcessingState::PublishConflict
    ) && (ledger.summary_json.is_none() || ledger.publication.is_none())
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if matches!(
        ledger.state,
        ProcessingState::SummarySubmitting | ProcessingState::SummaryAmbiguous
    ) && ledger.summary_json.is_some()
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if ledger.state == ProcessingState::Complete && !ledger.cleanup.temporary_tos_deleted {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if ledger.cleanup.temporary_tos_deleted
        && ledger.state != ProcessingState::Complete
        && !reprocess_active
        && ledger.state != ProcessingState::CanceledAfterUpload
        && !discard_active
        && !canceled_reprocess_history
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if ledger.cleanup.source_tracks_delete_after.is_some()
        && ledger.state != ProcessingState::Complete
        && !reprocess_active
        && !canceled_reprocess_history
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if ledger.cleanup.source_tracks_deleted
        && (ledger.state != ProcessingState::Complete
            && !reprocess_active
            && !canceled_reprocess_history
            || ledger.cleanup.source_tracks_delete_after.is_none())
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    if ledger.state == ProcessingState::Complete && ledger.canonical_backup.is_none() {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    Ok(())
}

fn validate_normalized(value: &NormalizedArtifactCheckpoint) -> Result<(), ProcessingError> {
    validate_relative_path(&value.relative_path)?;
    normalized_audio_extension(&value.relative_path)
        .ok_or_else(|| ProcessingError::new("invalid_artifact"))?;
    validate_sha256(&value.sha256, "normalized.sha256")
        .map_err(|_| ProcessingError::new("invalid_artifact"))?;
    if value.size_bytes == 0 {
        return Err(ProcessingError::new("invalid_artifact"));
    }
    Ok(())
}

fn validate_relative_path(value: &str) -> Result<(), ProcessingError> {
    let path = Path::new(value);
    if value.is_empty()
        || value.len() > MAX_TEXT
        || path.is_absolute()
        || path.components().any(|component| {
            !matches!(component, Component::Normal(_)) || component.as_os_str().to_str().is_none()
        })
    {
        return Err(ProcessingError::new("invalid_artifact"));
    }
    Ok(())
}

fn validate_text(value: &str) -> Result<(), ProcessingError> {
    if value.is_empty()
        || value.len() > MAX_TEXT
        || value.chars().any(|character| character.is_control())
    {
        return Err(ProcessingError::new("invalid_checkpoint"));
    }
    Ok(())
}

fn validate_optional_json(value: &Option<Value>) -> Result<(), ProcessingError> {
    if let Some(value) = value {
        validate_json(value)?;
    }
    Ok(())
}

fn validate_json(value: &Value) -> Result<(), ProcessingError> {
    if serde_json::to_vec(value)
        .map_err(|_| ProcessingError::new("invalid_checkpoint"))?
        .len()
        > MAX_JSON_BYTES
    {
        return Err(ProcessingError::new("checkpoint_too_large"));
    }
    fn walk(value: &Value, depth: usize) -> Result<(), ProcessingError> {
        if depth > MAX_JSON_DEPTH {
            return Err(ProcessingError::new("checkpoint_too_large"));
        }
        match value {
            Value::Array(values) => {
                if values.len() > MAX_JSON_ARRAY_ITEMS {
                    return Err(ProcessingError::new("checkpoint_too_large"));
                }
                for value in values {
                    walk(value, depth + 1)?;
                }
            }
            Value::Object(values) => {
                if values.len() > MAX_JSON_ARRAY_ITEMS {
                    return Err(ProcessingError::new("checkpoint_too_large"));
                }
                for (key, value) in values {
                    if key.len() > MAX_TEXT {
                        return Err(ProcessingError::new("checkpoint_too_large"));
                    }
                    walk(value, depth + 1)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    walk(value, 0)
}

fn validate_target_plan(targets: &[PublicationTargetPlan]) -> Result<(), ProcessingError> {
    if targets.is_empty() || targets.len() > MAX_TARGETS {
        return Err(ProcessingError::new("invalid_publication_plan"));
    }
    let mut ids = HashSet::new();
    for target in targets {
        validate_text(&target.id)?;
        if !ids.insert(&target.id) {
            return Err(ProcessingError::new("invalid_publication_plan"));
        }
    }
    Ok(())
}

fn validate_publication(
    publication: &PublicationCheckpoint,
    _: &NormalizedArtifactCheckpoint,
) -> Result<(), ProcessingError> {
    if publication.generation == 0
        || publication.targets.is_empty()
        || publication.targets.len() > MAX_TARGETS
    {
        return Err(ProcessingError::new("invalid_ledger"));
    }
    let mut ids = HashSet::new();
    let mut incomplete_seen = false;
    for target in &publication.targets {
        validate_text(&target.id)?;
        if !ids.insert(&target.id) {
            return Err(ProcessingError::new("invalid_ledger"));
        }
        if target.state == PublicationTargetState::Verified {
            let proof = target
                .proof
                .as_ref()
                .ok_or_else(|| ProcessingError::new("invalid_ledger"))?;
            validate_proof(proof)?;
            if incomplete_seen {
                return Err(ProcessingError::new("invalid_ledger"));
            }
        } else {
            incomplete_seen = true;
            if target.proof.is_some() {
                return Err(ProcessingError::new("invalid_ledger"));
            }
        }
    }
    Ok(())
}

fn validate_canonical_backup(
    backup: &CanonicalBackupCheckpoint,
    normalized: &NormalizedArtifactCheckpoint,
) -> Result<(), ProcessingError> {
    validate_text(&backup.locator)?;
    validate_text(&backup.version_id)?;
    validate_json(&backup.proof_json)?;
    if backup
        .proof_json
        .as_object()
        .is_none_or(|proof| proof.is_empty())
        || backup.sha256 != normalized.sha256
        || backup.size_bytes != normalized.size_bytes
    {
        return Err(ProcessingError::new("backup_mismatch"));
    }
    Ok(())
}

fn validate_proof(proof: &PublicationProof) -> Result<(), ProcessingError> {
    validate_text(&proof.locator)?;
    validate_text(&proof.version)?;
    validate_sha256(&proof.sha256, "publication.sha256")
        .map_err(|_| ProcessingError::new("invalid_checkpoint"))?;
    if proof.size_bytes == 0 {
        return Err(ProcessingError::new("invalid_checkpoint"));
    }
    Ok(())
}

fn transition(ledger: &mut ProcessingLedger, next: ProcessingState) -> Result<(), ProcessingError> {
    if !ledger.state.can_transition_to(next) {
        return Err(ProcessingError::new("invalid_state"));
    }
    ledger.state = next;
    Ok(())
}

fn require_state(ledger: &ProcessingLedger, state: ProcessingState) -> Result<(), ProcessingError> {
    if ledger.state != state {
        return Err(ProcessingError::new("invalid_state"));
    }
    Ok(())
}

fn bump_revision(ledger: &mut ProcessingLedger) -> Result<(), ProcessingError> {
    ledger.revision = ledger
        .revision
        .checked_add(1)
        .ok_or_else(|| ProcessingError::new("revision_exhausted"))?;
    Ok(())
}

fn deterministic_object_key(
    recording_id: Uuid,
    normalized: &NormalizedArtifactCheckpoint,
) -> String {
    let extension = normalized_audio_extension(&normalized.relative_path)
        .expect("validated normalized artifact extension");
    format!(
        "echowall/processing/v1/{recording_id}/{}.{extension}",
        normalized.sha256
    )
}

fn normalized_audio_extension(relative_path: &str) -> Option<&'static str> {
    let extension = Path::new(relative_path).extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("wav") {
        Some("wav")
    } else if extension.eq_ignore_ascii_case("mp3") {
        Some("mp3")
    } else if extension.eq_ignore_ascii_case("m4a") {
        Some("m4a")
    } else {
        None
    }
}

pub(crate) fn transcript_for_summary(transcript: &Value) -> Result<String, ProcessingError> {
    let sentences = transcript
        .as_array()
        .ok_or_else(|| ProcessingError::new("invalid_checkpoint"))?;
    let mut output = String::new();
    for sentence in sentences {
        let object = sentence
            .as_object()
            .ok_or_else(|| ProcessingError::new("invalid_checkpoint"))?;
        let content = object
            .get("content")
            .or_else(|| object.get("Text"))
            .and_then(Value::as_str)
            .ok_or_else(|| ProcessingError::new("invalid_checkpoint"))?;
        let speaker = object
            .get("speaker")
            .and_then(Value::as_object)
            .and_then(|speaker| speaker.get("id"))
            .and_then(Value::as_str)
            .or_else(|| object.get("SpeakerID").and_then(Value::as_str))
            .unwrap_or("?");
        if output.len().saturating_add(content.len())
            > echowall_local_summary_protocol::MAX_TRANSCRIPT_BYTES
        {
            return Err(ProcessingError::new("checkpoint_too_large"));
        }
        output.push_str("SPEAKER_");
        output.push_str(speaker);
        output.push_str(": ");
        output.push_str(content);
        output.push('\n');
    }
    if output.trim().is_empty() {
        return Err(ProcessingError::new("invalid_checkpoint"));
    }
    Ok(output)
}

fn deterministic_request_id(recording_id: Uuid) -> String {
    Uuid::new_v5(
        &Uuid::NAMESPACE_URL,
        format!("echowall:miaoji:{recording_id}").as_bytes(),
    )
    .to_string()
}

fn process_writer_lock(root: &Path) -> Result<Arc<Mutex<()>>, ProcessingError> {
    let registry = WRITER_LOCKS.get_or_init(|| Mutex::new(HashMap::new()));
    let mut registry = registry
        .lock()
        .map_err(|_| ProcessingError::new("storage_unavailable"))?;
    if let Some(lock) = registry.get(root).and_then(Weak::upgrade) {
        return Ok(lock);
    }
    let lock = Arc::new(Mutex::new(()));
    registry.insert(root.to_path_buf(), Arc::downgrade(&lock));
    Ok(lock)
}

fn absolute_path(path: &Path) -> Result<PathBuf, ProcessingError> {
    if let Ok(canonical) = fs::canonicalize(path) {
        return Ok(canonical);
    }
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    std::env::current_dir()
        .map(|directory| directory.join(path))
        .map_err(|_| ProcessingError::new("storage_unavailable"))
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ProcessingError> {
    fs::rename(source, destination).map_err(|_| ProcessingError::new("storage_unavailable"))
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), ProcessingError> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(ProcessingError::new("storage_unavailable"))
    } else {
        Ok(())
    }
}

#[cfg(not(target_os = "windows"))]
fn sync_directory(path: &Path) -> Result<(), ProcessingError> {
    File::open(path)
        .and_then(|directory| directory.sync_all())
        .map_err(|_| ProcessingError::new("storage_unavailable"))
}

#[cfg(target_os = "windows")]
fn sync_directory(_: &Path) -> Result<(), ProcessingError> {
    Ok(())
}

fn public_error(code: &str) -> &'static str {
    match code {
        "recording_not_found" => "processing recording was not found",
        "recording_collision" => "recording identity conflicts with persisted processing state",
        "submit_ambiguous" | "manual_resolution_required" => {
            "processing requires explicit reconciliation"
        }
        "unsafe_storage_layout" => "processing storage must be separate from inbox and archive",
        "checkpoint_too_large" => "processing checkpoint exceeds its size bound",
        "artifact_mismatch" => "processing artifact identity does not match",
        _ => "processing operation failed",
    }
}

#[cfg(test)]
mod tests;
