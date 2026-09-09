//! Embedded processing orchestration over the durable effect ledger.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use echowall_local_summary_protocol::{LocalSummaryRequest, LOCAL_SUMMARY_PROMPT_VERSION};
use serde_json::Value;
use tokio::sync::Notify;
use uuid::Uuid;

use crate::ingest::envelope::{RecordingEnvelope, SourceKind};
use crate::ingest::inbox::Inbox;

use super::local_models::{LocalModelPackProof, QwenCandidateModelPackProof};
use super::local_whisper::LocalWhisperResponse;
use super::{
    CanonicalBackupCheckpoint, LocalTranscriptionRequest, NormalizedArtifactCheckpoint,
    ProcessingError, ProcessingLedger, ProcessingStore, PublicationProof, PublicationTargetPlan,
    ResumeAction, TargetOutcome, TosObjectCheckpoint,
};

// A longest supported MOSS recording has26 windows plus preparation,
// reconciliation, summary and publication. Keep one bounded run large enough.
const MAX_IMMEDIATE_ACTIONS: usize = 32 + echowall_local_moss_protocol::windows::MAX_WINDOWS;

mod moss;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EffectErrorKind {
    NotDispatched,
    Temporary,
    Rejected,
    SubmitAmbiguous,
    PublicationConflict,
    PublicationAmbiguous,
    Verification,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EffectError {
    pub kind: EffectErrorKind,
    message: &'static str,
}

impl EffectError {
    pub fn new(kind: EffectErrorKind) -> Self {
        let message = match kind {
            EffectErrorKind::NotDispatched => "processing request was not dispatched",
            EffectErrorKind::Temporary => "processing dependency is temporarily unavailable",
            EffectErrorKind::Rejected => "processing dependency rejected the request",
            EffectErrorKind::SubmitAmbiguous => {
                "transcription submission requires manual reconciliation"
            }
            EffectErrorKind::PublicationConflict => "archive changed on another device",
            EffectErrorKind::PublicationAmbiguous => {
                "archive publication requires manual reconciliation"
            }
            EffectErrorKind::Verification => "processing effect could not be verified",
            EffectErrorKind::Cancelled => "local processing was canceled",
        };
        Self { kind, message }
    }
}

impl std::fmt::Display for EffectError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.message)
    }
}

impl std::error::Error for EffectError {}

#[derive(Clone, Debug, PartialEq)]
pub enum PollResult {
    Running,
    Complete(Value),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SummaryRequest {
    Gemini { transcript: String },
    Local(LocalSummaryRequest),
}

#[async_trait]
pub trait ProcessingEffects: Send + Sync + 'static {
    async fn transcribe_moss(
        &self,
        _request: &echowall_local_moss_protocol::MossRequest,
        _cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        Err(EffectError::new(EffectErrorKind::NotDispatched))
    }

    async fn diarize_moss(
        &self,
        _request: &super::local_whisper::LocalDiarizationRequest,
        _cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        Err(EffectError::new(EffectErrorKind::NotDispatched))
    }

    async fn summarize_moss(
        &self,
        request: &LocalSummaryRequest,
        _cancel: Arc<AtomicBool>,
    ) -> Result<Value, EffectError> {
        self.summarize(&SummaryRequest::Local(request.clone()))
            .await
    }

    async fn transcribe_local(
        &self,
        _request: &LocalTranscriptionRequest,
    ) -> Result<LocalWhisperResponse, EffectError> {
        Err(EffectError::new(EffectErrorKind::Rejected))
    }

