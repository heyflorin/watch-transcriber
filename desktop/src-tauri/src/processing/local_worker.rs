//! Fixed-path launcher for App-bundled, one-shot local inference workers.
//!
//! The launcher never invokes a shell and clears the child environment before
//! passing the single App-data root required by the worker. Queue ownership,
//! retry policy, credentials, and result persistence stay in the main Rust
//! processing engine.

use super::local_whisper::{
    LocalDiarizationRequest, LocalDiarizationResponse, LocalWhisperRequest, LocalWhisperResponse,
};
use echowall_local_qwen_protocol::{LocalQwenRequest, LocalQwenResponse};
use echowall_local_summary_protocol::{LocalSummaryRequest, LocalSummaryResponse};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LocalWhisperWorkerErrorKind {
    Unavailable,
    Temporary,
    Verification,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LocalWhisperWorkerError {
    pub kind: LocalWhisperWorkerErrorKind,
}

impl LocalWhisperWorkerError {
    const fn new(kind: LocalWhisperWorkerErrorKind) -> Self {
        Self { kind }
    }
}

impl std::fmt::Display for LocalWhisperWorkerError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("local Whisper worker is unavailable")
    }
}

impl std::error::Error for LocalWhisperWorkerError {}

pub struct LocalWhisperWorker {
    enabled: bool,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    app_data_root: std::path::PathBuf,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    executable: std::path::PathBuf,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    diarization_executable: std::path::PathBuf,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    summary_executable: std::path::PathBuf,
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    qwen_executable: std::path::PathBuf,
}

impl LocalWhisperWorker {
    pub fn bundled(
        app_data_root: impl AsRef<std::path::Path>,
        enabled: bool,
    ) -> Result<Self, LocalWhisperWorkerError> {
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            let metadata = std::fs::symlink_metadata(app_data_root.as_ref()).map_err(|_| {
                LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
            })?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(LocalWhisperWorkerError::new(
                    LocalWhisperWorkerErrorKind::Unavailable,
                ));
            }
            let app_data_root = std::fs::canonicalize(app_data_root.as_ref()).map_err(|_| {
                LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
            })?;
            let current_executable = std::env::current_exe().map_err(|_| {
                LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
            })?;
            let executable = current_executable
                .parent()
                .ok_or_else(|| {
                    LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
                })?
                .join("echowall-whisper-worker");
            let diarization_executable = current_executable
                .parent()
                .ok_or_else(|| {
                    LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
                })?
                .join("echowall-diarization-worker");
            let summary_executable = current_executable
                .parent()
                .ok_or_else(|| {
                    LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
                })?
                .join("echowall-summary-worker");
            let qwen_executable = current_executable
                .parent()
                .ok_or_else(|| {
                    LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
                })?
                .join("echowall-qwen-worker");
            Ok(Self {
                enabled,
                app_data_root,
                executable,
                diarization_executable,
                summary_executable,
                qwen_executable,
            })
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = (app_data_root, enabled);
            Ok(Self { enabled: false })
        }
    }

    #[cfg(all(test, target_os = "macos", target_arch = "aarch64"))]
    pub(crate) fn from_paths_for_test(
        app_data_root: std::path::PathBuf,
        executable: std::path::PathBuf,
        diarization_executable: std::path::PathBuf,
        summary_executable: std::path::PathBuf,
        qwen_executable: std::path::PathBuf,
    ) -> Self {
        Self {
            enabled: true,
            app_data_root,
            executable,
            diarization_executable,
            summary_executable,
            qwen_executable,
        }
    }

    pub async fn transcribe(
        &self,
        request: &LocalWhisperRequest,
    ) -> Result<LocalWhisperResponse, LocalWhisperWorkerError> {
        if !self.enabled {
            return Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ));
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.transcribe_apple_silicon(request).await
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ))
        }
    }

    pub async fn transcribe_qwen(
        &self,
        request: &LocalQwenRequest,
    ) -> Result<LocalQwenResponse, LocalWhisperWorkerError> {
        if !self.enabled {
            return Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ));
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.transcribe_qwen_apple_silicon(request).await
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ))
        }
    }

    pub async fn diarize(
        &self,
        request: &LocalDiarizationRequest,
    ) -> Result<LocalDiarizationResponse, LocalWhisperWorkerError> {
        if !self.enabled {
            return Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ));
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.diarize_apple_silicon(request).await
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ))
        }
    }

    pub async fn summarize(
        &self,
        request: &LocalSummaryRequest,
    ) -> Result<LocalSummaryResponse, LocalWhisperWorkerError> {
        if !self.enabled {
            return Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ));
        }
        #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
        {
            self.summarize_apple_silicon(request).await
        }
        #[cfg(not(all(target_os = "macos", target_arch = "aarch64")))]
        {
            let _ = request;
            Err(LocalWhisperWorkerError::new(
                LocalWhisperWorkerErrorKind::Unavailable,
            ))
        }
    }
}

