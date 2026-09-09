use sha2::{Digest, Sha256};
use std::os::unix::fs::{MetadataExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Seek, SeekFrom},
    path::{Component, Path, PathBuf},
};

pub fn validate_root(path: &Path) -> Result<PathBuf, &'static str> {
    let metadata = fs::symlink_metadata(path).map_err(|_| "invalid_root")?;
    if !path.is_absolute() || metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err("invalid_root");
    }
    fs::canonicalize(path).map_err(|_| "invalid_root")
}

pub struct VerifiedFile {
    file: File,
    size: u64,
    sha256: String,
}

impl VerifiedFile {
    pub fn open(
        root: &Path,
        relative: &str,
        size: u64,
        sha256: &str,
    ) -> Result<Self, &'static str> {
        let mut path = root.to_path_buf();
        let components: Vec<_> = Path::new(relative).components().collect();
        if components.is_empty() {
            return Err("invalid_path");
        }
        for (index, part) in components.iter().enumerate() {
            let Component::Normal(name) = part else {
                return Err("invalid_path");
            };
            path.push(name);
            let m = fs::symlink_metadata(&path).map_err(|_| "file_unavailable")?;
            if m.file_type().is_symlink()
                || (index + 1 < components.len() && !m.is_dir())
                || (index + 1 == components.len() && !m.is_file())
            {
                return Err("invalid_path");
            }
        }
        let resolved = fs::canonicalize(&path).map_err(|_| "invalid_path")?;
        if !resolved.starts_with(root) {
            return Err("invalid_path");
        }
        let file = OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(&resolved)
            .map_err(|_| "file_unavailable")?;
        let opened = file.metadata().map_err(|_| "file_unavailable")?;
        let named = fs::symlink_metadata(&resolved).map_err(|_| "file_unavailable")?;
        if !opened.is_file() || opened.ino() != named.ino() || opened.dev() != named.dev() {
            return Err("invalid_path");
        }
        let mut result = Self {
            file,
            size,
            sha256: sha256.into(),
        };
        result.revalidate()?;
        Ok(result)
    }

    pub fn reader(&mut self) -> Result<File, &'static str> {
        self.file.rewind().map_err(|_| "file_read_failed")?;
        self.file.try_clone().map_err(|_| "file_read_failed")
    }

    // /dev/fd aliases share the cursor on macOS: the native loader's magic
    // sniff would move later GGUF opens past the header. Keep this only as
    // regression evidence; inference uses a private verified snapshot below.
    #[cfg(test)]
    pub fn descriptor_path(&self) -> PathBuf {
        use std::os::fd::AsRawFd;
        PathBuf::from(format!("/dev/fd/{}", self.file.as_raw_fd()))
    }

    pub fn snapshot(&mut self, root: &Path) -> Result<ModelSnapshot, &'static str> {
        // Standalone/historical callers get the same locked recovery as the
        // App. Allocation below still fails closed on invalid ownership.
        let _ = crate::scratch::reclaim_orphans(root);
        let mut lease = crate::scratch::SnapshotLease::create(root)?;
        let copied = std::io::copy(&mut self.reader()?.take(self.size + 1), lease.writer()?)
            .map_err(|_| "snapshot_failed")?;
        if copied != self.size {
            return Err("identity_mismatch");
        }
        lease.seal()?;
        self.revalidate()?;
        let path = lease.path();
        let verified = Self::open(
            path.parent().ok_or("snapshot_failed")?,
            "model.gguf",
            self.size,
            &self.sha256,
        )?;
        Ok(ModelSnapshot {
            verified: Some(verified),
            lease,
        })
    }

    pub fn revalidate(&mut self) -> Result<(), &'static str> {
        let m = self.file.metadata().map_err(|_| "file_read_failed")?;
        if !m.is_file() || m.len() != self.size {
            return Err("identity_mismatch");
        }
        self.file
            .seek(SeekFrom::Start(0))
            .map_err(|_| "file_read_failed")?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0_u8; 64 * 1024];
        let mut total = 0_u64;
        loop {
            let n = self
                .file
                .read(&mut buffer)
                .map_err(|_| "file_read_failed")?;
            if n == 0 {
                break;
            }
            total += n as u64;
            if total > self.size {
                return Err("identity_mismatch");
            }
            digest.update(&buffer[..n]);
        }
        if total != self.size || hex::encode(digest.finalize()) != self.sha256 {
            return Err("identity_mismatch");
        }
        self.file.rewind().map_err(|_| "file_read_failed")
    }
}

