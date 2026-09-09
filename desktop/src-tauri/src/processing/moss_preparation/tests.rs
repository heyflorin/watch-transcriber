use std::{
    fs,
    path::PathBuf,
    sync::{atomic::Ordering, Arc, Barrier},
    thread,
};

use sha2::{Digest, Sha256};
use tempfile::TempDir;

use super::*;
use crate::processing::{local_models::moss::preparation_model_pins_fixture, ResumeAction};

mod policy;

const FRAMES: usize = 32_017;

struct Fixture {
    _temp: TempDir,
    app: PathBuf,
    archive: PathBuf,
    store: ProcessingStore,
    recording: Uuid,
    generation: Uuid,
    artifacts: MossArtifacts,
    selection: MossPreparationCheckpoint,
    source_bytes: Vec<u8>,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let app = temp.path().join("app");
        let archive = temp.path().join("archive");
        let store = ProcessingStore::open(&app, &app.join("inbox"), &archive).unwrap();
        let app = fs::canonicalize(app).unwrap();
        let recording = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let relative_path = format!("inbox/{recording}/tracks/source.wav");
        let source_bytes = wav_bytes(&vec![8192; FRAMES]);
        let path = app.join(&relative_path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, &source_bytes).unwrap();
        let source = SourceAudioIdentity {
            relative_path,
            sha256: hex::encode(Sha256::digest(&source_bytes)),
            size_bytes: source_bytes.len() as u64,
            // Deliberately stale. It must never control output sample count.
            duration_ms: 999_999,
        };
        store
            .enqueue(
                recording,
                NormalizedArtifactCheckpoint {
                    relative_path: "tracks/source.wav".into(),
                    sha256: source.sha256.clone(),
                    size_bytes: source.size_bytes,
                },
            )
            .unwrap();
        let artifacts = MossArtifacts::open(&app, recording, generation).unwrap();
        let (moss, speakers, summary) = preparation_model_pins_fixture();
        let selection = MossPreparationCheckpoint::new(
            recording,
            generation,
            source,
            Some("en".into()),
            &moss,
            &speakers,
            summary,
        )
        .unwrap();
        Self {
            _temp: temp,
            app,
            archive,
            store,
            recording,
            generation,
            artifacts,
            selection,
            source_bytes,
        }
    }

    fn begin(&self, owner: &OwnerLease) -> MossPreparationClaim {
        self.store
            .begin_moss_preparation(self.recording, owner, self.selection.clone())
            .unwrap();
        self.store
            .claim_moss_preparation(self.recording, owner)
            .unwrap()
    }

    fn source_unchanged(&self) {
        assert_eq!(
            fs::read(self.app.join(&self.selection.source().relative_path)).unwrap(),
            self.source_bytes
        );
    }

    fn publish(&self) -> PreparedMossWindow {
        files::publish_window(
            &self.app,
            self.recording,
            self.generation,
            0,
            0,
            &vec![8192; FRAMES],
            &AtomicBool::new(false),
        )
        .unwrap()
    }

    fn plan(&self, prepared: &PreparedMossWindow) -> LocalMossPlan {
        let mut source = self.selection.source().clone();
        source.duration_ms = (FRAMES as u64).div_ceil(16);
        LocalMossPlan::new_for_preparation(super::super::local_moss::LocalMossPlanSpec {
            schema_version: self.selection.plan_schema_version(),
            mapping_policy: self.selection.0.mapping_policy.clone(),
            recording_id: self.recording,
            diarization_request: self.selection.diarization_request(&source),
            source,
            pcm_sample_rate: 16_000,
            pcm_source_frames: FRAMES as u64,
            pcm_quantization_policy: Some(PCM_QUANTIZATION_POLICY.into()),
            window_policy: moss::windows::QUIET_WINDOW_POLICY.into(),
            windows: vec![super::super::local_moss::MossWindowRequestSpec {
                index: 0,
                start_frame: 0,
                end_frame: FRAMES as u64,
                request: moss::MossRequest {
                    schema_version: moss::PROTOCOL_VERSION,
                    recording_id: self.recording,
                    runtime_id: moss::RUNTIME_ID.into(),
                    model_id: moss::MODEL_ID.into(),
                    model_revision: moss::MODEL_REVISION.into(),
                    model_sha256: moss::MODEL_SHA256.into(),
                    model_size_bytes: moss::MODEL_SIZE_BYTES,
                    timing_policy: self.selection.timing_policy().into(),
                    audio_relative_path: prepared.relative_path.clone(),
                    audio_sha256: prepared.sha256.clone(),
                    audio_size_bytes: prepared.size_bytes,
                    audio_duration_ms: (FRAMES as u64).div_ceil(16),
                    language: Some("en".into()),
                },
            }],
        })
        .unwrap()
    }
}

