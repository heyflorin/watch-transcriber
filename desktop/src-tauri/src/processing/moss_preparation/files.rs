//! Create-only PCM publication and descriptor-bound source verification.
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

use sha2::{Digest, Sha256};

use super::{fail, window_path, PreparedMossWindow, ProcessingError, SourceAudioIdentity, Uuid};

pub(super) struct VerifiedSource {
    file: File,
    root: PathBuf,
    path: PathBuf,
    identity: SourceAudioIdentity,
}

impl VerifiedSource {
    pub(super) fn open(
        root: &Path,
        identity: &SourceAudioIdentity,
        cancel: &AtomicBool,
    ) -> Result<Self, ProcessingError> {
        let path = resolve_file(root, &identity.relative_path)?;
        let file = private_options()
            .read(true)
            .open(&path)
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        let mut source = Self {
            file,
            root: root.to_path_buf(),
            path,
            identity: identity.clone(),
        };
        source.verify(cancel)?;
        Ok(source)
    }

    pub(super) fn file(&self) -> &File {
        &self.file
    }

    pub(super) fn cloned_file(&mut self) -> Result<File, ProcessingError> {
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        self.file
            .try_clone()
            .map_err(|_| fail("moss_preparation_source_unavailable"))
    }

    pub(super) fn verify(&mut self, cancel: &AtomicBool) -> Result<(), ProcessingError> {
        check_cancel(cancel)?;
        if resolve_file(&self.root, &self.identity.relative_path)? != self.path {
            return Err(fail("unsafe_moss_preparation_path"));
        }
        let before = self
            .file
            .metadata()
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        let current = fs::symlink_metadata(&self.path)
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        if !same_regular_file(&before, &current) || before.len() != self.identity.size_bytes {
            return Err(fail("moss_preparation_source_changed"));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        let digest = bounded_hash(&mut self.file, self.identity.size_bytes, cancel)?;
        let after = fs::symlink_metadata(&self.path)
            .map_err(|_| fail("moss_preparation_source_changed"))?;
        if digest != self.identity.sha256
            || !same_regular_file(&before, &after)
            || after.len() != before.len()
        {
            return Err(fail("moss_preparation_source_changed"));
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        Ok(())
    }
}

pub(super) fn verify_window(
    root: &Path,
    window: &PreparedMossWindow,
    cancel: &AtomicBool,
) -> Result<(), ProcessingError> {
    let path = resolve_file(root, &window.relative_path)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| fail("moss_preparation_file_unavailable"))?;
    require_private(&metadata)?;
    let source = SourceAudioIdentity {
        relative_path: window.relative_path.clone(),
        sha256: window.sha256.clone(),
        size_bytes: window.size_bytes,
        duration_ms: (window.end_frame - window.start_frame).div_ceil(16),
    };
    let mut verified = VerifiedSource::open(root, &source, cancel)?;
    let mut header = [0_u8; 44];
    verified
        .file
        .read_exact(&mut header)
        .map_err(|_| fail("moss_preparation_file_unavailable"))?;
    if header != wav_header(window.end_frame - window.start_frame)? {
        return Err(fail("moss_preparation_file_conflict"));
    }
    Ok(())
}

pub(super) fn publish_window(
    root: &Path,
    recording: Uuid,
    generation: Uuid,
    index: usize,
    start_frame: u64,
    samples: &[i16],
    cancel: &AtomicBool,
) -> Result<PreparedMossWindow, ProcessingError> {
    check_cancel(cancel)?;
    let relative_path = window_path(recording, generation, index);
    let destination = destination(root, recording, &relative_path)?;
    let frames = samples.len() as u64;
    if frames == 0 || frames > echowall_local_moss_protocol::windows::MAX_WINDOW_FRAMES {
        return Err(fail("invalid_moss_preparation"));
    }
    let header = wav_header(frames)?;
    // Hash the exact versioned PCM bytes, not an approximate duration.
    let mut digest = Sha256::new();
    digest.update(header);
    for chunk in samples.chunks(4096) {
        check_cancel(cancel)?;
        for sample in chunk {
            digest.update(sample.to_le_bytes());
        }
    }
    let window = PreparedMossWindow {
        index,
        start_frame,
        end_frame: start_frame + frames,
        relative_path,
        sha256: hex::encode(digest.finalize()),
        size_bytes: frames * 2 + 44,
    };
    window.validate(recording, generation)?;
    if fs::symlink_metadata(&destination).is_ok() {
        verify_window(root, &window, cancel).map_err(|error| {
            if error.code == "moss_preparation_cancelled" {
                error
            } else {
                fail("moss_preparation_file_conflict")
            }
        })?;
        return Ok(window);
    }
    let parent = destination
        .parent()
        .ok_or_else(|| fail("unsafe_moss_preparation_path"))?;
    let pending_path = parent.join(format!(".moss_{generation}_{}.pending", Uuid::new_v4()));
    let mut file = private_options()
        .write(true)
        .create_new(true)
        .open(&pending_path)
        .map_err(|_| fail("moss_preparation_file_unavailable"))?;
    let pending = Pending(pending_path);
    file.write_all(&header)
        .map_err(|_| fail("moss_preparation_file_unavailable"))?;
    let mut bytes = Vec::with_capacity(8192);
    for chunk in samples.chunks(4096) {
        check_cancel(cancel)?;
        bytes.clear();
        for sample in chunk {
            bytes.extend(sample.to_le_bytes());
        }
        file.write_all(&bytes)
            .map_err(|_| fail("moss_preparation_file_unavailable"))?;
    }
    file.sync_all()
        .map_err(|_| fail("moss_preparation_file_unavailable"))?;
    drop(file);
    check_cancel(cancel)?;
    let checked_destination = destination_path(root, &window.relative_path)?;
    if checked_destination != destination {
        return Err(fail("unsafe_moss_preparation_path"));
    }
    match fs::hard_link(&pending.0, &destination) {
        Ok(()) => sync_directory(parent)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(fail("moss_preparation_file_unavailable")),
    }
    drop(pending);
    sync_directory(parent)?;
    verify_window(root, &window, cancel).map_err(|error| {
        if error.code == "moss_preparation_cancelled" {
            error
        } else {
            fail("moss_preparation_file_conflict")
        }
    })?;
    Ok(window)
}

fn bounded_hash(
    file: &mut File,
    expected_bytes: u64,
    cancel: &AtomicBool,
) -> Result<String, ProcessingError> {
    let mut digest = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        check_cancel(cancel)?;
        let read = file
            .read(&mut buffer)
            .map_err(|_| fail("moss_preparation_source_unavailable"))?;
        if read == 0 {
            break;
        }
        total = total
            .checked_add(read as u64)
            .ok_or_else(|| fail("moss_preparation_source_changed"))?;
        if total > expected_bytes {
            return Err(fail("moss_preparation_source_changed"));
        }
        digest.update(&buffer[..read]);
    }
    if total != expected_bytes {
        return Err(fail("moss_preparation_source_changed"));
    }
    Ok(hex::encode(digest.finalize()))
}

