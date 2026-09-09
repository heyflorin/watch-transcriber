//! Opt-in original-AAC44 transcript matrix through the ACTUAL App stages.
//! No fake model/provider, playback, summary call, reference-guided decision or
//! publication. Stop at Summarizing; the corpus grader is a separate process.
use super::live::{digest_file, link_verified, source_path};
use super::*;
use crate::{
    ingest::{
        envelope::Platform,
        import::{
            ConfirmImportReviewRequest, DesktopImporter, ImportReviewState,
            DEFAULT_MAX_IMPORT_BYTES,
        },
        inbox::Inbox,
    },
    processing::{
        archive::DeferredArchive, direct::DirectEffects,
        local_models::moss::preparation_model_pins, local_worker::LocalWhisperWorker,
        moss_worker::MossWorker, ProcessingStore, TranscriptionBackend,
    },
    secrets::ProcessingCredentialsState,
};
use echowall_local_moss_protocol as moss;
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::Path,
    time::{Duration, Instant},
};

const POLICY: &str = "original-aac-app-quiet12-coalesced2-graph3-v1";
const WINDOW_AWARE_POLICY: &str = "original-aac-app-quiet12-chronological3-window-aware-tail2-v1";
const MANIFEST_SHA: &str = "c6615a07f8e80fbe75d3da65b75898423a716749750a1033bca0db70b4506280";
const AUDIT_SHA: &str = "4cbf15eab57e221049c8d5dd777dcbea5eb7c461b327484c2ea5c4977266ae0a";

fn read_json(root: &Path, relative: &str, limit: u64) -> Value {
    let path = source_path(root, relative);
    assert!(fs::metadata(&path).unwrap().len() <= limit);
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn publish(path: &Path, value: &Value) {
    let mut bytes = serde_json::to_vec_pretty(value).unwrap();
    bytes.push(b'\n');
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap()).unwrap();
    temporary.write_all(&bytes).unwrap();
    temporary.as_file().sync_all().unwrap();
    match temporary.persist_noclobber(path) {
        Ok(_) => {
            File::open(path.parent().unwrap())
                .unwrap()
                .sync_all()
                .unwrap();
        }
        Err(error) if error.error.kind() == std::io::ErrorKind::AlreadyExists => {
            assert!(fs::symlink_metadata(path).unwrap().is_file());
            assert!(!fs::symlink_metadata(path).unwrap().file_type().is_symlink());
            assert_eq!(
                fs::read(path).unwrap(),
                bytes,
                "never overwrite a conflicting retained result"
            );
        }
        Err(error) => panic!("private checkpoint failed: {:?}", error.error.kind()),
    }
}

fn canonical(ledger: &ProcessingLedger) -> Value {
    assert_eq!(ledger.state, ProcessingState::Summarizing);
    assert_eq!(
        ledger.transcription_backend,
        TranscriptionBackend::MossLocal
    );
    assert!(ledger.tos_object.is_none() && ledger.miaoji.is_none());
    assert!(
        ledger.summary_json.is_none()
            && ledger.publication.is_none()
            && ledger.canonical_backup.is_none()
    );
    json!({"schema_version":1,"segments":ledger.transcript_json.as_ref().unwrap().as_array().unwrap()
        .iter().map(|segment| json!({"start_ms":segment["start_time"],"end_ms":segment["end_time"],
            "speaker":segment["speaker"]["id"],"text":segment["content"]})).collect::<Vec<_>>()})
}

fn safe_error(error: &EngineError) -> String {
    match error {
        EngineError::Local(error) => error.code.to_owned(),
        EngineError::Effect(error) => format!("effect_{:?}", error.kind),
        EngineError::InvalidRecording(_) => "invalid_recording".into(),
        EngineError::ManualResolutionRequired => "manual_resolution_required".into(),
        EngineError::StepLimit => "step_limit".into(),
    }
}

