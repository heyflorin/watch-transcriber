//! App-owned, immutable MOSS sidecars and one-owner recording leases.
//! The ledger must separately fence each completion by generation/claim token.
use std::{
    fs::{self, File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
};

use fs2::FileExt;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const MAX_BYTES: u64 = 16 * 1024 * 1024;
const MAX_WINDOWS: u32 = echowall_local_moss_protocol::windows::MAX_WINDOWS as u32;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    Plan,
    Window(u32),
    Anchors,
}

impl ArtifactKind {
    fn filename(self) -> Result<String, ArtifactError> {
        match self {
            Self::Plan => Ok("plan.json".into()),
            Self::Anchors => Ok("anchors.json".into()),
            Self::Window(index) if index < MAX_WINDOWS => Ok(format!("window-{index:02}.json")),
            Self::Window(_) => Err(ArtifactError::Invalid),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactRef {
    pub recording_id: Uuid,
    pub generation: Uuid,
    pub kind: ArtifactKind,
    pub sha256: String,
    pub size_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArtifactError {
    Invalid,
    UnsafePath,
    Unavailable,
    Conflict,
    Busy,
    OwnerMismatch,
}

impl ArtifactError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Invalid => "invalid_moss_artifact",
            Self::UnsafePath => "unsafe_moss_artifact_path",
            Self::Unavailable => "moss_artifact_unavailable",
            Self::Conflict => "moss_artifact_conflict",
            Self::Busy => "moss_recording_busy",
            Self::OwnerMismatch => "moss_owner_mismatch",
        }
    }
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for ArtifactError {}

#[derive(Clone, Debug)]
pub struct MossArtifacts {
    recording_id: Uuid,
    generation: Uuid,
    recording_root: PathBuf,
    generation_root: PathBuf,
}

/// Owned descriptor, never cloned; the OS releases the lease if the App dies.
pub struct OwnerLease {
    file: File,
    recording_root: PathBuf,
    generation_root: PathBuf,
    session_id: Uuid,
}
impl OwnerLease {
    /// Fresh for every successful acquisition, including same-process recovery.
    pub fn session_id(&self) -> Uuid {
        self.session_id
    }
}
impl Drop for OwnerLease {
    fn drop(&mut self) {
        let _ = FileExt::unlock(&self.file);
    }
}

struct PendingFile(PathBuf);
impl Drop for PendingFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

impl MossArtifacts {
    pub fn open(
        app_data: &Path,
        recording_id: Uuid,
        generation: Uuid,
    ) -> Result<Self, ArtifactError> {
        if recording_id.is_nil() || generation.is_nil() || !app_data.is_absolute() {
            return Err(ArtifactError::Invalid);
        }
        plain_directory(app_data)?;
        let app_data = fs::canonicalize(app_data).map_err(|_| ArtifactError::Unavailable)?;
        let mut path = app_data;
        for component in [
            "processing".to_owned(),
            "moss".to_owned(),
            recording_id.to_string(),
        ] {
            path.push(&component);
            create_private_directory(&path, component != "processing")?;
        }
        let recording_root = path;
        let generation_root = recording_root.join(generation.to_string());
        create_private_directory(&generation_root, true)?;
        Ok(Self {
            recording_id,
            generation,
            recording_root,
            generation_root,
        })
    }

    fn check_roots(&self) -> Result<(), ArtifactError> {
        let private_root = self
            .recording_root
            .parent()
            .ok_or(ArtifactError::UnsafePath)?;
        for path in [private_root, &self.recording_root, &self.generation_root] {
            plain_directory(path)?;
            require_private_directory(path)?;
            if fs::canonicalize(path).map_err(|_| ArtifactError::UnsafePath)? != *path {
                return Err(ArtifactError::UnsafePath);
            }
        }
        Ok(())
    }

    /// One owner across both generations and processes. Holding this descriptor
    /// is not itself a durable completion fence; caller must also check the ledger.
    pub fn try_owner(&self) -> Result<OwnerLease, ArtifactError> {
        self.check_roots()?;
        let path = self.recording_root.join("owner.lock");
        reject_nonregular(&path, true)?;
        let mut options = private_options();
        let file = options
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)
            .map_err(|_| ArtifactError::UnsafePath)?;
        if !file
            .metadata()
            .map_err(|_| ArtifactError::Unavailable)?
            .is_file()
        {
            return Err(ArtifactError::UnsafePath);
        }
        file.try_lock_exclusive().map_err(|_| ArtifactError::Busy)?;
        self.check_roots()?;
        Ok(OwnerLease {
            file,
            recording_root: self.recording_root.clone(),
            generation_root: self.generation_root.clone(),
            session_id: Uuid::new_v4(),
        })
    }

    pub fn validate_owner(&self, owner: &OwnerLease) -> Result<(), ArtifactError> {
        if owner.recording_root != self.recording_root
            || owner.generation_root != self.generation_root
        {
            return Err(ArtifactError::OwnerMismatch);
        }
        self.check_roots()
    }

    pub fn write(
        &self,
        owner: &OwnerLease,
        kind: ArtifactKind,
        bytes: &[u8],
    ) -> Result<ArtifactRef, ArtifactError> {
        self.validate_owner(owner)?;
        if bytes.is_empty() || bytes.len() as u64 > MAX_BYTES {
            return Err(ArtifactError::Invalid);
        }
        let destination = self.generation_root.join(kind.filename()?);
        let reference = ArtifactRef {
            recording_id: self.recording_id,
            generation: self.generation,
            kind,
            sha256: hex::encode(Sha256::digest(bytes)),
            size_bytes: bytes.len() as u64,
        };
        match fs::symlink_metadata(&destination) {
            Ok(_) => {
                self.read(&reference)?;
                return Ok(reference);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(ArtifactError::Unavailable),
        }
        let pending = PendingFile(
            self.generation_root
                .join(format!(".pending-{}.json", Uuid::new_v4())),
        );
        let mut file = private_options()
            .write(true)
            .create_new(true)
            .open(&pending.0)
            .map_err(|_| ArtifactError::Unavailable)?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|_| ArtifactError::Unavailable)?;
        drop(file);
        self.check_roots()?;
        // Atomic create-only publication; never use overwriting rename here.
        match fs::hard_link(&pending.0, &destination) {
            Ok(()) => sync_directory(&self.generation_root)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ArtifactError::Unavailable),
        }
        self.read(&reference)?;
        drop(pending);
        sync_directory(&self.generation_root)?;
        Ok(reference)
    }

