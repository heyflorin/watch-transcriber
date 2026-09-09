//! Mobile-native import and microphone-session adoption.
//!
//! Native payloads are untrusted hints. Every source is reopened beneath a
//! constructor-supplied app-container root, checked for symlinks and traversal,
//! probed, streamed, and rehashed before it can enter the shared Inbox.

use std::collections::{BTreeMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, FixedOffset, TimeDelta, Utc};
use serde::{Deserialize, Serialize};
use serde_json::json;
use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;
use uuid::Uuid;

#[cfg(mobile)]
use tauri_plugin_echowall_capture::CaptureExt as _;

use super::envelope::{
    valid_code, AudioTrack, CaptureScope, CaptureWarning, ImportReview, ImportTimestampConfidence,
    ImportTimestampSource, JobStatus, Platform, RecordingEnvelope, RecordingSource, SourceKind,
    TrackRole, MAX_ENVELOPE_TRACKS, RECORDING_ENVELOPE_VERSION,
};
use super::import::{ImportFailure, ImportFileResult, ImportFileStatus};
use super::inbox::{hash_file_streaming, hash_reader_streaming, FileDigest, Inbox, InboxEvent};
use super::state::JobState;

pub const DEFAULT_MAX_MOBILE_IMPORT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_BATCH_ITEMS: usize = 16;
const MAX_SESSION_SEGMENTS: usize = MAX_ENVELOPE_TRACKS;
const MAX_NATIVE_JSON_BYTES: u64 = 1024 * 1024;
const MAX_NATIVE_EVENTS_BYTES: u64 = 8 * 1024 * 1024;
const MAX_NATIVE_TREE_ENTRIES: usize = 2_048;