#[cfg(all(target_os = "macos", target_arch = "aarch64"))]
mod apple_silicon {
    use std::process::Stdio;
    use std::time::Duration;

    use echowall_local_qwen_protocol::{
        decode_response as decode_qwen_response, encode_request as encode_qwen_request,
        LocalQwenRequest, LocalQwenResponse, MAX_LOCAL_QWEN_RESPONSE_BYTES,
    };
    use echowall_local_summary_protocol::{
        decode_response as decode_summary_response, encode_request as encode_summary_request,
        LocalSummaryRequest, LocalSummaryResponse, MAX_LOCAL_SUMMARY_RESPONSE_BYTES,
    };
    use echowall_local_whisper_protocol::{
        decode_diarization_response, decode_response, encode_diarization_request, encode_request,
        MAX_LOCAL_DIARIZATION_RESPONSE_BYTES, MAX_LOCAL_WHISPER_RESPONSE_BYTES,
    };
    use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};
    use tokio::process::Command;

    use super::{
        LocalDiarizationRequest, LocalDiarizationResponse, LocalWhisperRequest,
        LocalWhisperResponse, LocalWhisperWorker, LocalWhisperWorkerError,
        LocalWhisperWorkerErrorKind,
    };

    const APP_DATA_ROOT_ENV: &str = "ECHOWALL_APP_DATA_ROOT";
    const OS_ACTIVITY_MODE_ENV: &str = "OS_ACTIVITY_MODE";
    const WHISPER_ERROR_PREFIX: &[u8] = b"echowall_whisper_worker_error:";
    const DIARIZATION_ERROR_PREFIX: &[u8] = b"echowall_diarization_worker_error:";
    const SUMMARY_ERROR_PREFIX: &[u8] = b"echowall_summary_worker_error:";
    const QWEN_ERROR_PREFIX: &[u8] = b"echowall_qwen_worker_error:";
    const MAX_STDERR_BYTES: usize = 64 * 1024;
    const MINIMUM_TIMEOUT: Duration = Duration::from_secs(120);
    const MAXIMUM_TIMEOUT: Duration = Duration::from_secs(2 * 60 * 60);
    const MAXIMUM_QWEN_TIMEOUT: Duration = Duration::from_secs(8 * 60 * 60);

    impl LocalWhisperWorker {
        pub(super) async fn transcribe_apple_silicon(
            &self,
            request: &LocalWhisperRequest,
        ) -> Result<LocalWhisperResponse, LocalWhisperWorkerError> {
            request.validate().map_err(|_| verification())?;
            let request_bytes = encode_request(request).map_err(|_| verification())?;
            let stdout = execute_worker(
                &self.executable,
                &self.app_data_root,
                &request_bytes,
                MAX_LOCAL_WHISPER_RESPONSE_BYTES,
                timeout_for_duration(request.audio_duration_ms),
                WHISPER_ERROR_PREFIX,
            )
            .await?;
            decode_response(&stdout, request).map_err(|_| verification())
        }

        pub(super) async fn transcribe_qwen_apple_silicon(
            &self,
            request: &LocalQwenRequest,
        ) -> Result<LocalQwenResponse, LocalWhisperWorkerError> {
            request.validate().map_err(|_| verification())?;
            let request_bytes = encode_qwen_request(request).map_err(|_| verification())?;
            let stdout = execute_worker(
                &self.qwen_executable,
                &self.app_data_root,
                &request_bytes,
                MAX_LOCAL_QWEN_RESPONSE_BYTES,
                qwen_timeout_for_duration(request.audio_duration_ms),
                QWEN_ERROR_PREFIX,
            )
            .await?;
            decode_qwen_response(&stdout, request).map_err(|_| verification())
        }

        pub(super) async fn diarize_apple_silicon(
            &self,
            request: &LocalDiarizationRequest,
        ) -> Result<LocalDiarizationResponse, LocalWhisperWorkerError> {
            request.validate().map_err(|_| verification())?;
            let request_bytes = encode_diarization_request(request).map_err(|_| verification())?;
            let stdout = execute_worker(
                &self.diarization_executable,
                &self.app_data_root,
                &request_bytes,
                MAX_LOCAL_DIARIZATION_RESPONSE_BYTES,
                timeout_for_duration(request.audio_duration_ms),
                DIARIZATION_ERROR_PREFIX,
            )
            .await?;
            decode_diarization_response(&stdout, request).map_err(|_| verification())
        }

        pub(super) async fn summarize_apple_silicon(
            &self,
            request: &LocalSummaryRequest,
        ) -> Result<LocalSummaryResponse, LocalWhisperWorkerError> {
            request.validate().map_err(|_| verification())?;
            let request_bytes = encode_summary_request(request).map_err(|_| verification())?;
            let stdout = execute_worker(
                &self.summary_executable,
                &self.app_data_root,
                &request_bytes,
                MAX_LOCAL_SUMMARY_RESPONSE_BYTES,
                MAXIMUM_TIMEOUT,
                SUMMARY_ERROR_PREFIX,
            )
            .await?;
            decode_summary_response(&stdout, request).map_err(|_| verification())
        }
    }

    async fn execute_worker(
        executable: &std::path::Path,
        app_data_root: &std::path::Path,
        request_bytes: &[u8],
        maximum_stdout_bytes: usize,
        timeout: Duration,
        error_prefix: &[u8],
    ) -> Result<Vec<u8>, LocalWhisperWorkerError> {
        verify_executable(executable)?;
        let mut command = Command::new(executable);
        command
            .env_clear()
            .env(APP_DATA_ROOT_ENV, app_data_root)
            .env(OS_ACTIVITY_MODE_ENV, "disable")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(|_| unavailable())?;
        let mut stdin = child.stdin.take().ok_or_else(unavailable)?;
        let stdout = child.stdout.take().ok_or_else(unavailable)?;
        let stderr = child.stderr.take().ok_or_else(unavailable)?;
        let stdout_reader = tokio::spawn(read_bounded(stdout, maximum_stdout_bytes));
        let stderr_reader = tokio::spawn(read_bounded(stderr, MAX_STDERR_BYTES));

        if stdin.write_all(request_bytes).await.is_err() || stdin.shutdown().await.is_err() {
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(temporary());
        }
        drop(stdin);

        let status = match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => status,
            Ok(Err(_)) => return Err(temporary()),
            Err(_) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(temporary());
            }
        };
        let stdout = stdout_reader
            .await
            .map_err(|_| temporary())?
            .map_err(|_| verification())?;
        let stderr = stderr_reader
            .await
            .map_err(|_| temporary())?
            .map_err(|_| verification())?;
        if !status.success() {
            return Err(classify_worker_failure(&stderr, error_prefix));
        }
        Ok(stdout)
    }

    async fn read_bounded(reader: impl AsyncRead + Unpin, maximum: usize) -> Result<Vec<u8>, ()> {
        let maximum = u64::try_from(maximum).map_err(|_| ())?;
        let mut bytes = Vec::with_capacity(8 * 1024);
        reader
            .take(maximum.saturating_add(1))
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| ())?;
        if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > maximum {
            Err(())
        } else {
            Ok(bytes)
        }
    }

    fn verify_executable(path: &std::path::Path) -> Result<(), LocalWhisperWorkerError> {
        let metadata = std::fs::symlink_metadata(path).map_err(|_| unavailable())?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(unavailable());
        }
        Ok(())
    }

    fn timeout_for_duration(audio_duration_ms: u64) -> Duration {
        Duration::from_millis(audio_duration_ms / 2)
            .saturating_add(MINIMUM_TIMEOUT)
            .min(MAXIMUM_TIMEOUT)
    }

    fn qwen_timeout_for_duration(audio_duration_ms: u64) -> Duration {
        Duration::from_millis(audio_duration_ms.saturating_mul(2))
            .saturating_add(Duration::from_secs(5 * 60))
            .min(MAXIMUM_QWEN_TIMEOUT)
    }

    fn classify_worker_failure(stderr: &[u8], prefix: &[u8]) -> LocalWhisperWorkerError {
        let code = stderr
            .split(|byte| *byte == b'\n')
            .rev()
            .find(|line| !line.is_empty())
            .and_then(|line| line.strip_prefix(prefix));
        match code {
            Some(b"model_unavailable" | b"root_missing" | b"invalid_root") => unavailable(),
            Some(
                b"identity_mismatch"
                | b"identity_changed"
                | b"invalid_input"
                | b"invalid_request"
                | b"invalid_limit"
                | b"input_too_large"
                | b"invalid_model_pack"
                | b"invalid_output"
                | b"invalid_response"
                | b"response_too_large"
                | b"segment_overlap"
                | b"segment_timestamp_reversed"
                | b"speaker_count_mismatch"
                | b"model_load_failed"
                | b"tokenization_failed",
            ) => verification(),
            _ => temporary(),
        }
    }

    const fn unavailable() -> LocalWhisperWorkerError {
        LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Unavailable)
    }

    const fn temporary() -> LocalWhisperWorkerError {
        LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Temporary)
    }

    const fn verification() -> LocalWhisperWorkerError {
        LocalWhisperWorkerError::new(LocalWhisperWorkerErrorKind::Verification)
    }

    #[cfg(test)]
    mod tests {
        use std::os::unix::fs::PermissionsExt;

        use tempfile::TempDir;
        use tokio::io::AsyncWriteExt;

        use super::*;

        fn request(duration_ms: u64) -> LocalWhisperRequest {
            LocalWhisperRequest {
                schema_version: 1,
                recording_id: "018f92d8-6ad4-7dc1-8e28-8b020d2942cb".parse().unwrap(),
                model_id: "tiny-q5_1".to_owned(),
                model_sha256: "a".repeat(64),
                model_size_bytes: 32_000_000,
                audio_relative_path: "derived/mixed.wav".to_owned(),
                audio_sha256: "b".repeat(64),
                audio_size_bytes: 3_200_000,
                audio_duration_ms: duration_ms,
                language: Some("en".to_owned()),
            }
        }

        #[test]
        fn timeout_is_bounded_and_scales_with_audio() {
            assert_eq!(timeout_for_duration(1_000), Duration::from_millis(120_500));
            assert_eq!(
                timeout_for_duration(5 * 60 * 60 * 1_000 - 1),
                MAXIMUM_TIMEOUT
            );
            assert_eq!(
                qwen_timeout_for_duration(12 * 60 * 1_000),
                Duration::from_secs(29 * 60)
            );
            assert_eq!(
                qwen_timeout_for_duration(5 * 60 * 60 * 1_000 - 1),
                MAXIMUM_QWEN_TIMEOUT
            );
        }

        #[tokio::test]
        async fn output_reader_fails_closed_before_unbounded_growth() {
            let (mut writer, reader) = tokio::io::duplex(256);
            let task = tokio::spawn(async move {
                writer.write_all(&[b'x'; 65]).await.unwrap();
            });
            assert!(read_bounded(reader, 64).await.is_err());
            task.await.unwrap();
        }

        #[test]
        fn worker_error_classification_accepts_only_closed_codes() {
            assert_eq!(
                classify_worker_failure(
                    b"echowall_whisper_worker_error:identity_mismatch\n",
                    WHISPER_ERROR_PREFIX,
                )
                .kind,
                LocalWhisperWorkerErrorKind::Verification
            );
            assert_eq!(
                classify_worker_failure(
                    b"safe native initialization log\nechowall_summary_worker_error:invalid_output\n",
                    SUMMARY_ERROR_PREFIX
                )
                .kind,
                LocalWhisperWorkerErrorKind::Verification
            );
            assert_eq!(
                classify_worker_failure(b"private path or transcript text\n", WHISPER_ERROR_PREFIX)
                    .kind,
                LocalWhisperWorkerErrorKind::Temporary
            );
        }

        #[tokio::test]
        async fn launcher_uses_fixed_executable_and_clears_parent_environment() {
            let root = TempDir::new().unwrap();
            let executable = root.path().join("fake-worker");
            std::fs::write(
                &executable,
                r###"#!/bin/sh
if [ "${ECHOWALL_TEST_SECRET+x}" = "x" ]; then
  printf '%s\n' 'echowall_whisper_worker_error:environment_leak' >&2
  exit 1
fi
IFS= read -r ignored
printf '%s\n' '{"schema_version":1,"recording_id":"018f92d8-6ad4-7dc1-8e28-8b020d2942cb","model_id":"tiny-q5_1","model_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","audio_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb","language":"en","segments":[{"start_ms":0,"end_ms":500,"text":"fabricated transcript"}]}'
"###,
            )
            .unwrap();
            let mut permissions = std::fs::metadata(&executable).unwrap().permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(&executable, permissions).unwrap();
            std::env::set_var("ECHOWALL_TEST_SECRET", "must-not-reach-worker");
            let worker = LocalWhisperWorker {
                enabled: true,
                app_data_root: std::fs::canonicalize(root.path()).unwrap(),
                diarization_executable: executable.clone(),
                summary_executable: executable.clone(),
                qwen_executable: executable.clone(),
                executable,
            };
            let response = worker.transcribe(&request(1_000)).await.unwrap();
            std::env::remove_var("ECHOWALL_TEST_SECRET");
            assert_eq!(response.segments[0].text, "fabricated transcript");
        }
    }
}