fn wav_bytes(samples: &[i16]) -> Vec<u8> {
    let data_bytes = samples.len() as u32 * 2;
    let mut bytes = b"RIFF".to_vec();
    bytes.extend((data_bytes + 36).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16_u32.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(1_u16.to_le_bytes());
    bytes.extend(16_000_u32.to_le_bytes());
    bytes.extend(32_000_u32.to_le_bytes());
    bytes.extend(2_u16.to_le_bytes());
    bytes.extend(16_u16.to_le_bytes());
    bytes.extend(b"data");
    bytes.extend(data_bytes.to_le_bytes());
    for sample in samples {
        bytes.extend(sample.to_le_bytes());
    }
    bytes
}

#[test]
fn selection_and_remote_resume_are_one_atomic_choice() {
    for _ in 0..12 {
        let fixture = Fixture::new();
        let owner = fixture.artifacts.try_owner().unwrap();
        let barrier = Arc::new(Barrier::new(2));
        thread::scope(|scope| {
            let gate = barrier.clone();
            let fixture = &fixture;
            let owner = &owner;
            let selection = scope.spawn(move || {
                gate.wait();
                fixture.store.begin_moss_preparation(
                    fixture.recording,
                    owner,
                    fixture.selection.clone(),
                )
            });
            barrier.wait();
            let resumed = fixture.store.resume(fixture.recording).unwrap();
            match selection.join().unwrap() {
                Ok(ledger) => {
                    assert_eq!(ledger.state, ProcessingState::PreparingLocalMoss);
                    assert_eq!(
                        resumed,
                        ResumeAction::PrepareMoss {
                            generation: fixture.generation
                        }
                    );
                    assert_eq!(
                        fixture.store.load(fixture.recording).unwrap().state,
                        ProcessingState::PreparingLocalMoss
                    );
                }
                Err(error) => {
                    assert_eq!(error.code, "remote_effect_already_started");
                    assert_eq!(resumed, ResumeAction::Upload);
                    let ledger = fixture.store.load(fixture.recording).unwrap();
                    assert_eq!(ledger.state, ProcessingState::Uploading);
                    assert!(ledger.local_moss_preparation.is_none());
                }
            }
        });
        assert!(!fixture
            .app
            .join(format!("inbox/{}/derived", fixture.recording))
            .exists());
        fixture.source_unchanged();
    }
}

#[test]
fn restart_after_selection_never_uploads_and_probes_current_effective_frames() {
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.begin(&owner);
    drop(owner);
    let reopened =
        ProcessingStore::open(&fixture.app, &fixture.app.join("inbox"), &fixture.archive).unwrap();
    assert_eq!(
        reopened.resume(fixture.recording).unwrap(),
        ResumeAction::PrepareMoss {
            generation: fixture.generation
        }
    );
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = reopened
        .claim_moss_preparation(fixture.recording, &owner)
        .unwrap();
    let ready = prepare_moss_source(
        &fixture.app,
        &reopened,
        &owner,
        &claim,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(ready.state, ProcessingState::LocalTranscribing);
    assert!(ready.local_moss_preparation.is_none());
    assert_eq!(
        ready.local_moss.as_ref().unwrap().source().duration_ms,
        2002
    );
    let bytes = fixture
        .artifacts
        .read(ready.local_moss.as_ref().unwrap().plan_ref())
        .unwrap();
    let plan = LocalMossPlan::from_json(&bytes).unwrap();
    assert_eq!(plan.spec().pcm_source_frames, FRAMES as u64);
    assert_eq!(plan.windows().last().unwrap().end_frame(), FRAMES as u64);
    assert_eq!(plan.windows()[0].request().audio_duration_ms, 2002);
    assert_eq!(plan.windows()[0].request().language.as_deref(), Some("en"));
    assert_eq!(
        fs::read(
            fixture
                .app
                .join(&plan.windows()[0].request().audio_relative_path)
        )
        .unwrap(),
        fixture.source_bytes
    );
    assert_eq!(ready.summary_backend, SummaryBackend::QwenLocal);
    assert_eq!(ready.publication_backend, PublicationBackend::LocalArchive);
    fixture.source_unchanged();
}

#[test]
fn crash_after_publication_adopts_only_identical_generation_owned_pcm() {
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    fixture.begin(&owner);
    let published = fixture.publish(); // crash before recording this reference
    let path = fixture.app.join(published.relative_path());
    let metadata = fs::metadata(&path).unwrap();
    drop(owner);
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = fixture
        .store
        .claim_moss_preparation(fixture.recording, &owner)
        .unwrap();
    prepare_moss_source(
        &fixture.app,
        &fixture.store,
        &owner,
        &claim,
        &AtomicBool::new(false),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::{MetadataExt, PermissionsExt};
        assert_eq!(metadata.ino(), fs::metadata(&path).unwrap().ino());
        assert_eq!(metadata.permissions().mode() & 0o077, 0);
    }
    fixture.source_unchanged();

    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = fixture.begin(&owner);
    let published = fixture.publish();
    let path = fixture.app.join(published.relative_path());
    fs::write(&path, b"do not overwrite this conflict").unwrap();
    assert_eq!(
        prepare_moss_source(
            &fixture.app,
            &fixture.store,
            &owner,
            &claim,
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .code,
        "moss_preparation_file_conflict"
    );
    assert_eq!(fs::read(path).unwrap(), b"do not overwrite this conflict");
    assert_eq!(
        fixture.store.load(fixture.recording).unwrap().state,
        ProcessingState::PreparingLocalMoss
    );
    assert!(fixture
        .store
        .load(fixture.recording)
        .unwrap()
        .local_moss
        .is_none());
    fixture.source_unchanged();
}

#[test]
fn atomic_ready_swap_preserves_old_ready_and_legacy_json_shapes() {
    let fixture = Fixture::new();
    let queued = fixture.store.load(fixture.recording).unwrap();
    let legacy = serde_json::to_value(&queued).unwrap();
    assert!(legacy.get("local_moss_preparation").is_none());
    let restored: ProcessingLedger = serde_json::from_value(legacy.clone()).unwrap();
    restored.validate().unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), legacy);
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = fixture.begin(&owner);
    let window = fixture.publish();
    fixture
        .store
        .record_moss_prepared_window(fixture.recording, &owner, &claim, window.clone())
        .unwrap();
    let plan = fixture.plan(&window);
    let reference = fixture
        .artifacts
        .write(&owner, ArtifactKind::Plan, plan.plan_bytes())
        .unwrap();
    let before = fixture.store.load(fixture.recording).unwrap();
    let mut wrong = reference.clone();
    wrong.sha256 = "0".repeat(64);
    assert!(fixture
        .store
        .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, wrong)
        .is_err());
    let unchanged = fixture.store.load(fixture.recording).unwrap();
    assert_eq!(unchanged.revision, before.revision);
    assert!(unchanged.local_moss.is_none());
    assert!(unchanged.local_moss_preparation.is_some());
    let ready = fixture
        .store
        .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, reference.clone())
        .unwrap();
    assert_eq!(ready.state, ProcessingState::LocalTranscribing);
    assert_eq!(ready.local_moss.as_ref().unwrap().plan_ref(), &reference);
    assert!(ready.local_moss_preparation.is_none());
    let old_ready = serde_json::to_value(&ready).unwrap();
    assert!(old_ready.get("local_moss_preparation").is_none());
    let restored: ProcessingLedger = serde_json::from_value(old_ready.clone()).unwrap();
    restored.validate().unwrap();
    assert_eq!(serde_json::to_value(restored).unwrap(), old_ready);
    assert!(fixture
        .store
        .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, reference)
        .is_err());
    assert_eq!(
        fixture.store.load(fixture.recording).unwrap().revision,
        ready.revision
    );
}