fn same_regular_file(left: &Metadata, right: &Metadata) -> bool {
    if !left.is_file() || !right.is_file() || right.file_type().is_symlink() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        left.dev() == right.dev() && left.ino() == right.ino()
    }
    #[cfg(not(unix))]
    {
        left.len() == right.len()
    }
}

fn require_private(metadata: &Metadata) -> Result<(), ProcessingError> {
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        return Err(fail("unsafe_moss_preparation_path"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if metadata.permissions().mode() & 0o077 != 0 {
            return Err(fail("unsafe_moss_preparation_path"));
        }
    }
    Ok(())
}

fn private_options() -> OpenOptions {
    let mut options = OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options
}

fn destination(root: &Path, recording: Uuid, relative: &str) -> Result<PathBuf, ProcessingError> {
    let package = format!("inbox/{recording}");
    let package_path = checked_directory(root, &package)?;
    let derived = package_path.join("derived");
    if !derived.exists() {
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&derived) {
            Ok(()) => sync_directory(&package_path)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(fail("moss_preparation_file_unavailable")),
        }
    }
    destination_path(root, relative)
}

fn destination_path(root: &Path, relative: &str) -> Result<PathBuf, ProcessingError> {
    let path = Path::new(relative);
    let parent = path
        .parent()
        .and_then(|path| path.to_str())
        .ok_or_else(|| fail("unsafe_moss_preparation_path"))?;
    let name = path
        .file_name()
        .ok_or_else(|| fail("unsafe_moss_preparation_path"))?;
    Ok(checked_directory(root, parent)?.join(name))
}