    fn reference_path(&self, reference: &ArtifactRef) -> Result<PathBuf, ArtifactError> {
        self.check_roots()?;
        if reference.recording_id != self.recording_id
            || reference.generation != self.generation
            || reference.size_bytes == 0
            || reference.size_bytes > MAX_BYTES
            || reference.sha256.len() != 64
            || !reference
                .sha256
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(ArtifactError::Invalid);
        }
        Ok(self.generation_root.join(reference.kind.filename()?))
    }

    pub fn read_if_present(
        &self,
        reference: &ArtifactRef,
    ) -> Result<Option<Vec<u8>>, ArtifactError> {
        let path = self.reference_path(reference)?;
        match fs::symlink_metadata(&path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            Err(_) => Err(ArtifactError::Unavailable),
            Ok(_) => self.read(reference).map(Some),
        }
    }

    pub fn read(&self, reference: &ArtifactRef) -> Result<Vec<u8>, ArtifactError> {
        let path = self.reference_path(reference)?;
        reject_nonregular(&path, false)?;
        let mut file = private_options()
            .read(true)
            .open(&path)
            .map_err(|_| ArtifactError::UnsafePath)?;
        let before = file.metadata().map_err(|_| ArtifactError::Unavailable)?;
        if !before.is_file() || before.len() != reference.size_bytes {
            return Err(ArtifactError::Conflict);
        }
        let mut bytes = Vec::with_capacity(reference.size_bytes as usize);
        (&mut file)
            .take(reference.size_bytes + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ArtifactError::Unavailable)?;
        if bytes.len() as u64 != reference.size_bytes
            || hex::encode(Sha256::digest(&bytes)) != reference.sha256
            || file
                .metadata()
                .map_err(|_| ArtifactError::Unavailable)?
                .len()
                != reference.size_bytes
        {
            return Err(ArtifactError::Conflict);
        }
        self.check_roots()?;
        Ok(bytes)
    }

    /// Preserve a pre-intent/legacy response as explicitly untrusted data.
    /// Only the next unreceipted response kind is supplied by the engine; no
    /// plan, committed prefix or original recording path is ever a target.
    #[cfg(unix)]
    pub fn preserve_unreceipted(
        &self,
        owner: &OwnerLease,
        kind: ArtifactKind,
    ) -> Result<Option<String>, ArtifactError> {
        use std::os::unix::fs::MetadataExt;
        self.validate_owner(owner)?;
        if kind == ArtifactKind::Plan {
            return Err(ArtifactError::Invalid);
        }
        let source = self.generation_root.join(kind.filename()?);
        match fs::symlink_metadata(&source) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(ArtifactError::Unavailable),
            Ok(_) => reject_nonregular(&source, false)?,
        }
        let mut input = private_options()
            .read(true)
            .open(&source)
            .map_err(|_| ArtifactError::UnsafePath)?;
        let before = input.metadata().map_err(|_| ArtifactError::Unavailable)?;
        if !before.is_file() || before.len() > MAX_BYTES {
            return Err(ArtifactError::Invalid);
        }
        let mut bytes = Vec::new();
        (&mut input)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|_| ArtifactError::Unavailable)?;
        if bytes.len() as u64 != before.len() {
            return Err(ArtifactError::Conflict);
        }
        let hash = hex::encode(Sha256::digest(&bytes));
        let name = format!(".unattested-{}-{hash}", kind.filename()?);
        let preserved = self.generation_root.join(&name);
        self.check_roots()?;
        match fs::hard_link(&source, &preserved) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(_) => return Err(ArtifactError::Unavailable),
        }
        reject_nonregular(&preserved, false)?;
        let mut saved = private_options()
            .read(true)
            .open(&preserved)
            .map_err(|_| ArtifactError::UnsafePath)?;
        let mut saved_bytes = Vec::new();
        (&mut saved)
            .take(MAX_BYTES + 1)
            .read_to_end(&mut saved_bytes)
            .map_err(|_| ArtifactError::Unavailable)?;
        if saved_bytes != bytes {
            return Err(ArtifactError::Conflict);
        }
        sync_directory(&self.generation_root)?;
        let current = fs::symlink_metadata(&source).map_err(|_| ArtifactError::UnsafePath)?;
        if !current.is_file()
            || current.file_type().is_symlink()
            || (
                current.dev(),
                current.ino(),
                current.len(),
                current.mtime(),
                current.mtime_nsec(),
            ) != (
                before.dev(),
                before.ino(),
                before.len(),
                before.mtime(),
                before.mtime_nsec(),
            )
        {
            return Err(ArtifactError::Conflict);
        }
        self.check_roots()?;
        // A durable exact-byte preserved copy already exists, so this unlink
        // only removes its old canonical name. No data is recursively deleted.
        fs::remove_file(source).map_err(|_| ArtifactError::Unavailable)?;
        sync_directory(&self.generation_root)?;
        Ok(Some(name))
    }

    #[cfg(not(unix))]
    pub fn preserve_unreceipted(
        &self,
        owner: &OwnerLease,
        kind: ArtifactKind,
    ) -> Result<Option<String>, ArtifactError> {
        self.validate_owner(owner)?;
        if kind == ArtifactKind::Plan {
            return Err(ArtifactError::Invalid);
        }
        match fs::symlink_metadata(self.generation_root.join(kind.filename()?)) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
            _ => Err(ArtifactError::UnsafePath), // MOSS is not enabled on these platforms.
        }
    }
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