#[test]
fn cancel_persists_first_and_rejects_late_prepared_refs_and_completion() {
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = fixture.begin(&owner);
    let window = fixture.publish();
    let plan = fixture.plan(&window);
    let reference = fixture
        .artifacts
        .write(&owner, ArtifactKind::Plan, plan.plan_bytes())
        .unwrap();
    let canceled = fixture.store.cancel(fixture.recording).unwrap();
    assert_eq!(canceled.state, ProcessingState::CanceledBeforeUpload);
    let flag = AtomicBool::new(false);
    flag.store(true, Ordering::Release);
    assert!(fixture
        .store
        .record_moss_prepared_window(fixture.recording, &owner, &claim, window)
        .is_err());
    assert!(fixture
        .store
        .complete_moss_preparation(fixture.recording, &owner, &claim, &plan, reference)
        .is_err());
    assert!(fixture
        .store
        .claim_moss_preparation(fixture.recording, &owner)
        .is_err());
    assert!(prepare_moss_source(&fixture.app, &fixture.store, &owner, &claim, &flag).is_err());
    let retained = fixture.store.load(fixture.recording).unwrap();
    assert_eq!(retained.revision, canceled.revision);
    assert!(retained.local_moss.is_none());
    assert_eq!(
        fixture.store.resume(fixture.recording).unwrap(),
        ResumeAction::Canceled
    );
    fixture.source_unchanged();
}

