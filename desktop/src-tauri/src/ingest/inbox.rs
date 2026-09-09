use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Mutex;

use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::envelope::{
    valid_code, validate_relative_path, validate_sha256, EnvelopeValidationError, RecordingEnvelope,
};
use super::state::JobState;

const RECORDING_FILE: &str = "recording.json";
const EVENTS_FILE: &str = "events.ndjson";
const EVENT_SCHEMA_VERSION: u32 = 1;
pub(crate) const MAX_RECORDING_JSON_BYTES: u64 = 1024 * 1024;
pub(crate) const MAX_INBOX_EVENTS_BYTES: u64 = 8 * 1024 * 1024;
pub(crate) const MAX_INBOX_EVENT_BYTES: u64 = 64 * 1024;

#[derive(Debug)]
pub enum InboxError {
    Io(std::io::Error),
    Json(serde_json::Error),
    InvalidEnvelope(EnvelopeValidationError),
    Boundary(String),
    InvalidEvent(String),
    RecordingMismatch { expected: Uuid, actual: Uuid },
    EmptySource,
    FileTooLarge { maximum_bytes: u64 },
    SourceChanged,
}

impl std::fmt::Display for InboxError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Io(error) => write!(formatter, "inbox I/O failed: {error}"),
            Self::Json(error) => write!(formatter, "inbox JSON is invalid: {error}"),
            Self::InvalidEnvelope(error) => {
                write!(formatter, "recording envelope is invalid: {error}")
            }
            Self::Boundary(message) | Self::InvalidEvent(message) => formatter.write_str(message),
            Self::RecordingMismatch { expected, actual } => write!(
                formatter,
                "recording directory {expected} contains envelope/event for {actual}"
            ),
            Self::EmptySource => formatter.write_str("source file must not be empty"),
            Self::FileTooLarge { maximum_bytes } => write!(
                formatter,
                "source file exceeds the {maximum_bytes}-byte import limit"
            ),
            Self::SourceChanged => {
                formatter.write_str("source file changed while it was being imported")
            }
        }
    }
}

impl std::error::Error for InboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Json(error) => Some(error),
            Self::InvalidEnvelope(error) => Some(error),
            _ => None,
        }
    }
}

impl From<std::io::Error> for InboxError {
    fn from(value: std::io::Error) -> Self {
        Self::Io(value)
    }
}

impl From<serde_json::Error> for InboxError {
    fn from(value: serde_json::Error) -> Self {
        Self::Json(value)
    }
}

impl From<EnvelopeValidationError> for InboxError {
    fn from(value: EnvelopeValidationError) -> Self {
        Self::InvalidEnvelope(value)
    }
}

/// App-owned durable packages. The root is required to be disjoint from the
/// Git-managed archive data directory before the first directory is created.
#[derive(Debug)]
pub struct Inbox {
    root: PathBuf,
    archive_data: PathBuf,
    write_lock: Mutex<()>,
}