fn reject_nonregular(path: &Path, allow_missing: bool) -> Result<(), ArtifactError> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => Ok(()),
        Err(error) if allow_missing && error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        _ => Err(ArtifactError::UnsafePath),
    }
}

fn plain_directory(path: &Path) -> Result<(), ArtifactError> {
    let metadata = fs::symlink_metadata(path).map_err(|_| ArtifactError::UnsafePath)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(ArtifactError::UnsafePath);
    }
    Ok(())
}

fn create_private_directory(path: &Path, require_private: bool) -> Result<(), ArtifactError> {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    match builder.create(path) {
        Ok(()) => sync_directory(path.parent().ok_or(ArtifactError::UnsafePath)?)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(_) => return Err(ArtifactError::Unavailable),
    }
    plain_directory(path)?;
    if require_private {
        require_private_directory(path)?;
    }
    Ok(())
}

fn require_private_directory(path: &Path) -> Result<(), ArtifactError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if fs::metadata(path)
            .map_err(|_| ArtifactError::UnsafePath)?
            .permissions()
            .mode()
            & 0o077
            != 0
        {
            return Err(ArtifactError::UnsafePath);
        }
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn sync_directory(path: &Path) -> Result<(), ArtifactError> {
    #[cfg(unix)]
    {
        File::open(path)
            .and_then(|file| file.sync_all())
            .map_err(|_| ArtifactError::Unavailable)?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn unreceipted_preservation_is_recoverable_idempotent_and_never_targets_plan() {
        use std::os::unix::fs::MetadataExt;
        let root = tempfile::tempdir().unwrap();
        let store = MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let lease = store.try_owner().unwrap();
        let plan = store
            .write(&lease, ArtifactKind::Plan, b"plan stays")
            .unwrap();
        let reference = store
            .write(&lease, ArtifactKind::Window(0), b"retained raw response")
            .unwrap();
        let name = store
            .preserve_unreceipted(&lease, ArtifactKind::Window(0))
            .unwrap()
            .unwrap();
        let saved = store.generation_root.join(&name);
        let inode = saved.metadata().unwrap().ino();
        assert_eq!(fs::read(&saved).unwrap(), b"retained raw response");
        assert!(store.read_if_present(&reference).unwrap().is_none());
        assert!(store
            .preserve_unreceipted(&lease, ArtifactKind::Window(0))
            .unwrap()
            .is_none());
        // A crash/retry may encounter the already preserved exact-byte file.
        store
            .write(&lease, ArtifactKind::Window(0), b"retained raw response")
            .unwrap();
        assert_eq!(
            store
                .preserve_unreceipted(&lease, ArtifactKind::Window(0))
                .unwrap()
                .as_deref(),
            Some(name.as_str())
        );
        assert_eq!(saved.metadata().unwrap().ino(), inode);
        assert_eq!(
            store.preserve_unreceipted(&lease, ArtifactKind::Plan),
            Err(ArtifactError::Invalid)
        );
        assert_eq!(store.read(&plan).unwrap(), b"plan stays");
        let other = MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        assert_eq!(
            other.preserve_unreceipted(&lease, ArtifactKind::Window(0)),
            Err(ArtifactError::OwnerMismatch)
        );
    }

    #[cfg(unix)]
    #[test]
    fn preservation_conflicts_and_symlinks_never_remove_or_overwrite_either_file() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let store = MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let lease = store.try_owner().unwrap();
        let reference = store
            .write(&lease, ArtifactKind::Window(0), b"original")
            .unwrap();
        let saved = store
            .generation_root
            .join(format!(".unattested-window-00.json-{}", reference.sha256));
        fs::write(&saved, b"conflict").unwrap();
        assert_eq!(
            store.preserve_unreceipted(&lease, ArtifactKind::Window(0)),
            Err(ArtifactError::Conflict)
        );
        assert_eq!(store.read(&reference).unwrap(), b"original");
        assert_eq!(fs::read(saved).unwrap(), b"conflict");
        let missing = root.path().join("does-not-exist");
        symlink(&missing, store.generation_root.join("window-01.json")).unwrap();
        let mut symlink_ref = reference.clone();
        symlink_ref.kind = ArtifactKind::Window(1);
        assert_eq!(
            store.read_if_present(&symlink_ref),
            Err(ArtifactError::UnsafePath)
        );
        assert_eq!(
            store.preserve_unreceipted(&lease, ArtifactKind::Window(1)),
            Err(ArtifactError::UnsafePath)
        );
        assert!(
            fs::symlink_metadata(store.generation_root.join("window-01.json"))
                .unwrap()
                .file_type()
                .is_symlink()
        );
        let mut wrong = reference;
        wrong.recording_id = Uuid::new_v4();
        assert_eq!(store.read_if_present(&wrong), Err(ArtifactError::Invalid));
    }

    #[test]
    fn immutable_roundtrip_rejects_conflicts_and_cross_generation_references() {
        let root = tempfile::tempdir().unwrap();
        let recording = Uuid::new_v4();
        let store = MossArtifacts::open(root.path(), recording, Uuid::new_v4()).unwrap();
        let lease = store.try_owner().unwrap();
        let first = store
            .write(&lease, ArtifactKind::Window(0), b"{\"complete\":true}")
            .unwrap();
        assert_eq!(store.read(&first).unwrap(), b"{\"complete\":true}");
        assert_eq!(
            store
                .write(&lease, ArtifactKind::Window(0), b"{\"complete\":true}")
                .unwrap(),
            first
        );
        assert_eq!(
            store.write(&lease, ArtifactKind::Window(0), b"different"),
            Err(ArtifactError::Conflict)
        );
        assert_eq!(store.read(&first).unwrap(), b"{\"complete\":true}");
        let other = MossArtifacts::open(root.path(), recording, Uuid::new_v4()).unwrap();
        assert_eq!(other.read(&first), Err(ArtifactError::Invalid));
        assert!(store
            .write(&lease, ArtifactKind::Window(26), b"{}")
            .is_err());
        assert!(store.write(&lease, ArtifactKind::Plan, b"").is_err());
        assert!(MossArtifacts::open(root.path(), Uuid::nil(), Uuid::new_v4()).is_err());
    }

    #[test]
    fn owner_is_exclusive_across_store_instances_and_generations_then_releases() {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let first = MossArtifacts::open(root.path(), id, Uuid::new_v4()).unwrap();
        let second = MossArtifacts::open(root.path(), id, Uuid::new_v4()).unwrap();
        let lease = first.try_owner().unwrap();
        let first_session = lease.session_id();
        assert!(matches!(second.try_owner(), Err(ArtifactError::Busy)));
        assert!(matches!(first.try_owner(), Err(ArtifactError::Busy)));
        drop(lease);
        let recovered = second.try_owner().unwrap();
        assert_ne!(recovered.session_id(), first_session);
        drop(recovered);
        let reacquired = first.try_owner().unwrap();
        assert_ne!(reacquired.session_id(), first_session);
    }

    #[test]
    fn write_requires_a_capability_for_this_recording_root_and_generation() {
        // Compile-time guard: publication has no unleased call signature.
        let _write: fn(
            &MossArtifacts,
            &OwnerLease,
            ArtifactKind,
            &[u8],
        ) -> Result<ArtifactRef, ArtifactError> = MossArtifacts::write;
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let store = MossArtifacts::open(root.path(), id, Uuid::new_v4()).unwrap();
        let lease = store.try_owner().unwrap();
        let other_generation = MossArtifacts::open(root.path(), id, Uuid::new_v4()).unwrap();
        let other_recording =
            MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let other_root = tempfile::tempdir().unwrap();
        let other_app = MossArtifacts::open(other_root.path(), id, store.generation).unwrap();
        for target in [other_generation, other_recording, other_app] {
            assert_eq!(
                target.write(&lease, ArtifactKind::Plan, b"{}"),
                Err(ArtifactError::OwnerMismatch)
            );
            assert!(!target.generation_root.join("plan.json").exists());
        }
    }

    #[test]
    fn separate_process_cannot_steal_owner_and_os_releases_after_crash() {
        let root = tempfile::tempdir().unwrap();
        let id = Uuid::new_v4();
        let generation = Uuid::new_v4();
        let store = MossArtifacts::open(root.path(), id, generation).unwrap();
        let child = |mode: &str| {
            std::process::Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "processing::moss_artifacts::tests::lease_child_probe",
                    "--ignored",
                    "--nocapture",
                ])
                .env("ECHOWALL_MOSS_LEASE_TEST_ROOT", root.path())
                .env("ECHOWALL_MOSS_LEASE_TEST_RECORDING", id.to_string())
                .env(
                    "ECHOWALL_MOSS_LEASE_TEST_GENERATION",
                    generation.to_string(),
                )
                .env("ECHOWALL_MOSS_LEASE_TEST_MODE", mode)
                .output()
                .unwrap()
        };
        let lease = store.try_owner().unwrap();
        let blocked = child("busy");
        assert!(blocked.status.success());
        assert!(String::from_utf8_lossy(&blocked.stdout).contains("lease-probe-complete"));
        drop(lease);
        let crashed = child("crash");
        assert_eq!(crashed.status.code(), Some(71));
        assert!(String::from_utf8_lossy(&crashed.stdout).contains("lease-probe-complete"));
        let _recovered = store.try_owner().unwrap();
    }

    #[test]
    #[ignore = "subprocess helper with an exact synthetic temporary root; invoked by owner test"]
    fn lease_child_probe() {
        let root =
            std::env::var_os("ECHOWALL_MOSS_LEASE_TEST_ROOT").expect("fixture root required");
        let id =
            Uuid::parse_str(&std::env::var("ECHOWALL_MOSS_LEASE_TEST_RECORDING").unwrap()).unwrap();
        let generation =
            Uuid::parse_str(&std::env::var("ECHOWALL_MOSS_LEASE_TEST_GENERATION").unwrap())
                .unwrap();
        let store = MossArtifacts::open(Path::new(&root), id, generation).unwrap();
        match std::env::var("ECHOWALL_MOSS_LEASE_TEST_MODE")
            .unwrap()
            .as_str()
        {
            "busy" => {
                assert!(matches!(store.try_owner(), Err(ArtifactError::Busy)));
                println!("lease-probe-complete");
            }
            "crash" => {
                let _lease = store.try_owner().unwrap();
                println!("lease-probe-complete");
                std::io::stdout().flush().unwrap();
                // Test only: exit without running OwnerLease::drop.
                std::process::exit(71);
            }
            _ => panic!("unknown fixture mode"),
        }
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_tampering_and_private_permissions_are_enforced() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let root = tempfile::tempdir().unwrap();
        let store = MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        let lease = store.try_owner().unwrap();
        let proof = store
            .write(&lease, ArtifactKind::Anchors, b"original")
            .unwrap();
        let target = store.generation_root.join("anchors.json");
        assert_eq!(
            target.metadata().unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            store
                .generation_root
                .metadata()
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        fs::write(&target, b"tampered").unwrap();
        assert_eq!(store.read(&proof), Err(ArtifactError::Conflict));
        let outside = root.path().join("outside");
        fs::write(&outside, b"private fixture").unwrap();
        symlink(&outside, store.generation_root.join("plan.json")).unwrap();
        assert_eq!(
            store.write(&lease, ArtifactKind::Plan, b"changed"),
            Err(ArtifactError::UnsafePath)
        );
        assert_eq!(fs::read(outside).unwrap(), b"private fixture");
        let second = MossArtifacts::open(root.path(), Uuid::new_v4(), Uuid::new_v4()).unwrap();
        symlink(&target, second.recording_root.join("owner.lock")).unwrap();
        assert!(matches!(second.try_owner(), Err(ArtifactError::UnsafePath)));
    }
}
