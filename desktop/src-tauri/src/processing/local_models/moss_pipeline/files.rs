use super::*;
use std::{
    fs::{self, File, Metadata, OpenOptions},
    io::{Read, Seek, SeekFrom},
    time::SystemTime,
};

#[derive(Clone, PartialEq, Eq)]
struct FileStamp {
    size: u64,
    modified: Option<SystemTime>,
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
    #[cfg(unix)]
    changed: (i64, i64),
}
impl FileStamp {
    fn new(metadata: &Metadata) -> Result<Self, LocalModelError> {
        if !metadata.is_file() || metadata.file_type().is_symlink() {
            return Err(fail("model_path_rejected"));
        }
        #[cfg(unix)]
        use std::os::unix::fs::MetadataExt;
        Ok(Self {
            size: metadata.len(),
            modified: metadata.modified().ok(),
            #[cfg(unix)]
            device: metadata.dev(),
            #[cfg(unix)]
            inode: metadata.ino(),
            #[cfg(unix)]
            changed: (metadata.ctime(), metadata.ctime_nsec()),
        })
    }
}

#[derive(Clone)]
pub(super) struct CachedFile {
    stamp: FileStamp,
    expected_sha256: String,
    verified: bool,
}

pub(super) fn verified_file(
    work: &Work,
    file: &CatalogFile,
    force: bool,
) -> Result<bool, LocalModelError> {
    work.check_cancel()?;
    let Some(path) = resolve_install_file(&work.root, &file.install_path, false)? else {
        work.with_state(|state| {
            state.cache.remove(&file.install_path);
        })?;
        return Ok(false);
    };
    let stamp =
        FileStamp::new(&fs::symlink_metadata(&path).map_err(|_| fail("model_state_unavailable"))?)?;
    if !force {
        if let Some(cached) =
            work.with_state(|state| state.cache.get(&file.install_path).cloned())?
        {
            if cached.stamp == stamp && cached.expected_sha256 == file.sha256 {
                return Ok(cached.verified && stamp.size == file.size_bytes);
            }
        }
    }
    let verified = if stamp.size == file.size_bytes {
        #[cfg(test)]
        work.with_state(|state| state.hashed_files += 1)?;
        let mut input = open_regular(&path)?;
        let opened = FileStamp::new(
            &input
                .metadata()
                .map_err(|_| fail("model_state_unavailable"))?,
        )?;
        if stamp != opened {
            return Err(fail("model_verification_failed"));
        }
        let (digest, size) = hash_prefix(&mut input, file.size_bytes, work)?;
        let resolved = resolve_install_file(&work.root, &file.install_path, true)?
            .ok_or_else(|| fail("model_path_rejected"))?;
        let after = FileStamp::new(
            &fs::symlink_metadata(&resolved).map_err(|_| fail("model_state_unavailable"))?,
        )?;
        if path != resolved || stamp != after {
            return Err(fail("model_verification_failed"));
        }
        size == file.size_bytes && hex::encode(digest.finalize()) == file.sha256
    } else {
        false
    };
    work.with_state(|state| {
        state.cache.insert(
            file.install_path.clone(),
            CachedFile {
                stamp,
                expected_sha256: file.sha256.clone(),
                verified,
            },
        );
    })?;
    Ok(verified)
}

fn component_ready(
    work: &Work,
    component: &ComponentPack,
    force: bool,
) -> Result<(bool, u64), LocalModelError> {
    let mut installed = true;
    let mut staged = 0_u64;
    for entry in &component.files {
        work.check_cancel()?;
        if verified_file(work, &entry.file, force)? {
            staged = staged.saturating_add(entry.file.size_bytes);
        } else {
            installed = false;
            // Presence is not proof, but retained unverified bytes must still
            // expose the explicit remove action instead of a retry dead end.
            if let Some(path) = resolve_install_file(&work.root, &entry.file.install_path, false)? {
                staged = staged.saturating_add(
                    fs::metadata(path)
                        .map_err(|_| fail("model_state_unavailable"))?
                        .len()
                        .min(entry.file.size_bytes),
                );
            }
            let partial = partial_path(&work.root.join(&entry.file.install_path))?;
            validate_partial(&partial, entry.file.size_bytes)?;
            if let Ok(metadata) = fs::symlink_metadata(&partial) {
                staged = staged.saturating_add(metadata.len());
            }
        }
    }
    if installed {
        installed =
            verify_exact_catalog_paths(&work.root, &component.plain_files(), &component.prefix)?;
    }
    work.with_state(|state| {
        state.component_installed.insert(component.id, installed);
    })?;
    Ok((installed, staged.min(component.total_bytes())))
}

pub(super) fn scan(work: &Work, force: bool) -> Result<(), LocalModelError> {
    let mut installed = true;
    let mut staged = 0_u64;
    for component in &work.catalog.components {
        let (ready, component_bytes) = component_ready(work, component, force)?;
        installed &= ready;
        staged = staged.saturating_add(component_bytes);
    }
    work.check_cancel()?;
    work.with_state(|state| {
        state.installed = installed;
        state.downloaded_bytes = staged.min(work.catalog.total_bytes());
    })
}

pub(super) fn verify_component(
    work: &Work,
    component: &ComponentPack,
    force: bool,
) -> Result<(), LocalModelError> {
    if component_ready(work, component, force)?.0 {
        Ok(())
    } else {
        Err(fail("model_verification_failed"))
    }
}

pub(super) fn partial_prefix(
    work: &Work,
    partial: &Path,
    file: &CatalogFile,
) -> Result<(Sha256, u64), LocalModelError> {
    work.check_cancel()?;
    validate_partial(partial, file.size_bytes)?;
    if !partial.exists() {
        return Ok((Sha256::new(), 0));
    }
    let before =
        FileStamp::new(&fs::symlink_metadata(partial).map_err(|_| fail("model_path_rejected"))?)?;
    let mut input = open_regular(partial)?;
    if FileStamp::new(&input.metadata().map_err(|_| fail("model_path_rejected"))?)? != before {
        return Err(fail("model_path_rejected"));
    }
    let (digest, size) = hash_prefix(&mut input, file.size_bytes, work)?;
    if FileStamp::new(&fs::symlink_metadata(partial).map_err(|_| fail("model_path_rejected"))?)?
        != before
    {
        return Err(fail("model_verification_failed"));
    }
    if size == file.size_bytes && hex::encode(digest.clone().finalize()) != file.sha256 {
        return Err(fail("model_verification_failed"));
    }
    Ok((digest, size))
}

fn open_regular(path: &Path) -> Result<File, LocalModelError> {
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    options.open(path).map_err(|_| fail("model_path_rejected"))
}

fn hash_prefix(
    input: &mut File,
    maximum: u64,
    work: &Work,
) -> Result<(Sha256, u64), LocalModelError> {
    input
        .seek(SeekFrom::Start(0))
        .map_err(|_| fail("model_state_unavailable"))?;
    let mut digest = Sha256::new();
    let mut size = 0_u64;
    let mut bytes = [0_u8; 64 * 1024];
    loop {
        work.check_cancel()?;
        let count = input
            .read(&mut bytes)
            .map_err(|_| fail("model_state_unavailable"))?;
        if count == 0 {
            break;
        }
        size = size
            .checked_add(count as u64)
            .ok_or_else(|| fail("model_download_rejected"))?;
        if size > maximum {
            return Err(fail("model_download_rejected"));
        }
        digest.update(&bytes[..count]);
    }
    Ok((digest, size))
}
