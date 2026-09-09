//! Leased, cancelable MOSS execution over the existing durable ledger.
use std::{
    collections::HashMap,
    future::Future,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
};

use sha2::{Digest, Sha256};
use tokio::sync::Notify;
use uuid::Uuid;

use super::{EffectError, EffectErrorKind, EngineError, ProcessingEffects, ProcessingEngine};
use crate::processing::{
    local_moss::{
        self, CompleteMossResponses, LocalMossPlan, ResponseBinding, ValidatedMossWindowResponse,
        ValidatedSpeakerKitResponse,
    },
    moss_artifacts::{ArtifactError, ArtifactKind, MossArtifacts, OwnerLease},
    moss_ledger::MossEffectKind,
    ProcessingError, ProcessingLedger, ProcessingState,
};

type Runs = Arc<Mutex<HashMap<Uuid, Arc<RunControl>>>>;

pub(super) struct RunControl {
    cancel: Arc<AtomicBool>,
    closing: AtomicBool,
    tasks: AtomicUsize,
    finished: Notify,
}

impl RunControl {
    pub(super) async fn wait_idle(&self) {
        loop {
            let notified = self.finished.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.tasks.load(Ordering::Acquire) == 0 {
                return;
            }
            notified.await;
        }
    }
}

pub(super) struct RunScope {
    id: Uuid,
    registry: Runs,
    control: Arc<RunControl>,
    pub(super) owner: Arc<OwnerLease>,
}

impl RunScope {
    pub(super) fn cancelled(&self) -> bool {
        self.control.cancel.load(Ordering::Acquire)
    }
    fn cancel_flag(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.control.cancel)
    }
    fn task(&self) -> TaskScope {
        self.control.tasks.fetch_add(1, Ordering::AcqRel);
        TaskScope {
            id: self.id,
            registry: Arc::clone(&self.registry),
            control: Arc::clone(&self.control),
            owner: Some(Arc::clone(&self.owner)),
        }
    }
}

fn remove_closed_run(id: Uuid, registry: &Runs, control: &Arc<RunControl>) {
    if !control.closing.load(Ordering::Acquire) || control.tasks.load(Ordering::Acquire) != 0 {
        return;
    }
    let mut entries = registry
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if entries
        .get(&id)
        .is_some_and(|entry| Arc::ptr_eq(entry, control))
    {
        entries.remove(&id);
    }
}

impl Drop for RunScope {
    fn drop(&mut self) {
        self.control.cancel.store(true, Ordering::Release);
        self.control.closing.store(true, Ordering::Release);
        remove_closed_run(self.id, &self.registry, &self.control);
    }
}

// Inference tasks only produce values; preparation writes claim-fenced local
// receipts. Publication begins after a durable non-cancelable commit fence and
// is reconciled after a crash. Every task retains the OS lease if its caller
// is dropped, until the effect has finished or cancellation has been reaped.
struct TaskScope {
    id: Uuid,
    registry: Runs,
    control: Arc<RunControl>,
    owner: Option<Arc<OwnerLease>>,
}
impl Drop for TaskScope {
    fn drop(&mut self) {
        drop(self.owner.take());
        self.control.tasks.fetch_sub(1, Ordering::AcqRel);
        self.control.finished.notify_waiters();
        remove_closed_run(self.id, &self.registry, &self.control);
    }
}

pub(super) async fn owned_effect<T: Send + 'static>(
    scope: &RunScope,
    future: impl Future<Output = Result<T, EffectError>> + Send + 'static,
) -> Result<T, EffectError> {
    let guard = scope.task();
    tokio::spawn(async move {
        let _guard = guard;
        future.await
    })
    .await
    .map_err(|_| EffectError::new(EffectErrorKind::Temporary))?
}