fn resolve_file(root: &Path, relative: &str) -> Result<PathBuf, ProcessingError> {
    let path = destination_path(root, relative)?;
    let metadata =
        fs::symlink_metadata(&path).map_err(|_| fail("moss_preparation_source_unavailable"))?;
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(fail("unsafe_moss_preparation_path"));
    }
    Ok(path)
}

fn checked_directory(root: &Path, relative: &str) -> Result<PathBuf, ProcessingError> {
    let metadata = fs::symlink_metadata(root).map_err(|_| fail("unsafe_moss_preparation_path"))?;
    if !root.is_absolute()
        || metadata.file_type().is_symlink()
        || !metadata.is_dir()
        || fs::canonicalize(root).map_err(|_| fail("unsafe_moss_preparation_path"))? != root
    {
        return Err(fail("unsafe_moss_preparation_path"));
    }
    let mut path = root.to_path_buf();
    for component in Path::new(relative).components() {
        let Component::Normal(component) = component else {
            return Err(fail("unsafe_moss_preparation_path"));
        };
        path.push(component);
        let metadata =
            fs::symlink_metadata(&path).map_err(|_| fail("unsafe_moss_preparation_path"))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            return Err(fail("unsafe_moss_preparation_path"));
        }
    }
    if fs::canonicalize(&path).map_err(|_| fail("unsafe_moss_preparation_path"))? != path {
        return Err(fail("unsafe_moss_preparation_path"));
    }
    Ok(path)
}

fn wav_header(frames: u64) -> Result<[u8; 44], ProcessingError> {
    let bytes = u32::try_from(
        frames
            .checked_mul(2)
            .ok_or_else(|| fail("invalid_moss_preparation"))?,
    )
    .map_err(|_| fail("invalid_moss_preparation"))?;
    let mut header = [0_u8; 44];
    header[0..4].copy_from_slice(b"RIFF");
    header[4..8].copy_from_slice(&(bytes + 36).to_le_bytes());
    header[8..16].copy_from_slice(b"WAVEfmt ");
    header[16..20].copy_from_slice(&16_u32.to_le_bytes());
    header[20..22].copy_from_slice(&1_u16.to_le_bytes());
    header[22..24].copy_from_slice(&1_u16.to_le_bytes());
    header[24..28].copy_from_slice(&16_000_u32.to_le_bytes());
    header[28..32].copy_from_slice(&32_000_u32.to_le_bytes());
    header[32..34].copy_from_slice(&2_u16.to_le_bytes());
    header[34..36].copy_from_slice(&16_u16.to_le_bytes());
    header[36..40].copy_from_slice(b"data");
    header[40..44].copy_from_slice(&bytes.to_le_bytes());
    Ok(header)
}

pub(super) fn check_cancel(cancel: &AtomicBool) -> Result<(), ProcessingError> {
    if cancel.load(Ordering::Acquire) {
        Err(fail("moss_preparation_cancelled"))
    } else {
        Ok(())
    }
}
fn sync_directory(path: &Path) -> Result<(), ProcessingError> {
    File::open(path)
        .and_then(|file| file.sync_all())
        .map_err(|_| fail("moss_preparation_file_unavailable"))
}
struct Pending(PathBuf);
impl Drop for Pending {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