    async fn upload_tos(
        &self,
        recording_id: Uuid,
        object_key: &str,
        source: &Path,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<TosObjectCheckpoint, EffectError>;

    async fn probe_tos(
        &self,
        recording_id: Uuid,
        object_key: &str,
        source: &Path,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<Option<TosObjectCheckpoint>, EffectError>;

    async fn submit_miaoji(
        &self,
        object: &TosObjectCheckpoint,
        request_id: &str,
        speaker_count: Option<u32>,
    ) -> Result<String, EffectError>;

    async fn poll_miaoji(&self, request_id: &str, task_id: &str)
        -> Result<PollResult, EffectError>;

    async fn summarize(&self, request: &SummaryRequest) -> Result<Value, EffectError>;

    fn publication_plan(
        &self,
        envelope: &RecordingEnvelope,
        backend: super::PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError>;

    async fn publish_target(
        &self,
        target_id: &str,
        generation: u64,
        envelope: &RecordingEnvelope,
        transcript: &Value,
        summary: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError>;

    async fn verify_canonical_backup(
        &self,
        backend: super::PublicationBackend,
        generation: u64,
        envelope: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError>;

    async fn cleanup_tos(&self, object: &TosObjectCheckpoint) -> Result<(), EffectError>;
}

#[derive(Debug)]
pub enum EngineError {
    Local(ProcessingError),
    Effect(EffectError),
    InvalidRecording(&'static str),
    ManualResolutionRequired,
    StepLimit,
}

impl std::fmt::Display for EngineError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::Local(_) => "local processing state is unavailable",
            Self::Effect(error) => return error.fmt(formatter),
            Self::InvalidRecording(message) => message,
            Self::ManualResolutionRequired => "processing requires manual reconciliation",
            Self::StepLimit => "processing yielded after its bounded work slice",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for EngineError {}

impl From<ProcessingError> for EngineError {
    fn from(value: ProcessingError) -> Self {
        Self::Local(value)
    }
}

impl From<EffectError> for EngineError {
    fn from(value: EffectError) -> Self {
        Self::Effect(value)
    }
}

pub struct ProcessingEngine<E: ProcessingEffects> {
    inbox: Arc<Inbox>,
    store: Arc<ProcessingStore>,
    effects: Arc<E>,
    tos_operations_in_flight: Mutex<HashSet<Uuid>>,
    tos_operation_finished: Notify,
    moss_operations: Arc<Mutex<HashMap<Uuid, Arc<moss::RunControl>>>>,
}

struct TosOperationGuard<'a> {
    recording_id: Uuid,
    operations: &'a Mutex<HashSet<Uuid>>,
    finished: &'a Notify,
}

impl Drop for TosOperationGuard<'_> {
    fn drop(&mut self) {
        self.operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(&self.recording_id);
        self.finished.notify_waiters();
    }
}

impl<E: ProcessingEffects> ProcessingEngine<E> {
    pub fn new(inbox: Arc<Inbox>, store: Arc<ProcessingStore>, effects: Arc<E>) -> Self {
        Self {
            inbox,
            store,
            effects,
            tos_operations_in_flight: Mutex::new(HashSet::new()),
            tos_operation_finished: Notify::new(),
            moss_operations: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    async fn wait_for_tos_operation(&self, recording_id: Uuid) {
        loop {
            let notified = self.tos_operation_finished.notified();
            let active = self
                .tos_operations_in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains(&recording_id);
            if !active {
                return;
            }
            notified.await;
        }
    }

    pub fn enqueue(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        if envelope.job.state != crate::ingest::state::JobState::Ready {
            return Err(EngineError::InvalidRecording("recording is not ready"));
        }
        let relative_path =
            envelope
                .normalized_audio
                .as_deref()
                .ok_or(EngineError::InvalidRecording(
                    "recording has no normalized audio",
                ))?;
        let expected_sha256 =
            envelope
                .normalized_sha256
                .as_deref()
                .ok_or(EngineError::InvalidRecording(
                    "recording has no normalized hash",
                ))?;
        let digest = self
            .inbox
            .hash_package_file(recording_id, relative_path)
            .map_err(|_| EngineError::InvalidRecording("recording audio is unavailable"))?;
        if digest.sha256 != expected_sha256 || digest.size_bytes == 0 {
            return Err(EngineError::InvalidRecording(
                "recording audio failed verification",
            ));
        }
        Ok(self.store.enqueue(
            recording_id,
            NormalizedArtifactCheckpoint {
                relative_path: relative_path.to_owned(),
                sha256: digest.sha256,
                size_bytes: digest.size_bytes,
            },
        )?)
    }

    pub fn list_recording_ids(&self) -> Result<Vec<Uuid>, EngineError> {
        Ok(self.store.list_recording_ids()?)
    }

    pub fn enqueue_ready_recordings(&self) -> Result<Vec<Uuid>, EngineError> {
        let recording_ids = self
            .inbox
            .list_ready_for_processing()
            .map_err(|_| EngineError::InvalidRecording("ready recordings are unavailable"))?;
        for recording_id in &recording_ids {
            self.enqueue(*recording_id)?;
        }
        Ok(recording_ids)
    }

    pub fn status(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        Ok(self.store.load(recording_id)?)
    }

    pub fn sweep_source_track_retention(&self, now: DateTime<Utc>) -> Result<usize, EngineError> {
        let mut completed = 0usize;
        for recording_id in self.store.list_recording_ids()? {
            let mut ledger = self.store.load(recording_id)?;
            if ledger.state != super::ProcessingState::Complete
                || ledger.cleanup.source_tracks_deleted
            {
                continue;
            }
            if ledger.cleanup.source_tracks_delete_after.is_none() {
                let generation = ledger
                    .publication
                    .as_ref()
                    .map(|publication| publication.generation)
                    .ok_or(EngineError::InvalidRecording(
                        "completed publication checkpoint is missing",
                    ))?;
                ledger = self.store.checkpoint_cleanup(recording_id, generation)?;
            }
            if ledger
                .cleanup
                .source_tracks_delete_after
                .is_none_or(|eligible| now < eligible)
            {
                continue;
            }
            self.inbox
                .prune_source_tracks(recording_id)
                .map_err(|_| EngineError::InvalidRecording("source tracks could not be pruned"))?;
            self.store
                .checkpoint_source_tracks_deleted(recording_id, now)?;
            completed = completed.saturating_add(1);
        }
        Ok(completed)
    }

    pub fn retry_provider(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        Ok(self.store.retry_provider(recording_id)?)
    }

    pub fn select_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        self.enqueue(recording_id)?;
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        Ok(self.store.select_full_local(
            recording_id,
            proof.whisper_model_id,
            proof.whisper_sha256,
            proof.whisper_size_bytes,
            envelope.duration_ms,
            language,
            proof.diarization_pack_id,
            proof.diarization_files,
            confirmed_speaker_count(&envelope),
            proof.summary_model_id,
            proof.summary_sha256,
            proof.summary_size_bytes,
        )?)
    }

    pub fn select_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        self.enqueue(recording_id)?;
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        Ok(self.store.select_full_local_qwen(
            recording_id,
            super::LocalQwenCheckpoint {
                runtime_id: qwen_proof.runtime_id,
                asr_model_id: qwen_proof.asr_model_id,
                asr_model_revision: qwen_proof.asr_model_revision,
                asr_model_files: qwen_proof.asr_model_files,
                aligner_model_id: qwen_proof.aligner_model_id,
                aligner_model_revision: qwen_proof.aligner_model_revision,
                aligner_model_files: qwen_proof.aligner_model_files,
                audio_duration_ms: envelope.duration_ms,
                language,
                chunk_policy: echowall_local_qwen_protocol::LOCAL_QWEN_CHUNK_POLICY.to_owned(),
                chunk_duration_ms: echowall_local_qwen_protocol::LOCAL_QWEN_CHUNK_DURATION_MS,
                split_search_ms: echowall_local_qwen_protocol::LOCAL_QWEN_SPLIT_SEARCH_MS,
            },
            super::LocalDiarizationCheckpoint {
                pack_id: base_proof.diarization_pack_id,
                quality_preset: Some(
                    super::local_whisper::LOCAL_DIARIZATION_QUALITY_PRESET.to_owned(),
                ),
                model_files: base_proof.diarization_files,
                expected_speaker_count: confirmed_speaker_count(&envelope),
            },
            super::LocalSummaryCheckpoint {
                model_id: base_proof.summary_model_id,
                model_sha256: base_proof.summary_sha256,
                model_size_bytes: base_proof.summary_size_bytes,
                prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
                transcript_sha256: None,
            },
        )?)
    }

    pub fn take_over_with_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        Ok(self.store.take_over_with_full_local(
            recording_id,
            proof.whisper_model_id,
            proof.whisper_sha256,
            proof.whisper_size_bytes,
            envelope.duration_ms,
            language,
            proof.diarization_pack_id,
            proof.diarization_files,
            confirmed_speaker_count(&envelope),
            proof.summary_model_id,
            proof.summary_sha256,
            proof.summary_size_bytes,
        )?)
    }

    pub fn take_over_with_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        Ok(self.store.take_over_with_full_local_qwen(
            recording_id,
            super::LocalQwenCheckpoint {
                runtime_id: qwen_proof.runtime_id,
                asr_model_id: qwen_proof.asr_model_id,
                asr_model_revision: qwen_proof.asr_model_revision,
                asr_model_files: qwen_proof.asr_model_files,
                aligner_model_id: qwen_proof.aligner_model_id,
                aligner_model_revision: qwen_proof.aligner_model_revision,
                aligner_model_files: qwen_proof.aligner_model_files,
                audio_duration_ms: envelope.duration_ms,
                language,
                chunk_policy: echowall_local_qwen_protocol::LOCAL_QWEN_CHUNK_POLICY.to_owned(),
                chunk_duration_ms: echowall_local_qwen_protocol::LOCAL_QWEN_CHUNK_DURATION_MS,
                split_search_ms: echowall_local_qwen_protocol::LOCAL_QWEN_SPLIT_SEARCH_MS,
            },
            super::LocalDiarizationCheckpoint {
                pack_id: base_proof.diarization_pack_id,
                quality_preset: Some(
                    super::local_whisper::LOCAL_DIARIZATION_QUALITY_PRESET.to_owned(),
                ),
                model_files: base_proof.diarization_files,
                expected_speaker_count: confirmed_speaker_count(&envelope),
            },
            super::LocalSummaryCheckpoint {
                model_id: base_proof.summary_model_id,
                model_sha256: base_proof.summary_sha256,
                model_size_bytes: base_proof.summary_size_bytes,
                prompt_version: LOCAL_SUMMARY_PROMPT_VERSION.to_owned(),
                transcript_sha256: None,
            },
        )?)
    }

    pub async fn accept_local_transcript_only(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        self.store.accept_local_transcript_only(recording_id)?;
        self.run_until_wait(recording_id).await
    }

    pub fn retry_publication(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        Ok(self.store.retry_publication(recording_id)?)
    }

    pub fn cancel(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        let ledger = self.store.cancel(recording_id)?;
        self.signal_moss_cancel(recording_id);
        Ok(ledger)
    }

    pub async fn cancel_and_cleanup(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        let (canceled, operation_guard, operation_active) = {
            let mut operations = self
                .tos_operations_in_flight
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let canceled = self.store.cancel(recording_id)?;
            let operation_active = operations.contains(&recording_id);
            let needs_tos_finalization = canceled.state == super::ProcessingState::CancelingUpload
                || canceled.state == super::ProcessingState::CanceledAfterUpload
                    && !canceled.cleanup.temporary_tos_deleted;
            let operation_guard = if needs_tos_finalization && !operation_active {
                operations.insert(recording_id);
                Some(TosOperationGuard {
                    recording_id,
                    operations: &self.tos_operations_in_flight,
                    finished: &self.tos_operation_finished,
                })
            } else {
                None
            };
            (canceled, operation_guard, operation_active)
        };
        if let Some(control) = self.signal_moss_cancel(recording_id) {
            control.wait_idle().await;
        }
        if operation_active {
            self.wait_for_tos_operation(recording_id).await;
            return self.resume_canceled_cleanup(recording_id).await;
        }
        let Some(operation_guard) = operation_guard else {
            return Ok(canceled);
        };
        let result = self.resume_canceled_cleanup_inner(recording_id).await;
        drop(operation_guard);
        result
    }

    pub async fn resume_canceled_cleanup(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        loop {
            let (ledger, operation_guard, operation_active) = {
                let mut operations = self
                    .tos_operations_in_flight
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let ledger = self.store.load(recording_id)?;
                let operation_active = operations.contains(&recording_id);
                let needs_tos_finalization = ledger.state
                    == super::ProcessingState::CancelingUpload
                    || ledger.state == super::ProcessingState::CanceledAfterUpload
                        && !ledger.cleanup.temporary_tos_deleted;
                let operation_guard = if needs_tos_finalization && !operation_active {
                    operations.insert(recording_id);
                    Some(TosOperationGuard {
                        recording_id,
                        operations: &self.tos_operations_in_flight,
                        finished: &self.tos_operation_finished,
                    })
                } else {
                    None
                };
                (ledger, operation_guard, operation_active)
            };
            if operation_active {
                self.wait_for_tos_operation(recording_id).await;
                continue;
            }
            let Some(operation_guard) = operation_guard else {
                return Ok(ledger);
            };
            let result = self.resume_canceled_cleanup_inner(recording_id).await;
            drop(operation_guard);
            return result;
        }
    }

    async fn resume_canceled_cleanup_inner(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        let mut ledger = self.store.load(recording_id)?;
        if ledger.state == super::ProcessingState::CancelingUpload {
            let source = self
                .inbox
                .package_file_path(recording_id, &ledger.normalized.relative_path)
                .map_err(|_| EngineError::InvalidRecording("recording audio is unavailable"))?;
            ledger = match self
                .effects
                .probe_tos(
                    recording_id,
                    &ledger.object_key(),
                    &source,
                    &ledger.normalized,
                )
                .await?
            {
                Some(checkpoint) => self.store.checkpoint_tos_object(
                    recording_id,
                    checkpoint.bucket,
                    checkpoint.version_id,
                    checkpoint.etag,
                    checkpoint.sha256,
                    checkpoint.size_bytes,
                )?,
                None => self.store.checkpoint_canceled_upload_absent(recording_id)?,
            };
        }
        if ledger.state != super::ProcessingState::CanceledAfterUpload
            || ledger.cleanup.temporary_tos_deleted
        {
            return Ok(ledger);
        }
        let object = ledger
            .tos_object
            .as_ref()
            .ok_or(EngineError::InvalidRecording("TOS checkpoint is missing"))?;
        self.effects.cleanup_tos(object).await?;
        Ok(self.store.checkpoint_canceled_tos_cleanup(recording_id)?)
    }

    pub fn discard_local(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        if self.moss_task_active(recording_id) {
            return Err(ProcessingError::new("processing_busy").into());
        }
        let ledger = match self.store.load(recording_id) {
            Ok(ledger) => ledger,
            Err(error) if error.code == "recording_not_found" => self.enqueue(recording_id)?,
            Err(error) => return Err(error.into()),
        };
        if ledger.state == super::ProcessingState::Discarded {
            return Ok(ledger);
        }
        if ledger.state != super::ProcessingState::Discarding {
            // Verify the only local copy immediately before committing the
            // destructive intent to durable state.
            self.enqueue(recording_id)?;
            self.store.begin_discard(recording_id)?;
        }
        self.inbox
            .discard_package(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording could not be discarded"))?;
        Ok(self.store.checkpoint_discarded(recording_id)?)
    }

    pub async fn retry(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        let ledger = self.store.load(recording_id)?;
        if ledger.transcription_backend == super::TranscriptionBackend::MossLocal {
            self.retry_moss_owned(&ledger)?;
            return self.run_until_wait(recording_id).await;
        }
        match ledger.state {
            super::ProcessingState::ProviderFailed => {
                self.store.retry_provider(recording_id)?;
            }
            super::ProcessingState::PublishFailed | super::ProcessingState::PublishConflict => {
                self.store.retry_publication(recording_id)?;
            }
            super::ProcessingState::SummaryAmbiguous => {
                self.store.resolve_summary_for_retry(recording_id)?;
            }
            super::ProcessingState::SubmitAmbiguous | super::ProcessingState::PublishAmbiguous => {
                return Err(EngineError::ManualResolutionRequired);
            }
            super::ProcessingState::Complete
            | super::ProcessingState::CanceledBeforeUpload
            | super::ProcessingState::CanceledAfterUpload
            | super::ProcessingState::Discarded => return Ok(ledger),
            _ => {}
        }
        self.run_until_wait(recording_id).await
    }

    /// Explicitly regenerate the summary and publish a new archive generation.
    /// The existing normalized audio, TOS identity, and 妙记 transcript remain
    /// authoritative and are verified/reused instead of uploaded or submitted
    /// again.
    pub async fn reprocess(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        self.require_moss_executable(&self.store.load(recording_id)?)?;
        self.enqueue(recording_id)?;
        self.store.begin_reprocess(recording_id)?;
        self.run_until_wait(recording_id).await
    }

    /// Explicitly upload a completed full-local archive generation to the
    /// user's configured private GitHub/R2 destinations. The stored local
    /// transcript and Qwen summary are reused; no TOS, 妙记, or Gemini effect
    /// is created.
    pub async fn back_up_local_to_cloud(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        self.require_moss_executable(&self.store.load(recording_id)?)?;
        let envelope = self
            .inbox
            .load_envelope(recording_id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        let plan = self
            .effects
            .publication_plan(&envelope, super::PublicationBackend::RemoteArchive)?;
        self.store.begin_remote_backup(recording_id, plan)?;
        self.run_until_wait(recording_id).await
    }

    pub async fn run_until_wait(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        let initial = self.store.load(recording_id)?;
        let moss_scope = if initial.transcription_backend == super::TranscriptionBackend::MossLocal
            && !initial.state.is_terminal()
        {
            match self.begin_moss_run(&initial)? {
                Some(scope) => Some(scope),
                None => return Ok(self.store.load(recording_id)?),
            }
        } else {
            None
        };
        if let Some(scope) = &moss_scope {
            self.store
                .recover_moss_local_summary(recording_id, &scope.owner)?;
        }
        for _ in 0..MAX_IMMEDIATE_ACTIONS {
            if moss_scope.as_ref().is_some_and(|scope| scope.cancelled()) {
                return Ok(self.store.load(recording_id)?);
            }
            let (action, tos_operation_guard, tos_operation_already_active) = {
                let mut operations = self
                    .tos_operations_in_flight
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let action = self.store.resume(recording_id)?;
                let owns_tos_operation = matches!(
                    action,
                    ResumeAction::Upload | ResumeAction::ReconcileCanceledUpload
                );
                let tos_operation_already_active =
                    owns_tos_operation && operations.contains(&recording_id);
                let tos_operation_guard = if owns_tos_operation && !tos_operation_already_active {
                    operations.insert(recording_id);
                    Some(TosOperationGuard {
                        recording_id,
                        operations: &self.tos_operations_in_flight,
                        finished: &self.tos_operation_finished,
                    })
                } else {
                    None
                };
                (action, tos_operation_guard, tos_operation_already_active)
            };
            if tos_operation_already_active {
                return Ok(self.store.load(recording_id)?);
            }
            let envelope = self
                .inbox
                .load_envelope(recording_id)
                .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
            let ledger = self.store.load(recording_id)?;
            match action {
                ResumeAction::PrepareMoss { generation } => {
                    let scope = moss_scope
                        .as_ref()
                        .ok_or_else(|| ProcessingError::new("moss_owner_required"))?;
                    self.run_moss_preparation(recording_id, generation, scope)
                        .await?;
                }
                ResumeAction::TranscribeMoss { generation } => {
                    let scope = moss_scope
                        .as_ref()
                        .ok_or_else(|| ProcessingError::new("moss_owner_required"))?;
                    self.run_moss_stage(recording_id, generation, scope).await?;
                }
                ResumeAction::TranscribeLocal { request } => {
                    match self.effects.transcribe_local(&request).await {
                        Ok(response) => {
                            self.store
                                .checkpoint_local_whisper_transcript(recording_id, response)?;
                        }
                        Err(error) => {
                            self.store.mark_local_whisper_failed(recording_id)?;
                            return Err(error.into());
                        }
                    }
                }
                ResumeAction::Upload => {
                    let tos_operation_guard =
                        tos_operation_guard.expect("upload action must own its in-flight marker");
                    let source = self
                        .inbox
                        .package_file_path(recording_id, &ledger.normalized.relative_path)
                        .map_err(|_| {
                            EngineError::InvalidRecording("recording audio is unavailable")
                        })?;
                    let upload_result = self
                        .effects
                        .upload_tos(
                            recording_id,
                            &ledger.object_key(),
                            &source,
                            &ledger.normalized,
                        )
                        .await;
                    let result: Result<ProcessingLedger, EngineError> = match upload_result {
                        Ok(checkpoint) => {
                            let ledger = self.store.checkpoint_tos_object(
                                recording_id,
                                checkpoint.bucket,
                                checkpoint.version_id,
                                checkpoint.etag,
                                checkpoint.sha256,
                                checkpoint.size_bytes,
                            )?;
                            Ok(ledger)
                        }
                        Err(error)
                            if matches!(
                                error.kind,
                                EffectErrorKind::NotDispatched | EffectErrorKind::Rejected
                            ) =>
                        {
                            let ledger = self.store.mark_upload_rejected(recording_id)?;
                            if matches!(
                                ledger.state,
                                super::ProcessingState::CanceledBeforeUpload
                                    | super::ProcessingState::CanceledAfterUpload
                            ) {
                                Ok(ledger)
                            } else {
                                Err(error.into())
                            }
                        }
                        Err(error) => Err(error.into()),
                    };
                    drop(tos_operation_guard);
                    let ledger = result?;
                    if matches!(
                        ledger.state,
                        super::ProcessingState::CanceledBeforeUpload
                            | super::ProcessingState::CanceledAfterUpload
                    ) {
                        return Ok(ledger);
                    }
                }
                ResumeAction::ReconcileCanceledUpload => {
                    let tos_operation_guard = tos_operation_guard
                        .expect("reconciliation action must own its in-flight marker");
                    let result = self.resume_canceled_cleanup_inner(recording_id).await;
                    drop(tos_operation_guard);
                    return result;
                }
                ResumeAction::DispatchMiaoji { request_id } => {
                    let object = ledger
                        .tos_object
                        .as_ref()
                        .ok_or(EngineError::InvalidRecording("TOS checkpoint is missing"))?;
                    let speaker_count = confirmed_speaker_count(&envelope);
                    match self
                        .effects
                        .submit_miaoji(object, &request_id, speaker_count)
                        .await
                    {
                        Ok(task_id) => {
                            self.store.checkpoint_miaoji_task(recording_id, task_id)?;
                        }
                        Err(error) if error.kind == EffectErrorKind::SubmitAmbiguous => {
                            self.store.mark_submit_ambiguous(recording_id)?;
                            return Err(error.into());
                        }
                        Err(error) if error.kind == EffectErrorKind::Rejected => {
                            self.store.mark_submit_rejected(recording_id)?;
                            return Err(error.into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                ResumeAction::PollMiaoji { task_id } => {
                    let request_id = ledger
                        .miaoji
                        .as_ref()
                        .map(|checkpoint| checkpoint.request_id.as_str())
                        .ok_or(EngineError::InvalidRecording(
                            "Miaoji checkpoint is missing",
                        ))?;
                    match self.effects.poll_miaoji(request_id, &task_id).await? {
                        PollResult::Running => return Ok(ledger),
                        PollResult::Complete(transcript) => {
                            self.store.checkpoint_transcript(recording_id, transcript)?;
                        }
                    }
                }
                ResumeAction::Summarize => {
                    let transcript =
                        ledger
                            .transcript_json
                            .as_ref()
                            .ok_or(EngineError::InvalidRecording(
                                "transcript checkpoint is missing",
                            ))?;
                    let text = super::transcript_for_summary(transcript)?;
                    let request = if ledger.summary_backend == super::SummaryBackend::QwenLocal {
                        SummaryRequest::Local(ledger.local_summary_request(text)?)
                    } else {
                        SummaryRequest::Gemini { transcript: text }
                    };
                    let result = if let Some(scope) = &moss_scope {
                        let SummaryRequest::Local(request) = request else {
                            return Err(ProcessingError::new("invalid_ledger").into());
                        };
                        self.run_moss_summary(request, scope).await
                    } else {
                        self.effects.summarize(&request).await
                    };
                    if moss_scope.as_ref().is_some_and(|scope| scope.cancelled()) {
                        return Ok(self.store.load(recording_id)?);
                    }
                    match result {
                        Ok(summary) => {
                            self.store.checkpoint_summary(recording_id, summary)?;
                        }
                        Err(error)
                            if matches!(
                                error.kind,
                                EffectErrorKind::NotDispatched | EffectErrorKind::Rejected
                            ) =>
                        {
                            self.store.abort_summary_before_dispatch(recording_id)?;
                            return Err(error.into());
                        }
                        Err(error) => return Err(error.into()),
                    }
                }
                ResumeAction::AwaitPublicationPlan => {
                    let plan = self
                        .effects
                        .publication_plan(&envelope, ledger.publication_backend)?;
                    self.store.plan_publication(recording_id, plan)?;
                }
                ResumeAction::PublishTarget {
                    generation,
                    target_id,
                } => {
                    self.store
                        .start_publication_target(recording_id, generation, &target_id)?;
                    let transcript =
                        ledger
                            .transcript_json
                            .as_ref()
                            .ok_or(EngineError::InvalidRecording(
                                "transcript checkpoint is missing",
                            ))?;
                    let summary =
                        ledger
                            .summary_json
                            .as_ref()
                            .ok_or(EngineError::InvalidRecording(
                                "summary checkpoint is missing",
                            ))?;
                    let result = if let Some(scope) = &moss_scope {
                        let effects = Arc::clone(&self.effects);
                        let target_id = target_id.clone();
                        let envelope = envelope.clone();
                        let transcript = transcript.clone();
                        let summary = summary.clone();
                        let artifact = ledger.normalized.clone();
                        moss::owned_effect(scope, async move {
                            effects
                                .publish_target(
                                    &target_id,
                                    generation,
                                    &envelope,
                                    &transcript,
                                    &summary,
                                    &artifact,
                                )
                                .await
                        })
                        .await
                    } else {
                        self.effects
                            .publish_target(
                                &target_id,
                                generation,
                                &envelope,
                                transcript,
                                summary,
                                &ledger.normalized,
                            )
                            .await
                    };
                    match result {
                        Ok(proof) => {
                            self.store.checkpoint_publication_target(
                                recording_id,
                                generation,
                                &target_id,
                                proof,
                            )?;
                        }
                        Err(error) if error.kind == EffectErrorKind::PublicationConflict => {
                            self.store.mark_publication_conflict(recording_id)?;
                            return Err(error.into());
                        }
                        Err(error) => {
                            let outcome = if error.kind == EffectErrorKind::PublicationAmbiguous {
                                TargetOutcome::Ambiguous
                            } else {
                                TargetOutcome::ExplicitFailure
                            };
                            self.store.fail_publication_target(
                                recording_id,
                                generation,
                                &target_id,
                                outcome,
                            )?;
                            return Err(error.into());
                        }
                    }
                }
                ResumeAction::VerifyCanonicalBackup { generation } => {
                    let proof = if let Some(scope) = &moss_scope {
                        let effects = Arc::clone(&self.effects);
                        let envelope = envelope.clone();
                        let artifact = ledger.normalized.clone();
                        let backend = ledger.publication_backend;
                        moss::owned_effect(scope, async move {
                            effects
                                .verify_canonical_backup(backend, generation, &envelope, &artifact)
                                .await
                        })
                        .await?
                    } else {
                        self.effects
                            .verify_canonical_backup(
                                ledger.publication_backend,
                                generation,
                                &envelope,
                                &ledger.normalized,
                            )
                            .await?
                    };
                    self.store
                        .checkpoint_canonical_backup(recording_id, generation, proof)?;
                }
                ResumeAction::Cleanup { generation } => {
                    if !ledger.cleanup.temporary_tos_deleted {
                        if let Some(object) = ledger.tos_object.as_ref() {
                            self.effects.cleanup_tos(object).await?;
                        } else if ledger.transcription_backend
                            == super::TranscriptionBackend::MiaojiRemote
                        {
                            return Err(EngineError::InvalidRecording("TOS checkpoint is missing"));
                        }
                    }
                    return Ok(self.store.checkpoint_cleanup(recording_id, generation)?);
                }
                ResumeAction::ManualSubmitResolution
                | ResumeAction::ManualLocalResolution
                | ResumeAction::ManualSummaryResolution
                | ResumeAction::ManualPublicationResolution => {
                    return Err(EngineError::ManualResolutionRequired);
                }
                ResumeAction::Canceled | ResumeAction::Done => return Ok(ledger),
            }
        }
        Err(EngineError::StepLimit)
    }
}

fn confirmed_speaker_count(envelope: &RecordingEnvelope) -> Option<u32> {
    if envelope.source.kind != SourceKind::FileImport {
        return None;
    }
    envelope
        .import_review
        .as_ref()
        .and_then(|review| review.confirmed_at.as_ref().and(review.speaker_count))
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::fs;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Mutex;
    use std::time::Duration;

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    use std::sync::atomic::AtomicUsize;

    use chrono::{TimeDelta, Utc};
    use serde_json::json;
    use sha2::Digest;
    use tempfile::TempDir;

    use crate::ingest::envelope::{
        AudioTrack, CaptureScope, JobStatus, Platform, RecordingSource, TrackRole,
        RECORDING_ENVELOPE_VERSION,
    };
    use crate::ingest::inbox::InboxEvent;
    use crate::ingest::state::JobState;

    use super::*;
    use crate::processing::{ProcessingState, RetryMode, TosObjectCheckpoint};

    #[derive(Default)]
    struct FakeEffects {
        calls: Mutex<Vec<&'static str>>,
        polls: Mutex<u32>,
        upload_failure: Mutex<Option<EffectErrorKind>>,
        block_upload: AtomicBool,
        upload_started: Notify,
        upload_release: Notify,
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    struct LiveLongMiaojiEffects {
        tos: crate::processing::tos::DirectTos,
        providers: crate::processing::providers::DirectProviders<
            crate::processing::providers::ReqwestProviderTransport,
        >,
        api_key: crate::processing::providers::SecretText,
        polls: AtomicUsize,
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn live_tos_error(error: crate::processing::tos::TosErrorSafe) -> EffectError {
        use crate::processing::tos::TosErrorKind;

        let kind = match error.kind {
            TosErrorKind::Network => EffectErrorKind::Temporary,
            TosErrorKind::Conflict => EffectErrorKind::PublicationConflict,
            TosErrorKind::Configuration | TosErrorKind::NotFound | TosErrorKind::Verification => {
                EffectErrorKind::Verification
            }
        };
        EffectError::new(kind)
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn live_provider_error(error: crate::processing::providers::ProviderError) -> EffectError {
        use crate::processing::providers::ProviderErrorKind;

        let kind = match error.kind {
            ProviderErrorKind::NotDispatched => EffectErrorKind::NotDispatched,
            ProviderErrorKind::Network => EffectErrorKind::Temporary,
            ProviderErrorKind::Rejected => EffectErrorKind::Rejected,
            ProviderErrorKind::SubmitAmbiguous => EffectErrorKind::SubmitAmbiguous,
            ProviderErrorKind::Configuration
            | ProviderErrorKind::InvalidResponse
            | ProviderErrorKind::ResponseTooLarge => EffectErrorKind::Verification,
        };
        EffectError::new(kind)
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[async_trait]
    impl ProcessingEffects for LiveLongMiaojiEffects {
        async fn upload_tos(
            &self,
            recording_id: Uuid,
            object_key: &str,
            source: &Path,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<TosObjectCheckpoint, EffectError> {
            let receipt = self
                .tos
                .upload_verified(
                    &recording_id.to_string(),
                    object_key,
                    source,
                    &artifact.sha256,
                    artifact.size_bytes,
                )
                .await
                .map_err(live_tos_error)?;
            Ok(TosObjectCheckpoint {
                bucket: receipt.bucket,
                key: receipt.key,
                version_id: receipt.version_id,
                etag: receipt.etag,
                sha256: receipt.sha256,
                size_bytes: receipt.size_bytes,
            })
        }

        async fn probe_tos(
            &self,
            recording_id: Uuid,
            object_key: &str,
            source: &Path,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<Option<TosObjectCheckpoint>, EffectError> {
            self.tos
                .probe_source_expected(
                    &recording_id.to_string(),
                    object_key,
                    source,
                    &artifact.sha256,
                    artifact.size_bytes,
                )
                .await
                .map(|receipt| {
                    receipt.map(|receipt| TosObjectCheckpoint {
                        bucket: receipt.bucket,
                        key: receipt.key,
                        version_id: receipt.version_id,
                        etag: receipt.etag,
                        sha256: receipt.sha256,
                        size_bytes: receipt.size_bytes,
                    })
                })
                .map_err(live_tos_error)
        }

        async fn submit_miaoji(
            &self,
            object: &TosObjectCheckpoint,
            request_id: &str,
            speaker_count: Option<u32>,
        ) -> Result<String, EffectError> {
            let receipt = crate::processing::tos::TosObjectReceipt {
                bucket: object.bucket.clone(),
                key: object.key.clone(),
                version_id: object.version_id.clone(),
                etag: object.etag.clone(),
                sha256: object.sha256.clone(),
                size_bytes: object.size_bytes,
            };
            let url = self
                .tos
                .presign_exact_get(&receipt, 7_200)
                .await
                .map_err(|_| EffectError::new(EffectErrorKind::NotDispatched))?;
            self.providers
                .submit_miaoji(&self.api_key, &url, request_id, speaker_count)
                .await
                .map_err(live_provider_error)
        }

        async fn poll_miaoji(
            &self,
            request_id: &str,
            task_id: &str,
        ) -> Result<PollResult, EffectError> {
            use crate::processing::providers::MiaojiPoll;

            self.polls.fetch_add(1, Ordering::Relaxed);
            match self
                .providers
                .poll_miaoji_once(&self.api_key, request_id, task_id)
                .await
                .map_err(live_provider_error)?
            {
                MiaojiPoll::Running => Ok(PollResult::Running),
                MiaojiPoll::Complete { transcript_url } => self
                    .providers
                    .fetch_miaoji_transcript(transcript_url)
                    .await
                    .map(PollResult::Complete)
                    .map_err(live_provider_error),
            }
        }

        async fn summarize(&self, _: &SummaryRequest) -> Result<Value, EffectError> {
            Ok(json!({
                "title": "Long fabricated provider proof",
                "summary": "The long Miaoji route completed.",
                "category": "other",
                "key_points": ["fabricated input"],
                "action_items": [],
                "decisions": [],
                "open_questions": []
            }))
        }

        fn publication_plan(
            &self,
            _: &RecordingEnvelope,
            _: super::super::PublicationBackend,
        ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
            Ok(vec![PublicationTargetPlan {
                id: "long_miaoji_local_proof".to_owned(),
                retry_mode: RetryMode::ReconcileBeforeRetry,
                required: true,
            }])
        }

        async fn publish_target(
            &self,
            _: &str,
            generation: u64,
            _: &RecordingEnvelope,
            _: &Value,
            _: &Value,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<PublicationProof, EffectError> {
            Ok(PublicationProof {
                locator: "local:long-miaoji-provider-proof".to_owned(),
                version: format!("fixture-{generation}"),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
            })
        }

        async fn verify_canonical_backup(
            &self,
            _: super::super::PublicationBackend,
            generation: u64,
            _: &RecordingEnvelope,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<CanonicalBackupCheckpoint, EffectError> {
            Ok(CanonicalBackupCheckpoint {
                locator: "local:long-miaoji-provider-proof".to_owned(),
                version_id: format!("fixture-{generation}"),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
                proof_json: json!({"test_only": true}),
            })
        }

        async fn cleanup_tos(&self, object: &TosObjectCheckpoint) -> Result<(), EffectError> {
            let receipt = crate::processing::tos::TosObjectReceipt {
                bucket: object.bucket.clone(),
                key: object.key.clone(),
                version_id: object.version_id.clone(),
                etag: object.etag.clone(),
                sha256: object.sha256.clone(),
                size_bytes: object.size_bytes,
            };
            self.tos
                .delete_exact(&receipt)
                .await
                .map_err(live_tos_error)?;
            let recording_id = object
                .key
                .split('/')
                .nth(3)
                .ok_or_else(|| EffectError::new(EffectErrorKind::Verification))?;
            let remaining = self
                .tos
                .probe_expected(
                    recording_id,
                    &object.key,
                    &object.sha256,
                    object.size_bytes,
                    0,
                )
                .await
                .map_err(live_tos_error)?;
            if remaining.is_some() {
                return Err(EffectError::new(EffectErrorKind::Verification));
            }
            Ok(())
        }
    }

    #[async_trait]
    impl ProcessingEffects for FakeEffects {
        async fn transcribe_local(
            &self,
            request: &LocalTranscriptionRequest,
        ) -> Result<LocalWhisperResponse, EffectError> {
            let (schema_version, recording_id, model_id, model_sha256, audio_sha256, language) =
                match (&request.whisper, &request.qwen) {
                    (Some(whisper), None) => (
                        whisper.schema_version,
                        whisper.recording_id,
                        whisper.model_id.clone(),
                        whisper.model_sha256.clone(),
                        whisper.audio_sha256.clone(),
                        whisper.language.clone().unwrap_or_else(|| "en".to_owned()),
                    ),
                    (None, Some(qwen)) => (
                        super::super::local_whisper::LOCAL_WHISPER_PROTOCOL_VERSION,
                        qwen.recording_id,
                        qwen.asr_model_id.clone(),
                        qwen.model_set_sha256()
                            .map_err(|_| EffectError::new(EffectErrorKind::Verification))?,
                        qwen.audio_sha256.clone(),
                        qwen.language.clone().unwrap_or_else(|| "und".to_owned()),
                    ),
                    _ => return Err(EffectError::new(EffectErrorKind::Verification)),
                };
            self.calls.lock().unwrap().push("local");
            Ok(LocalWhisperResponse {
                schema_version,
                recording_id,
                model_id,
                model_sha256,
                audio_sha256,
                language,
                segments: vec![super::super::local_whisper::LocalWhisperSegment {
                    start_ms: 0,
                    end_ms: 1,
                    text: "fabricated speech from local Whisper".to_owned(),
                    speaker_id: request
                        .diarization
                        .as_ref()
                        .map(|_| "local_speaker_01".to_owned()),
                }],
            })
        }

        async fn upload_tos(
            &self,
            _recording_id: Uuid,
            object_key: &str,
            _source: &Path,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<TosObjectCheckpoint, EffectError> {
            self.calls.lock().unwrap().push("upload");
            if self.block_upload.load(Ordering::SeqCst) {
                self.upload_started.notify_waiters();
                self.upload_release.notified().await;
            }
            if let Some(kind) = *self.upload_failure.lock().unwrap() {
                return Err(EffectError::new(kind));
            }
            Ok(TosObjectCheckpoint {
                bucket: "fabricated-bucket".to_owned(),
                key: object_key.to_owned(),
                version_id: "version-1".to_owned(),
                etag: "etag-1".to_owned(),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
            })
        }

        async fn probe_tos(
            &self,
            _recording_id: Uuid,
            _object_key: &str,
            _source: &Path,
            _artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<Option<TosObjectCheckpoint>, EffectError> {
            self.calls.lock().unwrap().push("probe");
            Ok(None)
        }

        async fn submit_miaoji(
            &self,
            _object: &TosObjectCheckpoint,
            _request_id: &str,
            _speaker_count: Option<u32>,
        ) -> Result<String, EffectError> {
            self.calls.lock().unwrap().push("submit");
            Ok("task-1".to_owned())
        }

        async fn poll_miaoji(
            &self,
            _request_id: &str,
            _task_id: &str,
        ) -> Result<PollResult, EffectError> {
            self.calls.lock().unwrap().push("poll");
            let mut polls = self.polls.lock().unwrap();
            *polls += 1;
            if *polls == 1 {
                Ok(PollResult::Running)
            } else {
                Ok(PollResult::Complete(json!([{
                    "speaker": {"id": "1"}, "content": "fabricated speech"
                }])))
            }
        }

        async fn summarize(&self, request: &SummaryRequest) -> Result<Value, EffectError> {
            let transcript = match request {
                SummaryRequest::Gemini { transcript } => transcript,
                SummaryRequest::Local(request) => &request.transcript,
            };
            assert!(transcript.contains("fabricated speech"));
            self.calls.lock().unwrap().push("summary");
            Ok(json!({"title": "Synthetic", "summary": "Safe"}))
        }

        fn publication_plan(
            &self,
            _envelope: &RecordingEnvelope,
            backend: super::super::PublicationBackend,
        ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
            Ok(vec![PublicationTargetPlan {
                id: if backend == super::super::PublicationBackend::LocalArchive {
                    "local_archive"
                } else {
                    "archive"
                }
                .to_owned(),
                retry_mode: RetryMode::ReconcileBeforeRetry,
                required: true,
            }])
        }

        async fn publish_target(
            &self,
            _target_id: &str,
            _generation: u64,
            _envelope: &RecordingEnvelope,
            _transcript: &Value,
            _summary: &Value,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<PublicationProof, EffectError> {
            self.calls.lock().unwrap().push("publish");
            Ok(PublicationProof {
                locator: "archive/synthetic".to_owned(),
                version: "commit-1".to_owned(),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
            })
        }

        async fn verify_canonical_backup(
            &self,
            _backend: super::super::PublicationBackend,
            _generation: u64,
            _envelope: &RecordingEnvelope,
            artifact: &NormalizedArtifactCheckpoint,
        ) -> Result<CanonicalBackupCheckpoint, EffectError> {
            self.calls.lock().unwrap().push("backup");
            Ok(CanonicalBackupCheckpoint {
                locator: "r2/synthetic".to_owned(),
                version_id: "r2-version-1".to_owned(),
                sha256: artifact.sha256.clone(),
                size_bytes: artifact.size_bytes,
                proof_json: json!({"verified": true}),
            })
        }

        async fn cleanup_tos(&self, _object: &TosObjectCheckpoint) -> Result<(), EffectError> {
            self.calls.lock().unwrap().push("cleanup");
            Ok(())
        }
    }

    pub(super) fn fixture() -> (TempDir, Arc<Inbox>, Arc<ProcessingStore>, Uuid) {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("archive");
        fs::create_dir(&archive).unwrap();
        let app_data = temp.path().join("app-data");
        let inbox = Arc::new(Inbox::open(&app_data, &archive).unwrap());
        let store = Arc::new(ProcessingStore::open(&app_data, inbox.root(), &archive).unwrap());
        let recording_id = Uuid::new_v4();
        let bytes = b"fabricated normalized audio";
        let sha256 = hex::encode(sha2::Sha256::digest(bytes));
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id,
            source: RecordingSource {
                kind: SourceKind::FileImport,
                platform: Platform::Macos,
                label: None,
                capture_scope: CaptureScope::ImportedFile,
            },
            captured_at: Utc::now().fixed_offset(),
            ended_at: Utc::now().fixed_offset(),
            duration_ms: 1_000,
            tracks: vec![AudioTrack {
                role: TrackRole::Imported,
                relative_path: "tracks/input.wav".to_owned(),
                codec: "pcm_s16le".to_owned(),
                sample_rate: 16_000,
                channels: 1,
                duration_ms: 1_000,
                clock_start_ns: 0,
                sha256: sha256.clone(),
            }],
            normalized_audio: Some("tracks/input.wav".to_owned()),
            normalized_sha256: Some(sha256.clone()),
            imported_name: Some("synthetic.wav".to_owned()),
            import_review: None,
            capture_warnings: Vec::new(),
            job: JobStatus {
                state: JobState::Ready,
                attempt: 0,
                remote_job_id: None,
                last_error: None,
            },
        };
        inbox.persist_envelope(&envelope).unwrap();
        let package = inbox.root().join(recording_id.to_string());
        fs::write(package.join("tracks/input.wav"), bytes).unwrap();
        inbox
            .append_event(
                &InboxEvent::new(
                    recording_id,
                    "fixture_ready",
                    Utc::now().fixed_offset(),
                    BTreeMap::new(),
                )
                .unwrap(),
            )
            .unwrap();
        (temp, inbox, store, recording_id)
    }

    #[tokio::test]
    async fn engine_runs_direct_effects_and_resumes_poll_without_a_service() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();

        let waiting = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(waiting.state, ProcessingState::Polling);
        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert!(complete.cleanup.temporary_tos_deleted);
        assert!(complete.cleanup.source_tracks_delete_after.is_some());
        assert_eq!(engine.list_recording_ids().unwrap(), [recording_id]);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["upload", "submit", "poll", "poll", "summary", "publish", "backup", "cleanup"]
        );

        let reprocessed = engine.reprocess(recording_id).await.unwrap();
        assert_eq!(reprocessed.state, ProcessingState::Complete);
        assert_eq!(reprocessed.publication.unwrap().generation, 2);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            [
                "upload", "submit", "poll", "poll", "summary", "publish", "backup", "cleanup",
                "summary", "publish", "backup"
            ]
        );
    }

    #[tokio::test]
    async fn pre_dispatch_upload_rejection_is_durable_and_retry_resumes_upload() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        *effects.upload_failure.lock().unwrap() = Some(EffectErrorKind::Rejected);
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();

        let error = engine.run_until_wait(recording_id).await.unwrap_err();
        assert!(matches!(
            error,
            EngineError::Effect(EffectError {
                kind: EffectErrorKind::Rejected,
                ..
            })
        ));
        let failed = store.load(recording_id).unwrap();
        assert_eq!(failed.state, ProcessingState::ProviderFailed);
        assert!(failed.tos_object.is_none());
        assert_eq!(
            store.resume(recording_id).unwrap(),
            ResumeAction::ManualSubmitResolution
        );

        *effects.upload_failure.lock().unwrap() = None;
        let retrying = engine.retry_provider(recording_id).unwrap();
        assert_eq!(retrying.state, ProcessingState::Uploading);
        let waiting = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(waiting.state, ProcessingState::Polling);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["upload", "upload", "submit", "poll"]
        );
    }

    #[tokio::test]
    async fn engine_local_whisper_path_skips_tos_and_miaoji_and_publishes_once() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();
        store
            .select_local_whisper(
                recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                1_500_000_000,
                60_000,
                Some("en".to_owned()),
            )
            .unwrap();

        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert!(complete.tos_object.is_none());
        assert!(complete.miaoji.is_none());
        assert_eq!(
            complete.transcript_json.as_ref().unwrap()[0]["stt_backend"],
            "whisper_local"
        );
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup"]
        );
        assert_eq!(engine.run_until_wait(recording_id).await.unwrap(), complete);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup"]
        );
    }

    #[tokio::test]
    async fn engine_full_local_path_requires_diarization_and_skips_remote_transcription() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();
        store
            .select_full_local(
                recording_id,
                "large-v3-turbo-q5_0".to_owned(),
                "b".repeat(64),
                574_041_195,
                60_000,
                Some("en".to_owned()),
                "fluid-v1".to_owned(),
                vec![super::super::local_whisper::LocalModelFileIdentity {
                    relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                    sha256: "c".repeat(64),
                    size_bytes: 43_063,
                }],
                Some(1),
                "qwen3.8-27b-ud-q4-k-xl".to_owned(),
                "d".repeat(64),
                17_559_178_144,
            )
            .unwrap();

        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert!(complete.tos_object.is_none());
        assert!(complete.miaoji.is_none());
        assert_eq!(
            complete.summary_backend,
            super::super::SummaryBackend::QwenLocal
        );
        assert_eq!(
            complete.publication_backend,
            super::super::PublicationBackend::LocalArchive
        );
        assert!(complete
            .local_summary
            .as_ref()
            .and_then(|checkpoint| checkpoint.transcript_sha256.as_ref())
            .is_some());
        assert_eq!(
            complete.transcript_json.as_ref().unwrap()[0]["speaker"]["id"],
            "local_speaker_01"
        );
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup"]
        );

        let backed_up = engine.back_up_local_to_cloud(recording_id).await.unwrap();
        assert_eq!(backed_up.state, ProcessingState::Complete);
        assert_eq!(
            backed_up.publication_backend,
            super::super::PublicationBackend::RemoteArchive
        );
        assert_eq!(backed_up.publication.as_ref().unwrap().generation, 2);
        assert_eq!(backed_up.transcript_json, complete.transcript_json);
        assert_eq!(backed_up.summary_json, complete.summary_json);
        assert!(backed_up.tos_object.is_none());
        assert!(backed_up.miaoji.is_none());
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup", "publish", "backup"]
        );
    }

    #[tokio::test]
    async fn engine_qwen_candidate_path_is_durable_full_local_and_zero_remote() {
        use echowall_local_qwen_protocol::{
            LocalQwenModelFileIdentity, LOCAL_QWEN_ALIGNER_FILES, LOCAL_QWEN_ALIGNER_MODEL_ID,
            LOCAL_QWEN_ALIGNER_REVISION, LOCAL_QWEN_ASR_FILES, LOCAL_QWEN_ASR_MODEL_ID,
            LOCAL_QWEN_ASR_REVISION, LOCAL_QWEN_RUNTIME_ID,
        };

        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        let base_proof = LocalModelPackProof {
            whisper_model_id: "large-v3-turbo-q5_0".to_owned(),
            whisper_sha256: "b".repeat(64),
            whisper_size_bytes: 574_041_195,
            diarization_pack_id: "fluid-v1".to_owned(),
            diarization_files: vec![super::super::local_whisper::LocalModelFileIdentity {
                relative_path: "speaker-diarization/Segmentation.mlmodelc/model.mil".to_owned(),
                sha256: "c".repeat(64),
                size_bytes: 43_063,
            }],
            summary_model_id: "qwen3.8-27b-ud-q4-k-xl".to_owned(),
            summary_sha256: "d".repeat(64),
            summary_size_bytes: 17_559_178_144,
        };
        let identities = |paths: &[&str]| {
            paths
                .iter()
                .map(|path| LocalQwenModelFileIdentity {
                    relative_path: (*path).to_owned(),
                    sha256: "e".repeat(64),
                    size_bytes: 1,
                })
                .collect()
        };
        let qwen_proof = QwenCandidateModelPackProof {
            runtime_id: LOCAL_QWEN_RUNTIME_ID.to_owned(),
            asr_model_id: LOCAL_QWEN_ASR_MODEL_ID.to_owned(),
            asr_model_revision: LOCAL_QWEN_ASR_REVISION.to_owned(),
            asr_model_files: identities(&LOCAL_QWEN_ASR_FILES),
            aligner_model_id: LOCAL_QWEN_ALIGNER_MODEL_ID.to_owned(),
            aligner_model_revision: LOCAL_QWEN_ALIGNER_REVISION.to_owned(),
            aligner_model_files: identities(&LOCAL_QWEN_ALIGNER_FILES),
        };
        let selected = engine
            .select_full_local_qwen(recording_id, base_proof, qwen_proof, None)
            .unwrap();
        assert_eq!(
            selected.transcription_backend,
            super::super::TranscriptionBackend::QwenLocal
        );
        assert!(selected.local_whisper.is_none());
        assert!(selected.local_qwen.is_some());
        assert!(selected.tos_object.is_none());
        assert!(selected.miaoji.is_none());

        let request = match store.resume(recording_id).unwrap() {
            ResumeAction::TranscribeLocal { request } => request,
            action => panic!("unexpected Qwen resume action: {action:?}"),
        };
        assert!(request.whisper.is_none());
        assert_eq!(
            request
                .qwen
                .as_ref()
                .map(|request| request.runtime_id.as_str()),
            Some(LOCAL_QWEN_RUNTIME_ID)
        );

        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert_eq!(
            complete.transcription_backend,
            super::super::TranscriptionBackend::QwenLocal
        );
        assert_eq!(
            complete.publication_backend,
            super::super::PublicationBackend::LocalArchive
        );
        assert!(complete.tos_object.is_none());
        assert!(complete.miaoji.is_none());
        assert_eq!(
            complete.transcript_json.as_ref().unwrap()[0]["stt_backend"],
            "qwen_local"
        );
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup"]
        );
        assert_eq!(engine.run_until_wait(recording_id).await.unwrap(), complete);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["local", "summary", "publish", "backup"]
        );
    }

    #[tokio::test]
    async fn canceled_uploaded_audio_is_cleaned_and_restart_is_idempotent() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();

        let waiting = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(waiting.state, ProcessingState::Polling);
        let canceled = engine.cancel_and_cleanup(recording_id).await.unwrap();
        assert_eq!(canceled.state, ProcessingState::CanceledAfterUpload);
        assert!(canceled.cleanup.temporary_tos_deleted);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["upload", "submit", "poll", "cleanup"]
        );

        let resumed = engine.resume_canceled_cleanup(recording_id).await.unwrap();
        assert_eq!(resumed, canceled);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["upload", "submit", "poll", "cleanup"]
        );
    }

    #[tokio::test]
    async fn canceled_inflight_upload_reconciles_absence_without_a_second_put() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();
        store.begin_upload(recording_id).unwrap();
        let pending = store.cancel(recording_id).unwrap();
        assert_eq!(pending.state, ProcessingState::CancelingUpload);

        let canceled = engine.resume_canceled_cleanup(recording_id).await.unwrap();
        assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
        assert!(canceled.tos_object.is_none());
        assert_eq!(effects.calls.lock().unwrap().as_slice(), ["probe"]);
    }

    #[tokio::test]
    async fn cancel_waits_for_the_owned_upload_then_cleans_the_exact_receipt_once() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        effects.block_upload.store(true, Ordering::SeqCst);
        let engine = Arc::new(ProcessingEngine::new(
            inbox,
            Arc::clone(&store),
            Arc::clone(&effects),
        ));
        engine.enqueue(recording_id).unwrap();

        let runner = {
            let engine = Arc::clone(&engine);
            tokio::spawn(async move { engine.run_until_wait(recording_id).await })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if effects.calls.lock().unwrap().as_slice() == ["upload"] {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("upload fixture must enter its controlled in-flight boundary");

        let canceler = {
            let engine = Arc::clone(&engine);
            tokio::spawn(async move { engine.cancel_and_cleanup(recording_id).await })
        };
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if store.load(recording_id).unwrap().state == ProcessingState::CancelingUpload {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancel must durably fence the in-flight upload");

        effects.upload_release.notify_one();
        let runner_result = runner.await.unwrap().unwrap();
        assert_eq!(runner_result.state, ProcessingState::CanceledAfterUpload);
        let canceled = canceler.await.unwrap().unwrap();
        assert_eq!(canceled.state, ProcessingState::CanceledAfterUpload);
        assert!(canceled.cleanup.temporary_tos_deleted);
        assert_eq!(
            effects.calls.lock().unwrap().as_slice(),
            ["upload", "cleanup"]
        );
    }

    #[test]
    fn explicit_discard_is_durable_and_removes_only_the_local_package() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let package = inbox.root().join(recording_id.to_string());
        let engine = ProcessingEngine::new(Arc::clone(&inbox), store, Arc::clone(&effects));

        let discarded = engine.discard_local(recording_id).unwrap();
        assert_eq!(discarded.state, ProcessingState::Discarded);
        assert!(!package.exists());
        assert!(effects.calls.lock().unwrap().is_empty());
        assert_eq!(engine.discard_local(recording_id).unwrap(), discarded);
    }

    #[test]
    fn discard_replays_after_package_removal_and_before_final_checkpoint() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), effects);
        engine.enqueue(recording_id).unwrap();
        store.begin_discard(recording_id).unwrap();
        assert!(inbox.discard_package(recording_id).unwrap());