fn failure(code: &'static str) -> EngineError {
    ProcessingError::new(code).into()
}
fn artifact_failure(error: ArtifactError) -> EngineError {
    failure(error.code())
}
fn verification() -> EffectError {
    EffectError::new(EffectErrorKind::Verification)
}
fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn generation(ledger: &ProcessingLedger) -> Result<Uuid, EngineError> {
    ledger
        .local_moss
        .as_ref()
        .map(|checkpoint| checkpoint.generation())
        .or_else(|| {
            ledger
                .local_moss_preparation
                .as_ref()
                .map(|checkpoint| checkpoint.generation())
        })
        .ok_or_else(|| failure("missing_checkpoint"))
}

impl<E: ProcessingEffects> ProcessingEngine<E> {
    pub(super) fn require_moss_executable(
        &self,
        ledger: &ProcessingLedger,
    ) -> Result<(), EngineError> {
        if ledger.transcription_backend != crate::processing::TranscriptionBackend::MossLocal {
            return Ok(());
        }
        let Some(checkpoint) = ledger.local_moss.as_ref() else {
            // Validated preparation checkpoints pin their executable version;
            // retained preparations keep that choice across App upgrades.
            return Ok(());
        };
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?;
        let artifacts = MossArtifacts::open(root, ledger.recording_id, checkpoint.generation())
            .map_err(artifact_failure)?;
        let plan = LocalMossPlan::from_json(
            &artifacts
                .read(checkpoint.plan_ref())
                .map_err(artifact_failure)?,
        )
        .map_err(|error| failure(error.code))?;
        plan.require_executable()
            .map_err(|error| failure(error.code))
    }

    /// Choose the local route before decoding, downloading or hashing model
    /// files. Compiled pins declare required identities, not installation proof;
    /// each standalone worker verifies its actual files before inference.
    /// Queued-only CAS refuses to claim privacy after a remote effect started.
    pub fn select_full_local_moss(
        &self,
        id: Uuid,
        language: Option<String>,
    ) -> Result<ProcessingLedger, EngineError> {
        let ledger = self.enqueue(id)?;
        let envelope = self
            .inbox
            .load_envelope(id)
            .map_err(|_| EngineError::InvalidRecording("recording package is invalid"))?;
        let (model, speakers, summary) =
            crate::processing::local_models::moss::preparation_model_pins()
                .map_err(|_| failure("invalid_moss_preparation_models"))?;
        let generation = Uuid::new_v4();
        let source = local_moss::SourceAudioIdentity {
            relative_path: format!("inbox/{id}/{}", ledger.normalized.relative_path),
            sha256: ledger.normalized.sha256.clone(),
            size_bytes: ledger.normalized.size_bytes,
            duration_ms: envelope.duration_ms,
        };
        let checkpoint = crate::processing::moss_preparation::MossPreparationCheckpoint::new(
            id, generation, source, language, &model, &speakers, summary,
        )?;
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?;
        let artifacts = MossArtifacts::open(root, id, generation).map_err(artifact_failure)?;
        let owner = artifacts.try_owner().map_err(artifact_failure)?;
        Ok(self.store.begin_moss_preparation(id, &owner, checkpoint)?)
    }

    pub(super) fn begin_moss_run(
        &self,
        ledger: &ProcessingLedger,
    ) -> Result<Option<RunScope>, EngineError> {
        let generation = generation(ledger)?;
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?;
        let mut entries = self
            .moss_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if entries.contains_key(&ledger.recording_id) {
            return Ok(None);
        }
        let artifacts =
            MossArtifacts::open(root, ledger.recording_id, generation).map_err(artifact_failure)?;
        let owner = match artifacts.try_owner() {
            Ok(owner) => Arc::new(owner),
            Err(ArtifactError::Busy) => return Ok(None),
            Err(error) => return Err(artifact_failure(error)),
        };
        // Gate every resumed effect, including already-finalized summaries and
        // archive generations, before making any durable dispatch claim.
        self.require_moss_executable(ledger)?;
        let control = Arc::new(RunControl {
            cancel: Arc::new(AtomicBool::new(false)),
            closing: AtomicBool::new(false),
            tasks: AtomicUsize::new(0),
            finished: Notify::new(),
        });
        entries.insert(ledger.recording_id, Arc::clone(&control));
        Ok(Some(RunScope {
            id: ledger.recording_id,
            registry: Arc::clone(&self.moss_operations),
            control,
            owner,
        }))
    }