#[cfg(test)]
pub(crate) const SILENT_AAC_M4A_HEX: &str = "0000001c667479704d344120000002004d34412069736f6d69736f3200000008667265650000001f6d646174dc004c61766336332e312e313031000230400e01182007000003026d6f6f760000006c6d76686400000000000000000000000000001f40000000a000010000010000000000000000000000000100000000000000000000000000000001000000000000000000000000000040000000000000000000000000000000000000000000000000000000000000020000022d7472616b0000005c746b68640000000300000000000000000000000100000000000000a000000000000000000000000101000000000100000000000000000000000000000001000000000000000000000000000040000000000000000000000000000024656474730000001c656c73740000000000000001000000a00000040000010000000001a56d646961000000206d64686400000000000000000000000000001f40000004a055c400000000002d68646c720000000000000000736f756e000000000000000000000000536f756e6448616e646c657200000001506d696e6600000010736d686400000000000000000000002464696e660000001c6472656600000000000000010000000c75726c2000000001000001147374626c0000006a7374736400000000000000010000005a6d7034610000000000000001000000000000000000010010000000001f40000000000036657364730000000003808080250001000480808017401500000000001f40000004db0580808005158856e50006808080010200000020737474730000000000000002000000010000040000000001000000a00000001c7374736300000000000000010000000100000002000000010000001c7374737a0000000000000000000000020000001300000004000000147374636f00000000000000010000002c0000001a7367706401000000726f6c6c0000000200000001ffff0000001c7362677000000000726f6c6c0000000100000002000000010000006175647461000000596d657461000000000000002168646c7200000000000000006d6469726170706c0000000000000000000000002c696c737400000024a9746f6f0000001c6461746100000001000000004c61766636332e312e313031";

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AdoptMobileImportsRequest {
    pub items: Vec<MobileImportItem>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct MobileImportItem {
    #[serde(default)]
    pub source_path: Option<String>,
    #[serde(default)]
    pub relative_path: Option<String>,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub import_id: Option<String>,
    #[serde(default)]
    pub intent_token: Option<String>,
    #[serde(default)]
    pub item_token: Option<String>,
    #[serde(default)]
    pub mime: Option<String>,
    #[serde(default)]
    pub declared_mime: Option<String>,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
    #[serde(default)]
    pub state: Option<String>,
    #[serde(default)]
    pub received_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AdoptMobileImportsResponse {
    pub results: Vec<ImportFileResult>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalizeMobileCaptureRequest {
    pub session: NativeMobileSession,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinalizePendingMobileCaptureRequest {
    pub recording_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub enum NativeMobileSession {
    Ios(IosStopPayload),
    Android(AndroidSessionPayload),
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosStopPayload {
    pub recording_id: String,
    pub state: String,
    pub started_at: String,
    #[serde(default)]
    pub ended_at: Option<String>,
    #[serde(default)]
    pub closed_duration_ms: Option<u64>,
    pub closed_segments: Vec<IosClosedSegment>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub current_segment: Option<String>,
    #[serde(default)]
    pub gaps: Vec<NativeGap>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct IosClosedSegment {
    pub relative_path: String,
    pub duration_ms: u64,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IosCaptureSnapshot {
    schema_version: u32,
    recording_id: String,
    state: String,
    started_at: String,
    #[serde(default)]
    ended_at: Option<String>,
    segments: Vec<IosPersistedSegment>,
    #[serde(default)]
    current_segment: Option<String>,
    #[serde(default)]
    warning_codes: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct IosPersistedSegment {
    relative_path: String,
    duration_ms: u64,
    closed_at: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidSessionPayload {
    pub session_id: String,
    pub relative_directory: String,
    pub state: String,
    pub started_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub segment_index: Option<u32>,
    #[serde(default)]
    pub total_pcm_bytes: Option<u64>,
    pub closed_segments: Vec<AndroidClosedSegment>,
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub gaps: Vec<NativeGap>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AndroidClosedSegment {
    pub relative_path: String,
    pub pcm_bytes: u64,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct NativeGap {
    pub code: String,
    #[serde(default)]
    pub at_ms: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FinalizeMobileCaptureResponse {
    pub recording_id: String,
    pub duration_ms: u64,
    pub normalized_sha256: String,
    pub status: MobileFinalizeStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum MobileFinalizeStatus {
    Finalized,
    Recovered,
    Duplicate,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingMobileRecordings {
    pub ready_recording_ids: Vec<String>,
    pub native_sessions: Vec<PendingNativeSession>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingNativeSession {
    pub recording_id: String,
    pub state: String,
    pub platform: Platform,
}

#[derive(Debug, Clone)]
pub struct PreparedMobileSegment {
    pub relative_path: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_ms: u64,
    pub sha256: String,
}

#[derive(Debug, Clone)]
pub struct FinalizedMobileAudio {
    pub relative_path: String,
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    pub duration_ms: u64,
    pub digest: FileDigest,
}

pub trait MobileCaptureFinalizer: Send + Sync {
    fn finalize(
        &self,
        package_directory: &Path,
        segments: &[PreparedMobileSegment],
    ) -> Result<FinalizedMobileAudio, MobileIngestError>;
}

#[derive(Debug)]
pub struct SymphoniaPcmFinalizer {
    maximum_output_bytes: u64,
}

impl SymphoniaPcmFinalizer {
    pub fn new(maximum_output_bytes: u64) -> Result<Self, MobileIngestError> {
        if maximum_output_bytes == 0 || maximum_output_bytes > u64::from(u32::MAX) - 44 {
            return Err(MobileIngestError::new(
                "invalid_limit",
                "mobile normalized WAV byte limit is invalid",
            ));
        }
        Ok(Self {
            maximum_output_bytes,
        })
    }
}

impl MobileCaptureFinalizer for SymphoniaPcmFinalizer {
    fn finalize(
        &self,
        package_directory: &Path,
        segments: &[PreparedMobileSegment],
    ) -> Result<FinalizedMobileAudio, MobileIngestError> {
        let first = segments.first().ok_or_else(|| {
            MobileIngestError::new(
                "no_closed_segments",
                "mobile capture has no closed segments",
            )
        })?;
        let output_relative = "derived/mixed.wav".to_owned();
        let output = package_directory.join(&output_relative);
        let temporary = package_directory
            .join("derived")
            .join(format!(".mobile-finalize-{}.tmp", Uuid::new_v4()));
        let mut writer = OpenOptions::new()
            .create_new(true)
            .read(true)
            .write(true)
            .open(&temporary)?;
        writer.write_all(&[0; 44])?;
        let result = (|| {
            let mut total_frames = 0_u64;
            for segment in segments {
                if segment.sample_rate != first.sample_rate || segment.channels != first.channels {
                    return Err(MobileIngestError::new(
                        "inconsistent_segments",
                        "mobile capture segments have inconsistent audio formats",
                    ));
                }
                total_frames = total_frames
                    .checked_add(decode_pcm16_into(
                        &package_directory.join(&segment.relative_path),
                        segment.sample_rate,
                        segment.channels,
                        &mut writer,
                        self.maximum_output_bytes,
                    )?)
                    .ok_or_else(|| {
                        MobileIngestError::new(
                            "normalized_too_large",
                            "mobile normalized audio duration overflowed",
                        )
                    })?;
            }
            if total_frames == 0 {
                return Err(MobileIngestError::new(
                    "no_audio",
                    "mobile closed segments contain no decodable audio frames",
                ));
            }
            write_wav_header(
                &mut writer,
                first.sample_rate,
                u16::try_from(first.channels).map_err(|_| {
                    MobileIngestError::new("invalid_media", "audio channel count is too large")
                })?,
                total_frames,
            )?;
            writer.sync_all()?;
            Ok(total_frames)
        })();
        drop(writer);
        let total_frames = match result {
            Ok(frames) => frames,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        reject_symlink_if_present(&output)?;
        fs::rename(&temporary, &output)?;
        sync_directory(output.parent().ok_or_else(|| {
            MobileIngestError::new("mobile_io", "normalized output has no parent directory")
        })?)?;
        let digest = hash_file_streaming(&output)?;
        Ok(FinalizedMobileAudio {
            relative_path: output_relative,
            codec: "pcm_s16le".to_owned(),
            sample_rate: first.sample_rate,
            channels: first.channels,
            duration_ms: total_frames.saturating_mul(1_000) / u64::from(first.sample_rate),
            digest,
        })
    }
}

pub struct MobileIngest {
    inbox: Arc<Inbox>,
    platform: Platform,
    container_roots: Vec<PathBuf>,
    maximum_bytes: u64,
    finalizer: Arc<dyn MobileCaptureFinalizer>,
}

impl std::fmt::Debug for MobileIngest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("MobileIngest")
            .field("platform", &self.platform)
            .field("container_root_count", &self.container_roots.len())
            .field("maximum_bytes", &self.maximum_bytes)
            .finish_non_exhaustive()
    }
}

impl MobileIngest {
    pub fn new(
        inbox: Arc<Inbox>,
        platform: Platform,
        container_roots: Vec<PathBuf>,
        maximum_bytes: u64,
        finalizer: Arc<dyn MobileCaptureFinalizer>,
    ) -> Result<Self, MobileIngestError> {
        if !matches!(platform, Platform::Ios | Platform::Android) {
            return Err(MobileIngestError::new(
                "unsupported_platform",
                "mobile adoption requires iOS or Android",
            ));
        }
        if maximum_bytes == 0 {
            return Err(MobileIngestError::new(
                "invalid_limit",
                "maximum mobile source bytes must be greater than zero",
            ));
        }
        let mut verified = Vec::new();
        for root in container_roots {
            if !root.is_absolute() {
                return Err(MobileIngestError::new(
                    "invalid_container_root",
                    "mobile app-container root must be absolute",
                ));
            }
            let metadata = fs::symlink_metadata(&root)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(MobileIngestError::new(
                    "invalid_container_root",
                    "mobile app-container root must be a plain directory",
                ));
            }
            let root = fs::canonicalize(root)?;
            if !verified.contains(&root) {
                verified.push(root);
            }
        }
        if verified.is_empty() {
            return Err(MobileIngestError::new(
                "invalid_container_root",
                "at least one verified mobile app-container root is required",
            ));
        }
        Ok(Self {
            inbox,
            platform,
            container_roots: verified,
            maximum_bytes,
            finalizer,
        })
    }

    pub fn adopt_imports(&self, request: AdoptMobileImportsRequest) -> AdoptMobileImportsResponse {
        if request.items.is_empty() || request.items.len() > MAX_BATCH_ITEMS {
            return AdoptMobileImportsResponse {
                results: request
                    .items
                    .into_iter()
                    .map(|item| {
                        self.import_error_result(
                            &item,
                            MobileIngestError::new(
                                "invalid_batch",
                                "mobile import batch must contain between 1 and 16 items",
                            ),
                        )
                    })
                    .collect(),
            };
        }
        AdoptMobileImportsResponse {
            results: request
                .items
                .into_iter()
                .map(|item| match self.adopt_one(&item) {
                    Ok(result) => result,
                    Err(error) => self.import_error_result(&item, error),
                })
                .collect(),
        }
    }

    pub fn finalize_capture(
        &self,
        request: FinalizeMobileCaptureRequest,
    ) -> Result<FinalizeMobileCaptureResponse, MobileIngestError> {
        let session = self.validate_session(request.session)?;
        let mut recovering = false;
        if let Ok(existing) = self.inbox.load_envelope(session.recording_id) {
            if existing.source.kind != SourceKind::MobileVoiceMemo
                || existing.source.platform != self.platform
            {
                return Err(MobileIngestError::new(
                    "recording_conflict",
                    "native session ID conflicts with another inbox recording",
                ));
            }
            if existing.job.state == JobState::Ready {
                let relative = existing.normalized_audio.as_deref().ok_or_else(|| {
                    MobileIngestError::new(
                        "recovery_integrity",
                        "ready mobile capture has no normalized audio",
                    )
                })?;
                let digest = self
                    .inbox
                    .hash_package_file(existing.recording_id, relative)?;
                if Some(digest.sha256.as_str()) != existing.normalized_sha256.as_deref() {
                    return Err(MobileIngestError::new(
                        "recovery_integrity",
                        "ready mobile capture normalized bytes are invalid",
                    ));
                }
                if self.cleanup_native_session(&session)? {
                    self.append_event(
                        session.recording_id,
                        "mobile_native_staging_cleaned",
                        BTreeMap::new(),
                    )?;
                }
                return Ok(FinalizeMobileCaptureResponse {
                    recording_id: existing.recording_id.to_string(),
                    duration_ms: existing.duration_ms,
                    normalized_sha256: digest.sha256,
                    status: MobileFinalizeStatus::Duplicate,
                });
            }
            if existing.job.state != JobState::Finalizing {
                return Err(MobileIngestError::new(
                    "invalid_state",
                    "mobile capture cannot resume from its current inbox state",
                ));
            }
            recovering = true;
        }

        let mut opened = self.open_native_segments(&session)?;
        let prepared = opened
            .iter()
            .enumerate()
            .map(|(index, segment)| PreparedMobileSegment {
                relative_path: format!("tracks/mobile-{index:04}.{}", segment.extension),
                codec: segment.media.codec.clone(),
                sample_rate: segment.media.sample_rate,
                channels: segment.media.channels,
                duration_ms: segment.media.duration_ms,
                sha256: segment.digest.sha256.clone(),
            })
            .collect::<Vec<_>>();
        let mut envelope = self.mobile_capture_envelope(&session, &prepared)?;
        self.inbox.persist_envelope(&envelope)?;
        self.append_event(
            session.recording_id,
            "mobile_finalize_started",
            BTreeMap::from([("closed_segments".to_owned(), json!(prepared.len()))]),
        )?;
        for (source, destination) in opened.iter_mut().zip(&prepared) {
            let copied = self.inbox.copy_open_file_into_package(
                session.recording_id,
                &destination.relative_path,
                &mut source.file,
                self.maximum_bytes,
                &source.digest.sha256,
            )?;
            if copied != source.digest || source.fingerprint != FileFingerprint::read(&source.file)?
            {
                return Err(MobileIngestError::new(
                    "source_changed",
                    "native capture segment changed during adoption",
                ));
            }
        }
        self.append_event(
            session.recording_id,
            "mobile_segments_adopted",
            BTreeMap::new(),
        )?;
        let package = self.inbox.root().join(session.recording_id.to_string());
        let finalized = self.finalizer.finalize(&package, &prepared)?;
        validate_finalized_output(&self.inbox, session.recording_id, &finalized)?;
        envelope.duration_ms = finalized.duration_ms;
        envelope.ended_at = session.ended_at.max(
            session.captured_at
                + TimeDelta::try_milliseconds(i64::try_from(finalized.duration_ms).map_err(
                    |_| MobileIngestError::new("invalid_media", "mobile duration is too large"),
                )?)
                .ok_or_else(|| {
                    MobileIngestError::new("invalid_media", "mobile duration is too large")
                })?,
        );
        for warning in &mut envelope.capture_warnings {
            warning.at_ms = warning.at_ms.min(finalized.duration_ms);
        }
        envelope.normalized_audio = Some(finalized.relative_path.clone());
        envelope.normalized_sha256 = Some(finalized.digest.sha256.clone());
        envelope.job.state = JobState::Finalizing.transition_to(JobState::Ready)?;
        envelope.validate()?;
        self.inbox.persist_envelope(&envelope)?;
        self.append_event(
            session.recording_id,
            "mobile_capture_ready",
            BTreeMap::from([
                ("duration_ms".to_owned(), json!(finalized.duration_ms)),
                ("sha256".to_owned(), json!(finalized.digest.sha256)),
            ]),
        )?;
        if self.cleanup_native_session(&session)? {
            self.append_event(
                session.recording_id,
                "mobile_native_staging_cleaned",
                BTreeMap::new(),
            )?;
        }
        Ok(FinalizeMobileCaptureResponse {
            recording_id: session.recording_id.to_string(),
            duration_ms: finalized.duration_ms,
            normalized_sha256: finalized.digest.sha256,
            status: if recovering {
                MobileFinalizeStatus::Recovered
            } else {
                MobileFinalizeStatus::Finalized
            },
        })
    }

    pub fn finalize_pending_capture(
        &self,
        request: FinalizePendingMobileCaptureRequest,
    ) -> Result<FinalizeMobileCaptureResponse, MobileIngestError> {
        let recording_id = Uuid::parse_str(&request.recording_id).map_err(|_| {
            MobileIngestError::new("invalid_session", "pending recording ID is invalid")
        })?;
        let session = match self.platform {
            Platform::Ios => NativeMobileSession::Ios(self.read_pending_ios_session(recording_id)?),
            Platform::Android => {
                NativeMobileSession::Android(self.read_pending_android_session(recording_id)?)
            }
            _ => {
                return Err(MobileIngestError::new(
                    "unsupported_platform",
                    "pending mobile finalization requires iOS or Android",
                ));
            }
        };
        self.finalize_capture(FinalizeMobileCaptureRequest { session })
    }

    pub fn list_pending(&self) -> Result<PendingMobileRecordings, MobileIngestError> {
        let mut ready_recording_ids = Vec::new();
        for entry in fs::read_dir(self.inbox.root())? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(recording_id) = Uuid::parse_str(&name) else {
                continue;
            };
            if entry.file_type()?.is_symlink() {
                return Err(MobileIngestError::new(
                    "unsafe_inbox",
                    "UUID-named inbox entry must not be a symlink",
                ));
            }
            let envelope = self.inbox.load_envelope(recording_id)?;
            if envelope.job.state == JobState::Ready {
                ready_recording_ids.push(recording_id.to_string());
            }
        }
        ready_recording_ids.sort();
        for value in &ready_recording_ids {
            let recording_id = Uuid::parse_str(value).map_err(|_| {
                MobileIngestError::new("invalid_session", "ready recording ID is invalid")
            })?;
            let relative = match self.platform {
                Platform::Ios => format!("native-capture/sessions/{recording_id}"),
                Platform::Android => format!("capture/sessions/{recording_id}"),
                _ => continue,
            };
            let _ = self.cleanup_native_storage(recording_id, &relative)?;
        }
        let mut native_sessions = match self.platform {
            Platform::Ios => self.list_ios_sessions()?,
            Platform::Android => self.list_android_sessions()?,
            _ => Vec::new(),
        };
        native_sessions.retain(|session| !ready_recording_ids.contains(&session.recording_id));
        native_sessions.sort_by(|left, right| left.recording_id.cmp(&right.recording_id));
        native_sessions.dedup_by(|left, right| left.recording_id == right.recording_id);
        Ok(PendingMobileRecordings {
            ready_recording_ids,
            native_sessions,
        })
    }

    fn adopt_one(&self, item: &MobileImportItem) -> Result<ImportFileResult, MobileIngestError> {
        if item.state.as_deref().is_some_and(|state| state != "ready") {
            return Err(MobileIngestError::new(
                "native_item_not_ready",
                "native mobile import item is not ready",
            ));
        }
        validate_optional_token(&item.import_id, "importId", true)?;
        validate_optional_token(&item.intent_token, "intentToken", false)?;
        validate_optional_token(&item.item_token, "itemToken", false)?;
        let source_path = item
            .source_path
            .as_deref()
            .or(item.relative_path.as_deref())
            .ok_or_else(|| {
                MobileIngestError::new(
                    "unsafe_path",
                    "mobile import item has no source or relative path",
                )
            })?;
        let path = self.resolve_container_file(source_path)?;
        let imported_name = validate_imported_name(
            item.display_name
                .as_deref()
                .or_else(|| path.file_name().and_then(|name| name.to_str()))
                .ok_or_else(|| {
                    MobileIngestError::new("unsafe_name", "mobile import filename is invalid")
                })?,
        )?;
        let mut source = OpenedSource::open(path, self.maximum_bytes)?;
        verify_declared_metadata(item.size_bytes, item.sha256.as_deref(), &source.digest)?;
        if let Some(mut existing) = self.inbox.find_envelope_by_sha256(&source.digest.sha256)? {
            let relative = existing.normalized_audio.as_deref().ok_or_else(|| {
                MobileIngestError::new(
                    "dedupe_integrity",
                    "matching inbox recording has no normalized audio",
                )
            })?;
            let stored = self
                .inbox
                .hash_package_file(existing.recording_id, relative);
            if !matches!(stored.as_ref(), Ok(digest) if digest == &source.digest) {
                if existing.source.kind != SourceKind::FileImport
                    || !matches!(
                        existing.job.state,
                        JobState::Importing | JobState::Finalizing
                    )
                {
                    return Err(MobileIngestError::new(
                        "dedupe_integrity",
                        "matching inbox recording bytes are invalid",
                    ));
                }
                let copied = self.inbox.copy_open_file_into_package(
                    existing.recording_id,
                    relative,
                    &mut source.file,
                    self.maximum_bytes,
                    &source.digest.sha256,
                )?;
                if copied != source.digest {
                    return Err(MobileIngestError::new(
                        "source_changed",
                        "mobile import changed during recovery",
                    ));
                }
                if existing.job.state == JobState::Importing {
                    existing.job.state = JobState::Importing.transition_to(JobState::Finalizing)?;
                }
                existing.job.state = JobState::Finalizing.transition_to(JobState::Ready)?;
                self.inbox.persist_envelope(&existing)?;
                self.append_event(
                    existing.recording_id,
                    "mobile_import_recovered",
                    BTreeMap::new(),
                )?;
                return Ok(import_result(
                    source_path,
                    imported_name,
                    ImportFileStatus::Recovered,
                    existing.recording_id,
                    &source,
                    existing.captured_at,
                ));
            }
            return Ok(import_result(
                source_path,
                imported_name,
                ImportFileStatus::Duplicate,
                existing.recording_id,
                &source,
                existing.captured_at,
            ));
        }
        let recording_id = Uuid::new_v4();
        let relative_path = format!("tracks/imported.{}", source.extension);
        let imported_at = match item.received_at_ms {
            Some(value) => validated_received_at(value)?,
            None => Utc::now().fixed_offset(),
        };
        let duration =
            TimeDelta::try_milliseconds(i64::try_from(source.media.duration_ms).map_err(|_| {
                MobileIngestError::new("invalid_media", "mobile import duration is too large")
            })?)
            .ok_or_else(|| {
                MobileIngestError::new("invalid_media", "mobile import duration is too large")
            })?;
        let mut envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id,
            source: RecordingSource {
                kind: SourceKind::FileImport,
                platform: self.platform,
                label: None,
                capture_scope: CaptureScope::ImportedFile,
            },
            captured_at: imported_at - duration,
            ended_at: imported_at,
            duration_ms: source.media.duration_ms,
            tracks: vec![AudioTrack {
                role: TrackRole::Imported,
                relative_path: relative_path.clone(),
                codec: source.media.codec.clone(),
                sample_rate: source.media.sample_rate,
                channels: source.media.channels,
                duration_ms: source.media.duration_ms,
                clock_start_ns: 0,
                sha256: source.digest.sha256.clone(),
            }],
            normalized_audio: Some(relative_path.clone()),
            normalized_sha256: Some(source.digest.sha256.clone()),
            imported_name: Some(imported_name.clone()),
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
        self.inbox.persist_envelope(&envelope)?;
        self.append_event(recording_id, "mobile_import_started", BTreeMap::new())?;
        let copied = self.inbox.copy_open_file_into_package(
            recording_id,
            &relative_path,
            &mut source.file,
            self.maximum_bytes,
            &source.digest.sha256,
        )?;
        if copied != source.digest || source.fingerprint != FileFingerprint::read(&source.file)? {
            return Err(MobileIngestError::new(
                "source_changed",
                "mobile import changed during adoption",
            ));
        }
        envelope.job.state = JobState::Importing.transition_to(JobState::Finalizing)?;
        self.inbox.persist_envelope(&envelope)?;
        envelope.job.state = JobState::Finalizing.transition_to(JobState::Ready)?;
        self.inbox.persist_envelope(&envelope)?;
        self.append_event(recording_id, "mobile_import_ready", BTreeMap::new())?;
        Ok(import_result(
            source_path,
            imported_name,
            ImportFileStatus::Imported,
            recording_id,
            &source,
            envelope.captured_at,
        ))
    }

    fn validate_session(
        &self,
        session: NativeMobileSession,
    ) -> Result<ValidatedSession, MobileIngestError> {
        match (self.platform, session) {
            (Platform::Ios, NativeMobileSession::Ios(value)) => {
                if !matches!(
                    value.state.as_str(),
                    "finalizing" | "interrupted" | "stopped"
                ) {
                    return Err(MobileIngestError::new(
                        "native_session_not_closed",
                        "iOS capture session is not closed or interrupted",
                    ));
                }
                let recording_id = Uuid::parse_str(&value.recording_id).map_err(|_| {
                    MobileIngestError::new("invalid_session", "iOS recording ID is invalid")
                })?;
                let captured_at =
                    DateTime::parse_from_rfc3339(&value.started_at).map_err(|_| {
                        MobileIngestError::new("invalid_session", "iOS start timestamp is invalid")
                    })?;
                let ended_at = value
                    .ended_at
                    .as_deref()
                    .map(DateTime::parse_from_rfc3339)
                    .transpose()
                    .map_err(|_| {
                        MobileIngestError::new("invalid_session", "iOS end timestamp is invalid")
                    })?
                    .unwrap_or(captured_at);
                if ended_at < captured_at {
                    return Err(MobileIngestError::new(
                        "invalid_session",
                        "iOS end timestamp precedes its start",
                    ));
                }
                if value.closed_segments.is_empty()
                    || value.closed_segments.len() > MAX_SESSION_SEGMENTS
                {
                    return Err(MobileIngestError::new(
                        "no_closed_segments",
                        "iOS session must contain a bounded set of closed segments",
                    ));
                }
                let mut segments = Vec::with_capacity(value.closed_segments.len());
                for (index, segment) in value.closed_segments.into_iter().enumerate() {
                    if value.current_segment.as_deref() == Some(segment.relative_path.as_str()) {
                        return Err(MobileIngestError::new(
                            "open_segment",
                            "iOS current/open segment cannot be finalized",
                        ));
                    }
                    validate_ios_segment_path(&segment.relative_path)?;
                    if segment.relative_path != format!("tracks/mic-{:04}.m4a", index + 1) {
                        return Err(MobileIngestError::new(
                            "invalid_segment",
                            "iOS closed segment sequence is not contiguous",
                        ));
                    }
                    segments.push(NativeSegmentHint {
                        relative_path: segment.relative_path,
                        duration_ms: Some(segment.duration_ms),
                        pcm_bytes: None,
                        size_bytes: segment.size_bytes,
                        sha256: segment.sha256,
                    });
                }
                let session_relative = format!("native-capture/sessions/{recording_id}");
                let mut gaps = value.gaps;
                gaps.extend(self.read_ios_journal_gaps(recording_id, captured_at, ended_at)?);
                Ok(ValidatedSession {
                    recording_id,
                    captured_at,
                    ended_at,
                    session_relative,
                    segments,
                    warnings: validate_warnings(value.warnings, gaps)?,
                    declared_closed_duration_ms: value.closed_duration_ms,
                })
            }
            (Platform::Android, NativeMobileSession::Android(value)) => {
                if !matches!(value.state.as_str(), "STOPPED" | "INTERRUPTED") {
                    return Err(MobileIngestError::new(
                        "native_session_not_closed",
                        "Android capture session is not stopped or interrupted",
                    ));
                }
                let recording_id = Uuid::parse_str(&value.session_id).map_err(|_| {
                    MobileIngestError::new("invalid_session", "Android session ID is invalid")
                })?;
                let expected_directory = format!("capture/sessions/{recording_id}");
                if value.relative_directory != expected_directory {
                    return Err(MobileIngestError::new(
                        "unsafe_path",
                        "Android native session directory does not match its session ID",
                    ));
                }
                let captured_at = millis_timestamp(value.started_at_ms, "Android start")?;
                let ended_at = millis_timestamp(value.updated_at_ms, "Android update")?;
                if ended_at < captured_at {
                    return Err(MobileIngestError::new(
                        "invalid_session",
                        "Android update timestamp precedes its start",
                    ));
                }
                if value.closed_segments.is_empty()
                    || value.closed_segments.len() > MAX_SESSION_SEGMENTS
                {
                    return Err(MobileIngestError::new(
                        "no_closed_segments",
                        "Android session must contain a bounded set of closed segments",
                    ));
                }
                let mut segments = Vec::with_capacity(value.closed_segments.len());
                for (index, segment) in value.closed_segments.into_iter().enumerate() {
                    let expected = format!("segment-{index:04}.wav");
                    if segment.relative_path != expected || segment.pcm_bytes == 0 {
                        return Err(MobileIngestError::new(
                            "invalid_segment",
                            "Android closed segment order or byte count is invalid",
                        ));
                    }
                    segments.push(NativeSegmentHint {
                        relative_path: segment.relative_path,
                        duration_ms: segment.duration_ms,
                        pcm_bytes: Some(segment.pcm_bytes),
                        size_bytes: None,
                        sha256: segment.sha256,
                    });
                }
                if value.segment_index.is_some_and(|index| {
                    usize::try_from(index)
                        .ok()
                        .is_some_and(|index| index + 1 < segments.len())
                }) {
                    return Err(MobileIngestError::new(
                        "invalid_segment",
                        "Android segment checkpoint precedes supplied closed segments",
                    ));
                }
                let declared_pcm = segments
                    .iter()
                    .try_fold(0_u64, |total, segment| {
                        total.checked_add(segment.pcm_bytes.unwrap_or(0))
                    })
                    .ok_or_else(|| {
                        MobileIngestError::new(
                            "invalid_segment",
                            "Android PCM byte count overflowed",
                        )
                    })?;
                if value
                    .total_pcm_bytes
                    .is_some_and(|total| total != declared_pcm)
                {
                    return Err(MobileIngestError::new(
                        "tampered_metadata",
                        "Android total PCM bytes do not match closed segments",
                    ));
                }
                Ok(ValidatedSession {
                    recording_id,
                    captured_at,
                    ended_at,
                    session_relative: expected_directory,
                    segments,
                    warnings: validate_warnings(value.warnings, value.gaps)?,
                    declared_closed_duration_ms: None,
                })
            }
            _ => Err(MobileIngestError::new(
                "platform_mismatch",
                "native mobile payload does not match the running platform",
            )),
        }
    }

    fn open_native_segments(
        &self,
        session: &ValidatedSession,
    ) -> Result<Vec<OpenedSource>, MobileIngestError> {
        let mut opened = Vec::with_capacity(session.segments.len());
        let mut paths = HashSet::new();
        let mut duration_total = 0_u64;
        for hint in &session.segments {
            if !paths.insert(hint.relative_path.as_str()) {
                return Err(MobileIngestError::new(
                    "invalid_segment",
                    "native closed segment list contains a duplicate path",
                ));
            }
            let source_path = format!("{}/{}", session.session_relative, hint.relative_path);
            let mut source = OpenedSource::open(
                self.resolve_container_file(&source_path)?,
                self.maximum_bytes,
            )?;
            if self.platform == Platform::Ios && source.extension != "m4a" {
                return Err(MobileIngestError::new(
                    "invalid_segment",
                    "iOS native capture segments must be AAC in an .m4a container",
                ));
            }
            if self.platform == Platform::Android && source.extension != "wav" {
                return Err(MobileIngestError::new(
                    "invalid_segment",
                    "Android native capture segments must be PCM WAV",
                ));
            }
            verify_declared_metadata(hint.size_bytes, hint.sha256.as_deref(), &source.digest)?;
            if let Some(pcm_bytes) = hint.pcm_bytes {
                if source.digest.size_bytes != pcm_bytes.saturating_add(44) {
                    return Err(MobileIngestError::new(
                        "tampered_metadata",
                        "Android PCM byte count does not match the WAV file",
                    ));
                }
            }
            if let Some(declared_duration) = hint.duration_ms {
                if source.media.duration_ms > 0
                    && source.media.duration_ms.abs_diff(declared_duration) > 250
                {
                    return Err(MobileIngestError::new(
                        "tampered_metadata",
                        "native segment duration does not match decoded media",
                    ));
                }
                if source.media.duration_ms == 0 {
                    source.media.duration_ms = declared_duration;
                }
            } else if source.media.duration_ms == 0 {
                if let Some(pcm_bytes) = hint.pcm_bytes {
                    let bytes_per_frame = u64::from(source.media.channels).saturating_mul(2);
                    source.media.duration_ms = pcm_bytes
                        .checked_div(bytes_per_frame)
                        .unwrap_or(0)
                        .saturating_mul(1_000)
                        .checked_div(u64::from(source.media.sample_rate))
                        .unwrap_or(0);
                }
            }
            duration_total = duration_total
                .checked_add(source.media.duration_ms)
                .ok_or_else(|| {
                    MobileIngestError::new("invalid_media", "mobile duration overflowed")
                })?;
            opened.push(source);
        }
        if session
            .declared_closed_duration_ms
            .is_some_and(|declared| duration_total > 0 && duration_total.abs_diff(declared) > 500)
        {
            return Err(MobileIngestError::new(
                "tampered_metadata",
                "native closed duration does not match decoded segments",
            ));
        }
        Ok(opened)
    }

    fn cleanup_native_session(
        &self,
        session: &ValidatedSession,
    ) -> Result<bool, MobileIngestError> {
        self.cleanup_native_storage(session.recording_id, &session.session_relative)
    }

    fn cleanup_native_storage(
        &self,
        recording_id: Uuid,
        session_relative: &str,
    ) -> Result<bool, MobileIngestError> {
        super::envelope::validate_relative_path(session_relative, "native_session_path")?;
        let mut matches = Vec::new();
        for root in &self.container_roots {
            let candidate = root.join(session_relative);
            if fs::symlink_metadata(&candidate).is_ok() {
                matches.push((root, candidate));
            }
        }
        let mut removed = false;
        if matches.is_empty() {
            // A crash may occur after removing the per-session directory but
            // before clearing Android's global active snapshot.
        } else if matches.len() != 1 {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "native session cleanup target is ambiguous",
            ));
        } else {
            let (root, directory) = matches.remove(0);
            reject_symlink_components(root, &directory)?;
            if directory.starts_with(self.inbox.root()) || self.inbox.root().starts_with(&directory)
            {
                return Err(MobileIngestError::new(
                    "storage_overlap",
                    "native session cleanup overlaps the Rust Inbox",
                ));
            }
            let metadata = fs::symlink_metadata(&directory)?;
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(MobileIngestError::new(
                    "symlink_not_allowed",
                    "native session cleanup target must be a plain directory",
                ));
            }
            reject_symlink_tree(&directory)?;
            fs::remove_dir_all(&directory)?;
            sync_directory(directory.parent().ok_or_else(|| {
                MobileIngestError::new("mobile_io", "native session has no parent directory")
            })?)?;
            removed = true;
        }
        if self.cleanup_android_active_snapshot(recording_id)? {
            removed = true;
        }
        Ok(removed)
    }

    fn cleanup_android_active_snapshot(
        &self,
        recording_id: Uuid,
    ) -> Result<bool, MobileIngestError> {
        if self.platform != Platform::Android {
            return Ok(false);
        }
        let relative = "capture/active-session.json";
        let mut matches = self
            .container_roots
            .iter()
            .filter_map(|root| {
                let path = root.join(relative);
                path.exists().then_some((root, path))
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Ok(false);
        }
        if matches.len() != 1 {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "Android active snapshot is ambiguous",
            ));
        }
        let (root, snapshot) = matches.remove(0);
        reject_symlink_components(root, &snapshot)?;
        let metadata = fs::symlink_metadata(&snapshot)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_NATIVE_JSON_BYTES
        {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "Android active snapshot is invalid",
            ));
        }
        let value: serde_json::Value = serde_json::from_reader(File::open(&snapshot)?)?;
        if value.get("sessionId").and_then(|field| field.as_str())
            != Some(recording_id.to_string().as_str())
        {
            return Ok(false);
        }
        if !matches!(
            value.get("state").and_then(|field| field.as_str()),
            Some("STOPPED" | "INTERRUPTED")
        ) {
            return Err(MobileIngestError::new(
                "invalid_state",
                "Android active snapshot is not safe to clean",
            ));
        }
        fs::remove_file(&snapshot)?;
        sync_directory(snapshot.parent().ok_or_else(|| {
            MobileIngestError::new("mobile_io", "Android snapshot has no parent directory")
        })?)?;
        Ok(true)
    }

    fn mobile_capture_envelope(
        &self,
        session: &ValidatedSession,
        segments: &[PreparedMobileSegment],
    ) -> Result<RecordingEnvelope, MobileIngestError> {
        let mut clock_start_ns = 0_u64;
        let tracks = segments
            .iter()
            .map(|segment| {
                let track = AudioTrack {
                    role: TrackRole::Microphone,
                    relative_path: segment.relative_path.clone(),
                    codec: segment.codec.clone(),
                    sample_rate: segment.sample_rate,
                    channels: segment.channels,
                    duration_ms: segment.duration_ms,
                    clock_start_ns,
                    sha256: segment.sha256.clone(),
                };
                clock_start_ns =
                    clock_start_ns.saturating_add(segment.duration_ms.saturating_mul(1_000_000));
                track
            })
            .collect();
        let duration_ms = clock_start_ns / 1_000_000;
        let mut warnings = session.warnings.clone();
        for warning in &mut warnings {
            warning.at_ms = warning.at_ms.min(duration_ms);
        }
        let envelope = RecordingEnvelope {
            schema_version: RECORDING_ENVELOPE_VERSION,
            recording_id: session.recording_id,
            source: RecordingSource {
                kind: SourceKind::MobileVoiceMemo,
                platform: self.platform,
                label: None,
                capture_scope: CaptureScope::Microphone,
            },
            captured_at: session.captured_at,
            ended_at: session.ended_at,
            duration_ms,
            tracks,
            normalized_audio: None,
            normalized_sha256: None,
            imported_name: None,
            import_review: None,
            capture_warnings: warnings,
            job: JobStatus {
                state: JobState::Finalizing,
                attempt: 0,
                remote_job_id: None,
                last_error: None,
            },
        };
        envelope.validate()?;
        Ok(envelope)
    }

    fn resolve_container_file(&self, supplied: &str) -> Result<PathBuf, MobileIngestError> {
        if supplied.is_empty() || supplied.contains('\0') || supplied.contains('\\') {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "mobile native path is invalid",
            ));
        }
        let supplied_path = Path::new(supplied);
        if supplied_path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::CurDir))
        {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "mobile native path contains traversal components",
            ));
        }
        let mut matches = Vec::new();
        if supplied_path.is_absolute() {
            if supplied_path.exists() {
                matches.push(supplied_path.to_path_buf());
            }
        } else {
            super::envelope::validate_relative_path(supplied, "mobile_native_path")?;
            for root in &self.container_roots {
                let candidate = root.join(supplied_path);
                if candidate.exists() {
                    matches.push(candidate);
                }
            }
        }
        if matches.len() != 1 {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "mobile native path is missing or ambiguous across container roots",
            ));
        }
        let candidate = matches.remove(0);
        for root in &self.container_roots {
            if candidate.starts_with(root) {
                reject_symlink_components(root, &candidate)?;
            }
        }
        let metadata = fs::symlink_metadata(&candidate)?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(MobileIngestError::new(
                "symlink_not_allowed",
                "mobile native source must be a plain file",
            ));
        }
        let canonical = fs::canonicalize(&candidate)?;
        let root = self
            .container_roots
            .iter()
            .find(|root| canonical.starts_with(root))
            .ok_or_else(|| {
                MobileIngestError::new(
                    "outside_app_container",
                    "mobile native source is outside verified app-container roots",
                )
            })?;
        reject_symlink_components(root, &canonical)?;
        if canonical.starts_with(self.inbox.root()) {
            return Err(MobileIngestError::new(
                "unsafe_path",
                "mobile native source cannot point back into the shared Inbox",
            ));
        }
        Ok(canonical)
    }

    fn import_error_result(
        &self,
        item: &MobileImportItem,
        error: MobileIngestError,
    ) -> ImportFileResult {
        ImportFileResult {
            source_path: item
                .source_path
                .clone()
                .or_else(|| item.relative_path.clone())
                .unwrap_or_default(),
            imported_name: item.display_name.clone(),
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
        }
    }

    fn append_event(
        &self,
        recording_id: Uuid,
        kind: &str,
        payload: BTreeMap<String, serde_json::Value>,
    ) -> Result<(), MobileIngestError> {
        self.inbox.append_event(&InboxEvent::new(
            recording_id,
            kind,
            Utc::now().fixed_offset(),
            payload,
        )?)?;
        Ok(())
    }

    fn list_ios_sessions(&self) -> Result<Vec<PendingNativeSession>, MobileIngestError> {
        let mut pending = Vec::new();
        for root in &self.container_roots {
            let recordings = root.join("native-capture/sessions");
            if !recordings.is_dir() {
                continue;
            }
            for entry in fs::read_dir(recordings)? {
                let entry = entry?;
                if entry.file_type()?.is_symlink() || !entry.file_type()?.is_dir() {
                    continue;
                }
                let snapshot = entry.path().join("capture.json");
                if !snapshot.is_file() || fs::metadata(&snapshot)?.len() > MAX_NATIVE_JSON_BYTES {
                    continue;
                }
                reject_symlink_components(root, &snapshot)?;
                let value: serde_json::Value = serde_json::from_reader(File::open(snapshot)?)?;
                let Some(recording_id) = value.get("recordingId").and_then(|value| value.as_str())
                else {
                    continue;
                };
                if Uuid::parse_str(recording_id).is_err() {
                    continue;
                }
                let state = value
                    .get("state")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown")
                    .to_owned();
                if matches!(state.as_str(), "finalizing" | "interrupted" | "stopped") {
                    pending.push(PendingNativeSession {
                        recording_id: recording_id.to_owned(),
                        state,
                        platform: Platform::Ios,
                    });
                }
            }
        }
        Ok(pending)
    }

    fn read_pending_ios_session(
        &self,
        recording_id: Uuid,
    ) -> Result<IosStopPayload, MobileIngestError> {
        let relative = format!("native-capture/sessions/{recording_id}/capture.json");
        let (_, path) = self.find_single_container_file(&relative)?;
        if fs::metadata(&path)?.len() > MAX_NATIVE_JSON_BYTES {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "iOS capture snapshot is too large",
            ));
        }
        let snapshot: IosCaptureSnapshot = serde_json::from_reader(File::open(path)?)?;
        if snapshot.schema_version != 1 || snapshot.recording_id != recording_id.to_string() {
            return Err(MobileIngestError::new(
                "invalid_session",
                "iOS capture snapshot identity is invalid",
            ));
        }
        if !matches!(
            snapshot.state.as_str(),
            "interrupted" | "finalizing" | "stopped"
        ) {
            return Err(MobileIngestError::new(
                "native_session_not_closed",
                "iOS pending capture has not been closed",
            ));
        }
        if snapshot.segments.len() > MAX_SESSION_SEGMENTS {
            return Err(MobileIngestError::new(
                "invalid_segment",
                "iOS pending capture has too many segments",
            ));
        }
        let closed_duration_ms = snapshot
            .segments
            .iter()
            .try_fold(0_u64, |total, segment| {
                total.checked_add(segment.duration_ms)
            })
            .ok_or_else(|| {
                MobileIngestError::new("invalid_segment", "iOS segment duration overflowed")
            })?;
        let ended_at = snapshot.ended_at.or_else(|| {
            snapshot
                .segments
                .last()
                .map(|segment| segment.closed_at.clone())
        });
        Ok(IosStopPayload {
            recording_id: snapshot.recording_id,
            state: snapshot.state,
            started_at: snapshot.started_at,
            ended_at,
            closed_duration_ms: Some(closed_duration_ms),
            closed_segments: snapshot
                .segments
                .into_iter()
                .map(|segment| IosClosedSegment {
                    relative_path: segment.relative_path,
                    duration_ms: segment.duration_ms,
                    size_bytes: None,
                    sha256: None,
                })
                .collect(),
            warnings: snapshot.warning_codes,
            current_segment: snapshot.current_segment,
            gaps: Vec::new(),
        })
    }

    fn read_ios_journal_gaps(
        &self,
        recording_id: Uuid,
        started_at: DateTime<FixedOffset>,
        ended_at: DateTime<FixedOffset>,
    ) -> Result<Vec<NativeGap>, MobileIngestError> {
        let relative = format!("native-capture/sessions/{recording_id}/capture-events.ndjson");
        let mut matches = self
            .container_roots
            .iter()
            .filter_map(|root| {
                let file = root.join(&relative);
                file.is_file().then_some((root, file))
            })
            .collect::<Vec<_>>();
        if matches.is_empty() {
            return Ok(Vec::new());
        }
        if matches.len() != 1 {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "iOS capture event journal is ambiguous across container roots",
            ));
        }
        let (root, journal) = matches.remove(0);
        let metadata = fs::symlink_metadata(&journal)?;
        if metadata.file_type().is_symlink() || metadata.len() > MAX_NATIVE_EVENTS_BYTES {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "iOS capture event journal is invalid",
            ));
        }
        reject_symlink_components(root, &journal)?;
        let bytes = fs::read(journal)?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "iOS capture event journal has an incomplete tail",
            ));
        }
        let mut paused_at = None;
        let mut gaps = Vec::new();
        let recording_id_text = recording_id.to_string();
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let event: serde_json::Value = serde_json::from_slice(line)?;
            if event.get("recording_id").and_then(|field| field.as_str())
                != Some(recording_id_text.as_str())
            {
                continue;
            }
            let timestamp = event
                .get("occurred_at")
                .and_then(|field| field.as_str())
                .map(DateTime::parse_from_rfc3339)
                .transpose()
                .map_err(|_| {
                    MobileIngestError::new(
                        "invalid_native_json",
                        "iOS capture event timestamp is invalid",
                    )
                })?;
            match event.get("kind").and_then(|field| field.as_str()) {
                Some("recording_paused") => paused_at = timestamp,
                Some("recording_resumed") => {
                    if let (Some(paused), Some(resumed)) = (paused_at.take(), timestamp) {
                        append_wall_clock_gap(&mut gaps, started_at, paused, resumed)?;
                    }
                }
                _ => {}
            }
        }
        if let Some(paused) = paused_at {
            append_wall_clock_gap(&mut gaps, started_at, paused, ended_at)?;
        }
        Ok(gaps)
    }

    fn list_android_sessions(&self) -> Result<Vec<PendingNativeSession>, MobileIngestError> {
        let mut pending = Vec::new();
        for root in &self.container_roots {
            let snapshot = root.join("capture/active-session.json");
            if !snapshot.is_file() || fs::metadata(&snapshot)?.len() > MAX_NATIVE_JSON_BYTES {
                continue;
            }
            reject_symlink_components(root, &snapshot)?;
            let value: serde_json::Value = serde_json::from_reader(File::open(snapshot)?)?;
            let Some(recording_id) = value.get("sessionId").and_then(|value| value.as_str()) else {
                continue;
            };
            if Uuid::parse_str(recording_id).is_err() {
                continue;
            }
            let state = value
                .get("state")
                .and_then(|value| value.as_str())
                .unwrap_or("UNKNOWN")
                .to_owned();
            if matches!(state.as_str(), "STOPPED" | "INTERRUPTED") {
                pending.push(PendingNativeSession {
                    recording_id: recording_id.to_owned(),
                    state,
                    platform: Platform::Android,
                });
            }
        }
        Ok(pending)
    }

    fn read_pending_android_session(
        &self,
        recording_id: Uuid,
    ) -> Result<AndroidSessionPayload, MobileIngestError> {
        let (root, snapshot) = self.find_single_container_file("capture/active-session.json")?;
        if fs::metadata(&snapshot)?.len() > MAX_NATIVE_JSON_BYTES {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "Android session snapshot exceeds its size limit",
            ));
        }
        let value: serde_json::Value = serde_json::from_reader(File::open(&snapshot)?)?;
        let requested_id = recording_id.to_string();
        if value.get("sessionId").and_then(|field| field.as_str()) != Some(requested_id.as_str()) {
            return Err(MobileIngestError::new(
                "pending_session_missing",
                "Android pending snapshot does not match the requested recording",
            ));
        }
        let state = value
            .get("state")
            .and_then(|field| field.as_str())
            .ok_or_else(|| {
                MobileIngestError::new("invalid_native_json", "Android session state is missing")
            })?
            .to_owned();
        let started_at_ms = value
            .get("startedAtMs")
            .and_then(|field| field.as_i64())
            .ok_or_else(|| {
                MobileIngestError::new(
                    "invalid_native_json",
                    "Android session start timestamp is missing",
                )
            })?;
        let updated_at_ms = value
            .get("updatedAtMs")
            .and_then(|field| field.as_i64())
            .ok_or_else(|| {
                MobileIngestError::new(
                    "invalid_native_json",
                    "Android session update timestamp is missing",
                )
            })?;
        let events = root.join("capture/session-events.ndjson");
        let metadata = fs::symlink_metadata(&events)?;
        if metadata.file_type().is_symlink()
            || !metadata.is_file()
            || metadata.len() > MAX_NATIVE_EVENTS_BYTES
        {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "Android native session event journal is invalid",
            ));
        }
        reject_symlink_components(&root, &events)?;
        let bytes = fs::read(&events)?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(MobileIngestError::new(
                "invalid_native_json",
                "Android native session event journal has an incomplete tail",
            ));
        }
        let mut segments = BTreeMap::new();
        let mut warnings = Vec::new();
        let mut gaps = Vec::new();
        let mut paused_at_ms = None;
        for line in bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
        {
            let event: serde_json::Value = serde_json::from_slice(line)?;
            if event.get("sessionId").and_then(|field| field.as_str())
                != Some(requested_id.as_str())
            {
                continue;
            }
            match event.get("kind").and_then(|field| field.as_str()) {
                Some("segment_completed") => {
                    let detail = event
                        .get("detail")
                        .and_then(|field| field.as_object())
                        .ok_or_else(|| {
                            MobileIngestError::new(
                                "invalid_native_json",
                                "Android segment event detail is missing",
                            )
                        })?;
                    let index = detail
                        .get("segmentIndex")
                        .and_then(|field| field.as_u64())
                        .and_then(|field| u32::try_from(field).ok())
                        .ok_or_else(|| {
                            MobileIngestError::new(
                                "invalid_native_json",
                                "Android segment event index is invalid",
                            )
                        })?;
                    let relative_path = detail
                        .get("relativePath")
                        .and_then(|field| field.as_str())
                        .ok_or_else(|| {
                            MobileIngestError::new(
                                "invalid_native_json",
                                "Android segment event path is missing",
                            )
                        })?
                        .to_owned();
                    let pcm_bytes = detail
                        .get("pcmBytes")
                        .and_then(|field| field.as_u64())
                        .filter(|field| *field > 0)
                        .ok_or_else(|| {
                            MobileIngestError::new(
                                "invalid_native_json",
                                "Android segment event byte count is invalid",
                            )
                        })?;
                    if segments
                        .insert(
                            index,
                            AndroidClosedSegment {
                                relative_path,
                                pcm_bytes,
                                duration_ms: None,
                                sha256: None,
                            },
                        )
                        .is_some()
                    {
                        return Err(MobileIngestError::new(
                            "invalid_native_json",
                            "Android session journal repeats a completed segment",
                        ));
                    }
                }
                Some("interrupted") => {
                    if let Some(code) = event
                        .get("detail")
                        .and_then(|field| field.get("code"))
                        .and_then(|field| field.as_str())
                    {
                        warnings.push(code.to_owned());
                    }
                }
                Some("paused") => {
                    paused_at_ms = event.get("atMs").and_then(|field| field.as_i64());
                }
                Some("recording") => {
                    if let (Some(paused), Some(resumed)) = (
                        paused_at_ms.take(),
                        event.get("atMs").and_then(|field| field.as_i64()),
                    ) {
                        if resumed > paused {
                            gaps.push(NativeGap {
                                code: "pause_gap".to_owned(),
                                at_ms: u64::try_from(paused.saturating_sub(started_at_ms))
                                    .unwrap_or(0),
                                duration_ms: u64::try_from(resumed - paused).unwrap_or(0),
                            });
                        }
                    }
                }
                _ => {}
            }
        }
        if let Some(paused) = paused_at_ms {
            if updated_at_ms > paused {
                gaps.push(NativeGap {
                    code: "pause_gap".to_owned(),
                    at_ms: u64::try_from(paused.saturating_sub(started_at_ms)).unwrap_or(0),
                    duration_ms: u64::try_from(updated_at_ms - paused).unwrap_or(0),
                });
            }
        }
        if state == "INTERRUPTED" && !warnings.iter().any(|code| code == "native_interrupted") {
            warnings.push("native_interrupted".to_owned());
        }
        let closed_segments = segments.into_values().collect::<Vec<_>>();
        let total_pcm_bytes = closed_segments
            .iter()
            .try_fold(0_u64, |total, segment| total.checked_add(segment.pcm_bytes))
            .ok_or_else(|| {
                MobileIngestError::new("invalid_segment", "Android PCM bytes overflowed")
            })?;
        Ok(AndroidSessionPayload {
            session_id: requested_id,
            relative_directory: format!("capture/sessions/{recording_id}"),
            state,
            started_at_ms,
            updated_at_ms,
            segment_index: value
                .get("segmentIndex")
                .and_then(|field| field.as_u64())
                .and_then(|field| u32::try_from(field).ok()),
            total_pcm_bytes: Some(total_pcm_bytes),
            closed_segments,
            warnings,
            gaps,
        })
    }

    fn find_single_container_file(
        &self,
        relative_path: &str,
    ) -> Result<(PathBuf, PathBuf), MobileIngestError> {
        super::envelope::validate_relative_path(relative_path, "native_metadata_path")?;
        let mut matches = self
            .container_roots
            .iter()
            .filter_map(|root| {
                let file = root.join(relative_path);
                file.is_file().then(|| (root.clone(), file))
            })
            .collect::<Vec<_>>();
        if matches.len() != 1 {
            return Err(MobileIngestError::new(
                "pending_session_missing",
                "native pending session file is missing or ambiguous",
            ));
        }
        let (root, file) = matches.remove(0);
        reject_symlink_components(&root, &file)?;
        Ok((root, file))
    }
}