        let recovered = engine.discard_local(recording_id).unwrap();
        assert_eq!(recovered.state, ProcessingState::Discarded);
        assert!(!inbox.root().join(recording_id.to_string()).exists());
    }

    #[test]
    fn launch_scan_enqueues_ready_capture_without_adopting_unreviewed_imports() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), effects);
        assert!(engine.enqueue_ready_recordings().unwrap().is_empty());

        let mut envelope = inbox.load_envelope(recording_id).unwrap();
        envelope.source.kind = SourceKind::DesktopVoiceMemo;
        envelope.source.capture_scope = CaptureScope::Microphone;
        envelope.tracks[0].role = TrackRole::Microphone;
        envelope.imported_name = None;
        inbox.persist_envelope(&envelope).unwrap();

        assert_eq!(engine.enqueue_ready_recordings().unwrap(), [recording_id]);
        assert_eq!(
            store.load(recording_id).unwrap().state,
            ProcessingState::Queued
        );
    }

    #[tokio::test]
    async fn retention_prunes_only_separate_capture_tracks_after_thirty_days() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("archive");
        fs::create_dir(&archive).unwrap();
        let app_data = temp.path().join("app-data");
        let inbox = Arc::new(Inbox::open(&app_data, &archive).unwrap());
        let store = Arc::new(ProcessingStore::open(&app_data, inbox.root(), &archive).unwrap());
        let recording_id = Uuid::new_v4();
        let microphone_bytes = b"fabricated source track";
        let mixed_bytes = b"fabricated normalized mix";
        let microphone_sha256 = hex::encode(sha2::Sha256::digest(microphone_bytes));
        let mixed_sha256 = hex::encode(sha2::Sha256::digest(mixed_bytes));
        let now = Utc::now().fixed_offset();
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id,
            source: RecordingSource {
                kind: SourceKind::DesktopVoiceMemo,
                platform: Platform::Macos,
                label: Some("Synthetic".to_owned()),
                capture_scope: CaptureScope::Microphone,
            },
            captured_at: now,
            ended_at: now,
            duration_ms: 0,
            tracks: vec![AudioTrack {
                role: TrackRole::Microphone,
                relative_path: "tracks/microphone-0000.wav".to_owned(),
                codec: "pcm_s16le".to_owned(),
                sample_rate: 16_000,
                channels: 1,
                duration_ms: 0,
                clock_start_ns: 0,
                sha256: microphone_sha256,
            }],
            normalized_audio: Some("derived/mixed.wav".to_owned()),
            normalized_sha256: Some(mixed_sha256),
            imported_name: None,
            import_review: None,
            capture_warnings: Vec::new(),
            job: JobStatus {
                state: JobState::Ready,
                attempt: 0,
                remote_job_id: None,
                last_error: None,
            },
        };
        inbox.persist_envelope(&envelope).unwrap();
        let package = inbox.root().join(recording_id.to_string());
        fs::write(package.join("tracks/microphone-0000.wav"), microphone_bytes).unwrap();
        fs::write(package.join("derived/mixed.wav"), mixed_bytes).unwrap();

        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(Arc::clone(&inbox), store, effects);
        engine.enqueue(recording_id).unwrap();
        engine.run_until_wait(recording_id).await.unwrap();
        let complete = engine.run_until_wait(recording_id).await.unwrap();
        let eligible = complete.cleanup.source_tracks_delete_after.unwrap();
        assert_eq!(
            engine
                .sweep_source_track_retention(eligible - TimeDelta::seconds(1))
                .unwrap(),
            0
        );
        assert!(package.join("tracks/microphone-0000.wav").is_file());
        assert_eq!(
            engine
                .sweep_source_track_retention(eligible + TimeDelta::seconds(1))
                .unwrap(),
            1
        );
        assert!(!package.join("tracks/microphone-0000.wav").exists());
        assert!(package.join("derived/mixed.wav").is_file());
        assert!(
            engine
                .status(recording_id)
                .unwrap()
                .cleanup
                .source_tracks_deleted
        );
        assert_eq!(
            engine
                .sweep_source_track_retention(eligible + TimeDelta::days(1))
                .unwrap(),
            0
        );
    }

    #[tokio::test]
    async fn retention_never_deletes_an_imported_normalized_original() {
        let (_temp, inbox, store, recording_id) = fixture();
        let effects = Arc::new(FakeEffects::default());
        let engine = ProcessingEngine::new(Arc::clone(&inbox), store, effects);
        engine.enqueue(recording_id).unwrap();
        engine.run_until_wait(recording_id).await.unwrap();
        let complete = engine.run_until_wait(recording_id).await.unwrap();
        let eligible = complete.cleanup.source_tracks_delete_after.unwrap();
        assert_eq!(
            engine
                .sweep_source_track_retention(eligible + TimeDelta::seconds(1))
                .unwrap(),
            1
        );
        assert!(inbox
            .package_file_path(recording_id, "tracks/input.wav")
            .is_ok());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    #[ignore = "performs an explicitly authorized two-hour fabricated TOS/Miaoji rate proof"]
    async fn live_long_miaoji_route_is_durable_rate_limited_and_exactly_cleaned() {
        use std::time::{Duration, Instant};

        use crate::ingest::import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES};
        use crate::processing::providers::{DirectProviders, ReqwestProviderTransport, SecretText};
        use crate::processing::tos::{DirectTos, TosConfiguration};
        use crate::secrets::ProcessingCredentials;

        assert_eq!(
            std::env::var("ECHOWALL_LIVE_LONG_MIAOJI_CONFIRM").as_deref(),
            Ok("two-hour-fabricated-audio-authorized"),
            "long Miaoji proof requires the exact confirmation guard"
        );
        let required = |name: &str| {
            std::env::var(name).unwrap_or_else(|_| panic!("missing live credential field {name}"))
        };
        let app_root = fs::canonicalize(required("ECHOWALL_LIVE_LONG_MIAOJI_ROOT"))
            .expect("long live root must be an existing isolated directory");
        let canonical_temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(
            app_root.starts_with(&canonical_temp),
            "long live proof root must remain under the system temporary directory"
        );
        let audio = app_root.join("fabricated-provider-long-input.m4a");
        let metadata = fs::symlink_metadata(&audio)
            .expect("long fabricated audio fixture metadata is unavailable");
        assert!(
            metadata.is_file()
                && !metadata.file_type().is_symlink()
                && (1..1_000_000_000).contains(&metadata.len()),
            "official Miaoji limit requires a regular file below one gigabyte"
        );

        let credentials: ProcessingCredentials = serde_json::from_value(json!({
            "tosAccessKey": required("VOLC_TOS_ACCESS_KEY"),
            "tosSecretKey": required("VOLC_TOS_SECRET_KEY"),
            "tosBucket": required("VOLC_TOS_BUCKET"),
            "tosRegion": required("VOLC_TOS_REGION"),
            "tosEndpoint": required("VOLC_TOS_ENDPOINT"),
            "volcApiKey": required("VOLC_API_KEY"),
            "geminiApiKey": required("GEMINI_API_KEY"),
            "geminiModel": required("GEMINI_MODEL"),
        }))
        .expect("long live processing credential schema must decode");
        let tos = DirectTos::new(
            TosConfiguration::from_credentials(&credentials)
                .expect("long live TOS configuration must validate"),
        )
        .expect("long live TOS adapter must initialize");
        let providers = DirectProviders::new(Arc::new(
            ReqwestProviderTransport::new().expect("provider transport must initialize"),
        ));
        let api_key = SecretText::new(credentials.volc_api_key().to_owned())
            .expect("Miaoji API key must validate");
        let effects = Arc::new(LiveLongMiaojiEffects {
            tos,
            providers,
            api_key,
            polls: AtomicUsize::new(0),
        });

        let archive_root = app_root.join("archive");
        fs::create_dir_all(&archive_root).unwrap();
        let inbox = Arc::new(Inbox::open(&app_root, &archive_root).unwrap());
        let importer = DesktopImporter::new(
            Arc::clone(&inbox),
            Platform::Macos,
            DEFAULT_MAX_IMPORT_BYTES,
        )
        .unwrap();
        let imported = importer.import_paths(vec![audio.to_string_lossy().into_owned()]);
        let recording_id = Uuid::parse_str(
            imported.results[0]
                .recording_id
                .as_deref()
                .expect("long fabricated audio must import"),
        )
        .unwrap();
        let envelope = inbox.load_envelope(recording_id).unwrap();
        assert!(
            (7_130_000..=7_150_000).contains(&envelope.duration_ms),
            "long fixture must remain just below the official two-hour limit"
        );
        assert_eq!(
            envelope.imported_name.as_deref(),
            Some("fabricated-provider-long-input.m4a")
        );

        let store =
            Arc::new(ProcessingStore::open(&app_root, inbox.root(), &archive_root).unwrap());
        let engine =
            ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), Arc::clone(&effects));
        engine.enqueue(recording_id).unwrap();
        if store.load(recording_id).unwrap().state == ProcessingState::ProviderFailed {
            engine
                .retry_provider(recording_id)
                .expect("an explicitly rejected long provider request must be retryable");
        }

        let started = Instant::now();
        let deadline = started + Duration::from_secs(6 * 60 * 60);
        let complete = loop {
            match engine.run_until_wait(recording_id).await {
                Ok(ledger) if ledger.state == ProcessingState::Complete => break ledger,
                Ok(ledger) if ledger.state == ProcessingState::Polling => {}
                Ok(ledger) => panic!("long Miaoji route stopped in {:?}", ledger.state),
                Err(EngineError::Effect(error))
                    if error.kind == EffectErrorKind::Temporary
                        && store.load(recording_id).unwrap().state == ProcessingState::Polling =>
                {
                    // The accepted task remains authoritative. Only the
                    // idempotent query is retried at the documented rate.
                }
                Err(error) => panic!("long Miaoji route failed safely: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "long Miaoji polling exceeded six hours"
            );
            tokio::time::sleep(Duration::from_secs(31)).await;
        };

        assert_eq!(
            complete.transcription_backend,
            super::super::TranscriptionBackend::MiaojiRemote
        );
        assert!(complete.cleanup.temporary_tos_deleted);
        let transcript = complete
            .transcript_json
            .as_ref()
            .and_then(Value::as_array)
            .expect("long Miaoji transcript must be a sentence array");
        assert!(!transcript.is_empty());
        let speaker_labeled_segments = transcript
            .iter()
            .filter(|segment| {
                segment
                    .get("speaker")
                    .and_then(|speaker| speaker.get("id"))
                    .and_then(Value::as_str)
                    .is_some_and(|speaker| !speaker.is_empty())
            })
            .count();
        assert!(speaker_labeled_segments > 0);

        println!(
            "{}",
            json!({
                "state": "complete",
                "durationMs": envelope.duration_ms,
                "inputBytes": metadata.len(),
                "polls": effects.polls.load(Ordering::Relaxed),
                "elapsedSeconds": started.elapsed().as_secs(),
                "transcriptSegments": transcript.len(),
                "speakerLabeledSegments": speaker_labeled_segments,
                "pollIntervalSeconds": 31,
                "temporaryTosDeleted": true,
                "remoteArchiveEffects": false,
            })
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    #[ignore = "performs an explicitly authorized fabricated TOS/Miaoji/Gemini/GitHub/R2 proof"]
    async fn live_fabricated_remote_route_uses_secure_store_and_refreshes_archive() {
        use std::time::{Duration, Instant};

        use crate::ingest::import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES};
        use crate::processing::archive::DeferredArchive;
        use crate::processing::direct::DirectEffects;
        use crate::processing::local_worker::LocalWhisperWorker;
        use crate::secrets::{ProcessingCredentials, ProcessingCredentialsState, SyncTokens};

        assert_eq!(
            std::env::var("ECHOWALL_LIVE_PROVIDER_CONFIRM").as_deref(),
            Ok("fabricated-audio-authorized"),
            "live provider proof requires the exact confirmation guard"
        );
        let required = |name: &str| {
            std::env::var(name).unwrap_or_else(|_| panic!("missing live credential field {name}"))
        };
        let app_root = fs::canonicalize(required("ECHOWALL_LIVE_PROVIDER_ROOT"))
            .expect("live root must be an existing isolated directory");
        let canonical_temp = fs::canonicalize(std::env::temp_dir()).unwrap();
        assert!(
            app_root.starts_with(&canonical_temp),
            "live proof root must remain under the system temporary directory"
        );
        let audio = app_root.join("fabricated-provider-input.wav");
        let audio_metadata =
            fs::symlink_metadata(&audio).expect("fabricated audio fixture metadata is unavailable");
        assert!(
            audio_metadata.is_file()
                && !audio_metadata.file_type().is_symlink()
                && (1..=10 * 1024 * 1024).contains(&audio_metadata.len()),
            "fabricated audio fixture must be a bounded regular file"
        );

        let processing_credentials: ProcessingCredentials = serde_json::from_value(json!({
            "tosAccessKey": required("VOLC_TOS_ACCESS_KEY"),
            "tosSecretKey": required("VOLC_TOS_SECRET_KEY"),
            "tosBucket": required("VOLC_TOS_BUCKET"),
            "tosRegion": required("VOLC_TOS_REGION"),
            "tosEndpoint": required("VOLC_TOS_ENDPOINT"),
            "volcApiKey": required("VOLC_API_KEY"),
            "geminiApiKey": required("GEMINI_API_KEY"),
            "geminiModel": required("GEMINI_MODEL"),
        }))
        .expect("live processing credential schema must decode");
        let processing_state = Arc::new(ProcessingCredentialsState::ephemeral());
        processing_state
            .save(&processing_credentials)
            .expect("live processing credentials must enter the platform secure store");

        let sync_tokens = SyncTokens {
            schema_version: 1,
            github_pat: required("ECHOWALL_GITHUB_PAT"),
            r2_account_id: required("CLOUDFLARE_ACCOUNT_ID"),
            r2_access_key_id: required("ECHOWALL_R2_ACCESS_KEY_ID"),
            r2_secret_access_key: required("ECHOWALL_R2_SECRET_ACCESS_KEY"),
            repo: required("ECHOWALL_ARCHIVE_REPO"),
            bucket: required("ECHOWALL_ARCHIVE_BUCKET"),
        };
        crate::secrets::validate_sync_tokens(&sync_tokens)
            .expect("live archive credentials must match the bound schema");
        crate::secrets::save(&sync_tokens)
            .expect("live archive credentials must enter the platform secure store");

        let archive_root = app_root.join("archive");
        fs::create_dir_all(&archive_root).unwrap();
        let inbox = Arc::new(Inbox::open(&app_root, &archive_root).unwrap());
        let importer = DesktopImporter::new(
            Arc::clone(&inbox),
            Platform::Macos,
            DEFAULT_MAX_IMPORT_BYTES,
        )
        .unwrap();
        let imported = importer.import_paths(vec![audio.to_string_lossy().into_owned()]);
        let recording_id = Uuid::parse_str(
            imported.results[0]
                .recording_id
                .as_deref()
                .expect("fabricated audio must import"),
        )
        .unwrap();
        let imported_envelope = inbox.load_envelope(recording_id).unwrap();
        assert!(
            (1..=30_000).contains(&imported_envelope.duration_ms),
            "fabricated provider proof is limited to thirty seconds"
        );
        assert_eq!(
            imported_envelope.imported_name.as_deref(),
            Some("fabricated-provider-input.wav")
        );
        let store =
            Arc::new(ProcessingStore::open(&app_root, inbox.root(), &archive_root).unwrap());
        let archive = Arc::new(
            DeferredArchive::new(
                archive_root.clone(),
                inbox.root().to_path_buf(),
                app_root.clone(),
            )
            .unwrap(),
        );
        let unused_worker = |name: &str| app_root.join(format!("unused-{name}-worker"));
        let workers = LocalWhisperWorker::from_paths_for_test(
            app_root.clone(),
            unused_worker("whisper"),
            unused_worker("diarization"),
            unused_worker("summary"),
            unused_worker("qwen"),
        );
        let effects = Arc::new(
            DirectEffects::production(Arc::clone(&processing_state), Arc::clone(&archive), workers)
                .unwrap(),
        );
        let engine = ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), effects);
        engine.enqueue(recording_id).unwrap();
        if store.load(recording_id).unwrap().state == ProcessingState::ProviderFailed {
            engine
                .retry_provider(recording_id)
                .expect("explicitly rejected provider request must be retryable");
        }
        if store.load(recording_id).unwrap().state == ProcessingState::PublishFailed {
            engine
                .retry_publication(recording_id)
                .expect("explicitly failed publication must be retryable");
        }

        let deadline = Instant::now() + Duration::from_secs(600);
        let complete = loop {
            match engine.run_until_wait(recording_id).await {
                Ok(ledger) if ledger.state == ProcessingState::Complete => break ledger,
                Ok(ledger) if ledger.state == ProcessingState::Polling => {}
                Ok(ledger) => panic!("live provider route stopped in {:?}", ledger.state),
                Err(EngineError::Effect(error))
                    if error.kind == EffectErrorKind::Temporary
                        && store.load(recording_id).unwrap().state == ProcessingState::Polling =>
                {
                    // An accepted transcription job remains authoritative;
                    // only its idempotent query is retried inside this bound.
                }
                Err(error) => panic!("live provider route failed safely: {error}"),
            }
            assert!(
                Instant::now() < deadline,
                "live provider polling exceeded ten minutes"
            );
            tokio::time::sleep(Duration::from_secs(5)).await;
        };

        assert_eq!(
            complete.transcription_backend,
            super::super::TranscriptionBackend::MiaojiRemote
        );
        assert_eq!(
            complete.summary_backend,
            super::super::SummaryBackend::GeminiRemote
        );
        assert_eq!(
            complete.publication_backend,
            super::super::PublicationBackend::RemoteArchive
        );
        assert!(complete.cleanup.temporary_tos_deleted);
        let transcript = complete
            .transcript_json
            .as_ref()
            .and_then(Value::as_array)
            .expect("Miaoji transcript must be a sentence array");
        assert!(!transcript.is_empty());
        let speaker_labeled_segments = transcript
            .iter()
            .filter(|segment| {
                segment
                    .get("speaker")
                    .and_then(|speaker| speaker.get("id"))
                    .and_then(Value::as_str)
                    .is_some_and(|speaker| !speaker.is_empty())
            })
            .count();
        assert!(speaker_labeled_segments > 0);
        let summary_fields = complete
            .summary_json
            .as_ref()
            .and_then(Value::as_object)
            .expect("Gemini summary must be a structured object")
            .len();
        assert!(summary_fields >= 7);
        let publication_targets = complete
            .publication
            .as_ref()
            .expect("remote publication checkpoint must exist")
            .targets
            .len();
        assert!(publication_targets > 0);
        assert!(complete.canonical_backup.is_some());

        let refresh_data = app_root.join("refreshed-archive").join("data");
        fs::create_dir_all(&refresh_data).unwrap();
        let refresh = crate::sync::SyncCtx::new(refresh_data.clone());
        crate::sync::pull(Arc::clone(&refresh)).await;
        assert_eq!(refresh.state.read().unwrap().as_str(), "ok");
        let refreshed_manifest: Value =
            serde_json::from_slice(&fs::read(refresh_data.join("manifest.json")).unwrap()).unwrap();
        let recording_id = recording_id.to_string();
        let remote_key = refreshed_manifest.as_object().and_then(|manifest| {
            manifest.iter().find_map(|(key, entry)| {
                (entry.get("recording_id").and_then(Value::as_str) == Some(recording_id.as_str()))
                    .then(|| key.clone())
            })
        });
        let remote_refresh_matched = remote_key.is_some();
        assert!(remote_refresh_matched);

        let remote_key = remote_key.expect("refreshed recording key must exist");
        archive
            .resume_pending_native_edits()
            .await
            .expect("a previously staged fabricated delete must reconcile");
        let local_manifest: Value =
            serde_json::from_slice(&fs::read(archive_root.join("manifest.json")).unwrap()).unwrap();
        let local_still_present = local_manifest.as_object().is_some_and(|manifest| {
            manifest.values().any(|entry| {
                entry.get("recording_id").and_then(Value::as_str) == Some(recording_id.as_str())
            })
        });
        if local_still_present {
            let planned =
                crate::processing::archive::plan_local_recording_delete(&archive_root, &remote_key)
                    .expect("fabricated archive delete must be fully owner-verifiable");
            let r2_delete = match (
                planned.r2_key.clone(),
                planned.recording_id.clone(),
                planned.r2_generation,
                planned.audio_sha256.clone(),
                planned.audio_size_bytes,
            ) {
                (
                    Some(key),
                    Some(recording_id),
                    Some(generation),
                    Some(sha256),
                    Some(size_bytes),
                ) if crate::r2::is_generation_owned_key(&key, &recording_id, generation) => {
                    Some(crate::processing::archive::R2DeleteIntent {
                        key,
                        recording_id,
                        generation,
                        sha256,
                        size_bytes,
                    })
                }
                _ => None,
            };
            assert!(r2_delete.is_some());
            let transaction =
                crate::processing::archive::begin_native_archive_transaction(&archive_root)
                    .await
                    .unwrap();
            archive
                .execute_native_edit_in_transaction(
                    &transaction,
                    crate::processing::archive::NativeArchiveEdit::Delete {
                        key: remote_key,
                        recording_id: planned.recording_id.clone(),
                        base_entry_sha256: Some(planned.base_entry_sha256),
                        deleted_paths: planned.deleted,
                        r2_delete,
                    },
                )
                .await
                .expect("fabricated archive and owned R2 object must be deleted");
        }

        let post_delete_data = app_root.join("post-delete-refresh").join("data");
        fs::create_dir_all(&post_delete_data).unwrap();
        let post_delete = crate::sync::SyncCtx::new(post_delete_data.clone());
        crate::sync::pull(Arc::clone(&post_delete)).await;
        assert_eq!(post_delete.state.read().unwrap().as_str(), "ok");
        let post_delete_manifest: Value =
            serde_json::from_slice(&fs::read(post_delete_data.join("manifest.json")).unwrap())
                .unwrap();
        assert!(post_delete_manifest.as_object().is_some_and(|manifest| {
            manifest.values().all(|entry| {
                entry.get("recording_id").and_then(Value::as_str) != Some(recording_id.as_str())
            })
        }));
        processing_state
            .delete()
            .expect("live processing credential cleanup must succeed");
        crate::secrets::delete_archive().expect("live archive credential cleanup must succeed");

        println!(
            "{}",
            json!({
                "state": "complete",
                "transcriptSegments": transcript.len(),
                "speakerLabeledSegments": speaker_labeled_segments,
                "summaryFields": summary_fields,
                "publicationTargets": publication_targets,
                "temporaryTosDeleted": true,
                "canonicalBackupVerified": true,
                "remoteRefreshMatched": true,
                "remoteCleanupVerified": true,
            })
        );
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    #[ignore = "requires the exact full local model pack, bundled workers, and fabricated audio"]
    async fn live_full_local_route_completes_without_credentials_or_remote_effects() {
        use crate::ingest::import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES};
        use crate::processing::archive::DeferredArchive;
        use crate::processing::direct::DirectEffects;
        use crate::processing::local_models::LocalModelPackManager;
        use crate::processing::local_worker::LocalWhisperWorker;
        use crate::secrets::ProcessingCredentialsState;

        let app_root = std::path::PathBuf::from(
            std::env::var("ECHOWALL_LIVE_FULL_LOCAL_ROOT")
                .expect("ECHOWALL_LIVE_FULL_LOCAL_ROOT must name an isolated App-data root"),
        );
        let worker_dir = std::path::PathBuf::from(
            std::env::var("ECHOWALL_LIVE_FULL_LOCAL_WORKER_DIR")
                .expect("ECHOWALL_LIVE_FULL_LOCAL_WORKER_DIR must name built workers"),
        );
        let audio = app_root.join("fabricated-input.wav");
        assert!(audio.is_file());
        let archive_root = app_root.join("archive");
        fs::create_dir_all(&archive_root).unwrap();
        let inbox = Arc::new(Inbox::open(&app_root, &archive_root).unwrap());
        let importer = DesktopImporter::new(
            Arc::clone(&inbox),
            Platform::Macos,
            DEFAULT_MAX_IMPORT_BYTES,
        )
        .unwrap();
        let imported = importer.import_paths(vec![audio.to_string_lossy().into_owned()]);
        let recording_id = Uuid::parse_str(
            imported.results[0]
                .recording_id
                .as_deref()
                .expect("fabricated audio must import"),
        )
        .unwrap();

        let proof = LocalModelPackManager::open(&app_root)
            .unwrap()
            .proof()
            .await
            .unwrap();
        let store =
            Arc::new(ProcessingStore::open(&app_root, inbox.root(), &archive_root).unwrap());
        let archive = Arc::new(
            DeferredArchive::new(
                archive_root.clone(),
                inbox.root().to_path_buf(),
                app_root.clone(),
            )
            .unwrap(),
        );
        let workers = LocalWhisperWorker::from_paths_for_test(
            fs::canonicalize(&app_root).unwrap(),
            worker_dir.join("echowall-whisper-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-diarization-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-summary-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-qwen-worker-aarch64-apple-darwin"),
        );
        let effects = Arc::new(
            DirectEffects::production(
                Arc::new(ProcessingCredentialsState::new()),
                archive,
                workers,
            )
            .unwrap(),
        );
        let engine = ProcessingEngine::new(Arc::clone(&inbox), store, effects);
        engine
            .select_full_local(recording_id, proof, Some("en".to_owned()))
            .unwrap();
        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert_eq!(
            complete.transcription_backend,
            super::super::TranscriptionBackend::WhisperLocal
        );
        assert_eq!(
            complete.summary_backend,
            super::super::SummaryBackend::QwenLocal
        );
        assert_eq!(
            complete.publication_backend,
            super::super::PublicationBackend::LocalArchive
        );
        assert!(complete.tos_object.is_none());
        assert!(complete.miaoji.is_none());
        assert!(complete.summary_json.is_some());
        assert!(complete
            .canonical_backup
            .as_ref()
            .is_some_and(|backup| backup.locator.starts_with("local:")));
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(archive_root.join("manifest.json")).unwrap()).unwrap();
        let entry = manifest
            .as_object()
            .and_then(|manifest| manifest.values().next())
            .unwrap();
        assert!(entry.get("r2_key").is_none());
        assert!(archive_root.join("index.html").is_file());
    }

    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    #[tokio::test]
    #[ignore = "requires both exact local model packs, bundled workers, and fabricated audio"]
    async fn live_qwen_candidate_route_completes_without_credentials_or_remote_effects() {
        use crate::ingest::import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES};
        use crate::processing::archive::DeferredArchive;
        use crate::processing::direct::DirectEffects;
        use crate::processing::local_models::LocalModelPackManager;
        use crate::processing::local_worker::LocalWhisperWorker;
        use crate::secrets::ProcessingCredentialsState;

        let app_root = std::path::PathBuf::from(
            std::env::var("ECHOWALL_LIVE_FULL_LOCAL_ROOT")
                .expect("ECHOWALL_LIVE_FULL_LOCAL_ROOT must name an isolated App-data root"),
        );
        let worker_dir = std::path::PathBuf::from(
            std::env::var("ECHOWALL_LIVE_FULL_LOCAL_WORKER_DIR")
                .expect("ECHOWALL_LIVE_FULL_LOCAL_WORKER_DIR must name built workers"),
        );
        let audio = app_root.join("fabricated-input.wav");
        assert!(audio.is_file());
        let archive_root = app_root.join("archive");
        fs::create_dir_all(&archive_root).unwrap();
        let inbox = Arc::new(Inbox::open(&app_root, &archive_root).unwrap());
        let importer = DesktopImporter::new(
            Arc::clone(&inbox),
            Platform::Macos,
            DEFAULT_MAX_IMPORT_BYTES,
        )
        .unwrap();
        let imported = importer.import_paths(vec![audio.to_string_lossy().into_owned()]);
        let recording_id = Uuid::parse_str(
            imported.results[0]
                .recording_id
                .as_deref()
                .expect("fabricated audio must import"),
        )
        .unwrap();

        let manager = LocalModelPackManager::open(&app_root).unwrap();
        let base_proof = manager.proof().await.unwrap();
        let qwen_proof = manager.qwen_proof().await.unwrap();
        let store =
            Arc::new(ProcessingStore::open(&app_root, inbox.root(), &archive_root).unwrap());
        let archive = Arc::new(
            DeferredArchive::new(
                archive_root.clone(),
                inbox.root().to_path_buf(),
                app_root.clone(),
            )
            .unwrap(),
        );
        let workers = LocalWhisperWorker::from_paths_for_test(
            fs::canonicalize(&app_root).unwrap(),
            worker_dir.join("echowall-whisper-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-diarization-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-summary-worker-aarch64-apple-darwin"),
            worker_dir.join("echowall-qwen-worker-aarch64-apple-darwin"),
        );
        let effects = Arc::new(
            DirectEffects::production(
                Arc::new(ProcessingCredentialsState::new()),
                archive,
                workers,
            )
            .unwrap(),
        );
        let engine = ProcessingEngine::new(Arc::clone(&inbox), store, effects);
        engine
            .select_full_local_qwen(recording_id, base_proof, qwen_proof, None)
            .unwrap();
        let complete = engine.run_until_wait(recording_id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert_eq!(
            complete.transcription_backend,
            super::super::TranscriptionBackend::QwenLocal
        );
        assert_eq!(
            complete.summary_backend,
            super::super::SummaryBackend::QwenLocal
        );
        assert_eq!(
            complete.publication_backend,
            super::super::PublicationBackend::LocalArchive
        );
        assert!(complete.tos_object.is_none());
        assert!(complete.miaoji.is_none());
        assert!(complete.summary_json.is_some());
        assert!(complete
            .canonical_backup
            .as_ref()
            .is_some_and(|backup| backup.locator.starts_with("local:")));
        let manifest: serde_json::Value =
            serde_json::from_slice(&fs::read(archive_root.join("manifest.json")).unwrap()).unwrap();
        let entry = manifest
            .as_object()
            .and_then(|manifest| manifest.values().next())
            .unwrap();
        assert!(entry.get("r2_key").is_none());
        assert!(archive_root.join("index.html").is_file());
    }
}
