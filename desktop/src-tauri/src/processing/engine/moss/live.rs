//! Opt-in source-engine proof with one hash-allowlisted public AMI excerpt.
//! Run the test binary under sandbox-exec deny-network; every worker is also
//! sandboxed by the production supervisor. No devices, playback or downloads.
use super::*;
use crate::{
    ingest::{
        envelope::Platform,
        import::{DesktopImporter, DEFAULT_MAX_IMPORT_BYTES},
        inbox::Inbox,
    },
    processing::{
        archive::DeferredArchive, direct::DirectEffects,
        local_models::moss::preparation_model_pins, local_worker::LocalWhisperWorker,
        moss_worker::MossWorker, ProcessingStore, PublicationBackend, TranscriptionBackend,
    },
    secrets::ProcessingCredentialsState,
};
use serde_json::{json, Value};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};

const SOURCE_SHA256: &str = "22c340e2193a29a14ecfcda5d6e7c0010b9d7efbbc3e4d0fc9ac374a2969fc8c";
const SOURCE_SIZE: u64 = 2_967_552;

pub(super) fn digest_file(path: &Path) -> String {
    let mut file = File::open(path).unwrap();
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = file.read(&mut buffer).unwrap();
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
    }
    hex::encode(digest.finalize())
}

pub(super) fn source_path(root: &Path, relative: &str) -> PathBuf {
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        assert!(matches!(component, std::path::Component::Normal(_)));
        path.push(component);
        assert!(!fs::symlink_metadata(&path)
            .unwrap()
            .file_type()
            .is_symlink());
    }
    assert!(fs::metadata(&path).unwrap().is_file());
    assert!(fs::canonicalize(&path).unwrap().starts_with(root));
    path
}

pub(super) fn link_verified(source: &Path, destination: &Path, size: u64, sha256: &str) {
    assert_eq!(fs::metadata(source).unwrap().len(), size);
    assert_eq!(digest_file(source), sha256);
    fs::create_dir_all(destination.parent().unwrap()).unwrap();
    // All sources are read-only public model/audio fixtures on this filesystem.
    // A hard link avoids another18GiB copy. Never chmod or write either link.
    fs::hard_link(source, destination).unwrap();
}

fn checkpoint(path: &Path, value: &Value) {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    use std::os::unix::fs::OpenOptionsExt;
    options.mode(0o600);
    let mut file = options.open(path).unwrap();
    file.write_all(&serde_json::to_vec_pretty(value).unwrap())
        .unwrap();
    file.sync_all().unwrap();
}