impl Inbox {
    pub fn open(
        app_data_directory: impl AsRef<Path>,
        archive_data_directory: impl AsRef<Path>,
    ) -> Result<Self, InboxError> {
        let candidate = resolve_for_comparison(&app_data_directory.as_ref().join("inbox"))?;
        let archive_data = resolve_for_comparison(archive_data_directory.as_ref())?;
        reject_overlap(&candidate, &archive_data)?;

        fs::create_dir_all(&candidate)?;
        let root = fs::canonicalize(&candidate)?;
        let archive_data = resolve_for_comparison(archive_data_directory.as_ref())?;
        reject_overlap(&root, &archive_data)?;

        Ok(Self {
            root,
            archive_data,
            write_lock: Mutex::new(()),
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Validate first, then atomically replace only this package's manifest.
    pub fn persist_envelope(&self, envelope: &RecordingEnvelope) -> Result<PathBuf, InboxError> {
        envelope.validate()?;
        let _guard = self.write_guard()?;
        self.persist_envelope_locked(envelope)
    }

    fn persist_envelope_locked(&self, envelope: &RecordingEnvelope) -> Result<PathBuf, InboxError> {
        self.ensure_storage_boundary()?;
        let package = self.ensure_package_directory(envelope.recording_id)?;
        for child in ["tracks", "derived"] {
            ensure_plain_directory(&package.join(child))?;
        }

        let mut contents = serde_json::to_vec_pretty(envelope)?;
        contents.push(b'\n');
        if contents.len() as u64 > MAX_RECORDING_JSON_BYTES {
            return Err(InboxError::Boundary(
                "recording.json exceeds the durable envelope size limit".to_owned(),
            ));
        }
        let destination = package.join(RECORDING_FILE);
        atomic_replace(&destination, &contents)?;
        Ok(destination)
    }

    /// Mutate one validated manifest while holding the inbox writer lock.
    /// This is the boundary used for user review immediately before enqueue,
    /// so a concurrent gateway transition cannot race the review checkpoint.
    pub fn update_envelope<F>(
        &self,
        recording_id: Uuid,
        update: F,
    ) -> Result<RecordingEnvelope, InboxError>
    where
        F: FnOnce(&mut RecordingEnvelope) -> Result<(), InboxError>,
    {
        let _guard = self.write_guard()?;
        let mut envelope = self.load_envelope_locked(recording_id)?;
        update(&mut envelope)?;
        envelope.validate()?;
        self.persist_envelope_locked(&envelope)?;
        Ok(envelope)
    }

    /// Load persisted JSON through serde and rerun all semantic invariants.
    pub fn load_envelope(&self, recording_id: Uuid) -> Result<RecordingEnvelope, InboxError> {
        let _guard = self.write_guard()?;
        self.load_envelope_locked(recording_id)
    }

    fn load_envelope_locked(&self, recording_id: Uuid) -> Result<RecordingEnvelope, InboxError> {
        self.ensure_storage_boundary()?;
        let package = self.existing_package_directory(recording_id)?;
        let path = package.join(RECORDING_FILE);
        if !path.exists() {
            recover_recording_file(&package, recording_id)?;
        }
        let bytes = read_bounded_regular_file(&path, MAX_RECORDING_JSON_BYTES, "recording.json")?;
        let envelope: RecordingEnvelope = serde_json::from_slice(&bytes)?;
        envelope.validate()?;
        if envelope.recording_id != recording_id {
            return Err(InboxError::RecordingMismatch {
                expected: recording_id,
                actual: envelope.recording_id,
            });
        }
        Ok(envelope)
    }

    /// Append one complete JSON line. A final partial line from a killed write
    /// is discarded (or completed when it is valid) before the next append;
    /// earlier events are parsed and never rewritten.
    pub fn append_event(&self, event: &InboxEvent) -> Result<(), InboxError> {
        event.validate()?;
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let envelope = self.load_envelope_locked(event.recording_id)?;
        if envelope.recording_id != event.recording_id {
            return Err(InboxError::RecordingMismatch {
                expected: envelope.recording_id,
                actual: event.recording_id,
            });
        }
        let package = self.existing_package_directory(event.recording_id)?;
        let path = package.join(EVENTS_FILE);
        let mut encoded = serde_json::to_vec(event)?;
        encoded.push(b'\n');
        if encoded.len() as u64 > MAX_INBOX_EVENT_BYTES {
            return Err(InboxError::InvalidEvent(
                "event exceeds the per-event size limit".to_owned(),
            ));
        }
        reject_symlink_if_present(&path)?;
        let mut file = OpenOptions::new()
            .create(true)
            .read(true)
            .append(true)
            .open(&path)?;
        recover_event_tail(&mut file, event.recording_id)?;
        if file
            .metadata()?
            .len()
            .checked_add(encoded.len() as u64)
            .is_none_or(|length| length > MAX_INBOX_EVENTS_BYTES)
        {
            return Err(InboxError::Boundary(
                "events.ndjson exceeds the durable journal size limit".to_owned(),
            ));
        }
        file.write_all(&encoded)?;
        file.sync_data()?;
        Ok(())
    }

    pub fn load_events(&self, recording_id: Uuid) -> Result<Vec<InboxEvent>, InboxError> {
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let package = self.existing_package_directory(recording_id)?;
        let path = package.join(EVENTS_FILE);
        reject_symlink_if_present(&path)?;
        if !path.exists() {
            return Ok(Vec::new());
        }
        let bytes = read_bounded_regular_file(&path, MAX_INBOX_EVENTS_BYTES, "events.ndjson")?;
        if !bytes.is_empty() && !bytes.ends_with(b"\n") {
            return Err(InboxError::InvalidEvent(
                "events.ndjson ends with an incomplete event".to_owned(),
            ));
        }
        parse_complete_events(&bytes, recording_id)
    }

    /// Hash a regular package file without loading it into memory.
    pub fn hash_package_file(
        &self,
        recording_id: Uuid,
        relative_path: &str,
    ) -> Result<FileDigest, InboxError> {
        validate_relative_path(relative_path, "relative_path")?;
        self.ensure_storage_boundary()?;
        let package = self.existing_package_directory(recording_id)?;
        let path = resolve_regular_package_file(&package, relative_path)?;
        hash_file_streaming(path)
    }

    /// Resolve a package file for streaming only after containment and
    /// symlink checks. Callers should verify its digest immediately before and
    /// after external I/O because another process can still replace a file.
    pub fn package_file_path(
        &self,
        recording_id: Uuid,
        relative_path: &str,
    ) -> Result<PathBuf, InboxError> {
        validate_relative_path(relative_path, "relative_path")?;
        self.ensure_storage_boundary()?;
        let package = self.existing_package_directory(recording_id)?;
        resolve_regular_package_file(&package, relative_path)
    }

    /// Remove only non-canonical capture source tracks after the processing
    /// ledger proves the configured retention grace period has elapsed.
    /// Imported files are their own normalized artifact and are therefore
    /// never selected by this operation.
    pub fn prune_source_tracks(&self, recording_id: Uuid) -> Result<Vec<String>, InboxError> {
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let envelope = self.load_envelope_locked(recording_id)?;
        let normalized_relative = envelope
            .normalized_audio
            .as_deref()
            .ok_or_else(|| InboxError::Boundary("normalized audio is missing".to_owned()))?;
        let normalized_sha256 = envelope
            .normalized_sha256
            .as_deref()
            .ok_or_else(|| InboxError::Boundary("normalized hash is missing".to_owned()))?;
        let package = self.existing_package_directory(recording_id)?;
        let normalized = resolve_regular_package_file(&package, normalized_relative)?;
        if hash_file_streaming(&normalized)?.sha256 != normalized_sha256 {
            return Err(InboxError::SourceChanged);
        }

        let tracks_root = package.join("tracks");
        ensure_plain_directory(&tracks_root)?;
        let canonical_tracks = fs::canonicalize(&tracks_root)?;
        if canonical_tracks.parent() != Some(package.as_path()) {
            return Err(InboxError::Boundary(
                "source track directory escaped the recording package".to_owned(),
            ));
        }
        let mut removed = Vec::new();
        for track in &envelope.tracks {
            if track.relative_path == normalized_relative
                || track.role == super::envelope::TrackRole::Imported
            {
                continue;
            }
            validate_relative_path(&track.relative_path, "track.relative_path")?;
            let path = package.join(&track.relative_path);
            let parent = path
                .parent()
                .ok_or_else(|| InboxError::Boundary("source track has no parent".to_owned()))?;
            let canonical_parent = match fs::canonicalize(parent) {
                Ok(parent) => parent,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(error.into()),
            };
            if canonical_parent != canonical_tracks {
                return Err(InboxError::Boundary(
                    "retention may remove only direct track files".to_owned(),
                ));
            }
            match fs::symlink_metadata(&path) {
                Ok(metadata) => {
                    if metadata.file_type().is_symlink() || !metadata.is_file() {
                        return Err(InboxError::Boundary(
                            "source track is not a plain file".to_owned(),
                        ));
                    }
                    fs::remove_file(&path)?;
                    removed.push(track.relative_path.clone());
                }
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(error.into()),
            }
        }
        File::open(&canonical_tracks)?.sync_all()?;
        Ok(removed)
    }

    /// Permanently remove one app-owned package after the processing ledger
    /// has durably entered `discarding`. The package root itself must be the
    /// expected plain direct child; nested symlinks are unlinked and never
    /// followed.
    pub fn discard_package(&self, recording_id: Uuid) -> Result<bool, InboxError> {
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let package = self.root.join(recording_id.to_string());
        let metadata = match fs::symlink_metadata(&package) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error.into()),
        };
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(InboxError::Boundary(
                "discard target is not an app-owned package directory".to_owned(),
            ));
        }
        if fs::canonicalize(&package)?.parent() != Some(self.root.as_path()) {
            return Err(InboxError::Boundary(
                "discard target escaped the inbox root".to_owned(),
            ));
        }
        remove_tree_without_following_links(&package)?;
        File::open(&self.root)?.sync_all()?;
        Ok(true)
    }

