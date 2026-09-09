use std::{
    fs,
    os::unix::fs::{symlink, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use echowall_local_summary_protocol::{
    self as summary, LocalSummaryDocument, LocalSummaryResponse,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use tokio::time::{sleep, timeout, Instant};
use uuid::Uuid;

use super::*;
use crate::processing::local_whisper::*;

struct Fixture {
    _temp: TempDir,
    root: PathBuf,
}
impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("App Data");
        fs::create_dir(&root).unwrap();
        let root = fs::canonicalize(root).unwrap();
        Self { _temp: temp, root }
    }
    fn script(&self, name: &str, body: &str) -> PathBuf {
        let path = self.root.join(name);
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        path
    }
    fn worker(&self, executable: &Path, deadline: Duration) -> MossWorker {
        MossWorker::from_paths_for_test(
            self.root.clone(),
            executable.into(),
            executable.into(),
            executable.into(),
            deadline,
        )
    }
    fn spinning(&self) -> PathBuf {
        self.script(
            "spin",
            "printf '%s' \"$$\" > \"$ECHOWALL_APP_DATA_ROOT/pid\"\nwhile :; do :; done",
        )
    }
    async fn pid(&self) -> i32 {
        timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(pid) = fs::read_to_string(self.root.join("pid")) {
                    if let Ok(pid) = pid.parse() {
                        break pid;
                    }
                }
                sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap()
    }
}

fn cancelled_flag() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}
fn sha(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
fn shell_literal(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

struct ParentMarker(Option<std::ffi::OsString>);
impl ParentMarker {
    fn install() -> Self {
        let previous = std::env::var_os("ECHOWALL_MOSS_LAUNCHER_TEST_MARKER");
        std::env::set_var(
            "ECHOWALL_MOSS_LAUNCHER_TEST_MARKER",
            "synthetic-parent-only",
        );
        Self(previous)
    }
}
impl Drop for ParentMarker {
    fn drop(&mut self) {
        if let Some(previous) = &self.0 {
            std::env::set_var("ECHOWALL_MOSS_LAUNCHER_TEST_MARKER", previous);
        } else {
            std::env::remove_var("ECHOWALL_MOSS_LAUNCHER_TEST_MARKER");
        }
    }
}

fn moss_request() -> moss::MossRequest {
    let recording_id = Uuid::from_u128(42);
    moss::MossRequest {
        schema_version: moss::PROTOCOL_VERSION,
        recording_id,
        runtime_id: moss::RUNTIME_ID.into(),
        model_id: moss::MODEL_ID.into(),
        model_revision: moss::MODEL_REVISION.into(),
        model_sha256: moss::MODEL_SHA256.into(),
        model_size_bytes: moss::MODEL_SIZE_BYTES,
        timing_policy: moss::COALESCING_TIMING_POLICY_V2.into(),
        audio_relative_path: format!("inbox/{recording_id}/derived/fake.wav"),
        audio_sha256: "b".repeat(64),
        audio_size_bytes: 44,
        audio_duration_ms: 1000,
        language: None,
    }
}

fn moss_output(request: &moss::MossRequest) -> Vec<u8> {
    let response = moss::MossResponse {
        schema_version: moss::PROTOCOL_VERSION,
        recording_id: request.recording_id,
        runtime_id: request.runtime_id.clone(),
        model_id: request.model_id.clone(),
        model_sha256: request.model_sha256.clone(),
        audio_sha256: request.audio_sha256.clone(),
        timing_policy: request.timing_policy.clone(),
        complete: true,
        segments: vec![moss::MossSegment {
            start_ms: 0,
            end_ms: 500,
            speaker_id: 1,
            text: "fabricated text".into(),
        }],
    };
    let mut bytes = b" \n".to_vec();
    bytes.extend(serde_json::to_vec_pretty(&response).unwrap());
    bytes.extend(b"\n\n");
    bytes
}

fn summary_request() -> LocalSummaryRequest {
    let transcript = "synthetic summary input".to_owned();
    LocalSummaryRequest {
        schema_version: summary::LOCAL_SUMMARY_PROTOCOL_VERSION,
        recording_id: Uuid::from_u128(42),
        model_id: "test-summary".into(),
        model_sha256: "a".repeat(64),
        model_size_bytes: 1,
        prompt_version: summary::LOCAL_SUMMARY_PROMPT_VERSION.into(),
        transcript_sha256: sha(transcript.as_bytes()),
        transcript,
    }
}

fn response_script(output: &[u8]) -> String {
    format!("[ \"$#\" -eq 0 ] || exit 11\n[ \"$OS_ACTIVITY_MODE\" = disable ] || exit 12\n[ -z \"${{CARGO_MANIFEST_DIR+x}}\" ] || exit 13\n[ -z \"${{ECHOWALL_MOSS_LAUNCHER_TEST_MARKER+x}}\" ] || exit 14\nIFS= read -r line || [ -n \"$line\" ]\nprintf '%s\\n' \"$line\" > \"$ECHOWALL_APP_DATA_ROOT/received.json\"\nprintf '%s' {}",
        shell_literal(std::str::from_utf8(output).unwrap()))
}

fn assert_reaped(pid: i32) {
    // Read-only process probes on the exact PID created by this test.
    assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
    let mut status = 0;
    assert_eq!(
        unsafe { libc::waitpid(pid, &mut status, libc::WNOHANG) },
        -1
    );
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

async fn await_reaped(pid: i32) {
    timeout(Duration::from_secs(2), async {
        loop {
            if unsafe { libc::kill(pid, 0) } == -1
                && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
            {
                break;
            }
            sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    assert_reaped(pid);
}

#[tokio::test]
async fn fabricated_snapshot_is_reclaimed_after_cancel_and_future_abort_only() {
    for abort in [false, true] {
        let fixture = Fixture::new();
        // Test harness mode is a synthetic byte-copy holder, not a native
        // diarizer/model proof. Production still launches its fixed no-arg bin.
        let executable = fixture.script(
            "snapshot-holder",
            &format!(
                "exec {} --exact {} --ignored",
                shell_literal(std::env::current_exe().unwrap().to_str().unwrap()),
                shell_literal(&scratch::tests::child_test_name()),
            ),
        );
        let worker = fixture.worker(&executable, Duration::from_secs(15));
        let cancel = cancelled_flag();
        let child_cancel = cancel.clone();
        let task =
            tokio::spawn(async move { worker.transcribe(&moss_request(), child_cancel).await });
        let pid = timeout(Duration::from_secs(10), async {
            loop {
                if let Ok(text) = fs::read_to_string(fixture.root.join("scratch-ready")) {
                    if let Ok(pid) = text.parse::<i32>() {
                        break pid;
                    }
                }
                assert!(!task.is_finished());
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        let path = fixture
            .root
            .join("processing/moss-snapshots-v1/slot-0/model.gguf");
        assert_eq!(fs::read(&path).unwrap(), b"synthetic owned copy");
        assert_eq!(scratch::reclaim_orphans(&fixture.root).unwrap(), 0);
        let live_other = scratch::SnapshotLease::create(&fixture.root).unwrap();
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            await_reaped(pid).await;
            timeout(Duration::from_secs(2), async {
                while path.exists() {
                    sleep(Duration::from_millis(5)).await;
                }
            })
            .await
            .unwrap();
        } else {
            cancel.store(true, Ordering::Release);
            assert_eq!(
                task.await.unwrap().unwrap_err().kind,
                MossWorkerErrorKind::Cancelled
            );
            assert_reaped(pid);
            assert!(!path.exists());
        }
        assert!(
            live_other.path().exists(),
            "a different live lease must survive cleanup"
        );
    }
}

#[tokio::test]
async fn sandbox_arguments_environment_and_exact_raw_stdout_are_bound() {
    let _marker = ParentMarker::install();
    let fixture = Fixture::new();
    let request = moss_request();
    let expected = moss_output(&request);
    let executable = fixture.script("moss", &response_script(&expected));
    let worker = fixture.worker(&executable, Duration::from_secs(3));
    let command = worker.native.command(&executable).unwrap();
    assert_eq!(command.as_std().get_program(), "/usr/bin/sandbox-exec");
    let arguments: Vec<_> = command
        .as_std()
        .get_args()
        .map(|arg| arg.to_os_string())
        .collect();
    assert_eq!(
        arguments,
        vec![
            std::ffi::OsString::from("-p"),
            native::SANDBOX_PROFILE.into(),
            executable.as_os_str().to_owned()
        ]
    );
    let environment: Vec<_> = command.as_std().get_envs().collect();
    assert_eq!(environment.len(), 3);
    let parent_pid = std::process::id().to_string();
    assert!(environment
        .iter()
        .any(|(key, value)| *key == "ECHOWALL_WORKER_PARENT_PID"
            && *value == Some(std::ffi::OsStr::new(&parent_pid))));
    assert!(environment
        .iter()
        .any(|(key, value)| *key == "ECHOWALL_APP_DATA_ROOT"
            && *value == Some(fixture.root.as_os_str())));
    assert!(environment
        .iter()
        .any(|(key, value)| *key == "OS_ACTIVITY_MODE"
            && *value == Some(std::ffi::OsStr::new("disable"))));
    assert_eq!(
        worker.transcribe(&request, cancelled_flag()).await.unwrap(),
        expected
    );
    assert_eq!(
        fs::read(fixture.root.join("received.json")).unwrap(),
        moss::encode_request(&request).unwrap()
    );
}

#[tokio::test]
async fn diarization_and_summary_use_same_sandbox_and_preserve_raw_responses() {
    let fixture = Fixture::new();
    let diarization = LocalDiarizationRequest {
        schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
        recording_id: Uuid::from_u128(42),
        pack_id: "speakerkit-v1".into(),
        quality_preset: LOCAL_DIARIZATION_SPEAKERKIT_PRESET.into(),
        model_files: vec![LocalModelFileIdentity {
            relative_path: "model.bin".into(),
            sha256: "a".repeat(64),
            size_bytes: 1,
        }],
        audio_relative_path: "derived/fake.wav".into(),
        audio_sha256: "b".repeat(64),
        audio_size_bytes: 44,
        audio_duration_ms: 1000,
        expected_speaker_count: None,
    };
    let response = LocalDiarizationResponse {
        schema_version: LOCAL_DIARIZATION_PROTOCOL_VERSION,
        recording_id: diarization.recording_id,
        pack_id: diarization.pack_id.clone(),
        quality_preset: diarization.quality_preset.clone(),
        audio_sha256: diarization.audio_sha256.clone(),
        speaker_count: 1,
        segments: vec![LocalDiarizationSegment {
            start_ms: 0,
            end_ms: 1000,
            speaker_slot: 1,
            confidence_milli: 0,
        }],
    };
    let expected = format!(" {}\n", serde_json::to_string(&response).unwrap()).into_bytes();
    let executable = fixture.script("diarization", &response_script(&expected));
    let worker = fixture.worker(&executable, Duration::from_secs(3));
    assert_eq!(
        worker
            .diarize(&diarization, cancelled_flag())
            .await
            .unwrap(),
        expected
    );
    let request = summary_request();
    let response = LocalSummaryResponse {
        schema_version: summary::LOCAL_SUMMARY_PROTOCOL_VERSION,
        recording_id: request.recording_id,
        model_id: request.model_id.clone(),
        model_sha256: request.model_sha256.clone(),
        prompt_version: request.prompt_version.clone(),
        transcript_sha256: request.transcript_sha256.clone(),
        summary: LocalSummaryDocument {
            title: "Synthetic".into(),
            category: "其他".into(),
            summary_en: "".into(),
            summary_zh: "".into(),
            key_points_en: vec![],
            key_points_zh: vec![],
            action_items: vec![],
        },
    };
    let expected = format!("\n{} \n", serde_json::to_string(&response).unwrap()).into_bytes();
    let executable = fixture.script("summary", &response_script(&expected));
    let worker = fixture.worker(&executable, Duration::from_secs(3));
    assert_eq!(
        worker.summarize(&request, cancelled_flag()).await.unwrap(),
        expected
    );
}

#[tokio::test]
async fn stderr_and_unverified_stdout_never_escape_closed_errors() {
    let fixture = Fixture::new();
    for (name, body, expected) in [
        ("bad-output", "IFS= read -r line\nprintf '%s' 'PRIVATE_STDOUT_SENTINEL'", MossWorkerErrorKind::Verification),
        ("bad-status", "IFS= read -r line\nprintf '%s' 'PRIVATE_STDERR_SENTINEL /private/fake-path' >&2\nexit 1", MossWorkerErrorKind::Temporary),
    ] {
        let executable = fixture.script(name, body);
        let failure = fixture.worker(&executable, Duration::from_secs(3)).transcribe(&moss_request(), cancelled_flag()).await.unwrap_err();
        assert_eq!(failure.kind, expected);
        assert!(!format!("{failure:?} {failure}").contains("PRIVATE_"));
    }
}

#[tokio::test]
async fn rejected_joint_timing_is_verification_failure_without_exposing_diagnostics() {
    let fixture = Fixture::new();
    for (index, code) in [
        "invalid_timing",
        "invalid_segment",
        "ambiguous_unknown_timing",
    ]
    .iter()
    .enumerate()
    {
        let body = format!(
            "IFS= read -r line\nprintf '%s\\n' 'PRIVATE_DIAGNOSTIC_SENTINEL' 'echowall_moss_worker_error:{code}' >&2\nexit 1"
        );
        let executable = fixture.script(&format!("invalid-joint-{index}"), &body);
        let error = fixture
            .worker(&executable, Duration::from_secs(3))
            .transcribe(&moss_request(), cancelled_flag())
            .await
            .unwrap_err();
        assert_eq!(error.kind, MossWorkerErrorKind::Verification);
        assert!(!format!("{error:?} {error}").contains("PRIVATE_"));
    }
}

#[tokio::test]
async fn overflowing_either_reader_kills_and_reaps_without_waiting_for_exit() {
    for stderr in [false, true] {
        let fixture = Fixture::new();
        let body = format!("printf '%s' \"$$\" > \"$ECHOWALL_APP_DATA_ROOT/pid\"\nwhile :; do printf '%s' {} {}; done",
            shell_literal(&"x".repeat(8192)), if stderr { ">&2" } else { "" });
        let executable = fixture.script("overflow", &body);
        let worker = fixture.worker(&executable, Duration::from_secs(5));
        let started = Instant::now();
        let result = if stderr {
            worker.transcribe(&moss_request(), cancelled_flag()).await
        } else {
            worker.summarize(&summary_request(), cancelled_flag()).await
        };
        assert_eq!(result.unwrap_err().kind, MossWorkerErrorKind::Verification);
        assert!(started.elapsed() < Duration::from_secs(2));
        assert_reaped(fixture.pid().await);
    }
}

#[tokio::test]
async fn blocked_stdin_is_inside_the_same_deadline_and_child_is_reaped() {
    let fixture = Fixture::new();
    let executable = fixture.spinning();
    let worker = fixture.worker(&executable, Duration::from_millis(150));
    let mut request = summary_request();
    request.transcript = "x".repeat(summary::MAX_TRANSCRIPT_BYTES);
    request.transcript_sha256 = sha(request.transcript.as_bytes());
    let started = Instant::now();
    assert_eq!(
        worker
            .summarize(&request, cancelled_flag())
            .await
            .unwrap_err()
            .kind,
        MossWorkerErrorKind::Temporary
    );
    assert!(started.elapsed() < Duration::from_secs(2));
    assert_reaped(fixture.pid().await);
}

#[tokio::test]
async fn explicit_cancel_and_aborted_future_kill_and_reap() {
    for abort in [false, true] {
        let fixture = Fixture::new();
        let executable = fixture.spinning();
        let worker = fixture.worker(&executable, Duration::from_secs(5));
        let cancel = cancelled_flag();
        let child_cancel = cancel.clone();
        let task =
            tokio::spawn(async move { worker.transcribe(&moss_request(), child_cancel).await });
        let pid = fixture.pid().await;
        if abort {
            task.abort();
            assert!(task.await.unwrap_err().is_cancelled());
            await_reaped(pid).await;
        } else {
            cancel.store(true, Ordering::Release);
            assert_eq!(
                task.await.unwrap().unwrap_err().kind,
                MossWorkerErrorKind::Cancelled
            );
            assert_reaped(pid);
        }
    }
}

#[tokio::test]
async fn invalid_paths_and_precancel_never_dispatch() {
    let fixture = Fixture::new();
    let executable = fixture.spinning();
    let worker = fixture.worker(&executable, Duration::from_secs(2));
    let cancelled = Arc::new(AtomicBool::new(true));
    assert_eq!(
        worker
            .transcribe(&moss_request(), cancelled)
            .await
            .unwrap_err()
            .kind,
        MossWorkerErrorKind::Cancelled
    );
    assert!(!fixture.root.join("pid").exists());
    let link = fixture.root.join("linked-worker");
    symlink(&executable, &link).unwrap();
    for rejected in [link, fixture.root.clone(), fixture.root.join("missing")] {
        assert_eq!(
            fixture
                .worker(&rejected, Duration::from_secs(2))
                .transcribe(&moss_request(), cancelled_flag())
                .await
                .unwrap_err()
                .kind,
            MossWorkerErrorKind::Unavailable
        );
    }
    let root_link = fixture.root.join("linked-root");
    symlink(&fixture.root, &root_link).unwrap();
    let mut worker = fixture.worker(&executable, Duration::from_secs(2));
    worker.native.root = root_link;
    assert_eq!(
        worker
            .transcribe(&moss_request(), cancelled_flag())
            .await
            .unwrap_err()
            .kind,
        MossWorkerErrorKind::Unavailable
    );
    assert!(!fixture.root.join("pid").exists());
}

#[test]
fn deadline_caps_are_explicit_and_protocol_requests_remain_unmodified() {
    assert_eq!(native::MOSS_DEADLINE, Duration::from_secs(1800));
    assert_eq!(native::diarization_deadline(1000), Duration::from_secs(302));
    assert_eq!(
        native::diarization_deadline(720_000),
        Duration::from_secs(1740)
    );
    assert_eq!(
        native::diarization_deadline(18_000_000 - 1),
        Duration::from_secs(7200)
    );
    let original = moss_request();
    let snapshot = json!(original);
    moss::encode_request(&original).unwrap();
    assert_eq!(json!(original), snapshot);
}