#[test]
fn failed_preparation_retries_same_pins_and_rejects_previous_claim() {
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    let old = fixture.begin(&owner);
    let failed = fixture
        .store
        .fail_moss_preparation(fixture.recording, &owner, &old)
        .unwrap();
    assert_eq!(failed.state, ProcessingState::ProviderFailed);
    assert!(!matches!(
        fixture.store.resume(fixture.recording).unwrap(),
        ResumeAction::Upload
    ));
    fixture
        .store
        .retry_moss_preparation(fixture.recording, &owner)
        .unwrap();
    let new = fixture
        .store
        .claim_moss_preparation(fixture.recording, &owner)
        .unwrap();
    assert_ne!(old.token(), new.token());
    assert_eq!(old.generation(), new.generation());
    assert_eq!(
        fixture
            .store
            .check_moss_preparation_claim(fixture.recording, &owner, &old)
            .unwrap_err()
            .code,
        "stale_moss_preparation_claim"
    );
    let window = fixture.publish();
    assert!(fixture
        .store
        .record_moss_prepared_window(fixture.recording, &owner, &old, window)
        .is_err());
    let ready = prepare_moss_source(
        &fixture.app,
        &fixture.store,
        &owner,
        &new,
        &AtomicBool::new(false),
    )
    .unwrap();
    assert_eq!(ready.state, ProcessingState::LocalTranscribing);
    fixture.source_unchanged();
}

#[test]
fn preparation_deserialization_rejects_pin_policy_language_and_generation_drift() {
    let fixture = Fixture::new();
    let source = serde_json::to_value(&fixture.selection).unwrap();
    for (pointer, value) in [
        (
            "/model_pins/model_sha256",
            serde_json::json!("0".repeat(64)),
        ),
        (
            "/model_pins/speakerkit_model_files/0/sha256",
            serde_json::json!("0".repeat(64)),
        ),
        ("/summary/model_sha256", serde_json::json!("0".repeat(64))),
        ("/language", serde_json::json!("auto-guess")),
        ("/generation", serde_json::json!(Uuid::nil())),
        ("/pcm_quantization_policy", serde_json::json!("truncate")),
        ("/window_policy", serde_json::json!("fixed12m")),
        (
            "/source/relative_path",
            serde_json::json!("inbox/../outside.wav"),
        ),
        (
            "/source/relative_path",
            serde_json::json!(window_path(fixture.recording, fixture.generation, 0)),
        ),
        (
            "/source/relative_path",
            serde_json::json!(
                window_path(fixture.recording, fixture.generation, 0).replace("/moss_", "/MOSS_")
            ),
        ),
    ] {
        let mut changed = source.clone();
        *changed.pointer_mut(pointer).unwrap() = value;
        assert!(
            serde_json::from_value::<MossPreparationCheckpoint>(changed).is_err(),
            "{pointer}"
        );
    }
    let mut changed = source;
    changed["unexpected"] = serde_json::json!(true);
    assert!(serde_json::from_value::<MossPreparationCheckpoint>(changed).is_err());
}

#[test]
fn media_inspection_cancels_before_and_between_packets_without_touching_source() {
    let fixture = Fixture::new();
    let file = fs::File::open(fixture.app.join(&fixture.selection.source().relative_path)).unwrap();
    let error = crate::ingest::import::inspect_media_with_cancel(&file, "wav", || true)
        .err()
        .unwrap();
    assert_eq!(error.code, "decode_cancelled");
    let mut polls = 0;
    let error = crate::ingest::import::inspect_media_with_cancel(&file, "wav", || {
        polls += 1;
        polls == 3
    })
    .err()
    .unwrap();
    assert_eq!(error.code, "decode_cancelled");
    assert_eq!(polls, 3);
    fixture.source_unchanged();
}

#[cfg(unix)]
#[test]
fn symlinked_source_and_prepared_destination_are_rejected_without_modification() {
    use std::os::unix::fs::symlink;
    let fixture = Fixture::new();
    let owner = fixture.artifacts.try_owner().unwrap();
    let claim = fixture.begin(&owner);
    let window = fixture.publish();
    let path = fixture.app.join(window.relative_path());
    fs::remove_file(&path).unwrap();
    let original = fixture.app.join(&fixture.selection.source().relative_path);
    symlink(&original, &path).unwrap();
    assert_eq!(
        prepare_moss_source(
            &fixture.app,
            &fixture.store,
            &owner,
            &claim,
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .code,
        "moss_preparation_file_conflict"
    );
    fixture.source_unchanged();
    let moved = original.with_extension("kept");
    fs::rename(&original, &moved).unwrap();
    symlink(&moved, &original).unwrap();
    assert_eq!(
        prepare_moss_source(
            &fixture.app,
            &fixture.store,
            &owner,
            &claim,
            &AtomicBool::new(false)
        )
        .unwrap_err()
        .code,
        "unsafe_moss_preparation_path"
    );
    assert_eq!(fs::read(moved).unwrap(), fixture.source_bytes);
}