    pub(super) fn signal_moss_cancel(&self, id: Uuid) -> Option<Arc<RunControl>> {
        let control = self
            .moss_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .cloned();
        if let Some(control) = &control {
            control.cancel.store(true, Ordering::Release);
        }
        control
    }

    pub(super) fn moss_task_active(&self, id: Uuid) -> bool {
        self.moss_operations
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(&id)
            .is_some_and(|control| control.tasks.load(Ordering::Acquire) != 0)
    }

    pub(super) fn retry_moss_owned(&self, ledger: &ProcessingLedger) -> Result<(), EngineError> {
        if ledger.state.is_terminal() {
            return Ok(());
        }
        self.require_moss_executable(ledger)?;
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?;
        let artifacts = MossArtifacts::open(root, ledger.recording_id, generation(ledger)?)
            .map_err(artifact_failure)?;
        let owner = artifacts.try_owner().map_err(artifact_failure)?;
        match ledger.state {
            ProcessingState::ProviderFailed if ledger.local_moss_preparation.is_some() => {
                self.store
                    .retry_moss_preparation(ledger.recording_id, &owner)?;
            }
            ProcessingState::ProviderFailed => {
                self.store.retry_local_moss(ledger.recording_id, &owner)?;
            }
            ProcessingState::SummaryAmbiguous => {
                self.store.resolve_summary_for_retry(ledger.recording_id)?;
            }
            ProcessingState::PublishFailed | ProcessingState::PublishConflict => {
                self.store.retry_publication(ledger.recording_id)?;
            }
            ProcessingState::SubmitAmbiguous | ProcessingState::PublishAmbiguous => {
                return Err(EngineError::ManualResolutionRequired)
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) async fn run_moss_preparation(
        &self,
        id: Uuid,
        generation: Uuid,
        scope: &RunScope,
    ) -> Result<(), EngineError> {
        let ledger = self.store.load(id)?;
        if scope.cancelled() || ledger.state.is_terminal() {
            return Ok(());
        }
        if ledger
            .local_moss_preparation
            .as_ref()
            .map(|checkpoint| checkpoint.generation())
            != Some(generation)
        {
            return Err(failure("stale_moss_generation"));
        }
        let claim = self.store.claim_moss_preparation(id, &scope.owner)?;
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?
            .to_path_buf();
        let store = Arc::clone(&self.store);
        let owner = Arc::clone(&scope.owner);
        let cancel = scope.cancel_flag();
        let task_scope = scope.task();
        let task_claim = claim.clone();
        let result = tokio::task::spawn_blocking(move || {
            let _task_scope = task_scope;
            crate::processing::moss_preparation::prepare_moss_source(
                &root,
                &store,
                &owner,
                &task_claim,
                &cancel,
            )
        })
        .await
        .map_err(|_| EngineError::Effect(EffectError::new(EffectErrorKind::Temporary)))
        .and_then(|result| result.map_err(EngineError::from));
        if let Err(error) = result {
            if scope.cancelled() || self.store.load(id)?.state.is_terminal() {
                return Ok(());
            }
            self.store.fail_moss_preparation(id, &scope.owner, &claim)?;
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn run_moss_stage(
        &self,
        id: Uuid,
        generation: Uuid,
        scope: &RunScope,
    ) -> Result<(), EngineError> {
        let ledger = self.store.load(id)?;
        if scope.cancelled() || ledger.state.is_terminal() {
            return Ok(());
        }
        let checkpoint = ledger
            .local_moss
            .as_ref()
            .ok_or_else(|| failure("missing_checkpoint"))?;
        if checkpoint.generation() != generation {
            return Err(failure("stale_moss_generation"));
        }
        let root = self
            .store
            .root()
            .parent()
            .ok_or_else(|| failure("unsafe_storage_layout"))?;
        let artifacts = MossArtifacts::open(root, id, generation).map_err(artifact_failure)?;
        artifacts
            .validate_owner(&scope.owner)
            .map_err(artifact_failure)?;
        let claim = self.store.claim_next_moss_effect(id, &scope.owner)?;
        let result: Result<(), EngineError> = async {
            let plan = LocalMossPlan::from_json(
                &artifacts
                    .read(checkpoint.plan_ref())
                    .map_err(artifact_failure)?,
            )
            .map_err(|error| failure(error.code))?;
            if let Some(pending) = checkpoint.pending_response() {
                if let Some(bytes) = artifacts
                    .read_if_present(pending.reference())
                    .map_err(artifact_failure)?
                {
                    // The pre-publication intent attests the exact body. A
                    // new claim may complete it without another model call.
                    match claim.kind() {
                        MossEffectKind::Window(index) => {
                            let response = ValidatedMossWindowResponse::decode(
                                &plan,
                                index,
                                pending.binding().clone(),
                                &bytes,
                            )
                            .map_err(|error| failure(error.code))?;
                            self.store.checkpoint_moss_window(
                                id,
                                &scope.owner,
                                &claim,
                                pending.reference().clone(),
                                &response,
                            )?;
                        }
                        MossEffectKind::Anchors => {
                            let response = ValidatedSpeakerKitResponse::decode(
                                &plan,
                                pending.binding().clone(),
                                &bytes,
                            )
                            .map_err(|error| failure(error.code))?;
                            self.store.checkpoint_moss_anchors(
                                id,
                                &scope.owner,
                                &claim,
                                pending.reference().clone(),
                                &response,
                            )?;
                        }
                        MossEffectKind::Finalize => return Err(failure("invalid_moss_checkpoint")),
                    }
                    return Ok(());
                }
                self.store
                    .clear_missing_moss_response(id, &scope.owner, &claim)?;
            }
            // Old versions could publish a body with no recorded hash. Keep
            // that data for inspection, but never treat it as trusted output
            // or let it obstruct a fresh, independently verified response.
            match claim.kind() {
                MossEffectKind::Window(index) => {
                    artifacts
                        .preserve_unreceipted(&scope.owner, ArtifactKind::Window(index as u32))
                        .map_err(artifact_failure)?;
                }
                MossEffectKind::Anchors => {
                    artifacts
                        .preserve_unreceipted(&scope.owner, ArtifactKind::Anchors)
                        .map_err(artifact_failure)?;
                }
                MossEffectKind::Finalize => {}
            }
            match claim.kind() {
                MossEffectKind::Window(index) => {
                    let window = plan
                        .windows()
                        .get(index)
                        .ok_or_else(|| failure("invalid_moss_window"))?;
                    let request = window.request().clone();
                    let cancel = scope.cancel_flag();
                    let effects = Arc::clone(&self.effects);
                    let bytes = owned_effect(scope, async move {
                        effects.transcribe_moss(&request, cancel).await
                    })
                    .await?;
                    if scope.cancelled() || self.store.load(id)?.state.is_terminal() {
                        return Ok(());
                    }
                    let binding = ResponseBinding {
                        plan_sha256: plan.plan_sha256().into(),
                        request_sha256: window.request_sha256().into(),
                        response_sha256: hash(&bytes),
                    };
                    let response =
                        ValidatedMossWindowResponse::decode(&plan, index, binding, &bytes)
                            .map_err(|error| failure(error.code))?;
                    let reference = self.store.prepare_moss_window_response(
                        id,
                        &scope.owner,
                        &claim,
                        &response,
                    )?;
                    let published = artifacts
                        .write(&scope.owner, ArtifactKind::Window(index as u32), &bytes)
                        .map_err(artifact_failure)?;
                    if published != reference {
                        return Err(failure("moss_response_intent_conflict"));
                    }
                    self.store.checkpoint_moss_window(
                        id,
                        &scope.owner,
                        &claim,
                        reference,
                        &response,
                    )?;
                }
                MossEffectKind::Anchors => {
                    let request = plan.diarization_request().clone();
                    let cancel = scope.cancel_flag();
                    let effects = Arc::clone(&self.effects);
                    let bytes = owned_effect(scope, async move {
                        effects.diarize_moss(&request, cancel).await
                    })
                    .await?;
                    if scope.cancelled() || self.store.load(id)?.state.is_terminal() {
                        return Ok(());
                    }
                    let binding = ResponseBinding {
                        plan_sha256: plan.plan_sha256().into(),
                        request_sha256: plan.diarization_request_sha256().into(),
                        response_sha256: hash(&bytes),
                    };
                    let response = ValidatedSpeakerKitResponse::decode(&plan, binding, &bytes)
                        .map_err(|error| failure(error.code))?;
                    let reference = self.store.prepare_moss_anchor_response(
                        id,
                        &scope.owner,
                        &claim,
                        &response,
                    )?;
                    let published = artifacts
                        .write(&scope.owner, ArtifactKind::Anchors, &bytes)
                        .map_err(artifact_failure)?;
                    if published != reference {
                        return Err(failure("moss_response_intent_conflict"));
                    }
                    self.store.checkpoint_moss_anchors(
                        id,
                        &scope.owner,
                        &claim,
                        reference,
                        &response,
                    )?;
                }
                MossEffectKind::Finalize => {
                    let checkpoint = checkpoint.clone();
                    let cancel = scope.cancel_flag();
                    let task_scope = scope.task();
                    let result = tokio::task::spawn_blocking(move || {
                        let _task_scope = task_scope;
                        let windows = CompleteMossResponses::try_collect(
                            &plan,
                            checkpoint.completed_windows().iter().enumerate().map(
                                |(index, saved)| {
                                    if cancel.load(Ordering::Acquire) {
                                        return Err(local_moss::LocalMossError {
                                            code: "mapping_cancelled",
                                        });
                                    }
                                    let bytes =
                                        artifacts.read(saved.reference()).map_err(|error| {
                                            local_moss::LocalMossError { code: error.code() }
                                        })?;
                                    ValidatedMossWindowResponse::decode(
                                        &plan,
                                        index,
                                        saved.binding().clone(),
                                        &bytes,
                                    )
                                },
                            ),
                        )?;
                        let anchor = checkpoint.anchors().ok_or(local_moss::LocalMossError {
                            code: "missing_checkpoint",
                        })?;
                        let bytes = artifacts
                            .read(anchor.reference())
                            .map_err(|error| local_moss::LocalMossError { code: error.code() })?;
                        let anchors = ValidatedSpeakerKitResponse::decode(
                            &plan,
                            anchor.binding().clone(),
                            &bytes,
                        )?;
                        local_moss::finalize_with_cancel(&plan, &windows, &anchors, || {
                            cancel.load(Ordering::Acquire)
                        })
                    })
                    .await
                    .map_err(|_| verification())?
                    .map_err(|error| failure(error.code))?;
                    if scope.cancelled() || self.store.load(id)?.state.is_terminal() {
                        return Ok(());
                    }
                    self.store
                        .finalize_moss_transcript(id, &scope.owner, &claim, &result)?;
                }
            }
            Ok(())
        }
        .await;
        if let Err(error) = result {
            if scope.cancelled() || self.store.load(id)?.state.is_terminal() {
                return Ok(());
            }
            self.store
                .mark_moss_effect_failed(id, &scope.owner, &claim)?;
            return Err(error);
        }
        Ok(())
    }

    pub(super) async fn run_moss_summary(
        &self,
        request: echowall_local_summary_protocol::LocalSummaryRequest,
        scope: &RunScope,
    ) -> Result<serde_json::Value, EffectError> {
        let effects = Arc::clone(&self.effects);
        let cancel = scope.cancel_flag();
        owned_effect(scope, async move {
            effects.summarize_moss(&request, cancel).await
        })
        .await
    }
}

#[cfg(test)]
mod tests;

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod live;

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
mod matrix;