pub struct MobileIngestState {
    ingest: Arc<Mutex<MobileIngest>>,
}

impl MobileIngestState {
    pub fn new(ingest: MobileIngest) -> Self {
        Self {
            ingest: Arc::new(Mutex::new(ingest)),
        }
    }
}

#[tauri::command]
pub async fn adopt_mobile_imports(
    state: tauri::State<'_, MobileIngestState>,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    request: AdoptMobileImportsRequest,
) -> Result<AdoptMobileImportsResponse, String> {
    if !features.audio_import {
        return Err("audio import is disabled for this release".to_owned());
    }
    let ingest = Arc::clone(&state.ingest);
    tauri::async_runtime::spawn_blocking(move || {
        ingest
            .lock()
            .map_err(|_| "mobile ingest state is unavailable".to_owned())
            .map(|ingest| ingest.adopt_imports(request))
    })
    .await
    .map_err(|_| "mobile import worker stopped unexpectedly".to_owned())?
}

fn require_mobile_recording_feature(
    features: &crate::features::RuntimeFeatures,
) -> Result<(), String> {
    features
        .recording
        .then_some(())
        .ok_or_else(|| "audio recording is disabled for this release".to_owned())
}

fn require_mobile_import_feature(
    features: &crate::features::RuntimeFeatures,
) -> Result<(), String> {
    features
        .audio_import
        .then_some(())
        .ok_or_else(|| "audio import is disabled for this release".to_owned())
}

