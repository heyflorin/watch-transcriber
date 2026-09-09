use std::collections::BTreeMap;
#[cfg(any(test, not(any(target_os = "android", target_os = "ios"))))]
use std::fs::OpenOptions;
use std::fs::{self, File};
#[cfg(any(test, not(any(target_os = "android", target_os = "ios"))))]
use std::io::{Read, Write};
use std::io::{Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
#[cfg(not(any(target_os = "android", target_os = "ios")))]
use tauri_plugin_dialog::DialogExt;
use uuid::Uuid;

use super::envelope::{
    AudioTrack, CaptureScope, ImportReview, ImportTimestampConfidence, ImportTimestampSource,
    JobStatus, Platform, RecordingEnvelope, RecordingSource, SourceKind, TrackRole,
    RECORDING_ENVELOPE_VERSION,
};
use super::inbox::{hash_reader_streaming, FileDigest, Inbox, InboxError, InboxEvent};
use super::state::JobState;

/// Shared pre-upload byte ceiling for the embedded Rust processing engine.
pub const DEFAULT_MAX_IMPORT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_IMPORT_DURATION_MS: u64 = 5 * 60 * 60 * 1_000;
const MAX_MEDIA_PACKETS: u64 = 1_000_000;
const MAX_MEDIA_PACKET_BYTES: usize = 8 * 1024 * 1024;
const MAX_MEDIA_VALIDATION_TIME: Duration = Duration::from_secs(120);

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImportAudioFilesRequest {
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportAudioFilesResponse {
    pub results: Vec<ImportFileResult>,
}

#[derive(Clone)]
pub struct RecordingFileState {
    inbox: Arc<Inbox>,
}

impl RecordingFileState {
    pub fn new(inbox: Arc<Inbox>) -> Self {
        Self { inbox }
    }
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExportRecordingRequest {
    pub recording_id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportRecordingResponse {
    pub exported: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportFileStatus {
    Imported,
    Duplicate,
    Recovered,
    Error,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFileResult {
    pub source_path: String,
    pub imported_name: Option<String>,
    pub status: ImportFileStatus,
    pub recording_id: Option<String>,
    pub sha256: Option<String>,
    pub size_bytes: Option<u64>,
    pub codec: Option<String>,
    pub duration_ms: Option<u64>,
    pub sample_rate: Option<u32>,
    pub channels: Option<u32>,
    pub proposed_captured_at: Option<String>,
    pub error: Option<ImportFailure>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportFailure {
    pub code: &'static str,
    pub message: String,
}

#[derive(Debug)]
pub struct DesktopImporter {
    inbox: Arc<Inbox>,
    platform: Platform,
    maximum_bytes: u64,
}

impl DesktopImporter {
    pub fn new(
        inbox: Arc<Inbox>,
        platform: Platform,
        maximum_bytes: u64,
    ) -> Result<Self, ImportError> {
        if !matches!(platform, Platform::Macos | Platform::Windows) {
            return Err(ImportError::new(
                "unsupported_platform",
                "desktop import requires macOS or Windows",
            ));
        }
        if maximum_bytes == 0 {
            return Err(ImportError::new(
                "invalid_limit",
                "maximum import bytes must be greater than zero",
            ));
        }
        Ok(Self {
            inbox,
            platform,
            maximum_bytes,
        })
    }

    /// Process every path independently and preserve the input order. A bad
    /// item never prevents later picker/drop items from being imported.
    pub fn import_paths(&self, paths: Vec<String>) -> ImportAudioFilesResponse {
        let results = paths
            .into_iter()
            .map(|source_path| match self.import_one(&source_path) {
                Ok(imported) => ImportFileResult {
                    source_path,
                    imported_name: Some(imported.imported_name),
                    status: imported.status,
                    recording_id: Some(imported.recording_id.to_string()),
                    sha256: Some(imported.digest.sha256),
                    size_bytes: Some(imported.digest.size_bytes),
                    codec: Some(imported.codec),
                    duration_ms: Some(imported.duration_ms),
                    sample_rate: Some(imported.sample_rate),
                    channels: Some(imported.channels),
                    proposed_captured_at: Some(imported.proposed_captured_at),
                    error: None,
                },
                Err(error) => ImportFileResult {
                    source_path,
                    imported_name: error.imported_name,
                    status: ImportFileStatus::Error,
                    recording_id: None,
                    sha256: None,
                    size_bytes: None,
                    codec: None,
                    duration_ms: None,
                    sample_rate: None,
                    channels: None,
                    proposed_captured_at: None,
                    error: Some(ImportFailure {
                        code: error.code,
                        message: error.message,
                    }),
                },
            })
            .collect();
        ImportAudioFilesResponse { results }
    }

    fn import_one(&self, source_path: &str) -> Result<ImportedRecording, ImportError> {
        let mut source = SourceFile::open(source_path, self.maximum_bytes)?;
        if let Some(mut existing) = self.inbox.find_envelope_by_sha256(&source.digest.sha256)? {
            let relative_path = existing.normalized_audio.clone().ok_or_else(|| {
                ImportError::new(
                    "dedupe_integrity",
                    "matching inbox envelope has no normalized audio path",
                )
            })?;
            let stored_digest = self
                .inbox
                .hash_package_file(existing.recording_id, &relative_path);
            let stored_matches = matches!(
                stored_digest.as_ref(),
                Ok(digest) if digest.sha256 == source.digest.sha256
                    && digest.size_bytes == source.digest.size_bytes
            );
            if !stored_matches {
                if existing.source.kind != SourceKind::FileImport {
                    return Err(ImportError::new(
                        "dedupe_integrity",
                        "matching non-import recording has missing or mismatched local bytes",
                    ));
                }
                self.copy_and_verify(&mut source, existing.recording_id, &relative_path)?;
            }

            let recovered = matches!(
                existing.job.state,
                JobState::Importing | JobState::Finalizing
            ) || !stored_matches;
            if existing.job.state == JobState::Importing {
                self.transition(&mut existing, JobState::Finalizing)?;
            }
            if existing.job.state == JobState::Finalizing {
                self.transition(&mut existing, JobState::Ready)?;
            }
            return Ok(ImportedRecording {
                recording_id: existing.recording_id,
                imported_name: existing
                    .imported_name
                    .clone()
                    .unwrap_or_else(|| source.imported_name.clone()),
                digest: source.digest,
                codec: existing.tracks[0].codec.clone(),
                duration_ms: existing.duration_ms,
                sample_rate: existing.tracks[0].sample_rate,
                channels: existing.tracks[0].channels,
                proposed_captured_at: existing.captured_at.to_rfc3339(),
                status: if recovered {
                    ImportFileStatus::Recovered
                } else {
                    ImportFileStatus::Duplicate
                },
            });
        }

        let recording_id = Uuid::new_v4();
        let relative_path = format!("tracks/imported.{}", source.extension);
        let mut envelope = self.importing_envelope(recording_id, &relative_path, &source)?;
        self.inbox.persist_envelope(&envelope)?;
        self.append_event(
            recording_id,
            "import_started",
            source.imported_at,
            BTreeMap::from([
                ("sha256".to_owned(), json!(source.digest.sha256)),
                ("size_bytes".to_owned(), json!(source.digest.size_bytes)),
            ]),
        )?;

        self.copy_and_verify(&mut source, recording_id, &relative_path)?;
        self.append_event(
            recording_id,
            "import_copied",
            source.imported_at,
            BTreeMap::new(),
        )?;
        self.transition(&mut envelope, JobState::Finalizing)?;
        self.transition(&mut envelope, JobState::Ready)?;

        Ok(ImportedRecording {
            recording_id,
            imported_name: source.imported_name,
            digest: source.digest,
            codec: source.media.codec,
            duration_ms: source.media.duration_ms,
            sample_rate: source.media.sample_rate,
            channels: source.media.channels,
            proposed_captured_at: envelope.captured_at.to_rfc3339(),
            status: ImportFileStatus::Imported,
        })
    }

    fn importing_envelope(
        &self,
        recording_id: Uuid,
        relative_path: &str,
        source: &SourceFile,
    ) -> Result<RecordingEnvelope, ImportError> {
        let duration = TimeDelta::try_milliseconds(
            i64::try_from(source.media.duration_ms)
                .map_err(|_| ImportError::new("invalid_media", "audio duration is too large"))?,
        )
        .ok_or_else(|| ImportError::new("invalid_media", "audio duration is too large"))?;
        let ended_at = source.imported_at;
        let captured_at = ended_at - duration;
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id,
            source: RecordingSource {
                kind: SourceKind::FileImport,
                platform: self.platform,
                label: None,
                capture_scope: CaptureScope::ImportedFile,
            },
            captured_at,
            ended_at,
            duration_ms: source.media.duration_ms,
            tracks: vec![AudioTrack {
                role: TrackRole::Imported,
                relative_path: relative_path.to_owned(),
                codec: source.media.codec.clone(),
                sample_rate: source.media.sample_rate,
                channels: source.media.channels,
                duration_ms: source.media.duration_ms,
                clock_start_ns: 0,
                sha256: source.digest.sha256.clone(),
            }],
            normalized_audio: Some(relative_path.to_owned()),
            normalized_sha256: Some(source.digest.sha256.clone()),
            imported_name: Some(source.imported_name.clone()),
            import_review: Some(ImportReview {
                timestamp_source: ImportTimestampSource::FileMtime,
                timestamp_confidence: ImportTimestampConfidence::Low,
                display_title: None,
                speaker_count: None,
                confirmed_at: None,
            }),
            capture_warnings: Vec::new(),
            job: JobStatus {
                state: JobState::Importing,
                attempt: 0,
                remote_job_id: None,
                last_error: None,
            },
        };
        envelope.validate()?;
        Ok(envelope)
    }

    fn copy_and_verify(
        &self,
        source: &mut SourceFile,
        recording_id: Uuid,
        relative_path: &str,
    ) -> Result<(), ImportError> {
        let copied = self.inbox.copy_open_file_into_package(
            recording_id,
            relative_path,
            &mut source.file,
            self.maximum_bytes,
            &source.digest.sha256,
        )?;
        if copied != source.digest || source.fingerprint != SourceFingerprint::read(&source.file)? {
            return Err(ImportError::new(
                "source_changed",
                "source file changed while it was being imported",
            ));
        }
        Ok(())
    }

    fn transition(
        &self,
        envelope: &mut RecordingEnvelope,
        next: JobState,
    ) -> Result<(), ImportError> {
        let previous = envelope.job.state;
        envelope.job.state = previous.transition_to(next)?;
        self.inbox.persist_envelope(envelope)?;
        self.append_event(
            envelope.recording_id,
            "state_transition",
            Utc::now().fixed_offset(),
            BTreeMap::from([
                ("from_state".to_owned(), json!(previous.as_str())),
                ("to_state".to_owned(), json!(next.as_str())),
            ]),
        )
    }

    fn append_event(
        &self,
        recording_id: Uuid,
        kind: &str,
        occurred_at: DateTime<FixedOffset>,
        payload: BTreeMap<String, Value>,
    ) -> Result<(), ImportError> {
        let event = InboxEvent::new(recording_id, kind, occurred_at, payload)?;
        self.inbox.append_event(&event)?;
        Ok(())
    }
}

pub struct DesktopImportState {
    importer: Arc<Mutex<DesktopImporter>>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfirmImportReviewRequest {
    pub recording_id: String,
    pub captured_at: String,
    pub display_title: Option<String>,
    pub speaker_count: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfirmImportReviewResponse {
    pub recording_id: String,
    pub captured_at: String,
    pub display_title: Option<String>,
    pub speaker_count: Option<u32>,
    pub confirmed_at: String,
}

pub struct ImportReviewState {
    inbox: Arc<Inbox>,
}

impl ImportReviewState {
    pub fn new(inbox: Arc<Inbox>) -> Self {
        Self { inbox }
    }

    pub(crate) fn confirm(
        &self,
        request: ConfirmImportReviewRequest,
    ) -> Result<ConfirmImportReviewResponse, ImportError> {
        let recording_id = Uuid::parse_str(&request.recording_id)
            .map_err(|_| ImportError::new("invalid_recording_id", "recording ID is invalid"))?;
        let captured_at = DateTime::parse_from_rfc3339(&request.captured_at)
            .map_err(|_| ImportError::new("invalid_captured_at", "recording time is invalid"))?;
        let now = Utc::now().fixed_offset();
        if captured_at > now + TimeDelta::days(1)
            || captured_at < DateTime::parse_from_rfc3339("1970-01-01T00:00:00Z").unwrap()
        {
            return Err(ImportError::new(
                "invalid_captured_at",
                "recording time is outside the supported range",
            ));
        }
        let display_title = request
            .display_title
            .map(|title| title.trim().to_owned())
            .filter(|title| !title.is_empty());
        if display_title
            .as_ref()
            .is_some_and(|title| title.chars().count() > 200 || title.contains(['\n', '\r']))
        {
            return Err(ImportError::new(
                "invalid_display_title",
                "display title is invalid",
            ));
        }
        if request
            .speaker_count
            .is_some_and(|count| !(1..=50).contains(&count))
        {
            return Err(ImportError::new(
                "invalid_speaker_count",
                "speaker count is out of range",
            ));
        }

        let confirmed_at = now;
        let title_for_update = display_title.clone();
        let speaker_count = request.speaker_count;
        let envelope = self.inbox.update_envelope(recording_id, |envelope| {
            if envelope.source.kind != SourceKind::FileImport
                || envelope.job.state != JobState::Ready
            {
                return Err(InboxError::Boundary(
                    "only a ready file import can be reviewed".to_owned(),
                ));
            }
            let duration =
                TimeDelta::try_milliseconds(i64::try_from(envelope.duration_ms).map_err(|_| {
                    InboxError::Boundary("recording duration is invalid".to_owned())
                })?)
                .ok_or_else(|| InboxError::Boundary("recording duration is invalid".to_owned()))?;
            envelope.captured_at = captured_at;
            envelope.ended_at = captured_at + duration;
            envelope.import_review = Some(ImportReview {
                timestamp_source: ImportTimestampSource::User,
                timestamp_confidence: ImportTimestampConfidence::UserConfirmed,
                display_title: title_for_update,
                speaker_count,
                confirmed_at: Some(confirmed_at),
            });
            Ok(())
        })?;
        self.inbox.append_event(&InboxEvent::new(
            recording_id,
            "import_review_confirmed",
            confirmed_at,
            BTreeMap::from([
                ("captured_at".to_owned(), json!(envelope.captured_at)),
                ("speaker_count".to_owned(), json!(speaker_count)),
            ]),
        )?)?;
        Ok(ConfirmImportReviewResponse {
            recording_id: recording_id.to_string(),
            captured_at: envelope.captured_at.to_rfc3339(),
            display_title,
            speaker_count,
            confirmed_at: confirmed_at.to_rfc3339(),
        })
    }
}

impl DesktopImportState {
    pub fn new(importer: DesktopImporter) -> Self {
        Self {
            importer: Arc::new(Mutex::new(importer)),
        }
    }
}

#[tauri::command]
pub async fn import_audio_files(
    state: tauri::State<'_, DesktopImportState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: ImportAudioFilesRequest,
) -> Result<ImportAudioFilesResponse, String> {
    if !features.audio_import {
        return Err("audio import is disabled for this release".to_owned());
    }
    let importer = Arc::clone(&state.importer);
    tauri::async_runtime::spawn_blocking(move || {
        importer
            .lock()
            .map_err(|_| "desktop import state is unavailable".to_owned())
            .map(|importer| importer.import_paths(request.paths))
    })
    .await
    .map_err(|_| "desktop import worker stopped unexpectedly".to_owned())?
}

#[tauri::command]
pub async fn confirm_import_review(
    state: tauri::State<'_, ImportReviewState>,
    request: ConfirmImportReviewRequest,
) -> Result<ConfirmImportReviewResponse, String> {
    let result = state.confirm(request);
    result.map_err(|_| "import review could not be saved".to_owned())
}

#[tauri::command]
pub async fn export_recording_original(
    app: tauri::AppHandle,
    state: tauri::State<'_, RecordingFileState>,
    request: ExportRecordingRequest,
) -> Result<ExportRecordingResponse, String> {
    let recording_id =
        Uuid::parse_str(&request.recording_id).map_err(|_| "recording ID is invalid".to_owned())?;
    let envelope = state
        .inbox
        .load_envelope(recording_id)
        .map_err(|_| "recording is unavailable".to_owned())?;
    let relative = envelope
        .normalized_audio
        .as_deref()
        .ok_or_else(|| "recording audio is unavailable".to_owned())?;
    let expected_sha256 = envelope
        .normalized_sha256
        .clone()
        .ok_or_else(|| "recording audio is unavailable".to_owned())?;
    let source = state
        .inbox
        .package_file_path(recording_id, relative)
        .map_err(|_| "recording audio is unavailable".to_owned())?;
    let digest = crate::ingest::inbox::hash_file_streaming(&source)
        .map_err(|_| "recording audio failed verification".to_owned())?;
    if digest.sha256 != expected_sha256 || digest.size_bytes == 0 {
        return Err("recording audio failed verification".to_owned());
    }
    let extension = source
        .extension()
        .and_then(|value| value.to_str())
        .filter(|value| matches!(*value, "wav" | "mp3" | "m4a"))
        .unwrap_or("wav");
    let file_name = envelope
        .imported_name
        .as_deref()
        .filter(|name| Path::new(name).file_name().and_then(|value| value.to_str()) == Some(*name))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{recording_id}.{extension}"));
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        use tauri_plugin_echowall_capture::CaptureExt as _;

        let result: serde_json::Value = app
            .echowall_capture()
            .invoke(
                "exportAudio",
                serde_json::json!({
                    "sourcePath": source.to_string_lossy(),
                    "fileName": file_name,
                    "expectedSizeBytes": digest.size_bytes,
                    "expectedSha256": expected_sha256,
                }),
            )
            .await
            .map_err(|_| "native export failed".to_owned())?;
        return Ok(ExportRecordingResponse {
            exported: result
                .get("exported")
                .and_then(serde_json::Value::as_bool)
                .unwrap_or(false),
        });
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        app.dialog()
            .file()
            .set_file_name(file_name)
            .add_filter("Audio", &[extension])
            .save_file(move |path| {
                let _ = sender.send(path);
            });
        let selected = receiver
            .await
            .map_err(|_| "export dialog closed unexpectedly".to_owned())?;
        let Some(selected) = selected else {
            return Ok(ExportRecordingResponse { exported: false });
        };
        let destination = selected
            .into_path()
            .map_err(|_| "selected export destination is unsupported".to_owned())?;
        tauri::async_runtime::spawn_blocking(move || {
            export_verified_copy(&source, &destination, &expected_sha256, digest.size_bytes)
        })
        .await
        .map_err(|_| "export worker stopped unexpectedly".to_owned())??;
        Ok(ExportRecordingResponse { exported: true })
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
fn export_verified_copy(
    source: &Path,
    destination: &Path,
    expected_sha256: &str,
    expected_size_bytes: u64,
) -> Result<(), String> {
    let source = fs::canonicalize(source).map_err(|_| "recording audio is unavailable")?;
    let source_metadata =
        fs::symlink_metadata(&source).map_err(|_| "recording audio is unavailable")?;
    if source_metadata.file_type().is_symlink()
        || !source_metadata.is_file()
        || source_metadata.len() != expected_size_bytes
    {
        return Err("recording audio failed verification".to_owned());
    }
    if destination.exists()
        && fs::canonicalize(destination)
            .ok()
            .is_some_and(|path| path == source)
    {
        return Err("export destination is the source recording".to_owned());
    }
    let mut input = File::open(&source).map_err(|_| "recording audio is unavailable")?;
    let mut output = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(destination)
        .map_err(|_| "export destination is unavailable")?;
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = input
            .read(&mut buffer)
            .map_err(|_| "recording export failed")?;
        if read == 0 {
            break;
        }
        output
            .write_all(&buffer[..read])
            .map_err(|_| "recording export failed")?;
    }
    output.sync_all().map_err(|_| "recording export failed")?;
    drop(output);
    let actual = crate::ingest::inbox::hash_file_streaming(destination)
        .map_err(|_| "recording export failed verification")?;
    if actual.sha256 != expected_sha256 || actual.size_bytes != expected_size_bytes {
        let _ = fs::remove_file(destination);
        return Err("recording export failed verification".to_owned());
    }
    Ok(())
}

pub fn current_desktop_platform() -> Result<Platform, ImportError> {
    #[cfg(target_os = "macos")]
    return Ok(Platform::Macos);
    #[cfg(target_os = "windows")]
    return Ok(Platform::Windows);
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    Err(ImportError::new(
        "unsupported_platform",
        "desktop import requires macOS or Windows",
    ))
}

#[derive(Debug)]
pub struct ImportError {
    code: &'static str,
    message: String,
    imported_name: Option<String>,
}

impl ImportError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            imported_name: None,
        }
    }

    fn with_name(mut self, imported_name: &str) -> Self {
        self.imported_name = Some(imported_name.to_owned());
        self
    }
}

impl std::fmt::Display for ImportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ImportError {}

impl From<InboxError> for ImportError {
    fn from(error: InboxError) -> Self {
        let code = match &error {
            InboxError::EmptySource => "zero_byte",
            InboxError::FileTooLarge { .. } => "oversized",
            InboxError::SourceChanged => "source_changed",
            _ => "inbox_error",
        };
        Self::new(code, error.to_string())
    }
}

impl From<super::envelope::EnvelopeValidationError> for ImportError {
    fn from(error: super::envelope::EnvelopeValidationError) -> Self {
        Self::new("invalid_envelope", error.to_string())
    }
}

impl From<super::state::InvalidJobTransition> for ImportError {
    fn from(error: super::state::InvalidJobTransition) -> Self {
        Self::new("invalid_state", error.to_string())
    }
}

impl From<std::io::Error> for ImportError {
    fn from(error: std::io::Error) -> Self {
        Self::new("io_error", error.to_string())
    }
}

#[derive(Debug)]
struct ImportedRecording {
    recording_id: Uuid,
    imported_name: String,
    digest: FileDigest,
    codec: String,
    duration_ms: u64,
    sample_rate: u32,
    channels: u32,
    proposed_captured_at: String,
    status: ImportFileStatus,
}

#[derive(Debug)]
struct SourceFile {
    file: File,
    imported_name: String,
    extension: String,
    digest: FileDigest,
    fingerprint: SourceFingerprint,
    media: MediaInfo,
    imported_at: DateTime<FixedOffset>,
}

impl SourceFile {
    fn open(source_path: &str, maximum_bytes: u64) -> Result<Self, ImportError> {
        let path = validate_source_path(source_path)?;
        let imported_name = validate_imported_name(&path)?;
        let extension = supported_extension(&path)
            .ok_or_else(|| {
                ImportError::new(
                    "unsupported_extension",
                    "supported desktop imports are .m4a, .mp3, and .wav",
                )
            })?
            .to_owned();
        let metadata = fs::symlink_metadata(&path)?;
        if metadata.file_type().is_symlink() {
            return Err(ImportError::new(
                "symlink_not_allowed",
                "symbolic-link imports are not allowed",
            )
            .with_name(&imported_name));
        }
        if !metadata.is_file() {
            return Err(ImportError::new("not_a_file", "import path is not a file")
                .with_name(&imported_name));
        }
        if metadata.len() == 0 {
            return Err(
                ImportError::new("zero_byte", "audio file is empty").with_name(&imported_name)
            );
        }
        if metadata.len() > maximum_bytes {
            return Err(ImportError::new(
                "oversized",
                format!("audio file exceeds the {maximum_bytes}-byte import limit"),
            )
            .with_name(&imported_name));
        }

        let mut file = File::open(&path)?;
        let fingerprint = SourceFingerprint::read(&file)?;
        let media = inspect_media(&file, &extension).map_err(|error| {
            ImportError::new(error.code, error.message).with_name(&imported_name)
        })?;
        file.seek(SeekFrom::Start(0))?;
        let digest = hash_reader_streaming(&mut file)?;
        file.seek(SeekFrom::Start(0))?;
        if digest.size_bytes != metadata.len() || fingerprint != SourceFingerprint::read(&file)? {
            return Err(ImportError::new(
                "source_changed",
                "source file changed while it was being inspected",
            )
            .with_name(&imported_name));
        }
        let imported_at = metadata
            .modified()
            .map(DateTime::<Utc>::from)
            .unwrap_or_else(|_| Utc::now())
            .fixed_offset();
        Ok(Self {
            file,
            imported_name,
            extension,
            digest,
            fingerprint,
            media,
            imported_at,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SourceFingerprint {
    size_bytes: u64,
    modified: Option<std::time::SystemTime>,
}

impl SourceFingerprint {
    fn read(file: &File) -> Result<Self, std::io::Error> {
        let metadata = file.metadata()?;
        Ok(Self {
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

#[derive(Debug)]
pub(crate) struct MediaInfo {
    pub(crate) codec: String,
    pub(crate) sample_rate: u32,
    pub(crate) channels: u32,
    pub(crate) duration_ms: u64,
}

pub(crate) struct MediaValidationError {
    pub(crate) code: &'static str,
    pub(crate) message: &'static str,
}

impl MediaValidationError {
    fn new(code: &'static str, message: &'static str) -> Self {
        Self { code, message }
    }
}

impl From<std::io::Error> for MediaValidationError {
    fn from(_: std::io::Error) -> Self {
        Self::new("io_error", "audio file could not be inspected")
    }
}

pub(crate) fn inspect_media(
    file: &File,
    extension: &str,
) -> Result<MediaInfo, MediaValidationError> {
    inspect_media_with_cancel(file, extension, || false)
}

pub(crate) fn inspect_media_with_cancel(
    file: &File,
    extension: &str,
    mut cancelled: impl FnMut() -> bool,
) -> Result<MediaInfo, MediaValidationError> {
    if cancelled() {
        return Err(MediaValidationError::new(
            "decode_cancelled",
            "audio inspection was canceled",
        ));
    }
    let mut cloned = file.try_clone()?;
    cloned.seek(SeekFrom::Start(0))?;
    // File clones share the seek offset. Parse and rewind container edits
    // before Symphonia receives this descriptor, never after probing it.
    let container_timeline =
        echowall_local_audio::ContainerTimeline::read(&mut cloned, &format!("source.{extension}"))
            .map_err(|_| {
                MediaValidationError::new(
                    "invalid_media",
                    "audio container edits are invalid or unsupported",
                )
            })?;
    let stream = MediaSourceStream::new(Box::new(cloned), Default::default());
    let mut hint = Hint::new();
    hint.with_extension(extension);
    let mut format = symphonia::default::get_probe()
        .probe(
            &hint,
            stream,
            FormatOptions::default(),
            MetadataOptions::default(),
        )
        .map_err(|_| {
            MediaValidationError::new("invalid_media", "audio container could not be parsed")
        })?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| {
        MediaValidationError::new("invalid_media", "audio container has no audio track")
    })?;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(|| {
            MediaValidationError::new("invalid_media", "audio track has no codec parameters")
        })?;
    let sample_rate = parameters.sample_rate.ok_or_else(|| {
        MediaValidationError::new(
            "invalid_media",
            "audio track does not declare a sample rate",
        )
    })?;
    let channels = u32::try_from(
        parameters
            .channels
            .as_ref()
            .ok_or_else(|| {
                MediaValidationError::new("invalid_media", "audio track does not declare channels")
            })?
            .count(),
    )
    .map_err(|_| MediaValidationError::new("invalid_media", "audio channel count is too large"))?;
    let codec = symphonia::default::get_codecs()
        .get_audio_decoder(parameters.codec)
        .map(|decoder| decoder.codec.info.short_name.to_owned())
        .ok_or_else(|| {
            MediaValidationError::new("unsupported_codec", "audio codec is not supported")
        })?;
    let declared_duration_ms = track
        .time_base
        .zip(track.duration)
        .and_then(|(time_base, duration)| time_base.calc_duration(duration))
        .and_then(|time| u64::try_from(time.as_millis()).ok())
        .unwrap_or(0);
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        // Apply trims once through the same selector as the native decoder:
        // AAC ignores this option, but MP3 otherwise trims a second time.
        .make_audio_decoder(parameters, &AudioDecoderOptions::default().gapless(false))
        .map_err(|_| {
            MediaValidationError::new("unsupported_codec", "audio decoder is unavailable")
        })?;
    let mut timeline = container_timeline
        .for_track(track_id, sample_rate)
        .map_err(|_| {
            MediaValidationError::new(
                "invalid_media",
                "audio track edits are invalid or unsupported",
            )
        })?;
    let maximum_frames = u64::from(sample_rate)
        .checked_mul(MAX_IMPORT_DURATION_MS / 1_000)
        .ok_or_else(|| {
            MediaValidationError::new("duration_limit", "audio duration is too large")
        })?;
    let started = Instant::now();
    let mut packets = 0_u64;
    loop {
        if cancelled() {
            return Err(MediaValidationError::new(
                "decode_cancelled",
                "audio inspection was canceled",
            ));
        }
        if started.elapsed() > MAX_MEDIA_VALIDATION_TIME {
            return Err(MediaValidationError::new(
                "decode_timeout",
                "audio validation exceeded its time limit",
            ));
        }
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => {
                return Err(MediaValidationError::new(
                    "invalid_media",
                    "audio packet stream is invalid",
                ));
            }
        };
        packets = packets.saturating_add(1);
        if packets > MAX_MEDIA_PACKETS || packet.data.len() > MAX_MEDIA_PACKET_BYTES {
            return Err(MediaValidationError::new(
                "decode_limit",
                "audio contains too many packets",
            ));
        }
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|_| {
            MediaValidationError::new("invalid_media", "audio decode validation failed")
        })?;
        if decoded.spec().rate() != sample_rate
            || decoded.spec().channels().count() != channels as usize
        {
            return Err(MediaValidationError::new(
                "invalid_media",
                "audio format changes within the file",
            ));
        }
        let frames = decoded
            .samples_interleaved()
            .checked_div(channels as usize)
            .ok_or_else(|| {
                MediaValidationError::new("invalid_media", "audio channel count is invalid")
            })?;
        if !decoded
            .samples_interleaved()
            .is_multiple_of(channels as usize)
        {
            return Err(MediaValidationError::new(
                "invalid_media",
                "audio frame shape is invalid",
            ));
        }
        timeline
            .select_packet(frames, packet.trim_start.get(), packet.trim_end.get())
            .map_err(|_| {
                MediaValidationError::new("invalid_media", "audio frame timing failed validation")
            })?;
        if timeline.effective_frames() >= maximum_frames {
            return Err(MediaValidationError::new(
                "duration_limit",
                "audio duration must be shorter than five hours",
            ));
        }
    }
    if cancelled() {
        return Err(MediaValidationError::new(
            "decode_cancelled",
            "audio inspection was canceled",
        ));
    }
    let timeline = timeline.finish().map_err(|_| {
        MediaValidationError::new(
            "invalid_media",
            "audio edited timeline is incomplete or empty",
        )
    })?;
    let decoded_duration_ms = timeline
        .decoded_frames()
        .checked_mul(1_000)
        .ok_or_else(|| {
            MediaValidationError::new("duration_limit", "audio duration is too large")
        })?
        / u64::from(sample_rate);
    if declared_duration_ms > 0 {
        let tolerance_ms = 2_000_u64.max(declared_duration_ms / 100);
        if declared_duration_ms.abs_diff(decoded_duration_ms) > tolerance_ms {
            return Err(MediaValidationError::new(
                "invalid_media",
                "audio decoded duration does not match its container",
            ));
        }
    }
    let effective_duration_ms = timeline.duration_ms_ceil();
    if effective_duration_ms >= MAX_IMPORT_DURATION_MS {
        return Err(MediaValidationError::new(
            "duration_limit",
            "audio duration must be shorter than five hours",
        ));
    }
    Ok(MediaInfo {
        codec,
        sample_rate,
        channels,
        // Container packet duration can include AAC priming and trailing
        // padding. Never promote it over the fully decoded effective timeline.
        duration_ms: effective_duration_ms,
    })
}

fn validate_source_path(source_path: &str) -> Result<PathBuf, ImportError> {
    if source_path.is_empty() || source_path.chars().any(|character| character == '\0') {
        return Err(ImportError::new("unsafe_path", "import path is invalid"));
    }
    let path = PathBuf::from(source_path);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
    {
        return Err(ImportError::new(
            "unsafe_path",
            "import path must be absolute and contain no traversal components",
        ));
    }
    Ok(path)
}

fn validate_imported_name(path: &Path) -> Result<String, ImportError> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| ImportError::new("unsafe_name", "audio filename is invalid UTF-8"))?;
    let character_count = name.chars().count();
    let invalid_character = name
        .chars()
        .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':' | '\0'));
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if !(1..=1024).contains(&character_count)
        || name == "."
        || name == ".."
        || stem.is_empty()
        || name.ends_with([' ', '.'])
        || invalid_character
        || reserved
    {
        return Err(ImportError::new(
            "unsafe_name",
            "audio filename is unsafe for a cross-platform recording package",
        ));
    }
    Ok(name.to_owned())
}

fn supported_extension(path: &Path) -> Option<&'static str> {
    let extension = path.extension()?.to_str()?;
    if extension.eq_ignore_ascii_case("m4a") {
        Some("m4a")
    } else if extension.eq_ignore_ascii_case("mp3") {
        Some("mp3")
    } else if extension.eq_ignore_ascii_case("wav") {
        Some("wav")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    const SILENT_MP3_HEX: &str = concat!(
        "ffe338c40000000348000000004c414d45342e30555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "555555554c414d45342e30555555555555555555555555555555555555555555",
        "555555555555555555555555555555555555555555555555ffe338c434000003",
        "4800000000555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "555555555555555555555555555555555555555555555555555555554c414d45",
        "342e305555555555555555555555555555555555555555555555555555555555",
        "55555555555555555555555555555555ffe338c4340000034800000000555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555555555555555555555555555555555555555555555555555",
        "5555555555555555",
    );

    #[test]
    #[ignore = "read-only public mixed10 file timing parity; no device, inference, or artifact writes"]
    fn public_mixed10_effective_duration_matches_native_pcm() -> Result<(), &'static str> {
        if std::env::var("ECHOWALL_IMPORT_TIMELINE_REPLAY_CONFIRM").as_deref()
            != Ok("public-media-timeline-replay-authorized")
        {
            return Err("confirmation_required");
        }
        let repository = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .ok_or("repository_missing")?;
        let mut results = Vec::new();
        for (relative, extension) in [
            ("local-eval/matrix/audio/mixed_10.m4a", "m4a"),
            ("local-eval/matrix/audio-moss/mixed_10.wav", "wav"),
        ] {
            let path = repository.join(relative);
            let metadata = fs::symlink_metadata(&path).map_err(|_| "fixture_missing")?;
            if !metadata.is_file()
                || metadata.file_type().is_symlink()
                || metadata.len() > DEFAULT_MAX_IMPORT_BYTES
            {
                return Err("fixture_rejected");
            }
            let before = crate::ingest::inbox::hash_file_streaming(&path)
                .map_err(|_| "fixture_hash_failed")?;
            let mut file = File::open(&path).map_err(|_| "fixture_missing")?;
            let media = inspect_media(&file, extension).map_err(|failure| failure.code)?;
            file.rewind().map_err(|_| "fixture_seek_failed")?;
            let samples = echowall_local_audio::decode_source(file, relative, 720_261)?;
            let after = crate::ingest::inbox::hash_file_streaming(&path)
                .map_err(|_| "fixture_hash_failed")?;
            if before != after || samples.len() != 11_524_167 {
                return Err("fixture_identity_or_pcm_mismatch");
            }
            results.push((extension, media.duration_ms, samples.len()));
        }
        println!(
            "{}",
            json!({"scope":"public_file_timing_only","results":results,"source_bytes_unchanged":true,"inference_run":false})
        );
        if results.iter().any(|(_, duration, _)| *duration != 720_261) {
            return Err("effective_duration_mismatch");
        }
        Ok(())
    }

    fn write_wav(path: &Path, sample_count: u32) -> Vec<u8> {
        write_wav_at_rate(path, sample_count, 8000)
    }

    fn write_wav_at_rate(path: &Path, sample_count: u32, rate: u32) -> Vec<u8> {
        let data_length = sample_count * 2;
        let mut bytes = Vec::new();
        bytes.extend_from_slice(b"RIFF");
        bytes.extend_from_slice(&(36 + data_length).to_le_bytes());
        bytes.extend_from_slice(b"WAVEfmt ");
        bytes.extend_from_slice(&16_u32.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&1_u16.to_le_bytes());
        bytes.extend_from_slice(&rate.to_le_bytes());
        bytes.extend_from_slice(&(rate * 2).to_le_bytes());
        bytes.extend_from_slice(&2_u16.to_le_bytes());
        bytes.extend_from_slice(&16_u16.to_le_bytes());
        bytes.extend_from_slice(b"data");
        bytes.extend_from_slice(&data_length.to_le_bytes());
        bytes.resize(bytes.len() + data_length as usize, 0);
        fs::write(path, &bytes).unwrap();
        bytes
    }

    #[test]
    fn fractional_wav_inspection_reports_ceil_without_changing_source() {
        let directory = TempDir::new().unwrap();
        let path = directory.path().join("fractional.wav");
        let original = write_wav_at_rate(&path, 16_401, 16_000);
        let file = File::open(&path).unwrap();
        let media = inspect_media(&file, "wav")
            .map_err(|failure| failure.code)
            .unwrap();
        assert_eq!(media.duration_ms, 1026);
        assert_eq!(media.sample_rate, 16_000);
        assert_eq!(fs::read(&path).unwrap(), original);
    }

    #[test]
    #[ignore = "synthetic AAC/MP3 file encoding only; no audio device, network, or model inference"]
    fn synthetic_gapless_aac_and_mp3_inspection_matches_effective_frames(
    ) -> Result<(), &'static str> {
        if std::env::var("ECHOWALL_CODEC_TEST_CONFIRM").as_deref()
            != Ok("synthetic-codec-files-authorized")
        {
            return Err("confirmation_required");
        }
        let directory = TempDir::new().map_err(|_| "temporary_directory_failed")?;
        let source = directory.path().join("fractional.wav");
        let original = write_wav_at_rate(&source, 16_401, 16_000);
        for (extension, codec) in [("m4a", "aac"), ("mp3", "libmp3lame")] {
            let path = directory.path().join(format!("fractional.{extension}"));
            let mut encoder = std::process::Command::new("/opt/homebrew/bin/ffmpeg");
            encoder
                .args([
                    "-nostdin",
                    "-n",
                    "-hide_banner",
                    "-loglevel",
                    "error",
                    "-f",
                    "wav",
                    "-i",
                ])
                .arg(&source)
                .args(["-c:a", codec, "-b:a", "64k"]);
            if extension == "m4a" {
                encoder.args(["-movie_timescale", "16000"]);
            }
            if !encoder
                .arg(&path)
                .output()
                .map_err(|_| "synthetic_encoder_unavailable")?
                .status
                .success()
            {
                return Err("synthetic_encoding_failed");
            }
            let bytes = fs::read(&path).map_err(|_| "synthetic_file_missing")?;
            let file = File::open(&path).map_err(|_| "synthetic_file_missing")?;
            let media = inspect_media(&file, extension).map_err(|failure| failure.code)?;
            let samples = echowall_local_audio::decode_source(
                File::open(&path).map_err(|_| "synthetic_file_missing")?,
                &format!("fractional.{extension}"),
                1026,
            )?;
            println!(
                "{}",
                json!({"scope":"synthetic_codec_timing_only","codec":extension,
                "inspected_duration_ms":media.duration_ms,"native_pcm_frames":samples.len()})
            );
            if media.duration_ms != 1026
                || samples.len() != 16_401
                || fs::read(&path).map_err(|_| "synthetic_file_missing")? != bytes
            {
                return Err("synthetic_effective_timeline_mismatch");
            }
        }
        if fs::read(&source).map_err(|_| "synthetic_file_missing")? != original {
            return Err("synthetic_source_changed");
        }
        Ok(())
    }

    fn write_sparse_wav(path: &Path, sample_rate: u32, duration_seconds: u32) {
        let data_length = sample_rate.checked_mul(duration_seconds).unwrap();
        let mut file = OpenOptions::new()
            .create_new(true)
            .write(true)
            .open(path)
            .unwrap();
        file.write_all(b"RIFF").unwrap();
        file.write_all(&(36 + data_length).to_le_bytes()).unwrap();
        file.write_all(b"WAVEfmt ").unwrap();
        file.write_all(&16_u32.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&sample_rate.to_le_bytes()).unwrap();
        file.write_all(&1_u16.to_le_bytes()).unwrap();
        file.write_all(&8_u16.to_le_bytes()).unwrap();
        file.write_all(b"data").unwrap();
        file.write_all(&data_length.to_le_bytes()).unwrap();
        file.set_len(u64::from(data_length) + 44).unwrap();
    }

    fn importer(temp: &TempDir, maximum_bytes: u64) -> DesktopImporter {
        let archive = temp.path().join("archive-data");
        fs::create_dir(&archive).unwrap();
        let inbox = Arc::new(Inbox::open(temp.path().join("app-data"), archive).unwrap());
        DesktopImporter::new(inbox, Platform::Macos, maximum_bytes).unwrap()
    }

    #[test]
    fn imports_uppercase_wav_without_modifying_the_source() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("Fabricated.WAV");
        let original = write_wav(&source, 8_000);
        let source_metadata = fs::metadata(&source).unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![source.to_string_lossy().into_owned()]);
        let result = &response.results[0];
        assert_eq!(result.status, ImportFileStatus::Imported);
        assert!(result.error.is_none());
        assert_eq!(result.codec.as_deref(), Some("pcm_s16le"));
        assert_eq!(result.duration_ms, Some(1_000));
        assert_eq!(result.sample_rate, Some(8_000));
        assert_eq!(result.channels, Some(1));
        assert!(result.proposed_captured_at.is_some());
        assert_eq!(fs::read(&source).unwrap(), original);
        assert_eq!(fs::metadata(&source).unwrap().len(), source_metadata.len());
        assert_eq!(
            fs::metadata(&source).unwrap().modified().unwrap(),
            source_metadata.modified().unwrap()
        );

        let recording_id = Uuid::parse_str(result.recording_id.as_ref().unwrap()).unwrap();
        let envelope = importer.inbox.load_envelope(recording_id).unwrap();
        assert_eq!(envelope.imported_name.as_deref(), Some("Fabricated.WAV"));
        assert_eq!(envelope.job.state, JobState::Ready);
        assert_eq!(envelope.duration_ms, 1_000);
        assert_eq!(envelope.tracks[0].sample_rate, 8_000);
        assert_eq!(envelope.tracks[0].channels, 1);
        let copied = importer
            .inbox
            .root()
            .join(recording_id.to_string())
            .join(envelope.normalized_audio.as_ref().unwrap());
        assert_eq!(fs::read(&copied).unwrap(), original);
        assert!(fs::read_dir(copied.parent().unwrap())
            .unwrap()
            .all(|entry| !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")));
    }

    #[test]
    fn imports_aac_m4a_without_mislabelling_the_canonical_artifact() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("voice-memo.m4a");
        fs::write(
            &source,
            hex::decode(crate::ingest::mobile::SILENT_AAC_M4A_HEX).unwrap(),
        )
        .unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let result = importer
            .import_paths(vec![source.to_string_lossy().into_owned()])
            .results
            .remove(0);
        assert_eq!(result.status, ImportFileStatus::Imported);
        assert_eq!(result.codec.as_deref(), Some("aac"));
        let recording_id = Uuid::parse_str(result.recording_id.as_ref().unwrap()).unwrap();
        let envelope = importer.inbox.load_envelope(recording_id).unwrap();
        assert_eq!(
            envelope.normalized_audio.as_deref(),
            Some("tracks/imported.m4a")
        );
        assert_eq!(
            envelope.normalized_sha256.as_deref(),
            result.sha256.as_deref()
        );
    }

    #[test]
    fn imports_mp3_without_mislabelling_the_canonical_artifact() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("meeting.mp3");
        fs::write(&source, hex::decode(SILENT_MP3_HEX).unwrap()).unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let result = importer
            .import_paths(vec![source.to_string_lossy().into_owned()])
            .results
            .remove(0);
        assert_eq!(result.status, ImportFileStatus::Imported);
        assert_eq!(result.codec.as_deref(), Some("mp3"));
        let recording_id = Uuid::parse_str(result.recording_id.as_ref().unwrap()).unwrap();
        let envelope = importer.inbox.load_envelope(recording_id).unwrap();
        assert_eq!(
            envelope.normalized_audio.as_deref(),
            Some("tracks/imported.mp3")
        );
        assert_eq!(
            envelope.normalized_sha256.as_deref(),
            result.sha256.as_deref()
        );
    }

    #[test]
    fn probes_a_three_and_a_half_hour_wav_within_the_provider_limits() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("three-and-a-half-hours.wav");
        write_sparse_wav(&source, 1_000, 3 * 3_600 + 30 * 60);

        let opened = SourceFile::open(&source.to_string_lossy(), DEFAULT_MAX_IMPORT_BYTES).unwrap();
        assert_eq!(opened.media.codec, "pcm_u8");
        assert_eq!(opened.media.duration_ms, 12_600_000);
        assert_eq!(opened.digest.size_bytes, 12_600_044);
    }

    #[test]
    fn rejects_five_hour_and_truncated_audio_before_packaging() {
        let temp = TempDir::new().unwrap();
        let five_hours = temp.path().join("five-hours.wav");
        write_sparse_wav(&five_hours, 1_000, 5 * 3_600);
        let truncated = temp.path().join("truncated.wav");
        write_wav(&truncated, 80_000);
        OpenOptions::new()
            .write(true)
            .open(&truncated)
            .unwrap()
            .set_len(44 + 40_000)
            .unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![
            five_hours.to_string_lossy().into_owned(),
            truncated.to_string_lossy().into_owned(),
        ]);
        assert_eq!(
            response.results[0].error.as_ref().unwrap().code,
            "duration_limit"
        );
        assert_eq!(
            response.results[1].error.as_ref().unwrap().code,
            "invalid_media"
        );
        assert!(fs::read_dir(importer.inbox.root())
            .unwrap()
            .all(|entry| !entry.unwrap().path().is_dir()));
    }

    #[test]
    fn deduplicates_identical_bytes_even_when_the_name_changes() {
        let temp = TempDir::new().unwrap();
        let first = temp.path().join("first.wav");
        let second = temp.path().join("renamed.wav");
        let bytes = write_wav(&first, 800);
        fs::write(&second, bytes).unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![
            first.to_string_lossy().into_owned(),
            second.to_string_lossy().into_owned(),
        ]);
        assert_eq!(response.results[0].status, ImportFileStatus::Imported);
        assert_eq!(response.results[1].status, ImportFileStatus::Duplicate);
        assert_eq!(
            response.results[0].recording_id,
            response.results[1].recording_id
        );
        assert_eq!(
            fs::read_dir(importer.inbox.root())
                .unwrap()
                .filter_map(Result::ok)
                .filter(|entry| entry.path().is_dir())
                .count(),
            1
        );
    }

    #[test]
    fn review_atomically_confirms_time_title_and_speaker_hint_before_enqueue() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("review.wav");
        write_wav(&source, 8_000);
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);
        let result = importer
            .import_paths(vec![source.to_string_lossy().into_owned()])
            .results
            .remove(0);
        let recording_id = Uuid::parse_str(result.recording_id.as_ref().unwrap()).unwrap();
        let initial = importer.inbox.load_envelope(recording_id).unwrap();
        let initial_review = initial.import_review.unwrap();
        assert_eq!(
            initial_review.timestamp_source,
            ImportTimestampSource::FileMtime
        );
        assert_eq!(
            initial_review.timestamp_confidence,
            ImportTimestampConfidence::Low
        );
        assert!(initial_review.confirmed_at.is_none());

        let state = ImportReviewState::new(Arc::clone(&importer.inbox));
        let confirmed = state
            .confirm(ConfirmImportReviewRequest {
                recording_id: recording_id.to_string(),
                captured_at: "2026-09-02T09:30:00-07:00".to_owned(),
                display_title: Some("  Synthetic interview  ".to_owned()),
                speaker_count: Some(2),
            })
            .unwrap();
        assert_eq!(
            confirmed.display_title.as_deref(),
            Some("Synthetic interview")
        );
        assert_eq!(confirmed.speaker_count, Some(2));

        let envelope = importer.inbox.load_envelope(recording_id).unwrap();
        assert_eq!(
            envelope.captured_at.to_rfc3339(),
            "2026-09-02T09:30:00-07:00"
        );
        assert_eq!(envelope.ended_at.to_rfc3339(), "2026-09-02T09:30:01-07:00");
        let review = envelope.import_review.unwrap();
        assert_eq!(review.timestamp_source, ImportTimestampSource::User);
        assert_eq!(
            review.timestamp_confidence,
            ImportTimestampConfidence::UserConfirmed
        );
        assert_eq!(review.display_title.as_deref(), Some("Synthetic interview"));
        assert_eq!(review.speaker_count, Some(2));
        assert!(review.confirmed_at.is_some());
        assert_eq!(
            importer
                .inbox
                .load_events(recording_id)
                .unwrap()
                .last()
                .unwrap()
                .kind,
            "import_review_confirmed"
        );
    }

    #[test]
    fn batch_results_are_independent_and_keep_input_order() {
        let temp = TempDir::new().unwrap();
        let valid = temp.path().join("valid.wav");
        let empty = temp.path().join("empty.mp3");
        let unsupported = temp.path().join("notes.txt");
        write_wav(&valid, 80);
        fs::write(&empty, []).unwrap();
        fs::write(&unsupported, b"not audio").unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![
            empty.to_string_lossy().into_owned(),
            valid.to_string_lossy().into_owned(),
            unsupported.to_string_lossy().into_owned(),
        ]);
        assert_eq!(response.results.len(), 3);
        assert_eq!(response.results[0].status, ImportFileStatus::Error);
        assert_eq!(
            response.results[0].error.as_ref().unwrap().code,
            "zero_byte"
        );
        assert_eq!(response.results[1].status, ImportFileStatus::Imported);
        assert_eq!(response.results[2].status, ImportFileStatus::Error);
        assert_eq!(
            response.results[2].error.as_ref().unwrap().code,
            "unsupported_extension"
        );
    }

    #[test]
    fn rejects_non_files_unsafe_paths_names_and_oversize() {
        let temp = TempDir::new().unwrap();
        let directory = temp.path().join("directory.wav");
        fs::create_dir(&directory).unwrap();
        let unsafe_name = temp.path().join("bad\nname.wav");
        write_wav(&unsafe_name, 80);
        let valid = temp.path().join("source.wav");
        write_wav(&valid, 80);
        let traversal = temp
            .path()
            .join("child")
            .join("..")
            .join("source.wav")
            .to_string_lossy()
            .into_owned();
        let importer = importer(&temp, 64);

        let response = importer.import_paths(vec![
            directory.to_string_lossy().into_owned(),
            unsafe_name.to_string_lossy().into_owned(),
            traversal,
            valid.to_string_lossy().into_owned(),
        ]);
        let codes: Vec<_> = response
            .results
            .iter()
            .map(|result| result.error.as_ref().unwrap().code)
            .collect();
        assert_eq!(
            codes,
            ["not_a_file", "unsafe_name", "unsafe_path", "oversized"]
        );
    }

    #[cfg(unix)]
    #[test]
    fn rejects_source_symlinks() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.wav");
        let link = temp.path().join("link.wav");
        write_wav(&source, 80);
        symlink(source, &link).unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![link.to_string_lossy().into_owned()]);
        assert_eq!(
            response.results[0].error.as_ref().unwrap().code,
            "symlink_not_allowed"
        );
    }

    #[test]
    fn advertised_extensions_are_case_insensitive() {
        for name in ["a.m4a", "a.M4A", "a.mp3", "a.MP3", "a.wav", "a.WAV"] {
            assert!(supported_extension(Path::new(name)).is_some(), "{name}");
        }
        assert!(supported_extension(Path::new("a.aac")).is_none());
    }

    #[test]
    fn corrupt_supported_file_is_rejected_before_any_package_is_created() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("fake.mp3");
        fs::write(&source, b"this is not audio").unwrap();
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);

        let response = importer.import_paths(vec![source.to_string_lossy().into_owned()]);
        assert_eq!(
            response.results[0].error.as_ref().unwrap().code,
            "invalid_media"
        );
        assert_eq!(fs::read_dir(importer.inbox.root()).unwrap().count(), 0);
    }

    #[test]
    fn command_request_rejects_unknown_fields() {
        assert!(serde_json::from_value::<ImportAudioFilesRequest>(json!({
            "paths": [],
            "maximumBytes": 1
        }))
        .is_err());
    }

    #[test]
    fn recovered_import_finishes_the_durable_state_machine() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("recover.wav");
        write_wav(&source, 800);
        let importer = importer(&temp, DEFAULT_MAX_IMPORT_BYTES);
        let source_file =
            SourceFile::open(&source.to_string_lossy(), DEFAULT_MAX_IMPORT_BYTES).unwrap();
        let id = Uuid::new_v4();
        let envelope = importer
            .importing_envelope(id, "tracks/imported.wav", &source_file)
            .unwrap();
        importer.inbox.persist_envelope(&envelope).unwrap();

        let recovered = importer.import_paths(vec![source.to_string_lossy().into_owned()]);
        assert_eq!(recovered.results[0].status, ImportFileStatus::Recovered);
        assert_eq!(
            importer.inbox.load_envelope(id).unwrap().job.state,
            JobState::Ready
        );
    }

    #[test]
    fn export_copy_is_hash_verified_and_never_overwrites_the_source() {
        let temp = TempDir::new().unwrap();
        let source = temp.path().join("source.wav");
        let destination = temp.path().join("export.wav");
        write_wav(&source, 800);
        let digest = crate::ingest::inbox::hash_file_streaming(&source).unwrap();
        export_verified_copy(&source, &destination, &digest.sha256, digest.size_bytes).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), fs::read(&source).unwrap());
        assert!(
            export_verified_copy(&source, &source, &digest.sha256, digest.size_bytes,).is_err()
        );
    }
}