#[tokio::test]
#[ignore = "authorized public english_01 only, exact installed models, no network/playback/devices"]
async fn public_moss_engine_reaches_verified_local_archive() {
    assert_eq!(
        std::env::var("ECHOWALL_MOSS_ENGINE_CONFIRM").as_deref(),
        Ok("public-source-engine-offline-authorized")
    );
    // Do not label an ordinary unsandboxed cargo test as offline proof.
    // These probes transmit no application data and must be denied by policy,
    // not merely fail because no service happens to listen on the port.
    let bind = std::net::TcpListener::bind("127.0.0.1:0").unwrap_err();
    assert!(matches!(
        bind.raw_os_error(),
        Some(libc::EPERM | libc::EACCES)
    ));
    let connect = std::net::TcpStream::connect_timeout(
        &"127.0.0.1:9".parse().unwrap(),
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert!(matches!(
        connect.raw_os_error(),
        Some(libc::EPERM | libc::EACCES)
    ));
    let repo = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let eval = fs::canonicalize(repo.join("local-eval")).unwrap();
    // Keep the evidence root on failure too; only this fresh root is mutated.
    let parent = eval.join("matrix/outputs/moss-app-engine-v1");
    fs::create_dir_all(&parent).unwrap();
    use std::os::unix::fs::PermissionsExt;
    let resume_root = std::env::var_os("ECHOWALL_MOSS_ENGINE_RESUME_ROOT");
    let resuming = resume_root.is_some();
    let root = if let Some(path) = resume_root {
        let root = fs::canonicalize(path).unwrap();
        assert_eq!(root.parent(), Some(parent.as_path()));
        assert!(root
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("english_01-"));
        let started: Value =
            serde_json::from_slice(&fs::read(source_path(&root, "started.json")).unwrap()).unwrap();
        assert_eq!(started["case_id"], "english_01");
        assert_eq!(started["source_sha256"], SOURCE_SHA256);
        assert_eq!(
            started["model_sha256"],
            echowall_local_moss_protocol::MODEL_SHA256
        );
        assert!(!root.join("completed.json").exists());
        root
    } else {
        tempfile::Builder::new()
            .prefix("english_01-")
            .permissions(fs::Permissions::from_mode(0o700))
            .tempdir_in(&parent)
            .unwrap()
            .keep()
    };
    println!("moss_engine_public_root={}", root.display());
    let started = Instant::now();
    let source = source_path(&eval, "matrix/audio/english_01.m4a");
    let audio = root.join("public-english_01.m4a");
    let (moss, speakers, summary) = preparation_model_pins().unwrap();
    if !resuming {
        link_verified(&source, &audio, SOURCE_SIZE, SOURCE_SHA256);
        link_verified(
            &source_path(
                &eval,
                "models/moss-transcribe-diarize-q8/MOSS-Transcribe-Diarize-Q8_0.gguf",
            ),
            &root.join(format!("models/moss/{}/model.gguf", moss.model_id)),
            moss.model_size_bytes,
            &moss.model_sha256,
        );
        link_verified(
            &source_path(
                &eval,
                &format!("model-root/models/summary/{}/model.gguf", summary.model_id),
            ),
            &root.join(format!("models/summary/{}/model.gguf", summary.model_id)),
            summary.model_size_bytes,
            &summary.model_sha256,
        );
        for file in &speakers.model_files {
            link_verified(
                &source_path(
                    &eval,
                    &format!(
                        "speakerkit-worker-root/models/diarization/{}/{}",
                        speakers.pack_id, file.relative_path
                    ),
                ),
                &root.join(format!(
                    "models/diarization/{}/{}",
                    speakers.pack_id, file.relative_path
                )),
                file.size_bytes,
                &file.sha256,
            );
        }
    }
    assert_eq!(
        digest_file(&source_path(&root, "public-english_01.m4a")),
        SOURCE_SHA256
    );
    let moss_binary = source_path(
        &fs::canonicalize(repo.join("desktop")).unwrap(),
        "src-tauri/binaries/echowall-moss-worker-aarch64-apple-darwin",
    );
    let speaker_binary = source_path(
        &fs::canonicalize(repo.join("desktop")).unwrap(),
        "src-tauri/binaries/echowall-diarization-worker-aarch64-apple-darwin",
    );
    let summary_binary = source_path(
        &fs::canonicalize(repo.join("desktop")).unwrap(),
        "src-tauri/binaries/echowall-summary-worker-aarch64-apple-darwin",
    );
    let worker_hashes = json!({"moss": digest_file(&moss_binary), "speakerkit": digest_file(&speaker_binary), "summary": digest_file(&summary_binary)});
    let attempt = Uuid::new_v4();
    checkpoint(
        &root.join(if resuming {
            format!("resumed-{attempt}.json")
        } else {
            "started.json".into()
        }),
        &json!({"scope":"source_engine_public_only", "case_id":"english_01", "source_sha256":SOURCE_SHA256,
        "worker_sha256":worker_hashes, "model_id":moss.model_id, "model_sha256":moss.model_sha256,
        "speakerkit_revision":speakers.model_revision, "summary_model":summary.model_id, "summary_sha256":summary.model_sha256,
        "network_policy":"outer-test-and-three-workers-deny-network", "credentials":"ephemeral-empty", "playback":false}),
    );
    let archive_root = root.join("archive");
    fs::create_dir_all(&archive_root).unwrap();
    let inbox = Arc::new(Inbox::open(&root, &archive_root).unwrap());
    let store = Arc::new(ProcessingStore::open(&root, inbox.root(), &archive_root).unwrap());
    let id = if resuming {
        let ids = store.list_recording_ids().unwrap();
        assert_eq!(ids.len(), 1);
        let ledger = store.load(ids[0]).unwrap();
        assert_eq!(ledger.state, ProcessingState::ProviderFailed);
        assert_eq!(
            ledger.transcription_backend,
            TranscriptionBackend::MossLocal
        );
        assert_eq!(ledger.normalized.sha256, SOURCE_SHA256);
        ids[0]
    } else {
        let imported = DesktopImporter::new(
            Arc::clone(&inbox),
            Platform::Macos,
            DEFAULT_MAX_IMPORT_BYTES,
        )
        .unwrap()
        .import_paths(vec![audio.to_string_lossy().into_owned()]);
        Uuid::parse_str(
            imported.results[0]
                .recording_id
                .as_deref()
                .expect("public source imports"),
        )
        .unwrap()
    };
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
        speaker_binary,
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
    let engine = Arc::new(ProcessingEngine::new(
        Arc::clone(&inbox),
        Arc::clone(&store),
        effects,
    ));
    let reused_windows = if resuming {
        store
            .load(id)
            .unwrap()
            .local_moss
            .as_ref()
            .unwrap()
            .completed_windows()
            .len()
    } else {
        let selected = engine.select_full_local_moss(id, None).unwrap();
        assert_eq!(selected.state, ProcessingState::PreparingLocalMoss);
        0
    };
    let runner = Arc::clone(&engine);
    let mut task = tokio::spawn(async move {
        if resuming {
            runner.retry(id).await
        } else {
            runner.run_until_wait(id).await
        }
    });
    let result = loop {
        tokio::select! {
            result = &mut task => break result.unwrap(),
            _ = tokio::time::sleep(Duration::from_secs(30)) => {
                let status = store.load(id).unwrap();
                println!("moss_engine_elapsed={:.1} state={:?} windows={}", started.elapsed().as_secs_f64(), status.state,
                    status.local_moss.as_ref().map_or(0, |checkpoint| checkpoint.completed_windows().len()));
            }
        }
    };
    if let Err(error) = &result {
        checkpoint(
            &root.join(if resuming {
                format!("failed-{attempt}.json")
            } else {
                "failed.json".into()
            }),
            &json!({"state":"failed", "safe_error":error.to_string(), "elapsed_seconds":started.elapsed().as_secs_f64()}),
        );
    }
    let complete = result.unwrap();
    assert_eq!(complete.state, ProcessingState::Complete);
    assert_eq!(
        complete.transcription_backend,
        TranscriptionBackend::MossLocal
    );
    assert_eq!(
        complete.publication_backend,
        PublicationBackend::LocalArchive
    );
    assert!(complete.tos_object.is_none() && complete.miaoji.is_none());
    assert!(complete
        .canonical_backup
        .as_ref()
        .unwrap()
        .locator
        .starts_with("local:"));
    assert!(complete.cleanup.temporary_tos_deleted);
    let manifest: Value =
        serde_json::from_slice(&fs::read(archive_root.join("manifest.json")).unwrap()).unwrap();
    let entry = manifest.as_object().unwrap().values().next().unwrap();
    assert!(entry.get("r2_key").is_none());
    assert!(archive_root.join("index.html").is_file());
    let archive_audio = archive_root.join(entry["audio"].as_str().unwrap());
    assert_eq!(digest_file(&archive_audio), SOURCE_SHA256);
    assert_eq!(digest_file(&source), SOURCE_SHA256);
    let again = engine.run_until_wait(id).await.unwrap();
    assert_eq!(again, complete);
    let moss_checkpoint = complete.local_moss.as_ref().unwrap();
    let artifacts = MossArtifacts::open(&root, id, moss_checkpoint.generation()).unwrap();
    let plan =
        LocalMossPlan::from_json(&artifacts.read(moss_checkpoint.plan_ref()).unwrap()).unwrap();
    let receipt = json!({"state":"pass", "scope":"source_engine_not_signed_app_or_quality_acceptance", "recording_id":id,
        "elapsed_seconds":started.elapsed().as_secs_f64(), "source_sha256":SOURCE_SHA256, "archive_audio_sha256":digest_file(&archive_audio),
        "selected_worker_sha256":worker_hashes, "reused_windows":reused_windows, "prior_worker_identity":"started.json",
        "segments":complete.transcript_json.as_ref().unwrap().as_array().unwrap().len(),
        "summary_fields":complete.summary_json.as_ref().unwrap().as_object().unwrap().len(), "windows":complete.local_moss.as_ref().unwrap().completed_windows().len(),
        "plan_version":plan.spec().schema_version, "mapping_policy":plan.mapping_policy(), "plan_sha256":plan.plan_sha256(),
        "diarization_preset":plan.diarization_request().quality_preset,
        "adaptation_policy":plan.adaptation_policy(),
        "network_policy":"outer-test-and-three-workers-deny-network", "credentials":"ephemeral-empty", "idempotent_resume":true,
        "playback":false, "personal_audio":false});
    checkpoint(&root.join("completed.json"), &receipt);
    println!("{}", serde_json::to_string(&receipt).unwrap());
}