async fn invoke_mobile_plugin(
    app: tauri::AppHandle,
    command: &'static str,
    payload: impl Serialize,
) -> Result<serde_json::Value, String> {
    #[cfg(mobile)]
    {
        app.echowall_capture().invoke(command, payload).await
    }
    #[cfg(not(mobile))]
    {
        let _ = (app, command, payload);
        Err("mobile native operation is unavailable".to_owned())
    }
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MobileRecordingArgs {
    recording_id: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MobileStartArgs {
    recording_id: String,
    mode: String,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct MobileAcknowledgeArgs {
    import_ids: Vec<String>,
}

#[tauri::command]
pub async fn mobile_check_permissions(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
) -> Result<serde_json::Value, String> {
    require_mobile_recording_feature(&features)?;
    invoke_mobile_plugin(app, "checkPermissions", ()).await
}

#[tauri::command]
pub async fn mobile_request_permissions(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
) -> Result<serde_json::Value, String> {
    require_mobile_recording_feature(&features)?;
    invoke_mobile_plugin(app, "requestPermissions", ()).await
}

#[tauri::command]
pub async fn mobile_preflight(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
) -> Result<serde_json::Value, String> {
    require_mobile_recording_feature(&features)?;
    invoke_mobile_plugin(app, "preflight", ()).await
}

#[tauri::command]
pub async fn mobile_start(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
    recording_id: String,
    mode: String,
) -> Result<serde_json::Value, String> {
    require_mobile_recording_feature(&features)?;
    invoke_mobile_plugin(app, "start", MobileStartArgs { recording_id, mode }).await
}

macro_rules! mobile_recording_command {
    ($name:ident, $native:literal) => {
        #[tauri::command]
        pub async fn $name(
            app: tauri::AppHandle,
            recording_id: String,
        ) -> Result<serde_json::Value, String> {
            invoke_mobile_plugin(app, $native, MobileRecordingArgs { recording_id }).await
        }
    };
}

mobile_recording_command!(mobile_pause, "pause");
mobile_recording_command!(mobile_resume, "resume");
mobile_recording_command!(mobile_stop, "stop");

#[tauri::command]
pub async fn mobile_status(
    app: tauri::AppHandle,
    recording_id: Option<String>,
) -> Result<serde_json::Value, String> {
    invoke_mobile_plugin(
        app,
        "status",
        serde_json::json!({ "recordingId": recording_id }),
    )
    .await
}

#[tauri::command]
pub async fn mobile_open_audio_picker(
    app: tauri::AppHandle,
    features: tauri::State<'_, crate::features::RuntimeFeatures>,
) -> Result<serde_json::Value, String> {
    require_mobile_import_feature(&features)?;
    invoke_mobile_plugin(app, "openAudioPicker", ()).await
}

#[tauri::command]
pub async fn mobile_drain_shared_imports(
    app: tauri::AppHandle,
) -> Result<serde_json::Value, String> {
    invoke_mobile_plugin(app, "drainSharedImports", ()).await
}

#[tauri::command]
pub async fn mobile_acknowledge_shared_imports(
    app: tauri::AppHandle,
    import_ids: Vec<String>,
) -> Result<serde_json::Value, String> {
    invoke_mobile_plugin(
        app,
        "acknowledgeSharedImports",
        MobileAcknowledgeArgs { import_ids },
    )
    .await
}

#[tauri::command]
pub async fn finalize_mobile_capture(
    state: tauri::State<'_, MobileIngestState>,
    request: FinalizeMobileCaptureRequest,
) -> Result<FinalizeMobileCaptureResponse, String> {
    let ingest = Arc::clone(&state.ingest);
    tauri::async_runtime::spawn_blocking(move || {
        ingest
            .lock()
            .map_err(|_| "mobile ingest state is unavailable".to_owned())?
            .finalize_capture(request)
            .map_err(|error| error.public_message())
    })
    .await
    .map_err(|_| "mobile finalizer worker stopped unexpectedly".to_owned())?
}

#[tauri::command]
pub async fn finalize_pending_mobile_capture(
    state: tauri::State<'_, MobileIngestState>,
    request: FinalizePendingMobileCaptureRequest,
) -> Result<FinalizeMobileCaptureResponse, String> {
    let ingest = Arc::clone(&state.ingest);
    tauri::async_runtime::spawn_blocking(move || {
        ingest
            .lock()
            .map_err(|_| "mobile ingest state is unavailable".to_owned())?
            .finalize_pending_capture(request)
            .map_err(|error| error.public_message())
    })
    .await
    .map_err(|_| "mobile pending finalizer stopped unexpectedly".to_owned())?
}

#[tauri::command]
pub async fn list_pending_mobile_recordings(
    state: tauri::State<'_, MobileIngestState>,
) -> Result<PendingMobileRecordings, String> {
    let ingest = Arc::clone(&state.ingest);
    tauri::async_runtime::spawn_blocking(move || {
        ingest
            .lock()
            .map_err(|_| "mobile ingest state is unavailable".to_owned())?
            .list_pending()
            .map_err(|error| error.public_message())
    })
    .await
    .map_err(|_| "mobile pending worker stopped unexpectedly".to_owned())?
}

pub fn current_mobile_platform() -> Result<Platform, MobileIngestError> {
    #[cfg(target_os = "ios")]
    return Ok(Platform::Ios);
    #[cfg(target_os = "android")]
    return Ok(Platform::Android);
    #[cfg(not(any(target_os = "ios", target_os = "android")))]
    Err(MobileIngestError::new(
        "unsupported_platform",
        "mobile ingest requires iOS or Android",
    ))
}

#[derive(Debug)]
pub struct MobileIngestError {
    pub code: &'static str,
    message: String,
}

impl MobileIngestError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn public_message(&self) -> String {
        format!("{}: {}", self.code, self.message)
    }
}

impl std::fmt::Display for MobileIngestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for MobileIngestError {}

impl From<std::io::Error> for MobileIngestError {
    fn from(_: std::io::Error) -> Self {
        Self::new("mobile_io", "mobile ingest filesystem operation failed")
    }
}

impl From<serde_json::Error> for MobileIngestError {
    fn from(_: serde_json::Error) -> Self {
        Self::new(
            "invalid_native_json",
            "mobile native metadata JSON is invalid",
        )
    }
}

impl From<super::envelope::EnvelopeValidationError> for MobileIngestError {
    fn from(error: super::envelope::EnvelopeValidationError) -> Self {
        Self::new("invalid_envelope", error.to_string())
    }
}

impl From<super::inbox::InboxError> for MobileIngestError {
    fn from(_: super::inbox::InboxError) -> Self {
        Self::new("inbox_error", "mobile Inbox operation failed")
    }
}

impl From<super::state::InvalidJobTransition> for MobileIngestError {
    fn from(_: super::state::InvalidJobTransition) -> Self {
        Self::new("invalid_state", "mobile Inbox state transition is invalid")
    }
}

struct OpenedSource {
    file: File,
    digest: FileDigest,
    fingerprint: FileFingerprint,
    extension: String,
    media: super::import::MediaInfo,
}

impl OpenedSource {
    fn open(path: PathBuf, maximum_bytes: u64) -> Result<Self, MobileIngestError> {
        let extension = supported_extension(&path).ok_or_else(|| {
            MobileIngestError::new(
                "unsupported_extension",
                "supported mobile audio is .m4a, .mp3, or .wav",
            )
        })?;
        let metadata = fs::metadata(&path)?;
        if metadata.len() == 0 {
            return Err(MobileIngestError::new(
                "zero_byte",
                "mobile audio source is empty",
            ));
        }
        if metadata.len() > maximum_bytes {
            return Err(MobileIngestError::new(
                "oversized",
                "mobile audio source exceeds the configured byte limit",
            ));
        }
        let mut file = File::open(&path)?;
        let fingerprint = FileFingerprint::read(&file)?;
        let media = super::import::inspect_media(&file, extension)
            .map_err(|error| MobileIngestError::new(error.code, error.message))?;
        file.seek(SeekFrom::Start(0))?;
        let digest = hash_reader_streaming(&mut file)?;
        file.seek(SeekFrom::Start(0))?;
        if digest.size_bytes != metadata.len() || fingerprint != FileFingerprint::read(&file)? {
            return Err(MobileIngestError::new(
                "source_changed",
                "mobile audio source changed while it was inspected",
            ));
        }
        Ok(Self {
            file,
            digest,
            fingerprint,
            extension: extension.to_owned(),
            media,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FileFingerprint {
    size_bytes: u64,
    modified: Option<std::time::SystemTime>,
}

impl FileFingerprint {
    fn read(file: &File) -> Result<Self, std::io::Error> {
        let metadata = file.metadata()?;
        Ok(Self {
            size_bytes: metadata.len(),
            modified: metadata.modified().ok(),
        })
    }
}

struct NativeSegmentHint {
    relative_path: String,
    duration_ms: Option<u64>,
    pcm_bytes: Option<u64>,
    size_bytes: Option<u64>,
    sha256: Option<String>,
}

struct ValidatedSession {
    recording_id: Uuid,
    captured_at: DateTime<FixedOffset>,
    ended_at: DateTime<FixedOffset>,
    session_relative: String,
    segments: Vec<NativeSegmentHint>,
    warnings: Vec<CaptureWarning>,
    declared_closed_duration_ms: Option<u64>,
}

fn decode_pcm16_into(
    path: &Path,
    expected_rate: u32,
    expected_channels: u32,
    writer: &mut File,
    maximum_output_bytes: u64,
) -> Result<u64, MobileIngestError> {
    let extension = supported_extension(path).ok_or_else(|| {
        MobileIngestError::new(
            "unsupported_extension",
            "mobile segment extension is unsupported",
        )
    })?;
    let stream = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
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
            MobileIngestError::new("invalid_media", "mobile segment could not be decoded")
        })?;
    let track = format.default_track(TrackType::Audio).ok_or_else(|| {
        MobileIngestError::new("invalid_media", "mobile segment has no audio track")
    })?;
    let parameters = track
        .codec_params
        .as_ref()
        .and_then(|parameters| parameters.audio())
        .ok_or_else(|| {
            MobileIngestError::new(
                "invalid_media",
                "mobile segment codec parameters are missing",
            )
        })?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(parameters, &AudioDecoderOptions::default())
        .map_err(|_| {
            MobileIngestError::new("unsupported_codec", "mobile segment decoder is unavailable")
        })?;
    let track_id = track.id;
    let mut frames = 0_u64;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(packet)) => packet,
            Ok(None) => break,
            Err(_) => {
                return Err(MobileIngestError::new(
                    "invalid_media",
                    "mobile segment packet stream is invalid",
                ));
            }
        };
        if packet.track_id != track_id {
            continue;
        }
        let decoded = decoder.decode(&packet).map_err(|_| {
            MobileIngestError::new("invalid_media", "mobile segment audio decode failed")
        })?;
        let spec = decoded.spec();
        let channels = u32::try_from(spec.channels().count()).map_err(|_| {
            MobileIngestError::new("invalid_media", "decoded channel count is invalid")
        })?;
        if spec.rate() != expected_rate || channels != expected_channels {
            return Err(MobileIngestError::new(
                "inconsistent_segments",
                "decoded segment format changed unexpectedly",
            ));
        }
        let mut samples = vec![0_i16; decoded.samples_interleaved()];
        decoded.copy_to_slice_interleaved(&mut samples);
        let additional_bytes = u64::try_from(samples.len())
            .ok()
            .and_then(|samples| samples.checked_mul(2))
            .ok_or_else(|| {
                MobileIngestError::new("normalized_too_large", "decoded audio size overflowed")
            })?;
        let current_bytes = writer.metadata()?.len().saturating_sub(44);
        if current_bytes.saturating_add(additional_bytes) > maximum_output_bytes {
            return Err(MobileIngestError::new(
                "normalized_too_large",
                "mobile normalized audio exceeds the configured byte limit",
            ));
        }
        let mut bytes = Vec::with_capacity(samples.len() * 2);
        for sample in samples {
            bytes.extend_from_slice(&sample.to_le_bytes());
        }
        writer.write_all(&bytes)?;
        frames = frames
            .checked_add(
                u64::try_from(bytes.len() / 2)
                    .unwrap_or(0)
                    .checked_div(u64::from(expected_channels))
                    .unwrap_or(0),
            )
            .ok_or_else(|| {
                MobileIngestError::new("normalized_too_large", "decoded duration overflowed")
            })?;
    }
    Ok(frames)
}

fn write_wav_header(
    writer: &mut File,
    sample_rate: u32,
    channels: u16,
    frames: u64,
) -> Result<(), MobileIngestError> {
    let block_align = channels.checked_mul(2).ok_or_else(|| {
        MobileIngestError::new("normalized_too_large", "WAV block alignment overflowed")
    })?;
    let data_bytes = frames
        .checked_mul(u64::from(block_align))
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| {
            MobileIngestError::new("normalized_too_large", "normalized WAV exceeds RIFF limits")
        })?;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(data_bytes + 36).to_le_bytes());
    header.extend_from_slice(b"WAVEfmt ");
    header.extend_from_slice(&16_u32.to_le_bytes());
    header.extend_from_slice(&1_u16.to_le_bytes());
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&(sample_rate * u32::from(block_align)).to_le_bytes());
    header.extend_from_slice(&block_align.to_le_bytes());
    header.extend_from_slice(&16_u16.to_le_bytes());
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_bytes.to_le_bytes());
    writer.seek(SeekFrom::Start(0))?;
    writer.write_all(&header)?;
    Ok(())
}

