//! Narrow Tauri IPC surface for the embedded processing engine.

use std::sync::Arc;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::engine::{EngineError, ProcessingEffects, ProcessingEngine};
use super::local_models::{LocalModelPackProof, LocalModelState, QwenCandidateModelPackProof};
use super::{ProcessingLedger, ProcessingState};

const MAX_BATCH: usize = 16;
const MAX_RECOVERY_ROWS: usize = 256;

fn require_processing_feature(features: &crate::features::RuntimeFeatures) -> Result<(), String> {
    if features.direct_processing {
        Ok(())
    } else {
        Err("direct processing is disabled for this release".to_owned())
    }
}

#[async_trait]
pub trait AppProcessor: Send + Sync + 'static {
    fn enqueue(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    fn enqueue_ready_recordings(&self) -> Result<Vec<Uuid>, EngineError>;
    fn select_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError>;
    fn take_over_with_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError>;
    fn select_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError>;
    fn take_over_with_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError>;
    fn select_full_local_moss(
        &self,
        recording_id: Uuid,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError>;
    async fn accept_local_transcript_only(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError>;
    async fn process(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    async fn retry(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    async fn reprocess(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    async fn back_up_local_to_cloud(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError>;
    async fn cancel(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    async fn resume_canceled_cleanup(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError>;
    fn discard_local(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    fn status(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError>;
    fn list_recording_ids(&self) -> Result<Vec<Uuid>, EngineError>;
    fn sweep_retention(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize, EngineError>;
}

#[async_trait]
impl<E: ProcessingEffects> AppProcessor for ProcessingEngine<E> {
    fn enqueue(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::enqueue(self, recording_id)
    }

    fn enqueue_ready_recordings(&self) -> Result<Vec<Uuid>, EngineError> {
        ProcessingEngine::enqueue_ready_recordings(self)
    }

    fn select_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::select_full_local(self, recording_id, proof, language)
    }

    fn take_over_with_full_local(
        &self,
        recording_id: Uuid,
        proof: LocalModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::take_over_with_full_local(self, recording_id, proof, language)
    }

    fn select_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::select_full_local_qwen(
            self,
            recording_id,
            base_proof,
            qwen_proof,
            language,
        )
    }

    fn take_over_with_full_local_qwen(
        &self,
        recording_id: Uuid,
        base_proof: LocalModelPackProof,
        qwen_proof: QwenCandidateModelPackProof,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::take_over_with_full_local_qwen(
            self,
            recording_id,
            base_proof,
            qwen_proof,
            language,
        )
    }

    fn select_full_local_moss(
        &self,
        recording_id: Uuid,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::select_full_local_moss(self, recording_id, language)
    }

    async fn accept_local_transcript_only(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::accept_local_transcript_only(self, recording_id).await
    }

    async fn process(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        self.enqueue(recording_id)?;
        self.run_until_wait(recording_id).await
    }

    async fn retry(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::retry(self, recording_id).await
    }

    async fn reprocess(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::reprocess(self, recording_id).await
    }

    async fn back_up_local_to_cloud(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::back_up_local_to_cloud(self, recording_id).await
    }

    async fn cancel(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::cancel_and_cleanup(self, recording_id).await
    }

    async fn resume_canceled_cleanup(
        &self,
        recording_id: Uuid,
    ) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::resume_canceled_cleanup(self, recording_id).await
    }

    fn discard_local(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::discard_local(self, recording_id)
    }

    fn status(&self, recording_id: Uuid) -> Result<ProcessingLedger, EngineError> {
        ProcessingEngine::status(self, recording_id)
    }

    fn list_recording_ids(&self) -> Result<Vec<Uuid>, EngineError> {
        ProcessingEngine::list_recording_ids(self)
    }

    fn sweep_retention(&self, now: chrono::DateTime<chrono::Utc>) -> Result<usize, EngineError> {
        ProcessingEngine::sweep_source_track_retention(self, now)
    }
}

pub struct EmbeddedProcessingState {
    processor: Arc<dyn AppProcessor>,
}

impl EmbeddedProcessingState {
    pub fn new(processor: Arc<dyn AppProcessor>) -> Self {
        Self { processor }
    }

    pub fn processor(&self) -> Arc<dyn AppProcessor> {
        Arc::clone(&self.processor)
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProcessRecordingsRequest {
    pub recording_ids: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordingActionRequest {
    pub recording_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FullLocalProcessingRequest {
    pub recording_id: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecordingsResponse {
    pub results: Vec<ProcessRecordingResult>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessRecordingResult {
    pub recording_id: String,
    pub status: Option<EmbeddedStatus>,
    pub error_code: Option<&'static str>,
    pub error_message: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct EmbeddedStatus {
    pub state: ProcessingState,
    pub revision: u64,
    pub transcription_backend: super::TranscriptionBackend,
    pub summary_backend: super::SummaryBackend,
    pub publication_backend: super::PublicationBackend,
    pub diarization_selected: bool,
    pub transcript_only_accepted: bool,
    pub remote_task_accepted: bool,
    pub remote_task_superseded: bool,
    pub transcript_available: bool,
    pub summary_available: bool,
    pub temporary_cleanup_pending: bool,
}

impl From<&ProcessingLedger> for EmbeddedStatus {
    fn from(ledger: &ProcessingLedger) -> Self {
        Self {
            state: ledger.state,
            revision: ledger.revision,
            transcription_backend: ledger.transcription_backend,
            summary_backend: ledger.summary_backend,
            publication_backend: ledger.publication_backend,
            diarization_selected: ledger.local_diarization.is_some()
                || ledger.local_moss.is_some()
                || ledger.local_moss_preparation.is_some(),
            transcript_only_accepted: ledger.transcript_only_accepted,
            remote_task_accepted: ledger
                .miaoji
                .as_ref()
                .and_then(|checkpoint| checkpoint.task_id.as_ref())
                .is_some(),
            remote_task_superseded: ledger
                .miaoji
                .as_ref()
                .is_some_and(|checkpoint| checkpoint.superseded),
            transcript_available: ledger.transcript_json.is_some(),
            summary_available: ledger.summary_json.is_some(),
            temporary_cleanup_pending: matches!(
                ledger.state,
                ProcessingState::CancelingUpload | ProcessingState::CanceledAfterUpload
            ) && !ledger.cleanup.temporary_tos_deleted,
        }
    }
}

#[tauri::command]
pub async fn process_recordings(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: ProcessRecordingsRequest,
) -> Result<ProcessRecordingsResponse, String> {
    require_processing_feature(&features)?;
    if request.recording_ids.is_empty() || request.recording_ids.len() > MAX_BATCH {
        return Err("processing batch must contain 1-16 recordings".to_owned());
    }
    let processor = state.processor();
    let mut results = Vec::with_capacity(request.recording_ids.len());
    for supplied_id in request.recording_ids {
        let parsed = Uuid::parse_str(&supplied_id);
        let result = match parsed {
            Ok(recording_id) => match processor.process(recording_id).await {
                Ok(ledger) => success(recording_id, &ledger),
                Err(error) => failure(&*processor, recording_id, error),
            },
            Err(_) => ProcessRecordingResult {
                recording_id: supplied_id,
                status: None,
                error_code: Some("invalid_recording_id"),
                error_message: Some("recording ID is invalid"),
            },
        };
        results.push(result);
    }
    let _ = processor.sweep_retention(chrono::Utc::now());
    Ok(ProcessRecordingsResponse { results })
}

#[tauri::command]
pub fn list_processing_recordings(
    state: tauri::State<'_, EmbeddedProcessingState>,
) -> Result<ProcessRecordingsResponse, String> {
    let processor = state.processor();
    let recording_ids = processor
        .list_recording_ids()
        .map_err(|_| "processing history is unavailable".to_owned())?;
    let mut results = Vec::new();
    for recording_id in recording_ids.into_iter().rev() {
        let Ok(ledger) = processor.status(recording_id) else {
            continue;
        };
        if matches!(
            ledger.state,
            ProcessingState::Complete | ProcessingState::Discarded
        ) {
            continue;
        }
        results.push(success(recording_id, &ledger));
        if results.len() >= MAX_RECOVERY_ROWS {
            break;
        }
    }
    Ok(ProcessRecordingsResponse { results })
}

#[tauri::command]
pub async fn retry_processing_recording(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(match processor.retry(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn process_recording_with_local_models(
    state: tauri::State<'_, EmbeddedProcessingState>,
    models: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: FullLocalProcessingRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    if !features.local_stt || !crate::features::full_local_supported() {
        return Err("full local processing is unavailable on this platform".to_owned());
    }
    let recording_id = parse_recording_id(&request.recording_id)?;
    let proof = models
        .manager()
        .proof()
        .await
        .map_err(|_| "the full local model pack is not ready".to_owned())?;
    let processor = state.processor();
    if let Err(error) = processor.select_full_local(recording_id, proof, request.language) {
        return Ok(failure(&*processor, recording_id, error));
    }
    Ok(match processor.process(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn process_recording_with_qwen_candidate(
    state: tauri::State<'_, EmbeddedProcessingState>,
    models: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: FullLocalProcessingRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    if !features.local_stt
        || !features.local_qwen_candidate
        || !crate::features::full_local_supported()
    {
        return Err("Qwen candidate processing is unavailable".to_owned());
    }
    let recording_id = parse_recording_id(&request.recording_id)?;
    let manager = models.manager();
    let base_proof = manager
        .proof()
        .await
        .map_err(|_| "the full local model pack is not ready".to_owned())?;
    let qwen_proof = manager
        .qwen_proof()
        .await
        .map_err(|_| "the Qwen candidate model pack is not ready".to_owned())?;
    let processor = state.processor();
    if let Err(error) =
        processor.select_full_local_qwen(recording_id, base_proof, qwen_proof, request.language)
    {
        return Ok(failure(&*processor, recording_id, error));
    }
    Ok(match processor.process(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

fn require_moss_processing(features: &crate::features::RuntimeFeatures) -> Result<(), String> {
    require_processing_feature(features)?;
    if !features.local_stt
        || !features.local_moss_candidate
        || !crate::features::full_local_supported()
    {
        return Err("MOSS candidate processing is unavailable".to_owned());
    }
    Ok(())
}

#[tauri::command]
pub async fn process_recording_with_moss_candidate(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: FullLocalProcessingRequest,
) -> Result<ProcessRecordingResult, String> {
    require_moss_processing(&features)?;
    let recording_id = parse_recording_id(&request.recording_id)?;
    if request
        .language
        .as_deref()
        .is_some_and(|language| !matches!(language, "en" | "zh"))
    {
        return Err("MOSS language must be en, zh, or unspecified".to_owned());
    }
    let processor = state.processor();
    // Persist the Queued-only local choice before the first await. Model-byte
    // verification is part of that persisted workflow, not an IPC preflight.
    // A racing upload must fail selection; this command never takes it over.
    if let Err(error) = processor.select_full_local_moss(recording_id, request.language) {
        return Ok(failure(&*processor, recording_id, error));
    }
    Ok(match processor.process(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn take_over_processing_with_local_models(
    state: tauri::State<'_, EmbeddedProcessingState>,
    models: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: FullLocalProcessingRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    if !features.local_stt || !crate::features::full_local_supported() {
        return Err("full local processing is unavailable on this platform".to_owned());
    }
    let recording_id = parse_recording_id(&request.recording_id)?;
    let proof = models
        .manager()
        .proof()
        .await
        .map_err(|_| "the full local model pack is not ready".to_owned())?;
    let processor = state.processor();
    if let Err(error) = processor.take_over_with_full_local(recording_id, proof, request.language) {
        return Ok(failure(&*processor, recording_id, error));
    }
    Ok(match processor.process(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn take_over_processing_with_qwen_candidate(
    state: tauri::State<'_, EmbeddedProcessingState>,
    models: tauri::State<'_, LocalModelState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: FullLocalProcessingRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    if !features.local_stt
        || !features.local_qwen_candidate
        || !crate::features::full_local_supported()
    {
        return Err("Qwen candidate processing is unavailable".to_owned());
    }
    let recording_id = parse_recording_id(&request.recording_id)?;
    let manager = models.manager();
    let base_proof = manager
        .proof()
        .await
        .map_err(|_| "the full local model pack is not ready".to_owned())?;
    let qwen_proof = manager
        .qwen_proof()
        .await
        .map_err(|_| "the Qwen candidate model pack is not ready".to_owned())?;
    let processor = state.processor();
    if let Err(error) = processor.take_over_with_full_local_qwen(
        recording_id,
        base_proof,
        qwen_proof,
        request.language,
    ) {
        return Ok(failure(&*processor, recording_id, error));
    }
    Ok(match processor.process(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn accept_local_transcript_only(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    if !features.local_stt || !crate::features::full_local_supported() {
        return Err("local transcription is unavailable on this platform".to_owned());
    }
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(
        match processor.accept_local_transcript_only(recording_id).await {
            Ok(ledger) => success(recording_id, &ledger),
            Err(error) => failure(&*processor, recording_id, error),
        },
    )
}

#[tauri::command]
pub async fn reprocess_recording(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(match processor.reprocess(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn back_up_local_recording_to_cloud(
    state: tauri::State<'_, EmbeddedProcessingState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    require_processing_feature(&features)?;
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(match processor.back_up_local_to_cloud(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub fn discard_processing_recording(
    state: tauri::State<'_, EmbeddedProcessingState>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(match processor.discard_local(recording_id) {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

#[tauri::command]
pub async fn cancel_processing_recording(
    state: tauri::State<'_, EmbeddedProcessingState>,
    request: RecordingActionRequest,
) -> Result<ProcessRecordingResult, String> {
    let recording_id = parse_recording_id(&request.recording_id)?;
    let processor = state.processor();
    Ok(match processor.cancel(recording_id).await {
        Ok(ledger) => success(recording_id, &ledger),
        Err(error) => failure(&*processor, recording_id, error),
    })
}

pub fn resume_pending(state: &EmbeddedProcessingState, direct_processing_enabled: bool) {
    let processor = state.processor();
    tauri::async_runtime::spawn(async move {
        let _ = processor.enqueue_ready_recordings();
        let Ok(recording_ids) = processor.list_recording_ids() else {
            return;
        };
        for recording_id in recording_ids {
            let Ok(ledger) = processor.status(recording_id) else {
                continue;
            };
            if matches!(
                ledger.state,
                ProcessingState::CancelingUpload | ProcessingState::CanceledAfterUpload
            ) && !ledger.cleanup.temporary_tos_deleted
            {
                let _ = processor.resume_canceled_cleanup(recording_id).await;
                continue;
            }
            if ledger.state == ProcessingState::Discarding {
                let _ = processor.discard_local(recording_id);
                continue;
            }
            if direct_processing_enabled && should_resume_processing(ledger.state) {
                let _ = processor.process(recording_id).await;
            }
        }
        let _ = processor.sweep_retention(chrono::Utc::now());
    });
}

fn should_resume_processing(state: ProcessingState) -> bool {
    // Discovery creates Queued ledgers without choosing local versus remote.
    // A restart must not turn that discovery (or an unavailable local model)
    // into permission to upload. Explicit selection starts the existing
    // workflow; its durable, already-started states remain resumable.
    !state.is_terminal()
        && !matches!(
            state,
            ProcessingState::Queued
                | ProcessingState::SubmitAmbiguous
                | ProcessingState::SummaryAmbiguous
                | ProcessingState::PublishAmbiguous
                | ProcessingState::PublishConflict
        )
}

fn parse_recording_id(value: &str) -> Result<Uuid, String> {
    Uuid::parse_str(value).map_err(|_| "recording ID is invalid".to_owned())
}

fn success(recording_id: Uuid, ledger: &ProcessingLedger) -> ProcessRecordingResult {
    ProcessRecordingResult {
        recording_id: recording_id.to_string(),
        status: Some(EmbeddedStatus::from(ledger)),
        error_code: None,
        error_message: None,
    }
}

fn failure(
    processor: &dyn AppProcessor,
    recording_id: Uuid,
    error: EngineError,
) -> ProcessRecordingResult {
    let status = processor
        .status(recording_id)
        .ok()
        .as_ref()
        .map(EmbeddedStatus::from);
    let (code, message) = public_error_message(error);
    ProcessRecordingResult {
        recording_id: recording_id.to_string(),
        status,
        error_code: Some(code),
        error_message: Some(message),
    }
}

fn public_error_message(error: EngineError) -> (&'static str, &'static str) {
    match error {
        EngineError::ManualResolutionRequired => (
            "manual_resolution_required",
            "processing requires manual reconciliation",
        ),
        EngineError::InvalidRecording(_) => ("invalid_recording", "recording package is invalid"),
        EngineError::Effect(effect) => match effect.kind {
            super::engine::EffectErrorKind::SubmitAmbiguous => (
                "submit_ambiguous",
                "transcription submission requires reconciliation",
            ),
            super::engine::EffectErrorKind::PublicationConflict => {
                ("publish_conflict", "archive changed on another device")
            }
            super::engine::EffectErrorKind::PublicationAmbiguous => (
                "publish_ambiguous",
                "archive publication requires reconciliation",
            ),
            super::engine::EffectErrorKind::Rejected => {
                ("provider_rejected", "provider rejected the request")
            }
            super::engine::EffectErrorKind::NotDispatched => (
                "processing_not_dispatched",
                "processing request was not dispatched",
            ),
            _ => (
                "processing_unavailable",
                "processing is temporarily unavailable",
            ),
        },
        EngineError::Local(local) if local.code == "publication_commit_in_progress" => (
            "publication_commit_in_progress",
            "archive commit has started; finish or retry archive verification before discarding",
        ),
        EngineError::Local(local) if local.code == "remote_effect_already_started" => (
            "remote_effect_already_started",
            "remote processing has already started; new local selection was not applied",
        ),
        EngineError::Local(_) | EngineError::StepLimit => (
            "processing_unavailable",
            "processing is temporarily unavailable",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_waits_for_an_explicit_route_but_resumes_started_work() {
        for state in [
            ProcessingState::Queued,
            ProcessingState::SubmitAmbiguous,
            ProcessingState::SummaryAmbiguous,
            ProcessingState::PublishAmbiguous,
            ProcessingState::PublishConflict,
            ProcessingState::Complete,
        ] {
            assert!(!should_resume_processing(state), "{state:?}");
        }
        for state in [
            ProcessingState::Uploading,
            ProcessingState::Polling,
            ProcessingState::PreparingLocalMoss,
            ProcessingState::LocalTranscribing,
            ProcessingState::Publishing,
        ] {
            assert!(should_resume_processing(state), "{state:?}");
        }
    }

    #[test]
    fn command_requests_are_closed_and_batch_is_bounded() {
        assert!(
            serde_json::from_value::<ProcessRecordingsRequest>(serde_json::json!({
                "recordingIds": [], "ownerId": "forbidden"
            }))
            .is_err()
        );
        assert!(
            serde_json::from_value::<RecordingActionRequest>(serde_json::json!({
                "recordingId": Uuid::new_v4(), "objectKey": "forbidden"
            }))
            .is_err()
        );
        assert!(parse_recording_id("not-a-uuid").is_err());
        assert!(
            require_processing_feature(&crate::features::RuntimeFeatures {
                recording: true,
                audio_import: true,
                direct_processing: false,
                browser_capture: true,
                local_stt: false,
                local_qwen_candidate: false,
                local_speakerkit_candidate: false,
                local_moss_candidate: false,
            })
            .is_err()
        );
    }

    #[test]
    fn moss_command_requires_each_independent_execution_flag() {
        let mut features = crate::features::RuntimeFeatures {
            recording: true,
            audio_import: true,
            direct_processing: true,
            browser_capture: true,
            local_stt: true,
            local_qwen_candidate: false,
            local_speakerkit_candidate: false,
            local_moss_candidate: true,
        };
        assert_eq!(
            require_moss_processing(&features).is_ok(),
            crate::features::full_local_supported()
        );
        features.local_moss_candidate = false;
        assert!(require_moss_processing(&features).is_err());
        features.local_moss_candidate = true;
        features.local_stt = false;
        assert!(require_moss_processing(&features).is_err());
        features.local_stt = true;
        features.direct_processing = false;
        assert!(require_moss_processing(&features).is_err());
        assert!(
            serde_json::from_value::<FullLocalProcessingRequest>(serde_json::json!({
                "recordingId": Uuid::new_v4(), "language": "en", "modelPath": "/outside/model.gguf"
            }))
            .is_err()
        );
    }

    #[test]
    fn remote_selection_and_archive_commit_fences_keep_specific_safe_messages() {
        let remote = public_error_message(EngineError::Local(super::super::ProcessingError::new(
            "remote_effect_already_started",
        )));
        assert_eq!(
            remote,
            (
                "remote_effect_already_started",
                "remote processing has already started; new local selection was not applied"
            )
        );
        let publication = public_error_message(EngineError::Local(
            super::super::ProcessingError::new("publication_commit_in_progress"),
        ));
        assert_eq!(publication, ("publication_commit_in_progress", "archive commit has started; finish or retry archive verification before discarding"));
        let unknown = public_error_message(EngineError::Local(super::super::ProcessingError::new(
            "sensitive_internal_detail",
        )));
        assert_eq!(
            unknown,
            (
                "processing_unavailable",
                "processing is temporarily unavailable"
            )
        );
    }
}