async fn transcript_stages(
    engine: &ProcessingEngine<DirectEffects<DeferredArchive>>,
    id: Uuid,
    ordinal: usize,
) -> Result<ProcessingLedger, EngineError> {
    let ledger = engine.status(id)?;
    if ledger.state == ProcessingState::Summarizing {
        return Ok(ledger);
    }
    if ledger.state == ProcessingState::ProviderFailed {
        // A retained failure is evidence, not authorization to repeat inference.
        return Err(failure("matrix_retained_provider_failure"));
    }
    let generation = generation(&ledger)?;
    let scope = engine
        .begin_moss_run(&ledger)?
        .ok_or_else(|| failure("matrix_owner_busy"))?;
    for _ in 0..=moss::windows::MAX_WINDOWS + 3 {
        let state = engine.status(id)?.state;
        let effect = async {
            match state {
                ProcessingState::PreparingLocalMoss => {
                    engine.run_moss_preparation(id, generation, &scope).await
                }
                ProcessingState::LocalTranscribing => {
                    engine.run_moss_stage(id, generation, &scope).await
                }
                _ => Err(failure("matrix_unexpected_state")),
            }
        };
        if state == ProcessingState::Summarizing {
            return engine.status(id);
        }
        tokio::pin!(effect);
        loop {
            tokio::select! {
                result = &mut effect => { result?; break; },
                _ = tokio::time::sleep(Duration::from_secs(20)) => {
                    let current = engine.status(id)?;
                    println!("{}", json!({"case_ordinal":ordinal,"state":current.state,
                        "completed_windows":current.local_moss.as_ref().map_or(0, |v|v.completed_windows().len())}));
                }
            }
        }
    }
    Err(failure("matrix_stage_limit"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "all44 hash-allowlisted public AACs; real App transcript stages; OS network deny; no playback or summary"]
async fn public_uniform_aac44_transcript_matrix() {
    run_uniform_matrix(false).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "all44 public AACs; explicit window-aware plan; OS network deny; no playback or summary"]
async fn public_window_aware_aac44_transcript_matrix() {
    run_uniform_matrix(true).await;
}

async fn run_uniform_matrix(window_aware: bool) {
    let (policy, directory, manifest_prefix, mapping_policy, adaptation_policy, confirmation) =
        if window_aware {
            (
                WINDOW_AWARE_POLICY,
                "outputs/moss-window-aware-app-v1",
                "manifest-moss-window-aware-app-v1",
                crate::processing::local_moss::COMPOSED_MAPPING_POLICY,
                moss::CHRONOLOGICAL_TIMING_POLICY_V3,
                "public-window-aware-app-transcript-matrix-authorized",
            )
        } else {
            (
                POLICY,
                "outputs/moss-uniform-app-v1",
                "manifest-moss-uniform-app-v1",
                moss::speakers::POLICY,
                moss::COALESCING_TIMING_POLICY_V2,
                "public-uniform-app-transcript-matrix-authorized",
            )
        };
    assert_eq!(
        std::env::var("ECHOWALL_MOSS_MATRIX_CONFIRM").as_deref(),
        Ok(confirmation)
    );
    let resume = std::env::var_os("ECHOWALL_MOSS_MATRIX_RESUME_ROOT");
    assert!(window_aware || resume.is_some(), "legacy matrix is readback/resume only; new imports use an explicitly versioned candidate run");
    for error in [
        std::net::TcpListener::bind("127.0.0.1:0").unwrap_err(),
        std::net::TcpStream::connect_timeout(
            &"127.0.0.1:9".parse().unwrap(),
            Duration::from_millis(100),
        )
        .unwrap_err(),
    ] {
        assert!(matches!(
            error.raw_os_error(),
            Some(libc::EPERM | libc::EACCES)
        ));
    }
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let eval = fs::canonicalize(repo.join("local-eval")).unwrap();
    let matrix = eval.join("matrix");
    assert_eq!(
        digest_file(&source_path(&matrix, "manifest.json")),
        MANIFEST_SHA
    );
    assert_eq!(
        digest_file(&source_path(
            &matrix,
            "outputs/source-duration-audit-v1.json"
        )),
        AUDIT_SHA
    );
    let manifest = read_json(&matrix, "manifest.json", 1024 * 1024);
    let audit = read_json(&matrix, "outputs/source-duration-audit-v1.json", 512 * 1024);
    assert_eq!(audit["manifest_sha256"], MANIFEST_SHA);
    let mut cases = manifest["cases"].as_array().unwrap().clone();
    assert_eq!(cases.len(), 44);
    // Test scheduling only. These IDs/strata never select model parameters,
    // count hints, text, timing policy, mapping policy or inference outputs.
    cases.sort_by_key(|case| {
        let id = case["case_id"].as_str().unwrap();
        (!id.starts_with("mixed_"), id.to_owned())
    });
    let parent = matrix.join(directory);
    fs::create_dir_all(&parent).unwrap();
    let resuming = resume.is_some();
    let root = if let Some(path) = resume {
        let root = fs::canonicalize(path).unwrap();
        assert_eq!(root.parent(), Some(parent.as_path()));
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("run-"));
        root
    } else {
        tempfile::Builder::new()
            .prefix("run-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&parent)
            .unwrap()
            .keep()
    };
    assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o077, 0);
    let lock_path = root.join("matrix.lock");
    if resuming {
        source_path(&root, "matrix.lock");
    }
    let lock = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(lock_path)
        .unwrap();
    fs2::FileExt::try_lock_exclusive(&lock).expect("matrix already has a live owner");
    let relative_root = root
        .strip_prefix(&matrix)
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let run_name = root.file_name().unwrap().to_str().unwrap();
    let (model, speakers, _) = preparation_model_pins().unwrap();
    if !resuming {
        link_verified(
            &source_path(
                &eval,
                "models/moss-transcribe-diarize-q8/MOSS-Transcribe-Diarize-Q8_0.gguf",
            ),
            &root.join(format!("models/moss/{}/model.gguf", model.model_id)),
            model.model_size_bytes,
            &model.model_sha256,
        );
        for file in &speakers.model_files {
            let relative = format!(
                "models/diarization/{}/{}",
                speakers.pack_id, file.relative_path
            );
            link_verified(
                &source_path(&eval, &format!("speakerkit-worker-root/{relative}")),
                &root.join(relative),
                file.size_bytes,
                &file.sha256,
            );
        }
        fs::create_dir(root.join("workers")).unwrap();
        fs::create_dir(root.join("canonical")).unwrap();
        fs::create_dir(root.join("case-results")).unwrap();
        for name in ["moss", "diarization", "summary"] {
            fs::copy(
                source_path(
                    &fs::canonicalize(repo.join("desktop")).unwrap(),
                    &format!("src-tauri/binaries/echowall-{name}-worker-aarch64-apple-darwin"),
                ),
                root.join(format!("workers/echowall-{name}-worker")),
            )
            .unwrap();
        }
    }
    let moss_binary = source_path(&root, "workers/echowall-moss-worker");
    let diar_binary = source_path(&root, "workers/echowall-diarization-worker");
    let summary_binary = source_path(&root, "workers/echowall-summary-worker");
    let identities = json!({"moss":digest_file(&moss_binary),"speakerkit":digest_file(&diar_binary),"summary_not_invoked":digest_file(&summary_binary)});
    let mut declaration = json!({"schema_version":1,"policy":policy,"manifest_sha256":MANIFEST_SHA,"audit_sha256":AUDIT_SHA,
        "worker_sha256":identities,"model_sha256":model.model_sha256,"speakerkit_revision":speakers.model_revision,
        "window_policy":moss::windows::QUIET_WINDOW_POLICY,"adaptation_policy":adaptation_policy,
        "mapping_policy":mapping_policy,"source_format":"original_aac","case_count":44,
        "language":"auto","speaker_count_hint":null,"reference_used_for_inference":false,
        "stop_state":"summarizing","network_policy":"outer-test-and-workers-deny-network","credentials":"ephemeral-empty",
        "summary_or_archive_acceptance":false,"playback":false,"personal_audio":false});
    if window_aware {
        declaration["speakerkit_quality_preset"] = json!(speakers.quality_preset);
    }
    if resuming {
        assert_eq!(read_json(&root, "started.json", 1024 * 1024), declaration);
    } else {
        publish(&root.join("started.json"), &declaration);
    }
    publish(
        &root.join(format!("attempt-{}.json", Uuid::new_v4())),
        &json!({"test_binary_sha256":digest_file(&std::env::current_exe().unwrap()),
        "harness_source_sha256":hash(include_bytes!("matrix.rs")),"started_at":chrono::Utc::now(),"resuming":resuming,"policy":policy}),
    );
    println!(
        "{}",
        json!({"matrix_root":relative_root,"cases":44,"source_format":"original_aac","policy":policy})
    );
    let archive_root = root.join("archive");
    fs::create_dir_all(&archive_root).unwrap();
    let inbox = Arc::new(Inbox::open(&root, &archive_root).unwrap());
    let store = Arc::new(ProcessingStore::open(&root, inbox.root(), &archive_root).unwrap());
    let archive = Arc::new(
        DeferredArchive::new(
            archive_root.clone(),
            inbox.root().to_path_buf(),
            root.clone(),
        )
        .unwrap(),
    );
    let worker = MossWorker::from_paths_for_test(
        root.clone(),
        moss_binary,
        diar_binary,
        summary_binary,
        Duration::from_secs(20 * 60),
    );
    let effects = Arc::new(
        DirectEffects::production(
            Arc::new(ProcessingCredentialsState::ephemeral()),
            archive,
            LocalWhisperWorker::bundled(&root, false).unwrap(),
        )
        .unwrap()
        .with_moss_worker(worker),
    );
    let engine = ProcessingEngine::new(Arc::clone(&inbox), Arc::clone(&store), effects);
    let importer = DesktopImporter::new(
        Arc::clone(&inbox),
        Platform::Macos,
        DEFAULT_MAX_IMPORT_BYTES,
    )
    .unwrap();
    let review = ImportReviewState::new(Arc::clone(&inbox));
    let mut successful = Vec::new();
    let mut results = Vec::new();
    for (ordinal, case) in cases.iter().enumerate() {
        let started = Instant::now();
        let case_id = case["case_id"].as_str().unwrap();
        assert!(
            case_id.len() <= 32
                && case_id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_')
        );
        let row = audit["cases"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["case_id"] == case_id)
            .unwrap();
        let source = source_path(&matrix, &format!("audio/{case_id}.m4a"));
        assert_eq!(digest_file(&source), row["aac"]["sha256"].as_str().unwrap());
        assert_eq!(
            fs::metadata(&source).unwrap().len(),
            row["aac"]["size_bytes"].as_u64().unwrap()
        );
        let imported = importer.import_paths(vec![source.to_string_lossy().into_owned()]);
        let item = imported.results.first().unwrap();
        assert_eq!(item.sha256.as_deref(), row["aac"]["sha256"].as_str());
        let id = Uuid::parse_str(item.recording_id.as_deref().unwrap()).unwrap();
        match engine.status(id) {
            Ok(ledger) => assert_eq!(
                ledger.transcription_backend,
                TranscriptionBackend::MossLocal
            ),
            Err(EngineError::Local(error)) if error.code == "recording_not_found" => {
                assert!(
                    window_aware,
                    "legacy attempt cannot create preparations under a newer policy"
                );
                review
                    .confirm(ConfirmImportReviewRequest {
                        recording_id: id.to_string(),
                        captured_at: item.proposed_captured_at.clone().unwrap(),
                        display_title: Some("Public uniform matrix fixture".into()),
                        speaker_count: None,
                    })
                    .unwrap();
                engine.select_full_local_moss(id, None).unwrap();
            }
            Err(_) => panic!("cannot read public matrix ledger"),
        }
        let before = engine.status(id).unwrap();
        if let Some(checkpoint) = before.local_moss.as_ref() {
            let artifacts = MossArtifacts::open(&root, id, checkpoint.generation()).unwrap();
            let plan =
                LocalMossPlan::from_json(&artifacts.read(checkpoint.plan_ref()).unwrap()).unwrap();
            assert_eq!(
                plan.mapping_policy(),
                mapping_policy,
                "retained mapping policy must match this run"
            );
            assert_eq!(plan.adaptation_policy(), adaptation_policy);
            if window_aware {
                assert_eq!(
                    plan.diarization_request().quality_preset,
                    speakers.quality_preset
                );
            }
        } else {
            assert!(
                window_aware,
                "legacy preparation needs its original pinned runner"
            );
            let preparation = before.local_moss_preparation.as_ref().unwrap();
            assert_eq!(preparation.mapping_policy(), mapping_policy);
            assert_eq!(preparation.timing_policy(), adaptation_policy);
            assert_eq!(
                serde_json::to_value(preparation).unwrap()["model_pins"]
                    ["speakerkit_quality_preset"],
                speakers.quality_preset
            );
        }
        assert_eq!(
            inbox.load_envelope(id).unwrap().duration_ms,
            row["effective_duration_ms_ceil"].as_u64().unwrap()
        );
        println!(
            "{}",
            json!({"case_ordinal":ordinal+1,"cases":44,"state":"starting"})
        );
        let attempt = Uuid::new_v4();
        let result = transcript_stages(&engine, id, ordinal + 1).await;
        let current = engine.status(id).unwrap();
        let checkpoint = current.local_moss.as_ref();
        let mut proof = json!({"case_id":case_id,"recording_id":id,"policy":policy,"state":current.state,
            "source_sha256":row["aac"]["sha256"],"source_size_bytes":row["aac"]["size_bytes"],
            "effective_frames":row["effective_source_frames"],"historical_manifest_duration_ms":case["duration_ms"],
            "elapsed_seconds_this_attempt":started.elapsed().as_secs_f64(),"successful":result.is_ok(),
            "generation":checkpoint.map(|v|v.generation()),"completed_windows":checkpoint.map_or(0,|v|v.completed_windows().len()),
            "summary_or_archive_executed":false});
        if let Err(error) = &result {
            proof["error_code"] = json!(safe_error(error));
        } else {
            let saved = canonical(&current);
            let artifacts =
                MossArtifacts::open(&root, id, checkpoint.unwrap().generation()).unwrap();
            let plan =
                LocalMossPlan::from_json(&artifacts.read(checkpoint.unwrap().plan_ref()).unwrap())
                    .unwrap();
            assert_eq!(plan.mapping_policy(), mapping_policy);
            assert_eq!(plan.adaptation_policy(), adaptation_policy);
            assert_eq!(
                plan.spec().pcm_source_frames,
                row["effective_source_frames"].as_u64().unwrap()
            );
            assert_eq!(
                plan.windows().len(),
                checkpoint.unwrap().completed_windows().len()
            );
            let output = format!("{relative_root}/canonical/{case_id}.json");
            publish(&matrix.join(&output), &saved);
            proof["canonical_sha256"] = json!(digest_file(&matrix.join(&output)));
            proof["segments"] = json!(saved["segments"].as_array().unwrap().len());
            proof["plan_sha256"] = json!(plan.plan_sha256());
            let mut projected = case.clone();
            projected["local"] = json!(output);
            successful.push(projected);
        }
        assert_eq!(digest_file(&source), row["aac"]["sha256"].as_str().unwrap());
        assert!(
            current.summary_json.is_none()
                && current.publication.is_none()
                && current.tos_object.is_none()
                && current.miaoji.is_none()
        );
        publish(
            &root.join(format!("case-results/{case_id}-{attempt}.json")),
            &proof,
        );
        println!(
            "{}",
            json!({"case_ordinal":ordinal+1,"successful":result.is_ok(),"state":current.state,
            "completed_windows":proof["completed_windows"],"elapsed_seconds":proof["elapsed_seconds_this_attempt"]})
        );
        results.push(proof);
    }
    let mut generated = manifest.clone();
    generated["cases"] = Value::Array(successful.clone());
    let manifest_name = format!("{manifest_prefix}-{run_name}.json");
    publish(&matrix.join(&manifest_name), &generated);
    let outcome = json!({"schema_version":1,"scope":"source-engine-transcript-matrix-not-full-local-completion",
        "policy":policy,"attempted_cases":44,"successful_cases":successful.len(),"failed_cases":44-successful.len(),
        "manifest":manifest_name,"all_sources_unchanged":true,"reference_used_for_inference":false,"cases":results});
    publish(
        &root.join(format!("outcome-{}.json", Uuid::new_v4())),
        &outcome,
    );
    assert_eq!(
        digest_file(&source_path(&matrix, "manifest.json")),
        MANIFEST_SHA
    );
    assert!(!archive_root.join("manifest.json").exists());
    println!(
        "{}",
        json!({"matrix_finished":true,"successful_cases":successful.len(),"failed_cases":44-successful.len(),
        "manifest":manifest_name,"quality_verdict":"not_evaluated"})
    );
    assert_eq!(
        successful.len(),
        44,
        "retain incomplete cases; native completion is not a quality verdict"
    );
}

#[test]
fn matrix_policy_matches_the_app_constants() {
    assert_eq!(POLICY, "original-aac-app-quiet12-coalesced2-graph3-v1");
    assert_eq!(
        moss::speakers::POLICY,
        "moss-speakerkit-window-max-overlap-conflict-graph-v3"
    );
    assert_eq!(
        moss::COALESCING_TIMING_POLICY_V2,
        "joint-adjacent-union-unknown-tail100-v2"
    );
}