fn validate_finalized_output(
    inbox: &Inbox,
    recording_id: Uuid,
    finalized: &FinalizedMobileAudio,
) -> Result<(), MobileIngestError> {
    if finalized.relative_path != "derived/mixed.wav"
        || finalized.codec != "pcm_s16le"
        || finalized.sample_rate == 0
        || finalized.channels == 0
        || finalized.duration_ms == 0
    {
        return Err(MobileIngestError::new(
            "invalid_finalizer_output",
            "mobile finalizer returned invalid normalized metadata",
        ));
    }
    let actual = inbox.hash_package_file(recording_id, &finalized.relative_path)?;
    if actual != finalized.digest {
        return Err(MobileIngestError::new(
            "invalid_finalizer_output",
            "mobile finalizer output hash or size is invalid",
        ));
    }
    Ok(())
}

fn import_result(
    source_path: &str,
    imported_name: String,
    status: ImportFileStatus,
    recording_id: Uuid,
    source: &OpenedSource,
    captured_at: DateTime<FixedOffset>,
) -> ImportFileResult {
    ImportFileResult {
        source_path: source_path.to_owned(),
        imported_name: Some(imported_name),
        status,
        recording_id: Some(recording_id.to_string()),
        sha256: Some(source.digest.sha256.clone()),
        size_bytes: Some(source.digest.size_bytes),
        codec: Some(source.media.codec.clone()),
        duration_ms: Some(source.media.duration_ms),
        sample_rate: Some(source.media.sample_rate),
        channels: Some(source.media.channels),
        proposed_captured_at: Some(captured_at.to_rfc3339()),
        error: None,
    }
}

