//! Fixed, offline one-shot worker supervision for the MOSS App path.
//! Dispatch owns stdin, both drains, and wait under one absolute deadline.
//! No drain task is detached. Cancellation confirms reap before returning;
//! dropped futures synchronously kill and arrange a bounded reap fallback.

use std::{
    path::Path,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};

use super::local_whisper::LocalDiarizationRequest;
use echowall_local_moss_protocol as moss;
use echowall_local_summary_protocol::LocalSummaryRequest;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MossWorkerErrorKind {
    Unavailable,
    Cancelled,
    Temporary,
    Verification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MossWorkerError {
    pub kind: MossWorkerErrorKind,
}

impl std::fmt::Display for MossWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self.kind {
            MossWorkerErrorKind::Unavailable => "local worker is unavailable",
            MossWorkerErrorKind::Cancelled => "local worker was cancelled",
            MossWorkerErrorKind::Temporary => "local worker operation failed",
            MossWorkerErrorKind::Verification => "local worker response failed verification",
        })
    }
}
impl std::error::Error for MossWorkerError {}

fn failure(kind: MossWorkerErrorKind) -> MossWorkerError {
    MossWorkerError { kind }
}

#[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
#[path = "moss_worker/tests.rs"]
mod tests;

// The dependency-free ownership helper is shared with the worker without
// linking its native inference runtime. Writer APIs are worker/test-only here.
#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
#[allow(dead_code)]
#[path = "../../../moss-worker/src/scratch.rs"]
mod scratch;

pub struct MossWorker {
    enabled: bool,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    native: native::Configuration,
}