    /// Return durable Ready packages that need no further user review. This
    /// closes the stop/quit crash window: captures are automatically adopted
    /// on launch, while file imports remain held until their review checkpoint.
    pub fn list_ready_for_processing(&self) -> Result<Vec<Uuid>, InboxError> {
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let mut ready = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(recording_id) = Uuid::parse_str(&name) else {
                continue;
            };
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_dir() {
                return Err(InboxError::Boundary(
                    "UUID-named inbox entry is not a plain recording directory".to_owned(),
                ));
            }
            let envelope = self.load_envelope_locked(recording_id)?;
            if envelope.job.state != JobState::Ready
                || envelope.normalized_audio.is_none()
                || (envelope.source.kind == super::envelope::SourceKind::FileImport
                    && envelope
                        .import_review
                        .as_ref()
                        .and_then(|review| review.confirmed_at.as_ref())
                        .is_none())
            {
                continue;
            }
            ready.push(recording_id);
        }
        ready.sort_unstable();
        Ok(ready)
    }

    /// Find the first non-canceled package with this normalized byte identity.
    /// Callers must still verify the referenced file before treating it as a
    /// local dedupe.
    pub fn find_envelope_by_sha256(
        &self,
        sha256: &str,
    ) -> Result<Option<RecordingEnvelope>, InboxError> {
        validate_sha256(sha256, "sha256")?;
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let mut recording_ids = Vec::new();
        for entry in fs::read_dir(&self.root)? {
            let entry = entry?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            let Ok(recording_id) = Uuid::parse_str(&name) else {
                continue;
            };
            let file_type = entry.file_type()?;
            if file_type.is_symlink() || !file_type.is_dir() {
                return Err(InboxError::Boundary(
                    "UUID-named inbox entry is not a plain recording directory".to_owned(),
                ));
            }
            recording_ids.push(recording_id);
        }
        recording_ids.sort_unstable();
        for recording_id in recording_ids {
            let envelope = self.load_envelope_locked(recording_id)?;
            if matches!(
                envelope.job.state,
                JobState::CanceledBeforeUpload | JobState::CanceledAfterUpload
            ) {
                continue;
            }
            if envelope.normalized_sha256.as_deref() == Some(sha256) {
                return Ok(Some(envelope));
            }
        }
        Ok(None)
    }

    /// Stream an already-open source handle into a package file. The copy is
    /// visible only after its expected digest is proven and the file is synced.
    pub fn copy_open_file_into_package(
        &self,
        recording_id: Uuid,
        relative_path: &str,
        source: &mut File,
        maximum_bytes: u64,
        expected_sha256: &str,
    ) -> Result<FileDigest, InboxError> {
        validate_relative_path(relative_path, "relative_path")?;
        validate_sha256(expected_sha256, "expected_sha256")?;
        if maximum_bytes == 0 {
            return Err(InboxError::Boundary(
                "maximum_bytes must be greater than zero".to_owned(),
            ));
        }
        let _guard = self.write_guard()?;
        self.ensure_storage_boundary()?;
        let package = self.ensure_package_directory(recording_id)?;
        for child in ["tracks", "derived"] {
            ensure_plain_directory(&package.join(child))?;
        }
        let destination = package.join(relative_path);
        let parent = destination
            .parent()
            .ok_or_else(|| InboxError::Boundary("package destination has no parent".to_owned()))?;
        let canonical_parent = fs::canonicalize(parent)?;
        if canonical_parent.parent() != Some(package.as_path()) {
            return Err(InboxError::Boundary(
                "package destination must be directly under tracks or derived".to_owned(),
            ));
        }
        reject_symlink_if_present(&destination)?;
        if destination.is_file() {
            let existing = hash_file_streaming(&destination)?;
            if existing.sha256 == expected_sha256
                && existing.size_bytes > 0
                && existing.size_bytes <= maximum_bytes
            {
                return Ok(existing);
            }
        }

        source.seek(SeekFrom::Start(0))?;
        let temporary = parent.join(format!(".import.{}.tmp", Uuid::new_v4()));
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)?;
        let copy_result = (|| -> Result<FileDigest, InboxError> {
            let mut digest = Sha256::new();
            let mut size_bytes = 0_u64;
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let read = source.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                size_bytes = size_bytes
                    .checked_add(read as u64)
                    .ok_or_else(|| InboxError::Boundary("file size overflow".to_owned()))?;
                if size_bytes > maximum_bytes {
                    return Err(InboxError::FileTooLarge { maximum_bytes });
                }
                digest.update(&buffer[..read]);
                output.write_all(&buffer[..read])?;
            }
            if size_bytes == 0 {
                return Err(InboxError::EmptySource);
            }
            let actual_sha256 = hex::encode(digest.finalize());
            if actual_sha256 != expected_sha256 {
                return Err(InboxError::SourceChanged);
            }
            output.sync_all()?;
            Ok(FileDigest {
                sha256: actual_sha256,
                size_bytes,
            })
        })();
        drop(output);
        let copied = match copy_result {
            Ok(copied) => copied,
            Err(error) => {
                let _ = fs::remove_file(&temporary);
                return Err(error);
            }
        };
        if let Err(error) = replace_file(&temporary, &destination) {
            let _ = fs::remove_file(&temporary);
            return Err(error);
        }
        sync_directory(parent)?;
        Ok(copied)
    }

    fn ensure_storage_boundary(&self) -> Result<(), InboxError> {
        reject_symlink(&self.root)?;
        let current_root = fs::canonicalize(&self.root)?;
        if current_root != self.root {
            return Err(InboxError::Boundary(
                "inbox root changed after initialization".to_owned(),
            ));
        }
        let current_archive = resolve_for_comparison(&self.archive_data)?;
        reject_overlap(&current_root, &current_archive)
    }

    fn write_guard(&self) -> Result<std::sync::MutexGuard<'_, ()>, InboxError> {
        self.write_lock.lock().map_err(|_| {
            InboxError::Boundary("inbox write lock was poisoned by a prior failure".to_owned())
        })
    }

    fn ensure_package_directory(&self, recording_id: Uuid) -> Result<PathBuf, InboxError> {
        let package = self.root.join(recording_id.to_string());
        ensure_plain_directory(&package)?;
        verify_direct_child(&self.root, &package)?;
        Ok(package)
    }

    fn existing_package_directory(&self, recording_id: Uuid) -> Result<PathBuf, InboxError> {
        let package = self.root.join(recording_id.to_string());
        reject_symlink(&package)?;
        if !package.is_dir() {
            return Err(InboxError::Boundary(
                "recording package does not exist or is not a directory".to_owned(),
            ));
        }
        verify_direct_child(&self.root, &package)?;
        Ok(package)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InboxEvent {
    pub schema_version: u32,
    pub event_id: Uuid,
    pub recording_id: Uuid,
    pub kind: String,
    pub occurred_at: DateTime<FixedOffset>,
    pub payload: BTreeMap<String, Value>,
}

impl InboxEvent {
    pub fn new(
        recording_id: Uuid,
        kind: impl Into<String>,
        occurred_at: DateTime<FixedOffset>,
        payload: BTreeMap<String, Value>,
    ) -> Result<Self, InboxError> {
        let event = Self {
            schema_version: EVENT_SCHEMA_VERSION,
            event_id: Uuid::new_v4(),
            recording_id,
            kind: kind.into(),
            occurred_at,
            payload,
        };
        event.validate()?;
        Ok(event)
    }

    pub fn validate(&self) -> Result<(), InboxError> {
        if self.schema_version != EVENT_SCHEMA_VERSION {
            return Err(InboxError::InvalidEvent(
                "event schema_version must be 1".to_owned(),
            ));
        }
        if !valid_code(&self.kind) {
            return Err(InboxError::InvalidEvent(
                "event kind must match ^[a-z][a-z0-9_]{1,63}$".to_owned(),
            ));
        }
        let encoded = serde_json::to_vec(self).map_err(|_| {
            InboxError::InvalidEvent("event cannot be encoded as bounded JSON".to_owned())
        })?;
        if encoded.len() as u64 > MAX_INBOX_EVENT_BYTES {
            return Err(InboxError::InvalidEvent(
                "event exceeds the per-event size limit".to_owned(),
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDigest {
    pub sha256: String,
    pub size_bytes: u64,
}

pub fn hash_file_streaming(path: impl AsRef<Path>) -> Result<FileDigest, InboxError> {
    let mut file = File::open(path)?;
    hash_reader_streaming(&mut file)
}

pub fn hash_reader_streaming(reader: &mut impl Read) -> Result<FileDigest, InboxError> {
    let mut digest = Sha256::new();
    let mut size_bytes = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let read = reader.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        digest.update(&buffer[..read]);
        size_bytes = size_bytes
            .checked_add(read as u64)
            .ok_or_else(|| InboxError::Boundary("file size overflow".to_owned()))?;
    }
    Ok(FileDigest {
        sha256: hex::encode(digest.finalize()),
        size_bytes,
    })
}

fn reject_overlap(inbox: &Path, archive: &Path) -> Result<(), InboxError> {
    if inbox == archive || inbox.starts_with(archive) || archive.starts_with(inbox) {
        Err(InboxError::Boundary(
            "app inbox and archive data directory must be disjoint".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn resolve_for_comparison(path: &Path) -> Result<PathBuf, InboxError> {
    let absolute = if path.is_absolute() {
        normalize_absolute(path)?
    } else {
        normalize_absolute(&std::env::current_dir()?.join(path))?
    };
    let mut ancestor = absolute.as_path();
    let mut missing = Vec::new();
    while !ancestor.exists() {
        let name = ancestor.file_name().ok_or_else(|| {
            InboxError::Boundary("path has no resolvable existing ancestor".to_owned())
        })?;
        missing.push(name.to_owned());
        ancestor = ancestor.parent().ok_or_else(|| {
            InboxError::Boundary("path has no resolvable existing ancestor".to_owned())
        })?;
    }
    let mut resolved = fs::canonicalize(ancestor)?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn normalize_absolute(path: &Path) -> Result<PathBuf, InboxError> {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => normalized.push(prefix.as_os_str()),
            Component::RootDir => normalized.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    return Err(InboxError::Boundary("path escapes its root".to_owned()));
                }
            }
            Component::Normal(part) => normalized.push(part),
        }
    }
    if normalized.is_absolute() {
        Ok(normalized)
    } else {
        Err(InboxError::Boundary(
            "path must resolve absolutely".to_owned(),
        ))
    }
}

fn ensure_plain_directory(path: &Path) -> Result<(), InboxError> {
    if path.exists() {
        reject_symlink(path)?;
        if !path.is_dir() {
            return Err(InboxError::Boundary(
                "inbox path component is not a directory".to_owned(),
            ));
        }
    } else {
        fs::create_dir(path)?;
    }
    Ok(())
}

fn reject_symlink(path: &Path) -> Result<(), InboxError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        Err(InboxError::Boundary(
            "symbolic links are not allowed in the inbox".to_owned(),
        ))
    } else {
        Ok(())
    }
}

fn reject_symlink_if_present(path: &Path) -> Result<(), InboxError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_symlink() => Err(InboxError::Boundary(
            "symbolic links are not allowed in the inbox".to_owned(),
        )),
        Ok(_) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_tree_without_following_links(path: &Path) -> Result<(), InboxError> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || metadata.is_file() {
        fs::remove_file(path)?;
        return Ok(());
    }
    if !metadata.is_dir() {
        return Err(InboxError::Boundary(
            "discard encountered an unsupported filesystem entry".to_owned(),
        ));
    }
    for entry in fs::read_dir(path)? {
        remove_tree_without_following_links(&entry?.path())?;
    }
    fs::remove_dir(path)?;
    Ok(())
}

fn verify_direct_child(root: &Path, package: &Path) -> Result<(), InboxError> {
    let canonical = fs::canonicalize(package)?;
    if canonical.parent() == Some(root) {
        Ok(())
    } else {
        Err(InboxError::Boundary(
            "recording package escaped the inbox root".to_owned(),
        ))
    }
}

fn resolve_regular_package_file(package: &Path, relative: &str) -> Result<PathBuf, InboxError> {
    let mut current = package.to_path_buf();
    for component in relative.split('/') {
        current.push(component);
        reject_symlink(&current)?;
    }
    let canonical_package = fs::canonicalize(package)?;
    let canonical_file = fs::canonicalize(&current)?;
    if !canonical_file.starts_with(&canonical_package) || !canonical_file.is_file() {
        return Err(InboxError::Boundary(
            "package path is not a contained regular file".to_owned(),
        ));
    }
    Ok(canonical_file)
}

fn atomic_replace(destination: &Path, contents: &[u8]) -> Result<(), InboxError> {
    reject_symlink_if_present(destination)?;
    let directory = destination
        .parent()
        .ok_or_else(|| InboxError::Boundary("atomic destination has no parent".to_owned()))?;
    let temporary = directory.join(format!(".{RECORDING_FILE}.{}.tmp", Uuid::new_v4()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)?;
    file.write_all(contents)?;
    file.sync_all()?;
    drop(file);
    replace_file(&temporary, destination)?;
    sync_directory(directory)?;
    Ok(())
}

fn recover_recording_file(package: &Path, recording_id: Uuid) -> Result<(), InboxError> {
    let mut candidates = Vec::new();
    for entry in fs::read_dir(package)? {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            continue;
        };
        if !name.starts_with(&format!(".{RECORDING_FILE}.")) || !name.ends_with(".tmp") {
            continue;
        }
        let path = entry.path();
        reject_symlink(&path)?;
        if !entry.file_type()?.is_file() {
            continue;
        }
        if entry.metadata()?.len() > MAX_RECORDING_JSON_BYTES {
            continue;
        }
        let bytes = match read_bounded_regular_file(
            &path,
            MAX_RECORDING_JSON_BYTES,
            "recording.json recovery candidate",
        ) {
            Ok(bytes) => bytes,
            Err(InboxError::Boundary(_)) => continue,
            Err(error) => return Err(error),
        };
        let Ok(envelope) = serde_json::from_slice::<RecordingEnvelope>(&bytes) else {
            continue;
        };
        if envelope.recording_id != recording_id || envelope.validate().is_err() {
            continue;
        }
        let modified = entry
            .metadata()?
            .modified()
            .unwrap_or(std::time::UNIX_EPOCH);
        candidates.push((modified, path));
    }
    candidates.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let (_, candidate) = candidates.pop().ok_or_else(|| {
        InboxError::Boundary("recording.json is missing and no valid atomic temp exists".to_owned())
    })?;
    replace_file(&candidate, &package.join(RECORDING_FILE))?;
    sync_directory(package)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> Result<(), InboxError> {
    fs::rename(source, destination)?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> Result<(), InboxError> {
    use std::os::windows::ffi::OsStrExt;

    const MOVEFILE_REPLACE_EXISTING: u32 = 0x1;
    const MOVEFILE_WRITE_THROUGH: u32 = 0x8;
    #[link(name = "kernel32")]
    extern "system" {
        fn MoveFileExW(existing: *const u16, new_name: *const u16, flags: u32) -> i32;
    }
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(InboxError::Io(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(unix)]
fn sync_directory(directory: &Path) -> Result<(), InboxError> {
    File::open(directory)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_directory: &Path) -> Result<(), InboxError> {
    Ok(())
}

fn recover_event_tail(file: &mut File, recording_id: Uuid) -> Result<(), InboxError> {
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(InboxError::Boundary(
            "events.ndjson must be a regular file".to_owned(),
        ));
    }
    if metadata.len() > MAX_INBOX_EVENTS_BYTES {
        return Err(InboxError::Boundary(
            "events.ndjson exceeds the durable journal size limit".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    Read::by_ref(file)
        .take(MAX_INBOX_EVENTS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_INBOX_EVENTS_BYTES {
        return Err(InboxError::Boundary(
            "events.ndjson exceeds the durable journal size limit".to_owned(),
        ));
    }
    if bytes.is_empty() || bytes.ends_with(b"\n") {
        parse_complete_events(&bytes, recording_id)?;
        return Ok(());
    }

    let complete_length = bytes
        .iter()
        .rposition(|byte| *byte == b'\n')
        .map_or(0, |index| index + 1);
    parse_complete_events(&bytes[..complete_length], recording_id)?;
    let tail = &bytes[complete_length..];
    match serde_json::from_slice::<InboxEvent>(tail) {
        Ok(event) if event.recording_id == recording_id && event.validate().is_ok() => {
            file.write_all(b"\n")?;
        }
        _ => {
            file.set_len(complete_length as u64)?;
        }
    }
    file.sync_data()?;
    Ok(())
}

fn read_bounded_regular_file(
    path: &Path,
    maximum_bytes: u64,
    label: &'static str,
) -> Result<Vec<u8>, InboxError> {
    let path_metadata = fs::symlink_metadata(path)?;
    if path_metadata.file_type().is_symlink() || !path_metadata.is_file() {
        return Err(InboxError::Boundary(format!(
            "{label} must be a regular file"
        )));
    }
    if path_metadata.len() > maximum_bytes {
        return Err(InboxError::Boundary(format!(
            "{label} exceeds its durable size limit"
        )));
    }
    let file = File::open(path)?;
    let opened_metadata = file.metadata()?;
    if !opened_metadata.is_file() {
        return Err(InboxError::Boundary(format!(
            "{label} must remain a regular file"
        )));
    }
    if opened_metadata.len() > maximum_bytes {
        return Err(InboxError::Boundary(format!(
            "{label} exceeds its durable size limit"
        )));
    }
    let mut bytes = Vec::with_capacity(
        usize::try_from(opened_metadata.len().min(maximum_bytes)).unwrap_or_default(),
    );
    file.take(maximum_bytes + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > maximum_bytes {
        return Err(InboxError::Boundary(format!(
            "{label} exceeds its durable size limit"
        )));
    }
    Ok(bytes)
}

fn parse_complete_events(bytes: &[u8], recording_id: Uuid) -> Result<Vec<InboxEvent>, InboxError> {
    let mut events = Vec::new();
    for (index, line) in bytes.split(|byte| *byte == b'\n').enumerate() {
        if line.is_empty() {
            continue;
        }
        let event: InboxEvent = serde_json::from_slice(line).map_err(|error| {
            InboxError::InvalidEvent(format!(
                "events.ndjson line {} is invalid: {error}",
                index + 1
            ))
        })?;
        event.validate()?;
        if event.recording_id != recording_id {
            return Err(InboxError::RecordingMismatch {
                expected: recording_id,
                actual: event.recording_id,
            });
        }
        events.push(event);
    }
    Ok(events)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use chrono::DateTime;
    use serde_json::json;
    use tempfile::TempDir;

    use super::*;

    const SHA_A: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn envelope(recording_id: Uuid) -> RecordingEnvelope {
        serde_json::from_value(json!({
            "schema_version": 1,
            "recording_id": recording_id,
            "source": {
                "kind": "file_import",
                "platform": "macos",
                "label": null,
                "capture_scope": "imported_file"
            },
            "captured_at": "2026-09-02T09:00:00-07:00",
            "ended_at": "2026-09-02T09:00:01-07:00",
            "duration_ms": 1_000,
            "tracks": [{
                "role": "imported",
                "relative_path": "tracks/source.wav",
                "codec": "pcm_s16le",
                "sample_rate": 48_000,
                "channels": 1,
                "duration_ms": 1_000,
                "clock_start_ns": 0,
                "sha256": SHA_A
            }],
            "normalized_audio": null,
            "normalized_sha256": null,
            "imported_name": "source.wav",
            "capture_warnings": [],
            "job": {
                "state": "ready",
                "attempt": 0,
                "remote_job_id": null,
                "last_error": null
            }
        }))
        .unwrap()
    }

    fn inbox(temp: &TempDir) -> Inbox {
        let archive = temp.path().join("archive-data");
        fs::create_dir(&archive).unwrap();
        Inbox::open(temp.path().join("app-data"), archive).unwrap()
    }

    #[test]
    fn atomically_persists_and_revalidates_recording_json() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        let original = envelope(id);
        let path = inbox.persist_envelope(&original).unwrap();

        fs::write(
            path.parent().unwrap().join(".recording.json.crash.tmp"),
            b"{",
        )
        .unwrap();
        assert_eq!(inbox.load_envelope(id).unwrap(), original);

        let mut invalid = original.clone();
        invalid.imported_name = None;
        assert!(inbox.persist_envelope(&invalid).is_err());
        assert_eq!(inbox.load_envelope(id).unwrap(), original);
    }

    #[test]
    fn load_envelope_rejects_oversized_json_before_parsing() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        let path = inbox.persist_envelope(&envelope(id)).unwrap();
        fs::write(&path, vec![b' '; MAX_RECORDING_JSON_BYTES as usize + 1]).unwrap();

        assert!(matches!(
            inbox.load_envelope(id).unwrap_err(),
            InboxError::Boundary(_)
        ));
        assert_eq!(
            fs::metadata(path).unwrap().len(),
            MAX_RECORDING_JSON_BYTES + 1
        );
    }

    #[test]
    fn recovers_a_fully_written_atomic_temp_after_a_pre_rename_crash() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        let original = envelope(id);
        let path = inbox.persist_envelope(&original).unwrap();
        let recovery = path
            .parent()
            .unwrap()
            .join(format!(".{RECORDING_FILE}.{}.tmp", Uuid::new_v4()));
        fs::rename(&path, &recovery).unwrap();

        assert_eq!(inbox.load_envelope(id).unwrap(), original);
        assert!(path.exists());
        assert!(!recovery.exists());
    }

    #[test]
    fn appends_events_and_recovers_only_an_incomplete_tail() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let at = DateTime::parse_from_rfc3339("2026-09-02T09:00:01-07:00").unwrap();
        let first = InboxEvent::new(id, "file_imported", at, BTreeMap::new()).unwrap();
        inbox.append_event(&first).unwrap();

        let events_path = inbox.root().join(id.to_string()).join(EVENTS_FILE);
        OpenOptions::new()
            .append(true)
            .open(&events_path)
            .unwrap()
            .write_all(b"{\"truncated\":")
            .unwrap();
        let second = InboxEvent::new(id, "state_transition", at, BTreeMap::new()).unwrap();
        inbox.append_event(&second).unwrap();

        assert_eq!(inbox.load_events(id).unwrap(), vec![first, second]);
    }

    #[test]
    fn event_boundaries_reject_oversized_payloads_and_journals() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let at = DateTime::parse_from_rfc3339("2026-09-02T09:00:01-07:00").unwrap();
        let payload = BTreeMap::from([(
            "message".to_owned(),
            Value::String("x".repeat(MAX_INBOX_EVENT_BYTES as usize)),
        )]);
        let payload_error = match InboxEvent::new(id, "file_imported", at, payload) {
            Ok(_) => panic!("oversized inbox event payload unexpectedly accepted"),
            Err(error) => error,
        };
        assert!(matches!(payload_error, InboxError::InvalidEvent(_)));

        let events_path = inbox.root().join(id.to_string()).join(EVENTS_FILE);
        fs::write(
            &events_path,
            vec![b'\n'; MAX_INBOX_EVENTS_BYTES as usize + 1],
        )
        .unwrap();
        assert!(matches!(
            inbox.load_events(id).unwrap_err(),
            InboxError::Boundary(_)
        ));
        assert_eq!(
            fs::metadata(events_path).unwrap().len(),
            MAX_INBOX_EVENTS_BYTES + 1
        );
    }

    #[test]
    fn hashes_package_files_in_streaming_chunks() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let track = inbox.root().join(id.to_string()).join("tracks/source.wav");
        fs::write(track, b"abc").unwrap();

        let digest = inbox.hash_package_file(id, "tracks/source.wav").unwrap();
        assert_eq!(digest.size_bytes, 3);
        assert_eq!(
            digest.sha256,
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
        assert!(inbox.hash_package_file(id, "../archive/secret").is_err());
    }

    #[test]
    fn refuses_to_place_the_inbox_inside_archive_data() {
        let temp = TempDir::new().unwrap();
        let archive = temp.path().join("data");
        fs::create_dir(&archive).unwrap();
        let app_data = archive.join("client");

        assert!(Inbox::open(&app_data, &archive).is_err());
        assert!(!app_data.exists());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_symlinked_package_files() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let outside = temp.path().join("outside.wav");
        fs::write(&outside, b"private").unwrap();
        let track = inbox.root().join(id.to_string()).join("tracks/source.wav");
        symlink(outside, track).unwrap();

        assert!(inbox.hash_package_file(id, "tracks/source.wav").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn refuses_a_broken_event_symlink_without_writing_its_target() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let outside = temp.path().join("outside-events.ndjson");
        let event_path = inbox.root().join(id.to_string()).join(EVENTS_FILE);
        symlink(&outside, event_path).unwrap();
        let at = DateTime::parse_from_rfc3339("2026-09-02T09:00:01-07:00").unwrap();
        let event = InboxEvent::new(id, "file_imported", at, BTreeMap::new()).unwrap();

        assert!(inbox.append_event(&event).is_err());
        assert!(!outside.exists());
    }

    #[cfg(unix)]
    #[test]
    fn discard_removes_only_the_direct_package_and_never_follows_links() {
        use std::os::unix::fs::symlink;

        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let id = Uuid::new_v4();
        inbox.persist_envelope(&envelope(id)).unwrap();
        let outside = temp.path().join("outside-private.wav");
        fs::write(&outside, b"must survive").unwrap();
        symlink(
            &outside,
            inbox.root().join(id.to_string()).join("derived/linked.wav"),
        )
        .unwrap();

        assert!(inbox.discard_package(id).unwrap());
        assert_eq!(fs::read(&outside).unwrap(), b"must survive");
        assert!(!inbox.root().join(id.to_string()).exists());
        assert!(!inbox.discard_package(id).unwrap());
    }

    #[test]
    fn ready_scan_auto_adopts_capture_but_holds_unreviewed_import() {
        let temp = TempDir::new().unwrap();
        let inbox = inbox(&temp);
        let capture_id = Uuid::new_v4();
        let mut capture = envelope(capture_id);
        capture.source.kind = super::super::envelope::SourceKind::DesktopVoiceMemo;
        capture.source.capture_scope = super::super::envelope::CaptureScope::Microphone;
        capture.tracks[0].role = super::super::envelope::TrackRole::Microphone;
        capture.normalized_audio = Some("tracks/source.wav".to_owned());
        capture.normalized_sha256 = Some(SHA_A.to_owned());
        capture.imported_name = None;
        inbox.persist_envelope(&capture).unwrap();

        let import_id = Uuid::new_v4();
        let mut unreviewed_import = envelope(import_id);
        unreviewed_import.normalized_audio = Some("tracks/source.wav".to_owned());
        unreviewed_import.normalized_sha256 = Some(SHA_A.to_owned());
        inbox.persist_envelope(&unreviewed_import).unwrap();

        assert_eq!(inbox.list_ready_for_processing().unwrap(), [capture_id]);
    }
}