/// Worker-leased 0700 directory + read-only model, copied from the verified
/// descriptor and rehashed. Independent native opens have independent cursors;
/// no shared App model pathname is passed to the native parser. Drop cleans up
/// only this run's inode-bound model, after its model/session have been freed.
pub struct ModelSnapshot {
    verified: Option<VerifiedFile>,
    lease: crate::scratch::SnapshotLease,
}

impl ModelSnapshot {
    pub fn path(&self) -> PathBuf {
        self.lease.path()
    }
    pub fn revalidate(&mut self) -> Result<(), &'static str> {
        self.verified
            .as_mut()
            .ok_or("snapshot_failed")?
            .revalidate()
    }
}

impl Drop for ModelSnapshot {
    fn drop(&mut self) {
        // Close our final descriptor before SnapshotLease unlinks its model.
        self.verified.take();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::{symlink, PermissionsExt};

    #[test]
    fn snapshot_has_independent_cursors_and_is_not_redirected_by_source_replacement() {
        let temp = tempfile::tempdir().unwrap();
        let root = validate_root(temp.path()).unwrap();
        fs::write(root.join("model.bin"), b"GGUFnext").unwrap();
        let hash = hex::encode(Sha256::digest(b"GGUFnext"));
        let mut verified = VerifiedFile::open(&root, "model.bin", 8, &hash).unwrap();
        let mut snapshot = verified.snapshot(&root).unwrap();
        fs::rename(root.join("model.bin"), root.join("old.bin")).unwrap();
        fs::write(root.join("model.bin"), b"badmodel").unwrap();
        let snapshot_path = snapshot.path();
        assert_eq!(
            fs::metadata(snapshot_path.parent().unwrap())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&snapshot_path).unwrap().permissions().mode() & 0o777,
            0o400
        );
        for _ in 0..2 {
            let mut header = [0_u8; 4];
            File::open(&snapshot_path)
                .unwrap()
                .read_exact(&mut header)
                .unwrap();
            assert_eq!(&header, b"GGUF");
        }
        snapshot.revalidate().unwrap();
        drop(snapshot);
        assert!(!snapshot_path.exists());
        assert!(root.join("model.bin").is_file());
    }

    #[test]
    fn macos_descriptor_path_reopens_share_an_offset() {
        let temp = tempfile::tempdir().unwrap();
        let root = validate_root(temp.path()).unwrap();
        fs::write(root.join("model.bin"), b"GGUFnext").unwrap();
        let hash = hex::encode(Sha256::digest(b"GGUFnext"));
        let verified = VerifiedFile::open(&root, "model.bin", 8, &hash).unwrap();
        let mut first = File::open(verified.descriptor_path()).unwrap();
        let mut second = File::open(verified.descriptor_path()).unwrap();
        let mut header = [0_u8; 4];
        first.read_exact(&mut header).unwrap();
        assert_eq!(&header, b"GGUF");
        second.read_exact(&mut header).unwrap();
        assert_eq!(
            &header, b"next",
            "descriptor aliases share the native loader's cursor"
        );
    }

    #[test]
    fn descriptor_stays_bound_when_path_is_replaced_and_detects_content_mutation() {
        let temp = tempfile::tempdir().unwrap();
        let root = validate_root(temp.path()).unwrap();
        let path = root.join("model.bin");
        fs::write(&path, b"owned").unwrap();
        let hash = hex::encode(Sha256::digest(b"owned"));
        let mut verified = VerifiedFile::open(&root, "model.bin", 5, &hash).unwrap();
        fs::rename(&path, root.join("original.bin")).unwrap();
        fs::write(&path, b"other").unwrap();
        assert_eq!(fs::read(verified.descriptor_path()).unwrap(), b"owned");
        verified.revalidate().unwrap();
        fs::write(root.join("original.bin"), b"wrong").unwrap();
        assert_eq!(verified.revalidate().unwrap_err(), "identity_mismatch");
        symlink(&path, root.join("link.bin")).unwrap();
        assert!(VerifiedFile::open(&root, "link.bin", 5, &hash).is_err());
        assert!(VerifiedFile::open(&root, "../outside.bin", 5, &hash).is_err());
    }
}