impl MossWorker {
    pub fn bundled(
        app_data_root: impl AsRef<Path>,
        enabled: bool,
    ) -> Result<Self, MossWorkerError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            Ok(Self {
                enabled,
                native: native::Configuration::bundled(app_data_root.as_ref(), enabled)?,
            })
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = (app_data_root, enabled);
            Err(failure(MossWorkerErrorKind::Unavailable))
        }
    }

    pub async fn transcribe(
        &self,
        request: &moss::MossRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, MossWorkerError> {
        self.ready(&cancel)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let input = moss::encode_request(request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            let output = self
                .native
                .execute(
                    &self.native.moss,
                    &input,
                    moss::MAX_RESPONSE_BYTES,
                    native::MOSS_DEADLINE,
                    &cancel,
                )
                .await?;
            moss::decode_response(&output, request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            if cancel.load(Ordering::Acquire) {
                return Err(failure(MossWorkerErrorKind::Cancelled));
            }
            Ok(output)
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(failure(MossWorkerErrorKind::Unavailable))
        }
    }

    pub async fn diarize(
        &self,
        request: &LocalDiarizationRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, MossWorkerError> {
        self.ready(&cancel)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let input = super::local_whisper::encode_diarization_request(request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            let output = self
                .native
                .execute(
                    &self.native.diarization,
                    &input,
                    super::local_whisper::MAX_LOCAL_DIARIZATION_RESPONSE_BYTES,
                    native::diarization_deadline(request.audio_duration_ms),
                    &cancel,
                )
                .await?;
            super::local_whisper::decode_diarization_response(&output, request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            if cancel.load(Ordering::Acquire) {
                return Err(failure(MossWorkerErrorKind::Cancelled));
            }
            Ok(output)
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(failure(MossWorkerErrorKind::Unavailable))
        }
    }

    pub async fn summarize(
        &self,
        request: &LocalSummaryRequest,
        cancel: Arc<AtomicBool>,
    ) -> Result<Vec<u8>, MossWorkerError> {
        self.ready(&cancel)?;
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let input = echowall_local_summary_protocol::encode_request(request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            let output = self
                .native
                .execute(
                    &self.native.summary,
                    &input,
                    echowall_local_summary_protocol::MAX_LOCAL_SUMMARY_RESPONSE_BYTES,
                    native::SUMMARY_DEADLINE,
                    &cancel,
                )
                .await?;
            echowall_local_summary_protocol::decode_response(&output, request)
                .map_err(|_| failure(MossWorkerErrorKind::Verification))?;
            if cancel.load(Ordering::Acquire) {
                return Err(failure(MossWorkerErrorKind::Cancelled));
            }
            Ok(output)
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(failure(MossWorkerErrorKind::Unavailable))
        }
    }

    fn ready(&self, cancel: &AtomicBool) -> Result<(), MossWorkerError> {
        if cancel.load(Ordering::Acquire) {
            return Err(failure(MossWorkerErrorKind::Cancelled));
        }
        if !self.enabled {
            return Err(failure(MossWorkerErrorKind::Unavailable));
        }
        Ok(())
    }

    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(crate) fn from_paths_for_test(
        root: std::path::PathBuf,
        moss: std::path::PathBuf,
        diarization: std::path::PathBuf,
        summary: std::path::PathBuf,
        deadline: std::time::Duration,
    ) -> Self {
        Self {
            enabled: true,
            native: native::Configuration {
                root,
                moss,
                diarization,
                summary,
                test_sandbox: None,
                test_deadline: Some(deadline),
            },
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod native {
    use super::{failure, MossWorkerError, MossWorkerErrorKind};
    use std::{
        fs,
        path::{Component, Path, PathBuf},
        process::Stdio,
        sync::{
            atomic::{AtomicBool, Ordering},
            Arc, Mutex,
        },
        time::Duration,
    };
    use tokio::{
        io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
        process::{Child, Command},
        time::Instant,
    };

    pub(super) const MOSS_DEADLINE: Duration = Duration::from_secs(30 * 60);
    pub(super) const SUMMARY_DEADLINE: Duration = Duration::from_secs(2 * 60 * 60);
    const STDERR_LIMIT: usize = 64 * 1024;
    const CANCEL_POLL: Duration = Duration::from_millis(50);
    const DROP_REAP_LIMIT: Duration = Duration::from_secs(5);
    const SANDBOX: &str = "/usr/bin/sandbox-exec";
    pub(super) const SANDBOX_PROFILE: &str = "(version 1) (allow default) (deny network*)";

    pub(super) struct Configuration {
        pub root: PathBuf,
        pub moss: PathBuf,
        pub diarization: PathBuf,
        pub summary: PathBuf,
        #[cfg(test)]
        pub test_sandbox: Option<PathBuf>,
        #[cfg(test)]
        pub test_deadline: Option<Duration>,
    }

    impl Configuration {
        pub fn bundled(root: &Path, enabled: bool) -> Result<Self, MossWorkerError> {
            verify_path(root, true)?;
            let root = fs::canonicalize(root).map_err(|_| unavailable())?;
            let current = std::env::current_exe()
                .and_then(fs::canonicalize)
                .map_err(|_| unavailable())?;
            let directory = current.parent().ok_or_else(unavailable)?;
            let result = Self {
                root,
                moss: directory.join("echowall-moss-worker"),
                diarization: directory.join("echowall-diarization-worker"),
                summary: directory.join("echowall-summary-worker"),
                #[cfg(test)]
                test_sandbox: None,
                #[cfg(test)]
                test_deadline: None,
            };
            if enabled {
                for executable in [
                    result.moss.as_path(),
                    result.diarization.as_path(),
                    result.summary.as_path(),
                    Path::new(SANDBOX),
                ] {
                    verify_path(executable, false)?;
                }
            }
            Ok(result)
        }

        fn sandbox(&self) -> &Path {
            #[cfg(test)]
            if let Some(path) = &self.test_sandbox {
                return path;
            }
            Path::new(SANDBOX)
        }

        pub(super) fn command(&self, executable: &Path) -> Result<Command, MossWorkerError> {
            verify_path(&self.root, true)?;
            verify_path(executable, false)?;
            verify_path(self.sandbox(), false)?;
            let mut command = Command::new(self.sandbox());
            command
                .args(["-p", SANDBOX_PROFILE])
                .arg(executable)
                .env_clear()
                .env("ECHOWALL_APP_DATA_ROOT", &self.root)
                .env("OS_ACTIVITY_MODE", "disable")
                .env("ECHOWALL_WORKER_PARENT_PID", std::process::id().to_string())
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            Ok(command)
        }

        pub async fn execute(
            &self,
            executable: &Path,
            input: &[u8],
            stdout_limit: usize,
            duration: Duration,
            cancel: &AtomicBool,
        ) -> Result<Vec<u8>, MossWorkerError> {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let mut command = self.command(executable)?;
            let scratch_root = (executable == self.moss).then(|| self.root.clone());
            if let Some(root) = &scratch_root {
                // Best effort: malformed foreign state stays untouched. The
                // worker's allocation also validates/reclaims under its lease
                // and fails closed if no valid slot can be acquired.
                let _ = super::scratch::reclaim_orphans(root);
            }
            #[cfg(test)]
            let duration = self.test_deadline.unwrap_or(duration);
            let deadline = Instant::now() + duration;
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let mut child = OwnedChild {
                child: Some(command.spawn().map_err(|_| unavailable())?),
                reaped: false,
                scratch_root,
            };
            let mut stdin = child.child_mut().stdin.take().ok_or_else(unavailable)?;
            let stdout = child.child_mut().stdout.take().ok_or_else(unavailable)?;
            let stderr = child.child_mut().stderr.take().ok_or_else(unavailable)?;
            let write_input = async move {
                stdin.write_all(input).await.map_err(|_| temporary())?;
                stdin.shutdown().await.map_err(|_| temporary())?;
                drop(stdin);
                Ok::<(), MossWorkerError>(())
            };
            let read_stdout = read_bounded(stdout, stdout_limit);
            let read_stderr = read_bounded(stderr, STDERR_LIMIT);
            tokio::pin!(write_input, read_stdout, read_stderr);
            let mut poll = tokio::time::interval(CANCEL_POLL);
            poll.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            let mut wrote = false;
            let (mut output, mut errors, mut status) = (None, None, None);
            loop {
                // Every future stays in this scope. Overflow is observed while
                // the process is still running, not after waiting for its exit.
                let event_error = tokio::select! {
                    biased;
                    _ = poll.tick() => {
                        if cancel.load(Ordering::Acquire) { Some(cancelled()) } else { None }
                    },
                    _ = tokio::time::sleep_until(deadline) => Some(temporary()),
                    result = &mut write_input, if !wrote => {
                        match result { Ok(()) => { wrote = true; None }, Err(error) => Some(error) }
                    },
                    result = &mut read_stdout, if output.is_none() => {
                        match result { Ok(bytes) => { output = Some(bytes); None }, Err(error) => Some(error) }
                    },
                    result = &mut read_stderr, if errors.is_none() => {
                        match result { Ok(bytes) => { errors = Some(bytes); None }, Err(error) => Some(error) }
                    },
                    result = child.child_mut().wait(), if status.is_none() => {
                        match result { Ok(exit) => { child.reaped = true; status = Some(exit); None }, Err(_) => Some(temporary()) }
                    },
                };
                if let Some(error) = event_error {
                    child.kill_and_reap().await?;
                    return Err(error);
                }
                if wrote && output.is_some() && errors.is_some() && status.is_some() {
                    break;
                }
            }
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            if !status.ok_or_else(temporary)?.success() {
                return Err(classify_failure(&errors.ok_or_else(temporary)?));
            }
            output.ok_or_else(temporary)
        }
    }

    /// Operational cap, not a promise that arbitrary five-hour inputs finish:
    /// permit2x audio+5min up to2h, matching the existing bounded worker budget.
    pub(super) fn diarization_deadline(duration_ms: u64) -> Duration {
        Duration::from_millis(duration_ms.saturating_mul(2))
            .saturating_add(Duration::from_secs(5 * 60))
            .min(SUMMARY_DEADLINE)
    }

    fn verify_path(path: &Path, directory: bool) -> Result<(), MossWorkerError> {
        if !path.is_absolute()
            || path.as_os_str().len() > 4096
            || path
                .components()
                .any(|component| matches!(component, Component::CurDir | Component::ParentDir))
        {
            return Err(unavailable());
        }
        let mut current = PathBuf::new();
        for component in path.components() {
            current.push(component);
            if fs::symlink_metadata(&current)
                .map_err(|_| unavailable())?
                .file_type()
                .is_symlink()
            {
                return Err(unavailable());
            }
        }
        let metadata = fs::symlink_metadata(path).map_err(|_| unavailable())?;
        if directory && !metadata.is_dir() || !directory && !metadata.is_file() {
            return Err(unavailable());
        }
        Ok(())
    }

    async fn read_bounded(
        mut reader: impl AsyncRead + Unpin,
        limit: usize,
    ) -> Result<Vec<u8>, MossWorkerError> {
        let mut output = Vec::new();
        let mut chunk = [0_u8; 8192];
        loop {
            let count = reader.read(&mut chunk).await.map_err(|_| temporary())?;
            if count == 0 {
                return Ok(output);
            }
            if count > limit.saturating_sub(output.len()) {
                return Err(verification());
            }
            output.extend_from_slice(&chunk[..count]);
        }
    }

    struct OwnedChild {
        child: Option<Child>,
        reaped: bool,
        scratch_root: Option<PathBuf>,
    }
    impl OwnedChild {
        fn child_mut(&mut self) -> &mut Child {
            self.child.as_mut().expect("owned child until scope exit")
        }

        async fn kill_and_reap(&mut self) -> Result<(), MossWorkerError> {
            if self.reaped {
                return Ok(());
            }
            // Await confirmed exit before reporting cancellation/timeout. If
            // wait itself fails, Drop retains the kill-and-reap fallback.
            let _ = self.child_mut().start_kill();
            self.child_mut().wait().await.map_err(|_| temporary())?;
            self.reaped = true;
            Ok(())
        }
    }

    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if self.reaped {
                if let Some(root) = self.scratch_root.take() {
                    let _ = super::scratch::reclaim_orphans(&root);
                }
                return;
            }
            let Some(mut child) = self.child.take() else {
                return;
            };
            let _ = child.start_kill();
            // This is only a bounded reap task; no input/output drain survives
            // the aborted future. It also works during Tokio runtime shutdown.
            let slot = Arc::new(Mutex::new(Some((child, self.scratch_root.take()))));
            let worker_slot = slot.clone();
            let spawned = std::thread::Builder::new()
                .name("moss-child-reap".into())
                .spawn(move || {
                    if let Some((child, scratch_root)) = worker_slot
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .take()
                    {
                        reap_bounded(child, scratch_root);
                    }
                });
            if spawned.is_err() {
                // Thread-resource exhaustion must not silently drop ownership.
                if let Some((child, scratch_root)) = slot
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .take()
                {
                    reap_bounded(child, scratch_root);
                }
            }
        }
    }

    fn reap_bounded(mut child: Child, scratch_root: Option<PathBuf>) {
        let deadline = std::time::Instant::now() + DROP_REAP_LIMIT;
        loop {
            match child.try_wait() {
                Ok(Some(_)) => {
                    if let Some(root) = &scratch_root {
                        let _ = super::scratch::reclaim_orphans(root);
                    }
                    return;
                }
                Err(_) => break,
                Ok(None) => {}
            }
            if std::time::Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let _ = child.start_kill();
        if matches!(child.try_wait(), Ok(Some(_))) {
            if let Some(root) = &scratch_root {
                let _ = super::scratch::reclaim_orphans(root);
            }
        }
        // Tokio's kill-on-drop/orphan reaper remains a last OS-error fallback.
    }

    fn classify_failure(stderr: &[u8]) -> MossWorkerError {
        let code = stderr
            .split(|byte| *byte == b'\n')
            .rev()
            .find(|line| !line.is_empty())
            .and_then(|line| {
                [
                    b"echowall_moss_worker_error:".as_slice(),
                    b"echowall_diarization_worker_error:".as_slice(),
                    b"echowall_summary_worker_error:".as_slice(),
                ]
                .iter()
                .find_map(|prefix| line.strip_prefix(*prefix))
            });
        match code {
            Some(b"model_unavailable" | b"root_missing" | b"invalid_root") => unavailable(),
            Some(
                b"identity_mismatch"
                | b"identity_changed"
                | b"invalid_request"
                | b"invalid_response"
                | b"invalid_timing"
                | b"invalid_segment"
                | b"ambiguous_unknown_timing"
                | b"invalid_output"
                | b"response_too_large"
                | b"raw_output_rejected"
                | b"raw_timing_incomplete",
            ) => verification(),
            _ => temporary(),
        }
    }
    fn unavailable() -> MossWorkerError {
        failure(MossWorkerErrorKind::Unavailable)
    }
    fn cancelled() -> MossWorkerError {
        failure(MossWorkerErrorKind::Cancelled)
    }
    fn temporary() -> MossWorkerError {
        failure(MossWorkerErrorKind::Temporary)
    }
    fn verification() -> MossWorkerError {
        failure(MossWorkerErrorKind::Verification)
    }
}