fn verify_declared_metadata(
    size_bytes: Option<u64>,
    sha256: Option<&str>,
    actual: &FileDigest,
) -> Result<(), MobileIngestError> {
    if size_bytes.is_some_and(|size| size != actual.size_bytes) {
        return Err(MobileIngestError::new(
            "tampered_metadata",
            "native declared size does not match mobile source bytes",
        ));
    }
    if let Some(sha256) = sha256 {
        super::envelope::validate_sha256(sha256, "native.sha256")?;
        if sha256 != actual.sha256 {
            return Err(MobileIngestError::new(
                "tampered_metadata",
                "native declared hash does not match mobile source bytes",
            ));
        }
    }
    Ok(())
}

fn validate_optional_token(
    value: &Option<String>,
    field: &str,
    uuid: bool,
) -> Result<(), MobileIngestError> {
    let Some(value) = value else { return Ok(()) };
    let valid = if uuid {
        Uuid::parse_str(value).is_ok()
    } else {
        value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    };
    if valid {
        Ok(())
    } else {
        Err(MobileIngestError::new(
            "tampered_metadata",
            format!("native {field} is invalid"),
        ))
    }
}

fn validate_imported_name(value: &str) -> Result<String, MobileIngestError> {
    let count = value.chars().count();
    let invalid = value
        .chars()
        .any(|character| character.is_control() || matches!(character, '/' | '\\' | ':' | '\0'));
    let stem = value
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit()
            && stem.as_bytes()[3] != b'0');
    if !(1..=1024).contains(&count)
        || matches!(value, "." | "..")
        || stem.is_empty()
        || value.ends_with([' ', '.'])
        || invalid
        || reserved
    {
        return Err(MobileIngestError::new(
            "unsafe_name",
            "mobile import display name is unsafe",
        ));
    }
    Ok(value.to_owned())
}

fn validated_received_at(value: i64) -> Result<DateTime<FixedOffset>, MobileIngestError> {
    let timestamp = DateTime::<Utc>::from_timestamp_millis(value).ok_or_else(|| {
        MobileIngestError::new(
            "tampered_metadata",
            "native mobile received timestamp is invalid",
        )
    })?;
    let earliest = DateTime::parse_from_rfc3339("2001-01-01T00:00:00Z")
        .expect("static timestamp is valid")
        .with_timezone(&Utc);
    if timestamp < earliest || timestamp > Utc::now() + TimeDelta::days(1) {
        return Err(MobileIngestError::new(
            "tampered_metadata",
            "native mobile received timestamp is outside the accepted range",
        ));
    }
    Ok(timestamp.fixed_offset())
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

fn validate_ios_segment_path(path: &str) -> Result<(), MobileIngestError> {
    super::envelope::validate_relative_path(path, "ios_segment.relative_path")?;
    let Some(name) = path.strip_prefix("tracks/mic-") else {
        return Err(MobileIngestError::new(
            "invalid_segment",
            "iOS closed segment path has an invalid prefix",
        ));
    };
    let Some(number) = name.strip_suffix(".m4a") else {
        return Err(MobileIngestError::new(
            "invalid_segment",
            "iOS closed segment must use an .m4a container",
        ));
    };
    if number.len() != 4 || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(MobileIngestError::new(
            "invalid_segment",
            "iOS closed segment sequence is invalid",
        ));
    }
    Ok(())
}

fn validate_warnings(
    warning_codes: Vec<String>,
    gaps: Vec<NativeGap>,
) -> Result<Vec<CaptureWarning>, MobileIngestError> {
    let mut warnings = Vec::new();
    for code in warning_codes {
        if !valid_code(&code) {
            return Err(MobileIngestError::new(
                "invalid_warning",
                "native mobile warning code is invalid",
            ));
        }
        warnings.push(CaptureWarning {
            message: format!("Native mobile capture reported {code}"),
            code,
            at_ms: 0,
        });
    }
    for gap in gaps {
        if !valid_code(&gap.code) || gap.duration_ms == 0 {
            return Err(MobileIngestError::new(
                "invalid_warning",
                "native mobile gap metadata is invalid",
            ));
        }
        warnings.push(CaptureWarning {
            code: gap.code,
            message: format!("Mobile capture contains a {} ms gap", gap.duration_ms),
            at_ms: gap.at_ms,
        });
    }
    warnings.sort_by(|left, right| (&left.code, left.at_ms).cmp(&(&right.code, right.at_ms)));
    warnings.dedup_by(|left, right| left.code == right.code && left.at_ms == right.at_ms);
    Ok(warnings)
}

fn millis_timestamp(
    value: i64,
    label: &'static str,
) -> Result<DateTime<FixedOffset>, MobileIngestError> {
    DateTime::<Utc>::from_timestamp_millis(value)
        .map(|timestamp| timestamp.fixed_offset())
        .ok_or_else(|| {
            MobileIngestError::new("invalid_session", format!("{label} timestamp is invalid"))
        })
}

