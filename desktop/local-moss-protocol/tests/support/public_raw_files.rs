use std::{
    fs::{self, OpenOptions},
    io::{ErrorKind, Write},
    path::{Component, Path, PathBuf},
};

pub fn safe_path(root: &Path, relative: &str) -> Result<PathBuf, &'static str> {
    if relative.is_empty()
        || relative.contains(['\\', '\0'])
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | ".."))
        || !Path::new(relative)
            .components()
            .all(|part| matches!(part, Component::Normal(_)))
    {
        return Err("fixture_path_rejected");
    }
    let mut path = root.to_owned();
    for component in Path::new(relative).components() {
        path.push(component);
        match fs::symlink_metadata(&path) {
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err("fixture_path_rejected");
            }
            Ok(_) => {}
            Err(error) if error.kind() == ErrorKind::NotFound => {}
            Err(_) => return Err("fixture_path_rejected"),
        }
    }
    Ok(path)
}

pub fn read_bounded(root: &Path, relative: &str, limit: u64) -> Result<Vec<u8>, &'static str> {
    let supplied = safe_path(root, relative)?;
    let metadata = fs::symlink_metadata(&supplied).map_err(|_| "fixture_missing")?;
    let path = fs::canonicalize(&supplied).map_err(|_| "fixture_missing")?;
    if !metadata.is_file() || metadata.len() > limit || !path.starts_with(root) {
        return Err("fixture_rejected");
    }
    let bytes = fs::read(path).map_err(|_| "fixture_read_failed")?;
    if bytes.len() as u64 > limit {
        return Err("fixture_rejected");
    }
    Ok(bytes)
}

fn check_existing(root: &Path, relative: &str, bytes: &[u8]) -> Result<bool, &'static str> {
    let path = safe_path(root, relative)?;
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            if !metadata.is_file() {
                return Err("artifact_rejected");
            }
            if metadata.len() != bytes.len() as u64 {
                return Err("artifact_conflict");
            }
            if read_bounded(root, relative, bytes.len() as u64)? != bytes {
                return Err("artifact_conflict");
            }
            Ok(true)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(false),
        Err(_) => Err("artifact_rejected"),
    }
}

/// Preflight the entire batch, then use create_new so no old evidence is replaced.
/// A partial new batch is safe to resume only when every existing byte matches.
pub fn publish(root: &Path, artifacts: &[(String, Vec<u8>)]) -> Result<usize, &'static str> {
    for (relative, bytes) in artifacts {
        check_existing(root, relative, bytes)?;
    }
    let mut created = 0;
    for (relative, bytes) in artifacts {
        if check_existing(root, relative, bytes)? {
            continue;
        }
        let path = safe_path(root, relative)?;
        let parent = fs::canonicalize(path.parent().ok_or("artifact_rejected")?)
            .map_err(|_| "artifact_rejected")?;
        if !parent.starts_with(root) {
            return Err("artifact_rejected");
        }
        match OpenOptions::new().write(true).create_new(true).open(path) {
            Ok(mut file) => {
                file.write_all(bytes).map_err(|_| "artifact_write_failed")?;
                file.sync_all().map_err(|_| "artifact_write_failed")?;
                created += 1;
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => {
                if !check_existing(root, relative, bytes)? {
                    return Err("artifact_conflict");
                }
            }
            Err(_) => return Err("artifact_write_failed"),
        }
    }
    Ok(created)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};

    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "echowall-public-raw-{}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(fs::canonicalize(path).unwrap())
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            fs::remove_dir_all(&self.0).unwrap();
        }
    }

    #[test]
    fn publication_is_idempotent_and_conflict_preflight_preserves_all_evidence() {
        let scratch = Scratch::new();
        let artifacts = vec![("existing.json".into(), b"original\n".to_vec())];
        assert_eq!(publish(&scratch.0, &artifacts), Ok(1));
        assert_eq!(publish(&scratch.0, &artifacts), Ok(0));
        let conflicting = vec![
            ("new.json".into(), b"new\n".to_vec()),
            ("existing.json".into(), b"changed\n".to_vec()),
        ];
        assert_eq!(publish(&scratch.0, &conflicting), Err("artifact_conflict"));
        assert!(!scratch.0.join("new.json").exists());
        assert_eq!(
            fs::read(scratch.0.join("existing.json")).unwrap(),
            b"original\n"
        );
    }

    #[test]
    fn paths_and_input_sizes_are_bounded() {
        let scratch = Scratch::new();
        for relative in [
            "",
            "/absolute",
            "../escape",
            "a/../b",
            "a//b",
            "a\\b",
            "./file",
        ] {
            assert!(safe_path(&scratch.0, relative).is_err());
        }
        publish(&scratch.0, &[("small.json".into(), b"123".to_vec())]).unwrap();
        assert!(read_bounded(&scratch.0, "small.json", 2).is_err());
        assert_eq!(read_bounded(&scratch.0, "small.json", 3).unwrap(), b"123");
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_files_and_parent_directories_are_rejected() {
        let scratch = Scratch::new();
        fs::create_dir(scratch.0.join("real")).unwrap();
        publish(
            &scratch.0,
            &[("real/file.json".into(), b"original".to_vec())],
        )
        .unwrap();
        std::os::unix::fs::symlink(scratch.0.join("real"), scratch.0.join("link")).unwrap();
        std::os::unix::fs::symlink(
            scratch.0.join("real/file.json"),
            scratch.0.join("file.json"),
        )
        .unwrap();
        for relative in ["link/file.json", "file.json"] {
            assert!(read_bounded(&scratch.0, relative, 32).is_err());
            assert!(publish(&scratch.0, &[(relative.into(), b"changed".to_vec())]).is_err());
        }
        assert_eq!(
            fs::read(scratch.0.join("real/file.json")).unwrap(),
            b"original"
        );
    }
}
