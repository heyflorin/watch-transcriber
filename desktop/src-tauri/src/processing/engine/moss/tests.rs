use super::super::{PollResult, SummaryRequest};
use super::*;
use crate::{
    ingest::envelope::RecordingEnvelope,
    processing::{
        local_moss::{
            LocalMossPlanSpec, MossWindowRequestSpec, SourceAudioIdentity, LOCAL_MOSS_PLAN_VERSION,
        },
        local_whisper::*,
        CanonicalBackupCheckpoint, LocalSummaryCheckpoint, NormalizedArtifactCheckpoint,
        PublicationBackend, PublicationProof, PublicationTargetPlan, RetryMode,
        TosObjectCheckpoint,
    },
};
use async_trait::async_trait;
use echowall_local_moss_protocol as moss;
use serde_json::{json, Value};
use std::{path::Path, time::Duration};
use tempfile::TempDir;

#[derive(Default)]
struct Effects {
    calls: Mutex<Vec<&'static str>>,
    allow_remote_archive: AtomicBool,
    block_step: AtomicUsize,
    hold_reap: AtomicBool,
    started: Notify,
    canceled: Notify,
    release: Notify,
}

impl Effects {
    fn call(&self, name: &'static str) {
        self.calls.lock().unwrap().push(name);
    }
    async fn pause(&self, step: usize, cancel: &AtomicBool) -> Result<(), EffectError> {
        if self.block_step.load(Ordering::Acquire) != step {
            return Ok(());
        }
        self.started.notify_one();
        while !cancel.load(Ordering::Acquire) {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        self.canceled.notify_one();
        if self.hold_reap.load(Ordering::Acquire) {
            self.release.notified().await;
        }
        Err(EffectError::new(EffectErrorKind::Cancelled))
    }
}

#[async_trait]
impl ProcessingEffects for Effects {
    async fn transcribe_moss(
        &self,
        request: &moss::MossRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        self.call("window");
        self.pause(1, &cancel).await?;
        Ok(serde_json::to_vec(&moss::MossResponse {
            schema_version: moss::PROTOCOL_VERSION,
            recording_id: request.recording_id,
            runtime_id: request.runtime_id.clone(),
            model_id: request.model_id.clone(),
            model_sha256: request.model_sha256.clone(),
            audio_sha256: request.audio_sha256.clone(),
            timing_policy: request.timing_policy.clone(),
            complete: true,
            segments: vec![
                moss::MossSegment {
                    start_ms: 0,
                    end_ms: 800,
                    speaker_id: 1,
                    text: "first 世界".into(),
                },
                moss::MossSegment {
                    start_ms: 400,
                    end_ms: 1000,
                    speaker_id: 2,
                    text: "second".into(),
                },
            ],
        })
        .unwrap())
    }
    async fn diarize_moss(
        &self,
        request: &LocalDiarizationRequest,
        _: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, EffectError> {
        self.call("anchors");
        Ok(serde_json::to_vec(&LocalDiarizationResponse {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: request.recording_id,
            pack_id: request.pack_id.clone(),
            quality_preset: request.quality_preset.clone(),
            audio_sha256: request.audio_sha256.clone(),
            speaker_count: 2,
            segments: vec![
                LocalDiarizationSegment {
                    start_ms: 0,
                    end_ms: 500,
                    speaker_slot: 1,
                    confidence_milli: 0,
                },
                LocalDiarizationSegment {
                    start_ms: 500,
                    end_ms: 1000,
                    speaker_slot: 2,
                    confidence_milli: 0,
                },
            ],
        })
        .unwrap())
    }
    async fn summarize_moss(
        &self,
        request: &echowall_local_summary_protocol::LocalSummaryRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Value, EffectError> {
        self.call("summary");
        assert!(request.transcript.contains("first 世界"));
        assert!(request.transcript.contains("second"));
        self.pause(2, &cancel).await?;
        Ok(json!({"title": "Synthetic MOSS", "summary": "Two overlapping turns"}))
    }
    async fn upload_tos(
        &self,
        _: Uuid,
        _: &str,
        _: &Path,
        _: &NormalizedArtifactCheckpoint,
    ) -> Result<TosObjectCheckpoint, EffectError> {
        panic!("MOSS must never upload")
    }
    async fn probe_tos(
        &self,
        _: Uuid,
        _: &str,
        _: &Path,
        _: &NormalizedArtifactCheckpoint,
    ) -> Result<Option<TosObjectCheckpoint>, EffectError> {
        panic!("MOSS must never probe TOS")
    }
    async fn submit_miaoji(
        &self,
        _: &TosObjectCheckpoint,
        _: &str,
        _: Option<u32>,
    ) -> Result<String, EffectError> {
        panic!("MOSS must never submit Miaoji")
    }
    async fn poll_miaoji(&self, _: &str, _: &str) -> Result<PollResult, EffectError> {
        panic!("MOSS must never poll Miaoji")
    }
    async fn summarize(&self, _: &SummaryRequest) -> Result<Value, EffectError> {
        panic!("MOSS must use cancelable local summary")
    }
    fn publication_plan(
        &self,
        _: &RecordingEnvelope,
        backend: PublicationBackend,
    ) -> Result<Vec<PublicationTargetPlan>, EffectError> {
        assert!(
            backend == PublicationBackend::LocalArchive
                || self.allow_remote_archive.load(Ordering::Acquire)
        );
        Ok(vec![PublicationTargetPlan {
            id: if backend == PublicationBackend::LocalArchive {
                "local_archive"
            } else {
                "archive"
            }
            .into(),
            retry_mode: RetryMode::ReconcileBeforeRetry,
            required: true,
        }])
    }
    async fn publish_target(
        &self,
        _: &str,
        _: u64,
        _: &RecordingEnvelope,
        transcript: &Value,
        _: &Value,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<PublicationProof, EffectError> {
        self.call("publish");
        assert!(transcript.is_array());
        if self.block_step.load(Ordering::Acquire) == 3 {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(PublicationProof {
            locator: "archive/synthetic".into(),
            version: "v1".into(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
        })
    }
    async fn verify_canonical_backup(
        &self,
        backend: PublicationBackend,
        _: u64,
        _: &RecordingEnvelope,
        artifact: &NormalizedArtifactCheckpoint,
    ) -> Result<CanonicalBackupCheckpoint, EffectError> {
        assert!(
            backend == PublicationBackend::LocalArchive
                || self.allow_remote_archive.load(Ordering::Acquire)
        );
        self.call("backup");
        if self.block_step.load(Ordering::Acquire) == 4 {
            self.started.notify_one();
            self.release.notified().await;
        }
        Ok(CanonicalBackupCheckpoint {
            locator: "archive/synthetic".into(),
            version_id: "v1".into(),
            sha256: artifact.sha256.clone(),
            size_bytes: artifact.size_bytes,
            proof_json: json!({"verified": true}),
        })
    }
    async fn cleanup_tos(&self, _: &TosObjectCheckpoint) -> Result<(), EffectError> {
        panic!("new local selection has no TOS object")
    }
}

struct Fixture {
    _temp: Option<TempDir>,
    engine: Arc<ProcessingEngine<Effects>>,
    effects: Arc<Effects>,
    id: Uuid,
    artifacts: MossArtifacts,
}

impl Fixture {
    fn new() -> Self {
        let (temp, inbox, store, id) = super::super::tests::fixture();
        let effects = Arc::new(Effects::default());
        let engine = Arc::new(ProcessingEngine::new(
            inbox,
            Arc::clone(&store),
            Arc::clone(&effects),
        ));
        let ledger = engine.enqueue(id).unwrap();
        let generation = Uuid::new_v4();
        let source = SourceAudioIdentity {
            relative_path: format!("inbox/{id}/{}", ledger.normalized.relative_path),
            sha256: ledger.normalized.sha256.clone(),
            size_bytes: ledger.normalized.size_bytes,
            duration_ms: 1000,
        };
        let diarization_request = LocalDiarizationRequest {
            schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
            recording_id: id,
            pack_id: "speakerkit-v1".into(),
            quality_preset: LOCAL_DIARIZATION_SPEAKERKIT_PRESET.into(),
            model_files: LOCAL_DIARIZATION_SPEAKERKIT_FILES
                .iter()
                .map(|path| LocalModelFileIdentity {
                    relative_path: (*path).into(),
                    sha256: hash(path.as_bytes()),
                    size_bytes: 1,
                })
                .collect(),
            audio_relative_path: ledger.normalized.relative_path.clone(),
            audio_sha256: source.sha256.clone(),
            audio_size_bytes: source.size_bytes,
            audio_duration_ms: 1000,
            expected_speaker_count: None,
        };
        let plan = LocalMossPlan::new(LocalMossPlanSpec {
            schema_version: LOCAL_MOSS_PLAN_VERSION,
            mapping_policy: Some(crate::processing::local_moss::COMPOSED_MAPPING_POLICY.into()),
            recording_id: id,
            source,
            pcm_sample_rate: 16000,
            pcm_source_frames: 16000,
            pcm_quantization_policy: None,
            window_policy: moss::windows::QUIET_WINDOW_POLICY.into(),
            diarization_request,
            windows: vec![MossWindowRequestSpec {
                index: 0,
                start_frame: 0,
                end_frame: 16000,
                request: moss::MossRequest {
                    schema_version: moss::PROTOCOL_VERSION,
                    recording_id: id,
                    runtime_id: moss::RUNTIME_ID.into(),
                    model_id: moss::MODEL_ID.into(),
                    model_revision: moss::MODEL_REVISION.into(),
                    model_sha256: moss::MODEL_SHA256.into(),
                    model_size_bytes: moss::MODEL_SIZE_BYTES,
                    timing_policy: moss::COALESCING_TIMING_POLICY_V2.into(),
                    audio_relative_path: format!("inbox/{id}/derived/moss_{generation}_0.wav"),
                    audio_sha256: hash(b"fabricated PCM"),
                    audio_size_bytes: 32044,
                    audio_duration_ms: 1000,
                    language: None,
                },
            }],
        })
        .unwrap();
        let artifacts =
            MossArtifacts::open(store.root().parent().unwrap(), id, generation).unwrap();
        let owner = artifacts.try_owner().unwrap();
        let reference = artifacts
            .write(&owner, ArtifactKind::Plan, plan.plan_bytes())
            .unwrap();
        store
            .select_full_local_moss(
                id,
                &owner,
                &plan,
                reference,
                LocalSummaryCheckpoint {
                    model_id: "qwen3.8-27b-ud-q4-k-xl".into(),
                    model_sha256: hash(b"synthetic summary model"),
                    model_size_bytes: 1,
                    prompt_version: echowall_local_summary_protocol::LOCAL_SUMMARY_PROMPT_VERSION
                        .into(),
                    transcript_sha256: None,
                },
            )
            .unwrap();
        drop(owner);
        Self {
            _temp: Some(temp),
            engine,
            effects,
            id,
            artifacts,
        }
    }
    fn spawn(&self) -> tokio::task::JoinHandle<Result<ProcessingLedger, EngineError>> {
        let engine = Arc::clone(&self.engine);
        let id = self.id;
        tokio::spawn(async move { engine.run_until_wait(id).await })
    }
    async fn wait_idle(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if !self
                    .engine
                    .moss_operations
                    .lock()
                    .unwrap()
                    .contains_key(&self.id)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
}

async fn notified(signal: &Notify) {
    tokio::time::timeout(Duration::from_secs(2), signal.notified())
        .await
        .unwrap();
}

#[tokio::test]
async fn full_local_pipeline_preserves_overlap_and_resumes_idempotently() {
    let fixture = Fixture::new();
    let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert_eq!(
        fixture.effects.calls.lock().unwrap().as_slice(),
        ["window", "anchors", "summary", "publish", "backup"]
    );
    let transcript = complete
        .transcript_json
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap();
    assert_eq!(transcript.len(), 2);
    assert_eq!(transcript[0]["content"], "first 世界");
    assert_eq!(transcript[1]["content"], "second");
    assert!(
        transcript[0]["end_time"].as_f64().unwrap() > transcript[1]["start_time"].as_f64().unwrap()
    );
    assert_ne!(
        transcript[0]["speaker"]["id"],
        transcript[1]["speaker"]["id"]
    );
    assert!(complete.tos_object.is_none());
    assert!(complete.cleanup.temporary_tos_deleted);
    let again = fixture.engine.run_until_wait(fixture.id).await.unwrap();
    assert_eq!(complete, again);
    assert_eq!(fixture.effects.calls.lock().unwrap().len(), 5);
}

#[tokio::test]
async fn concurrent_resume_does_not_launch_another_worker_or_fence_live_summary() {
    for step in [1, 2] {
        let fixture = Fixture::new();
        fixture.effects.block_step.store(step, Ordering::Release);
        let running = fixture.spawn();
        notified(&fixture.effects.started).await;
        let before = fixture.engine.store.load(fixture.id).unwrap();
        let concurrent = fixture.engine.run_until_wait(fixture.id).await.unwrap();
        assert_eq!(before, concurrent);
        assert_ne!(concurrent.state, ProcessingState::SummaryAmbiguous);
        fixture.engine.cancel_and_cleanup(fixture.id).await.unwrap();
        let canceled = running.await.unwrap().unwrap();
        assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
        assert!(!fixture.effects.calls.lock().unwrap().contains(&"publish"));
    }
}

#[tokio::test]
async fn abort_retains_owner_until_worker_reaped_then_resumes_uncheckpointed_window() {
    let fixture = Fixture::new();
    fixture.effects.block_step.store(1, Ordering::Release);
    fixture.effects.hold_reap.store(true, Ordering::Release);
    let running = fixture.spawn();
    notified(&fixture.effects.started).await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    notified(&fixture.effects.canceled).await;
    assert!(matches!(
        fixture.artifacts.try_owner(),
        Err(ArtifactError::Busy)
    ));
    let before = fixture.engine.store.load(fixture.id).unwrap();
    assert_eq!(
        fixture.engine.run_until_wait(fixture.id).await.unwrap(),
        before
    );
    assert!(before
        .local_moss
        .as_ref()
        .unwrap()
        .completed_windows()
        .is_empty());
    fixture.effects.release.notify_one();
    fixture.wait_idle().await;
    drop(fixture.artifacts.try_owner().unwrap());
    fixture.effects.block_step.store(0, Ordering::Release);
    let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert_eq!(
        fixture.effects.calls.lock().unwrap().as_slice(),
        ["window", "window", "anchors", "summary", "publish", "backup"]
    );
}

#[tokio::test]
async fn crashed_local_summary_recovers_without_repeating_asr_or_cloud_effects() {
    let fixture = Fixture::new();
    fixture.effects.block_step.store(2, Ordering::Release);
    let running = fixture.spawn();
    notified(&fixture.effects.started).await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    fixture.wait_idle().await;
    assert_eq!(
        fixture.engine.store.load(fixture.id).unwrap().state,
        ProcessingState::SummarySubmitting
    );
    fixture.effects.block_step.store(0, Ordering::Release);
    let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert_eq!(
        fixture.effects.calls.lock().unwrap().as_slice(),
        ["window", "anchors", "summary", "summary", "publish", "backup"]
    );
}

#[tokio::test]
async fn archive_commit_rejects_false_cancellation_and_keeps_owner_until_reconciled() {
    for step in [3, 4] {
        let fixture = Fixture::new();
        fixture.effects.block_step.store(step, Ordering::Release);
        let running = fixture.spawn();
        notified(&fixture.effects.started).await;
        let before = fixture.engine.store.load(fixture.id).unwrap();
        assert_eq!(before.state, ProcessingState::Publishing);
        let error = fixture
            .engine
            .cancel_and_cleanup(fixture.id)
            .await
            .unwrap_err();
        assert!(
            matches!(error, EngineError::Local(error) if error.code == "publication_commit_in_progress")
        );
        assert_eq!(fixture.engine.store.load(fixture.id).unwrap(), before);
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        assert!(matches!(
            fixture.artifacts.try_owner(),
            Err(ArtifactError::Busy)
        ));
        assert_eq!(
            fixture.engine.run_until_wait(fixture.id).await.unwrap(),
            before
        );
        fixture.effects.release.notify_one();
        fixture.wait_idle().await;
        fixture.effects.block_step.store(0, Ordering::Release);
        let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        let calls = fixture.effects.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|&&call| call == "window").count(), 1);
        assert_eq!(calls.iter().filter(|&&call| call == "summary").count(), 1);
        assert_eq!(
            calls.iter().filter(|&&call| call == "publish").count(),
            if step == 3 { 2 } else { 1 }
        );
        assert_eq!(
            calls.iter().filter(|&&call| call == "backup").count(),
            if step == 4 { 2 } else { 1 }
        );
    }
}

#[tokio::test]
async fn cancellation_before_archive_commit_fences_target_start() {
    let fixture = Fixture::new();
    fixture.effects.block_step.store(2, Ordering::Release);
    let running = fixture.spawn();
    notified(&fixture.effects.started).await;
    running.abort();
    assert!(running.await.unwrap_err().is_cancelled());
    fixture.wait_idle().await;
    fixture
        .engine
        .store
        .checkpoint_summary(fixture.id, json!({"title": "Synthetic", "summary": "Safe"}))
        .unwrap();
    let ledger = fixture
        .engine
        .store
        .plan_publication(
            fixture.id,
            vec![PublicationTargetPlan {
                id: "local_archive".into(),
                retry_mode: RetryMode::ReconcileBeforeRetry,
                required: true,
            }],
        )
        .unwrap();
    let generation = ledger.publication.unwrap().generation;
    let canceled = fixture.engine.cancel_and_cleanup(fixture.id).await.unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
    assert!(fixture
        .engine
        .store
        .start_publication_target(fixture.id, generation, "local_archive")
        .is_err());
    assert!(!fixture.effects.calls.lock().unwrap().contains(&"publish"));
}

#[tokio::test]
async fn prior_verified_archive_does_not_prevent_canceling_a_new_summary() {
    let fixture = Fixture::new();
    fixture.engine.run_until_wait(fixture.id).await.unwrap();
    fixture.effects.block_step.store(2, Ordering::Release);
    let engine = Arc::clone(&fixture.engine);
    let id = fixture.id;
    let running = tokio::spawn(async move { engine.reprocess(id).await });
    notified(&fixture.effects.started).await;
    let before = fixture.engine.store.load(id).unwrap();
    assert_eq!(before.state, ProcessingState::SummarySubmitting);
    assert!(before
        .publication
        .as_ref()
        .unwrap()
        .targets
        .iter()
        .all(|target| target.state == crate::processing::PublicationTargetState::Verified));
    assert_eq!(
        fixture.engine.cancel_and_cleanup(id).await.unwrap().state,
        ProcessingState::CanceledBeforeUpload
    );
    assert_eq!(
        running.await.unwrap().unwrap().state,
        ProcessingState::CanceledBeforeUpload
    );
    assert_eq!(
        fixture
            .effects
            .calls
            .lock()
            .unwrap()
            .iter()
            .filter(|&&call| call == "publish")
            .count(),
        1
    );
}

#[tokio::test]
async fn explicit_cloud_backup_has_the_same_abort_and_commit_fence_without_reprocessing() {
    for step in [3, 4] {
        let fixture = Fixture::new();
        fixture.engine.run_until_wait(fixture.id).await.unwrap();
        fixture
            .effects
            .allow_remote_archive
            .store(true, Ordering::Release);
        fixture.effects.block_step.store(step, Ordering::Release);
        let engine = Arc::clone(&fixture.engine);
        let id = fixture.id;
        let running = tokio::spawn(async move { engine.back_up_local_to_cloud(id).await });
        notified(&fixture.effects.started).await;
        let before = fixture.engine.store.load(id).unwrap();
        assert_eq!(
            before.publication_backend,
            PublicationBackend::RemoteArchive
        );
        assert_eq!(before.publication.as_ref().unwrap().generation, 2);
        assert!(
            matches!(fixture.engine.cancel_and_cleanup(id).await.unwrap_err(), EngineError::Local(error) if error.code == "publication_commit_in_progress")
        );
        running.abort();
        assert!(running.await.unwrap_err().is_cancelled());
        assert!(matches!(
            fixture.artifacts.try_owner(),
            Err(ArtifactError::Busy)
        ));
        assert_eq!(fixture.engine.run_until_wait(id).await.unwrap(), before);
        fixture.effects.release.notify_one();
        fixture.wait_idle().await;
        fixture.effects.block_step.store(0, Ordering::Release);
        assert_eq!(
            fixture.engine.run_until_wait(id).await.unwrap().state,
            ProcessingState::Complete
        );
        let calls = fixture.effects.calls.lock().unwrap();
        assert_eq!(calls.iter().filter(|&&call| call == "window").count(), 1);
        assert_eq!(calls.iter().filter(|&&call| call == "summary").count(), 1);
    }
}

#[tokio::test]
async fn queued_selection_prepares_real_file_only_pcm_and_runs_one_offline_route() {
    let (_temp, inbox, store, id) = super::super::tests::fixture();
    let frames: u32 = 32017;
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"RIFF");
    bytes.extend_from_slice(&(36 + frames * 2).to_le_bytes());
    bytes.extend_from_slice(b"WAVEfmt ");
    bytes.extend_from_slice(&16_u32.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&1_u16.to_le_bytes());
    bytes.extend_from_slice(&16000_u32.to_le_bytes());
    bytes.extend_from_slice(&32000_u32.to_le_bytes());
    bytes.extend_from_slice(&2_u16.to_le_bytes());
    bytes.extend_from_slice(&16_u16.to_le_bytes());
    bytes.extend_from_slice(b"data");
    bytes.extend_from_slice(&(frames * 2).to_le_bytes());
    for index in 0..frames {
        bytes.extend_from_slice(&((index % 100) as i16 * 10).to_le_bytes());
    }
    let source_path = inbox.root().join(id.to_string()).join("tracks/input.wav");
    std::fs::write(&source_path, &bytes).unwrap();
    let mut envelope = inbox.load_envelope(id).unwrap();
    envelope.normalized_sha256 = Some(hash(&bytes));
    envelope.tracks[0].sha256 = hash(&bytes);
    inbox.persist_envelope(&envelope).unwrap();
    let effects = Arc::new(Effects::default());
    let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
    let selected = engine.select_full_local_moss(id, None).unwrap();
    assert_eq!(selected.state, ProcessingState::PreparingLocalMoss);
    assert!(selected.local_moss.is_none());
    assert!(selected.local_moss_preparation.is_some());
    assert!(effects.calls.lock().unwrap().is_empty());
    let complete = engine.run_until_wait(id).await.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert!(complete.local_moss_preparation.is_none());
    let checkpoint = complete.local_moss.unwrap();
    let artifacts =
        MossArtifacts::open(store.root().parent().unwrap(), id, checkpoint.generation()).unwrap();
    let plan = LocalMossPlan::from_json(&artifacts.read(checkpoint.plan_ref()).unwrap()).unwrap();
    assert_eq!(plan.spec().pcm_source_frames, u64::from(frames));
    assert_eq!(plan.spec().source.duration_ms, 2002);
    assert_eq!(
        plan.spec().pcm_quantization_policy.as_deref(),
        Some(echowall_local_audio::PCM_QUANTIZATION_POLICY)
    );
    assert_eq!(std::fs::read(source_path).unwrap(), bytes);
    assert_eq!(
        effects.calls.lock().unwrap().as_slice(),
        ["window", "anchors", "summary", "publish", "backup"]
    );
}

#[tokio::test]
async fn failed_preparation_is_durable_and_retry_never_falls_back_to_upload() {
    let (_temp, inbox, store, id) = super::super::tests::fixture();
    let effects = Arc::new(Effects::default());
    let engine = ProcessingEngine::new(inbox, Arc::clone(&store), Arc::clone(&effects));
    engine.select_full_local_moss(id, None).unwrap();
    // The base fixture is deliberately invalid WAV bytes, not an inference
    // failure. The selected intent must survive both the decoder and retry.
    assert!(engine.run_until_wait(id).await.is_err());
    let failed = store.load(id).unwrap();
    assert_eq!(failed.state, ProcessingState::ProviderFailed);
    assert!(failed.local_moss_preparation.is_some());
    assert!(failed.tos_object.is_none());
    assert!(engine.retry(id).await.is_err());
    assert_eq!(
        store.load(id).unwrap().state,
        ProcessingState::ProviderFailed
    );
    assert!(effects.calls.lock().unwrap().is_empty());
}

// Model a retained pre-v2 experiment by writing its historical bytes into this
// test's own temporary fixture. Production cannot select/create a legacy plan.
fn retain_legacy_v1(fixture: &Fixture) {
    let ledger = fixture.engine.store.load(fixture.id).unwrap();
    let checkpoint = ledger.local_moss.as_ref().unwrap();
    let mut plan: Value =
        serde_json::from_slice(&fixture.artifacts.read(checkpoint.plan_ref()).unwrap()).unwrap();
    plan["schema_version"] = json!(1);
    plan.as_object_mut().unwrap().remove("mapping_policy");
    plan["diarization_request"]["audio_relative_path"] = plan["source"]["relative_path"].clone();
    let bytes = serde_json::to_vec_pretty(&plan).unwrap();
    let retained = LocalMossPlan::from_json(&bytes).unwrap();
    let mut value = serde_json::to_value(&ledger).unwrap();
    let checkpoint_value = &mut value["local_moss"];
    checkpoint_value["plan"]["sha256"] = json!(retained.plan_sha256());
    checkpoint_value["plan"]["size_bytes"] = json!(bytes.len());
    checkpoint_value["anchor_request_sha256"] = json!(retained.diarization_request_sha256());
    for window in checkpoint_value["completed_windows"]
        .as_array_mut()
        .unwrap()
    {
        window["binding"]["plan_sha256"] = json!(retained.plan_sha256());
    }
    if !checkpoint_value["anchors"].is_null() {
        checkpoint_value["anchors"]["binding"]["plan_sha256"] = json!(retained.plan_sha256());
        checkpoint_value["anchors"]["binding"]["request_sha256"] =
            json!(retained.diarization_request_sha256());
    }
    let plan_path = fixture
        .engine
        .store
        .root()
        .join("moss")
        .join(fixture.id.to_string())
        .join(checkpoint.generation().to_string())
        .join("plan.json");
    std::fs::write(plan_path, bytes).unwrap();
    std::fs::write(
        fixture
            .engine
            .store
            .root()
            .join("jobs")
            .join(format!("{}.json", fixture.id)),
        serde_json::to_vec(&value).unwrap(),
    )
    .unwrap();
}

fn assert_legacy_rejected(result: Result<ProcessingLedger, EngineError>) {
    assert!(
        matches!(result.unwrap_err(), EngineError::Local(error) if error.code == "legacy_moss_plan_requires_reprepare")
    );
}

#[tokio::test]
async fn legacy_unreceipted_response_is_preserved_without_stranding_new_inference() {
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    let ledger = fixture.engine.store.load(fixture.id).unwrap();
    let checkpoint = ledger.local_moss.as_ref().unwrap();
    let plan =
        LocalMossPlan::from_json(&fixture.artifacts.read(checkpoint.plan_ref()).unwrap()).unwrap();
    fixture
        .engine
        .store
        .claim_next_moss_effect(fixture.id, &owner)
        .unwrap();
    let bytes = fixture
        .effects
        .transcribe_moss(
            plan.windows()[0].request(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap();
    body["segments"][0]["text"] = json!("first 世界 retained before crash");
    let retained = serde_json::to_vec(&body).unwrap();
    fixture
        .artifacts
        .write(&owner, ArtifactKind::Window(0), &retained)
        .unwrap();
    fixture.effects.calls.lock().unwrap().clear();
    // Simulate process loss after create-only publication, before its ledger receipt.
    drop(owner);
    let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert_eq!(
        fixture.effects.calls.lock().unwrap().as_slice(),
        ["window", "anchors", "summary", "publish", "backup"]
    );
    let saved = fixture
        .engine
        .store
        .root()
        .join("moss")
        .join(fixture.id.to_string())
        .join(checkpoint.generation().to_string())
        .join(format!(".unattested-window-00.json-{}", hash(&retained)));
    assert_eq!(std::fs::read(saved).unwrap(), retained);
    assert_eq!(
        complete.transcript_json.unwrap()[0]["content"],
        "first 世界"
    );
}

async fn interrupted_intent(
    fixture: &Fixture,
    anchors: bool,
    publish: bool,
    crash_without_drop: bool,
) -> (crate::processing::moss_artifacts::ArtifactRef, Vec<u8>) {
    let owner = fixture.artifacts.try_owner().unwrap();
    let ledger = fixture.engine.store.load(fixture.id).unwrap();
    let plan = LocalMossPlan::from_json(
        &fixture
            .artifacts
            .read(ledger.local_moss.as_ref().unwrap().plan_ref())
            .unwrap(),
    )
    .unwrap();
    let window_claim = fixture
        .engine
        .store
        .claim_next_moss_effect(fixture.id, &owner)
        .unwrap();
    let bytes = fixture
        .effects
        .transcribe_moss(
            plan.windows()[0].request(),
            Arc::new(AtomicBool::new(false)),
        )
        .await
        .unwrap();
    let mut body: Value = serde_json::from_slice(&bytes).unwrap();
    if !anchors {
        body["segments"][0]["text"] = json!("first 世界 saved before crash");
    }
    let bytes = serde_json::to_vec(&body).unwrap();
    let response = ValidatedMossWindowResponse::decode(
        &plan,
        0,
        ResponseBinding {
            plan_sha256: plan.plan_sha256().into(),
            request_sha256: plan.windows()[0].request_sha256().into(),
            response_sha256: hash(&bytes),
        },
        &bytes,
    )
    .unwrap();
    let reference = fixture
        .engine
        .store
        .prepare_moss_window_response(fixture.id, &owner, &window_claim, &response)
        .unwrap();
    let result = if anchors {
        fixture
            .artifacts
            .write(&owner, ArtifactKind::Window(0), &bytes)
            .unwrap();
        fixture
            .engine
            .store
            .checkpoint_moss_window(fixture.id, &owner, &window_claim, reference, &response)
            .unwrap();
        let claim = fixture
            .engine
            .store
            .claim_next_moss_effect(fixture.id, &owner)
            .unwrap();
        let bytes = fixture
            .effects
            .diarize_moss(plan.diarization_request(), Arc::new(AtomicBool::new(false)))
            .await
            .unwrap();
        let mut body: Value = serde_json::from_slice(&bytes).unwrap();
        body["segments"][0]["speaker_slot"] = json!(2);
        body["segments"][1]["speaker_slot"] = json!(1);
        let bytes = serde_json::to_vec(&body).unwrap();
        let response = ValidatedSpeakerKitResponse::decode(
            &plan,
            ResponseBinding {
                plan_sha256: plan.plan_sha256().into(),
                request_sha256: plan.diarization_request_sha256().into(),
                response_sha256: hash(&bytes),
            },
            &bytes,
        )
        .unwrap();
        let reference = fixture
            .engine
            .store
            .prepare_moss_anchor_response(fixture.id, &owner, &claim, &response)
            .unwrap();
        if publish {
            fixture
                .artifacts
                .write(&owner, ArtifactKind::Anchors, &bytes)
                .unwrap();
        }
        (reference, bytes)
    } else {
        if publish {
            fixture
                .artifacts
                .write(&owner, ArtifactKind::Window(0), &bytes)
                .unwrap();
        }
        (reference, bytes)
    };
    fixture.effects.calls.lock().unwrap().clear();
    if crash_without_drop {
        std::process::exit(73);
    }
    // No receipt is committed; a new exclusive owner must reconcile the intent.
    drop(owner);
    result
}

#[tokio::test]
async fn published_intent_recovers_exact_response_without_repeating_its_worker() {
    for anchors in [false, true] {
        let fixture = Fixture::new();
        let (reference, bytes) = interrupted_intent(&fixture, anchors, true, false).await;
        let before = fixture.engine.store.load(fixture.id).unwrap();
        assert_eq!(
            before
                .local_moss
                .as_ref()
                .unwrap()
                .pending_response()
                .unwrap()
                .reference(),
            &reference
        );
        let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        assert!(complete
            .local_moss
            .as_ref()
            .unwrap()
            .pending_response()
            .is_none());
        assert_eq!(fixture.artifacts.read(&reference).unwrap(), bytes);
        assert!(!fixture.effects.calls.lock().unwrap().contains(&"window"));
        if anchors {
            assert!(!fixture.effects.calls.lock().unwrap().contains(&"anchors"));
            assert_eq!(
                complete
                    .local_moss
                    .as_ref()
                    .unwrap()
                    .anchors()
                    .unwrap()
                    .reference(),
                &reference
            );
        } else {
            assert_eq!(
                complete.transcript_json.as_ref().unwrap()[0]["content"],
                "first 世界 saved before crash"
            );
        }
    }
}

#[tokio::test]
async fn intent_without_published_file_can_repeat_only_the_missing_local_effect() {
    for anchors in [false, true] {
        let fixture = Fixture::new();
        interrupted_intent(&fixture, anchors, false, false).await;
        let complete = fixture.engine.run_until_wait(fixture.id).await.unwrap();
        assert_eq!(complete.state, ProcessingState::Complete);
        let calls = fixture.effects.calls.lock().unwrap();
        assert_eq!(
            calls.iter().filter(|&&call| call == "window").count(),
            usize::from(!anchors)
        );
        assert_eq!(calls.iter().filter(|&&call| call == "anchors").count(), 1);
        assert!(complete
            .local_moss
            .as_ref()
            .unwrap()
            .pending_response()
            .is_none());
    }
}

#[tokio::test]
async fn pending_hash_mismatch_is_not_adopted_overwritten_or_silently_recomputed() {
    let fixture = Fixture::new();
    let (reference, _) = interrupted_intent(&fixture, false, true, false).await;
    let path = fixture
        .engine
        .store
        .root()
        .join("moss")
        .join(fixture.id.to_string())
        .join(reference.generation.to_string())
        .join("window-00.json");
    std::fs::write(&path, b"tampered cached response").unwrap();
    assert!(fixture.engine.run_until_wait(fixture.id).await.is_err());
    let failed = fixture.engine.store.load(fixture.id).unwrap();
    assert_eq!(failed.state, ProcessingState::ProviderFailed);
    assert_eq!(
        failed
            .local_moss
            .as_ref()
            .unwrap()
            .pending_response()
            .unwrap()
            .reference(),
        &reference
    );
    assert!(failed.transcript_json.is_none());
    assert!(fixture.effects.calls.lock().unwrap().is_empty());
    assert_eq!(std::fs::read(path).unwrap(), b"tampered cached response");
}

#[tokio::test]
async fn legacy_plan_cannot_claim_dispatch_or_resume_a_finalized_summary() {
    for summary_started in [false, true] {
        let fixture = Fixture::new();
        if summary_started {
            fixture.effects.block_step.store(2, Ordering::Release);
            let running = fixture.spawn();
            notified(&fixture.effects.started).await;
            running.abort();
            assert!(running.await.unwrap_err().is_cancelled());
            fixture.wait_idle().await;
        }
        retain_legacy_v1(&fixture);
        let before = fixture.engine.store.load(fixture.id).unwrap();
        let calls = fixture.effects.calls.lock().unwrap().clone();
        assert_legacy_rejected(fixture.engine.run_until_wait(fixture.id).await);
        assert_legacy_rejected(fixture.engine.retry(fixture.id).await);
        assert_eq!(fixture.engine.store.load(fixture.id).unwrap(), before);
        assert_eq!(*fixture.effects.calls.lock().unwrap(), calls);
    }
}

#[tokio::test]
async fn legacy_complete_plan_cannot_reprocess_or_start_cloud_backup() {
    let fixture = Fixture::new();
    fixture.engine.run_until_wait(fixture.id).await.unwrap();
    retain_legacy_v1(&fixture);
    let before = fixture.engine.store.load(fixture.id).unwrap();
    assert_eq!(before.state, ProcessingState::Complete);
    let calls = fixture.effects.calls.lock().unwrap().clone();
    assert_legacy_rejected(fixture.engine.reprocess(fixture.id).await);
    assert_legacy_rejected(fixture.engine.back_up_local_to_cloud(fixture.id).await);
    assert_eq!(fixture.engine.store.load(fixture.id).unwrap(), before);
    assert_eq!(*fixture.effects.calls.lock().unwrap(), calls);
}

#[path = "recovery_process.rs"]
mod recovery_process;

#[tokio::test]
async fn idle_control_does_not_wait_for_a_notification() {
    let control = RunControl {
        cancel: Arc::new(AtomicBool::new(false)),
        closing: AtomicBool::new(false),
        tasks: AtomicUsize::new(0),
        finished: Notify::new(),
    };
    tokio::time::timeout(std::time::Duration::from_secs(1), control.wait_idle())
        .await
        .unwrap();
}