fn append_wall_clock_gap(
    gaps: &mut Vec<NativeGap>,
    recording_started_at: DateTime<FixedOffset>,
    gap_started_at: DateTime<FixedOffset>,
    gap_ended_at: DateTime<FixedOffset>,
) -> Result<(), MobileIngestError> {
    if gap_ended_at <= gap_started_at || gap_started_at < recording_started_at {
        return Err(MobileIngestError::new(
            "invalid_native_json",
            "native capture gap timestamps are invalid",
        ));
    }
    let at_ms = (gap_started_at - recording_started_at)
        .num_milliseconds()
        .try_into()
        .map_err(|_| {
            MobileIngestError::new(
                "invalid_native_json",
                "native capture gap offset overflowed",
            )
        })?;
    let duration_ms = (gap_ended_at - gap_started_at)
        .num_milliseconds()
        .try_into()
        .map_err(|_| {
            MobileIngestError::new(
                "invalid_native_json",
                "native capture gap duration overflowed",
            )
        })?;
    gaps.push(NativeGap {
        code: "pause_gap".to_owned(),
        at_ms,
        duration_ms,
    });
    Ok(())
}

fn reject_symlink_components(root: &Path, file: &Path) -> Result<(), MobileIngestError> {
    let relative = file.strip_prefix(root).map_err(|_| {
        MobileIngestError::new(
            "outside_app_container",
            "mobile source escaped its app-container root",
        )
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        current.push(component.as_os_str());
        if fs::symlink_metadata(&current)?.file_type().is_symlink() {
            return Err(MobileIngestError::new(
                "symlink_not_allowed",
                "mobile native source path contains a symbolic link",
            ));
        }
    }
    Ok(())
}

fn reject_symlink_if_present(path: &Path) -> Result<(), MobileIngestError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(MobileIngestError::new(
            "symlink_not_allowed",
            "mobile normalized output cannot replace a symbolic link",
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(MobileIngestError::new(
            "mobile_io",
            "mobile normalized output could not be inspected",
        )),
    }
}

fn reject_symlink_tree(root: &Path) -> Result<(), MobileIngestError> {
    let mut pending = vec![root.to_path_buf()];
    let mut entries = 0_usize;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            entries = entries.saturating_add(1);
            if entries > MAX_NATIVE_TREE_ENTRIES {
                return Err(MobileIngestError::new(
                    "unsafe_path",
                    "native session cleanup tree is too large",
                ));
            }
            let kind = entry.file_type()?;
            if kind.is_symlink() {
                return Err(MobileIngestError::new(
                    "symlink_not_allowed",
                    "native session cleanup tree contains a symbolic link",
                ));
            }
            if kind.is_dir() {
                pending.push(entry.path());
            } else if !kind.is_file() {
                return Err(MobileIngestError::new(
                    "unsafe_path",
                    "native session cleanup tree contains an unsupported entry",
                ));
            }
        }
    }
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), MobileIngestError> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    fn test_ingest(temp: &TempDir, platform: Platform) -> (MobileIngest, Arc<Inbox>, PathBuf) {
        let container = temp.path().join("container");
        let archive = temp.path().join("archive");
        fs::create_dir_all(&container).unwrap();
        fs::create_dir_all(&archive).unwrap();
        let inbox = Arc::new(Inbox::open(container.join("app-data"), archive).unwrap());
        let finalizer = Arc::new(SymphoniaPcmFinalizer::new(32 * 1024 * 1024).unwrap());
        let ingest = MobileIngest::new(
            Arc::clone(&inbox),
            platform,
            vec![container.clone()],
            DEFAULT_MAX_MOBILE_IMPORT_BYTES,
            finalizer,
        )
        .unwrap();
        (ingest, inbox, container)
    }

    fn write_wav(path: &Path, frames: u32, sample_rate: u32) -> FileDigest {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = File::create(path).unwrap();
        file.write_all(&[0; 44]).unwrap();
        for frame in 0..frames {
            let sample = (((frame % 32) as i16) - 16) * 512;
            file.write_all(&sample.to_le_bytes()).unwrap();
        }
        write_wav_header(&mut file, sample_rate, 1, u64::from(frames)).unwrap();
        file.sync_all().unwrap();
        hash_file_streaming(path).unwrap()
    }

    fn import_item(relative_path: &str, digest: &FileDigest) -> MobileImportItem {
        MobileImportItem {
            source_path: None,
            relative_path: Some(relative_path.to_owned()),
            display_name: Some("meeting.wav".to_owned()),
            import_id: Some(Uuid::new_v4().to_string()),
            intent_token: Some("a".repeat(64)),
            item_token: Some("b".repeat(64)),
            mime: Some("audio/wav".to_owned()),
            declared_mime: None,
            size_bytes: Some(digest.size_bytes),
            sha256: Some(digest.sha256.clone()),
            state: Some("ready".to_owned()),
            received_at_ms: Some(1_788_000_000_000),
        }
    }

    #[test]
    fn adopts_container_import_and_deduplicates_replayed_native_metadata() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Android);
        let path = container.join("inbox/native.wav");
        let digest = write_wav(&path, 8_000, 8_000);
        let item = import_item("inbox/native.wav", &digest);

        let first = ingest.adopt_imports(AdoptMobileImportsRequest {
            items: vec![item.clone()],
        });
        assert_eq!(first.results[0].status, ImportFileStatus::Imported);
        let second = ingest.adopt_imports(AdoptMobileImportsRequest { items: vec![item] });
        assert_eq!(second.results[0].status, ImportFileStatus::Duplicate);
        let id = Uuid::parse_str(first.results[0].recording_id.as_deref().unwrap()).unwrap();
        assert_eq!(inbox.load_envelope(id).unwrap().job.state, JobState::Ready);
    }

    #[test]
    fn replay_recovers_an_import_checkpoint_missing_its_copied_bytes() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Android);
        let path = container.join("inbox/recover.wav");
        let digest = write_wav(&path, 8_000, 8_000);
        let item = import_item("inbox/recover.wav", &digest);
        let first = ingest.adopt_imports(AdoptMobileImportsRequest {
            items: vec![item.clone()],
        });
        let id = Uuid::parse_str(first.results[0].recording_id.as_deref().unwrap()).unwrap();
        let mut envelope = inbox.load_envelope(id).unwrap();
        envelope.job.state = JobState::Finalizing;
        inbox.persist_envelope(&envelope).unwrap();
        fs::remove_file(
            inbox
                .root()
                .join(id.to_string())
                .join("tracks/imported.wav"),
        )
        .unwrap();

        let replay = ingest.adopt_imports(AdoptMobileImportsRequest { items: vec![item] });
        assert_eq!(replay.results[0].status, ImportFileStatus::Recovered);
        assert_eq!(inbox.load_envelope(id).unwrap().job.state, JobState::Ready);
    }

    #[test]
    fn batch_import_rejects_forged_paths_symlinks_and_tampered_metadata_independently() {
        let temp = TempDir::new().unwrap();
        let (ingest, _, container) = test_ingest(&temp, Platform::Ios);
        let valid = container.join("Documents/valid.wav");
        let digest = write_wav(&valid, 800, 8_000);
        let outside = temp.path().join("outside.wav");
        write_wav(&outside, 800, 8_000);
        #[cfg(unix)]
        std::os::unix::fs::symlink(&valid, container.join("Documents/link.wav")).unwrap();
        let mut tampered = import_item("Documents/valid.wav", &digest);
        tampered.sha256 = Some("0".repeat(64));
        let mut items = vec![
            MobileImportItem {
                source_path: Some(outside.to_string_lossy().into_owned()),
                ..import_item("unused.wav", &digest)
            },
            tampered,
            MobileImportItem {
                relative_path: Some("../outside.wav".to_owned()),
                source_path: None,
                ..import_item("unused.wav", &digest)
            },
        ];
        #[cfg(unix)]
        items.insert(1, import_item("Documents/link.wav", &digest));
        items.push(MobileImportItem {
            source_path: Some(valid.to_string_lossy().into_owned()),
            relative_path: None,
            ..import_item("unused.wav", &digest)
        });

        let response = ingest.adopt_imports(AdoptMobileImportsRequest { items });
        assert_eq!(
            response.results.last().unwrap().status,
            ImportFileStatus::Imported
        );
        assert!(response.results[..response.results.len() - 1]
            .iter()
            .all(|result| result.status == ImportFileStatus::Error));
    }

    #[test]
    fn android_closed_wav_segments_finalize_and_ignore_orphan_files() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Android);
        let id = Uuid::new_v4();
        let session = container.join(format!("capture/sessions/{id}"));
        let first = write_wav(&session.join("segment-0000.wav"), 16_000, 16_000);
        let second = write_wav(&session.join("segment-0001.wav"), 8_000, 16_000);
        write_wav(&session.join("segment-0002.wav"), 4_000, 16_000);
        let request = FinalizeMobileCaptureRequest {
            session: NativeMobileSession::Android(AndroidSessionPayload {
                session_id: id.to_string(),
                relative_directory: format!("capture/sessions/{id}"),
                state: "INTERRUPTED".to_owned(),
                started_at_ms: 1_788_000_000_000,
                updated_at_ms: 1_788_000_003_000,
                segment_index: Some(1),
                total_pcm_bytes: Some((first.size_bytes - 44) + (second.size_bytes - 44)),
                closed_segments: vec![
                    AndroidClosedSegment {
                        relative_path: "segment-0000.wav".to_owned(),
                        pcm_bytes: first.size_bytes - 44,
                        duration_ms: Some(1_000),
                        sha256: Some(first.sha256),
                    },
                    AndroidClosedSegment {
                        relative_path: "segment-0001.wav".to_owned(),
                        pcm_bytes: second.size_bytes - 44,
                        duration_ms: Some(500),
                        sha256: Some(second.sha256),
                    },
                ],
                warnings: vec!["audio_route_changed".to_owned()],
                gaps: vec![NativeGap {
                    code: "background_gap".to_owned(),
                    at_ms: 500,
                    duration_ms: 250,
                }],
            }),
        };
        let response = ingest.finalize_capture(request).unwrap();
        assert_eq!(response.duration_ms, 1_500);
        let envelope = inbox.load_envelope(id).unwrap();
        assert_eq!(envelope.source.kind, SourceKind::MobileVoiceMemo);
        assert_eq!(envelope.tracks.len(), 2);
        assert_eq!(envelope.capture_warnings.len(), 2);
        assert!(!envelope
            .tracks
            .iter()
            .any(|track| track.relative_path.contains("0002")));
        assert!(!session.exists());
        assert!(inbox
            .root()
            .join(id.to_string())
            .join("recording.json")
            .is_file());
    }

    #[test]
    fn ios_stop_payload_decodes_closed_aac_into_normalized_wav() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Ios);
        let id = Uuid::new_v4();
        let segment = container.join(format!("native-capture/sessions/{id}/tracks/mic-0001.m4a"));
        fs::create_dir_all(segment.parent().unwrap()).unwrap();
        fs::write(&segment, hex::decode(SILENT_AAC_M4A_HEX).unwrap()).unwrap();
        let paused = json!({
            "recording_id": id,
            "kind": "recording_paused",
            "occurred_at": "2026-09-02T09:00:00.050-07:00"
        });
        let resumed = json!({
            "recording_id": id,
            "kind": "recording_resumed",
            "occurred_at": "2026-09-02T09:00:00.100-07:00"
        });
        fs::write(
            segment
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .join("capture-events.ndjson"),
            format!("{paused}\n{resumed}\n"),
        )
        .unwrap();
        let response = ingest
            .finalize_capture(FinalizeMobileCaptureRequest {
                session: NativeMobileSession::Ios(IosStopPayload {
                    recording_id: id.to_string(),
                    state: "finalizing".to_owned(),
                    started_at: "2026-09-02T09:00:00-07:00".to_owned(),
                    ended_at: Some("2026-09-02T09:00:01-07:00".to_owned()),
                    closed_duration_ms: None,
                    closed_segments: vec![IosClosedSegment {
                        relative_path: "tracks/mic-0001.m4a".to_owned(),
                        duration_ms: 150,
                        size_bytes: None,
                        sha256: None,
                    }],
                    warnings: vec!["audio_interruption".to_owned()],
                    current_segment: None,
                    gaps: Vec::new(),
                }),
            })
            .unwrap();

        assert!(response.duration_ms > 0);
        let envelope = inbox.load_envelope(id).unwrap();
        assert_eq!(envelope.source.platform, Platform::Ios);
        assert_eq!(
            envelope.normalized_audio.as_deref(),
            Some("derived/mixed.wav")
        );
        assert!(envelope
            .capture_warnings
            .iter()
            .any(|warning| warning.code == "audio_interruption"));
        assert!(envelope
            .capture_warnings
            .iter()
            .any(|warning| warning.code == "pause_gap"));
    }

    #[test]
    fn pending_ios_snapshot_recovers_without_webview_local_storage() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Ios);
        let id = Uuid::new_v4();
        let session = container.join(format!("native-capture/sessions/{id}"));
        let segment = session.join("tracks/mic-0001.m4a");
        fs::create_dir_all(segment.parent().unwrap()).unwrap();
        fs::write(&segment, hex::decode(SILENT_AAC_M4A_HEX).unwrap()).unwrap();
        fs::write(
            session.join("capture.json"),
            serde_json::to_vec(&json!({
                "schemaVersion": 1,
                "recordingId": id,
                "state": "interrupted",
                "startedAt": "2026-09-02T09:00:00-07:00",
                "endedAt": null,
                "segments": [{
                    "relativePath": "tracks/mic-0001.m4a",
                    "durationMs": 150,
                    "closedAt": "2026-09-02T09:00:00.150-07:00"
                }],
                "currentSegment": null,
                "warningCodes": ["process_restarted"]
            }))
            .unwrap(),
        )
        .unwrap();

        let pending = ingest.list_pending().unwrap();
        assert_eq!(pending.native_sessions.len(), 1);
        assert_eq!(pending.native_sessions[0].recording_id, id.to_string());
        let response = ingest
            .finalize_pending_capture(FinalizePendingMobileCaptureRequest {
                recording_id: id.to_string(),
            })
            .unwrap();
        assert!(response.duration_ms > 0);
        let envelope = inbox.load_envelope(id).unwrap();
        assert_eq!(envelope.job.state, JobState::Ready);
        assert!(envelope
            .capture_warnings
            .iter()
            .any(|warning| warning.code == "process_restarted"));
        assert!(!session.exists());
        assert!(inbox
            .root()
            .join(id.to_string())
            .join("recording.json")
            .is_file());
    }

    #[cfg(unix)]
    #[test]
    fn ready_mobile_capture_retries_native_staging_cleanup_without_reprocessing() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Ios);
        let id = Uuid::new_v4();
        let session = container.join(format!("native-capture/sessions/{id}"));
        let segment = session.join("tracks/mic-0001.m4a");
        fs::create_dir_all(segment.parent().unwrap()).unwrap();
        fs::write(&segment, hex::decode(SILENT_AAC_M4A_HEX).unwrap()).unwrap();
        let outside = temp.path().join("outside");
        fs::create_dir_all(&outside).unwrap();
        let unsafe_link = session.join("unsafe-link");
        std::os::unix::fs::symlink(&outside, &unsafe_link).unwrap();
        let request = || FinalizeMobileCaptureRequest {
            session: NativeMobileSession::Ios(IosStopPayload {
                recording_id: id.to_string(),
                state: "interrupted".to_owned(),
                started_at: "2026-09-02T09:00:00-07:00".to_owned(),
                ended_at: Some("2026-09-02T09:00:00.150-07:00".to_owned()),
                closed_duration_ms: Some(150),
                closed_segments: vec![IosClosedSegment {
                    relative_path: "tracks/mic-0001.m4a".to_owned(),
                    duration_ms: 150,
                    size_bytes: None,
                    sha256: None,
                }],
                warnings: vec!["process_restarted".to_owned()],
                current_segment: None,
                gaps: Vec::new(),
            }),
        };

        assert_eq!(
            ingest.finalize_capture(request()).unwrap_err().code,
            "symlink_not_allowed"
        );
        assert_eq!(inbox.load_envelope(id).unwrap().job.state, JobState::Ready);
        fs::remove_file(unsafe_link).unwrap();
        let replay = ingest.finalize_capture(request()).unwrap();
        assert_eq!(replay.status, MobileFinalizeStatus::Duplicate);
        assert!(!session.exists());
        assert!(inbox
            .root()
            .join(id.to_string())
            .join("recording.json")
            .is_file());
    }

    #[test]
    fn native_staging_cleanup_refuses_any_rust_inbox_overlap() {
        let temp = TempDir::new().unwrap();
        let container = temp.path().join("container");
        let archive = temp.path().join("archive");
        fs::create_dir_all(&container).unwrap();
        fs::create_dir_all(&archive).unwrap();
        let inbox = Arc::new(Inbox::open(container.join("recordings"), archive).unwrap());
        let id = Uuid::new_v4();
        let unsafe_session = inbox.root().join(format!("native-capture/sessions/{id}"));
        fs::create_dir_all(&unsafe_session).unwrap();
        let ingest = MobileIngest::new(
            Arc::clone(&inbox),
            Platform::Ios,
            vec![inbox.root().to_path_buf()],
            DEFAULT_MAX_MOBILE_IMPORT_BYTES,
            Arc::new(SymphoniaPcmFinalizer::new(1024 * 1024).unwrap()),
        )
        .unwrap();
        let at = DateTime::parse_from_rfc3339("2026-09-02T09:00:00-07:00").unwrap();
        let session = ValidatedSession {
            recording_id: id,
            captured_at: at,
            ended_at: at,
            session_relative: format!("native-capture/sessions/{id}"),
            segments: Vec::new(),
            warnings: Vec::new(),
            declared_closed_duration_ms: None,
        };

        assert_eq!(
            ingest.cleanup_native_session(&session).unwrap_err().code,
            "storage_overlap"
        );
        assert!(unsafe_session.is_dir());
    }

    #[test]
    fn rejects_open_forged_and_tampered_native_segments() {
        let temp = TempDir::new().unwrap();
        let (ingest, _, container) = test_ingest(&temp, Platform::Ios);
        let id = Uuid::new_v4();
        let path = container.join(format!("native-capture/sessions/{id}/tracks/mic-0001.m4a"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not m4a").unwrap();
        let request = FinalizeMobileCaptureRequest {
            session: NativeMobileSession::Ios(IosStopPayload {
                recording_id: id.to_string(),
                state: "finalizing".to_owned(),
                started_at: "2026-09-02T09:00:00-07:00".to_owned(),
                ended_at: Some("2026-09-02T09:00:01-07:00".to_owned()),
                closed_duration_ms: Some(1_000),
                closed_segments: vec![IosClosedSegment {
                    relative_path: "tracks/mic-0001.m4a".to_owned(),
                    duration_ms: 1_000,
                    size_bytes: None,
                    sha256: None,
                }],
                warnings: Vec::new(),
                current_segment: Some("tracks/mic-0001.m4a".to_owned()),
                gaps: Vec::new(),
            }),
        };
        assert_eq!(
            ingest.finalize_capture(request).unwrap_err().code,
            "open_segment"
        );

        #[cfg(unix)]
        {
            let symlink_id = Uuid::new_v4();
            let directory = container.join(format!("native-capture/sessions/{symlink_id}/tracks"));
            fs::create_dir_all(&directory).unwrap();
            let target = directory.join("closed.m4a");
            fs::write(&target, hex::decode(SILENT_AAC_M4A_HEX).unwrap()).unwrap();
            std::os::unix::fs::symlink(&target, directory.join("mic-0001.m4a")).unwrap();
            let symlinked = FinalizeMobileCaptureRequest {
                session: NativeMobileSession::Ios(IosStopPayload {
                    recording_id: symlink_id.to_string(),
                    state: "finalizing".to_owned(),
                    started_at: "2026-09-02T09:00:00-07:00".to_owned(),
                    ended_at: None,
                    closed_duration_ms: None,
                    closed_segments: vec![IosClosedSegment {
                        relative_path: "tracks/mic-0001.m4a".to_owned(),
                        duration_ms: 150,
                        size_bytes: None,
                        sha256: None,
                    }],
                    warnings: Vec::new(),
                    current_segment: None,
                    gaps: Vec::new(),
                }),
            };
            assert_eq!(
                ingest.finalize_capture(symlinked).unwrap_err().code,
                "symlink_not_allowed"
            );
        }

        let android_id = Uuid::new_v4();
        let forged = FinalizeMobileCaptureRequest {
            session: NativeMobileSession::Android(AndroidSessionPayload {
                session_id: android_id.to_string(),
                relative_directory: "../../outside".to_owned(),
                state: "STOPPED".to_owned(),
                started_at_ms: 1,
                updated_at_ms: 2,
                segment_index: None,
                total_pcm_bytes: None,
                closed_segments: vec![AndroidClosedSegment {
                    relative_path: "segment-0000.wav".to_owned(),
                    pcm_bytes: 1,
                    duration_ms: None,
                    sha256: None,
                }],
                warnings: Vec::new(),
                gaps: Vec::new(),
            }),
        };
        let (android, _, _) = test_ingest(&temp, Platform::Android);
        assert_eq!(
            android.finalize_capture(forged).unwrap_err().code,
            "unsafe_path"
        );

        let tampered_id = Uuid::new_v4();
        let tampered = FinalizeMobileCaptureRequest {
            session: NativeMobileSession::Android(AndroidSessionPayload {
                session_id: tampered_id.to_string(),
                relative_directory: format!("capture/sessions/{tampered_id}"),
                state: "STOPPED".to_owned(),
                started_at_ms: 1,
                updated_at_ms: 2,
                segment_index: Some(0),
                total_pcm_bytes: Some(2),
                closed_segments: vec![AndroidClosedSegment {
                    relative_path: "segment-0000.wav".to_owned(),
                    pcm_bytes: 1,
                    duration_ms: None,
                    sha256: None,
                }],
                warnings: Vec::new(),
                gaps: Vec::new(),
            }),
        };
        assert_eq!(
            android.finalize_capture(tampered).unwrap_err().code,
            "tampered_metadata"
        );
    }

    #[test]
    fn list_pending_reports_native_closed_sessions_and_ready_inbox_ids() {
        let temp = TempDir::new().unwrap();
        let (ingest, _, container) = test_ingest(&temp, Platform::Android);
        let id = Uuid::new_v4();
        fs::create_dir_all(container.join("capture")).unwrap();
        fs::write(
            container.join("capture/active-session.json"),
            serde_json::to_vec(&json!({"sessionId": id, "state": "STOPPED"})).unwrap(),
        )
        .unwrap();
        let pending = ingest.list_pending().unwrap();
        assert_eq!(pending.native_sessions[0].recording_id, id.to_string());
    }

    #[test]
    fn mobile_release_flags_block_new_native_work_without_blocking_recovery_helpers() {
        let disabled = crate::features::RuntimeFeatures {
            recording: false,
            audio_import: false,
            direct_processing: false,
            browser_capture: false,
            local_stt: false,
            local_qwen_candidate: false,
            local_speakerkit_candidate: false,
            local_moss_candidate: false,
        };
        assert!(require_mobile_recording_feature(&disabled).is_err());
        assert!(require_mobile_import_feature(&disabled).is_err());

        let enabled = crate::features::RuntimeFeatures {
            recording: true,
            audio_import: true,
            direct_processing: false,
            browser_capture: false,
            local_stt: false,
            local_qwen_candidate: false,
            local_speakerkit_candidate: false,
            local_moss_candidate: false,
        };
        assert!(require_mobile_recording_feature(&enabled).is_ok());
        assert!(require_mobile_import_feature(&enabled).is_ok());
        // Status/stop/drain/ack wrappers deliberately have no feature gate so
        // a held rollout cannot strand already-created local work.
    }

    #[test]
    fn pending_android_session_derives_closed_segments_from_native_journal() {
        let temp = TempDir::new().unwrap();
        let (ingest, inbox, container) = test_ingest(&temp, Platform::Android);
        let id = Uuid::new_v4();
        let session = container.join(format!("capture/sessions/{id}"));
        let digest = write_wav(&session.join("segment-0000.wav"), 16_000, 16_000);
        fs::create_dir_all(container.join("capture")).unwrap();
        fs::write(
            container.join("capture/active-session.json"),
            serde_json::to_vec(&json!({
                "sessionId": id,
                "relativeDirectory": format!("capture/sessions/{id}"),
                "state": "STOPPED",
                "startedAtMs": 1_788_000_000_000_i64,
                "updatedAtMs": 1_788_000_001_000_i64,
                "segmentIndex": 0,
                "totalPcmBytes": digest.size_bytes - 44
            }))
            .unwrap(),
        )
        .unwrap();
        let event = json!({
            "kind": "segment_completed",
            "sessionId": id,
            "atMs": 1_788_000_001_000_i64,
            "detail": {
                "segmentIndex": 0,
                "relativePath": "segment-0000.wav",
                "pcmBytes": digest.size_bytes - 44
            }
        });
        fs::write(
            container.join("capture/session-events.ndjson"),
            format!("{}\n", serde_json::to_string(&event).unwrap()),
        )
        .unwrap();

        let response = ingest
            .finalize_pending_capture(FinalizePendingMobileCaptureRequest {
                recording_id: id.to_string(),
            })
            .unwrap();
        assert_eq!(response.duration_ms, 1_000);
        assert_eq!(inbox.load_envelope(id).unwrap().job.state, JobState::Ready);
        assert!(!container.join("capture/active-session.json").exists());

        let stale_session = container.join(format!("capture/sessions/{id}"));
        fs::create_dir_all(&stale_session).unwrap();
        fs::write(stale_session.join("leftover.wav"), b"stale native copy").unwrap();
        fs::write(
            container.join("capture/active-session.json"),
            serde_json::to_vec(&json!({
                "sessionId": id,
                "state": "STOPPED"
            }))
            .unwrap(),
        )
        .unwrap();
        let pending = ingest.list_pending().unwrap();
        assert!(pending.native_sessions.is_empty());
        assert!(!stale_session.exists());
        assert!(!container.join("capture/active-session.json").exists());
    }
}
