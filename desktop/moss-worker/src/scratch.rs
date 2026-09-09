//! Private snapshot ownership shared verbatim with the App supervisor. No C++
//! linkage, env handoff, PID liveness, directory globbing, or recursive removal.
//!
//! Trust boundary: the App root is owned by this uid; hostile same-uid writers
//! are outside the process-isolation boundary. Nonetheless all opens/unlinks
//! are directory-fd-relative, no-follow, and inode/owner/layout checked. Four
//! permanent lock inodes protect four exact slots. A worker holds its lock
//! until native model/session destruction. A crash releases that kernel lock.
//! The fsynced run journal precedes model creation; its inode binding precedes
//! any model bytes. Interrupted journal initialization therefore leaves only
//! a recoverable empty file, never an unregistered large copy.
//!
//! Invalid/foreign layouts are left untouched. Old `.moss-model-*` directories
//! are deliberately outside this versioned store and are never inspected.

use std::{
    ffi::{CStr, CString},
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::{ffi::OsStrExt, fs::MetadataExt},
    },
    path::{Path, PathBuf},
};

const STORE: &str = "moss-snapshots-v1";
const SLOTS: [&str; 4] = ["slot-0", "slot-1", "slot-2", "slot-3"];
const MAX_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const JOURNAL_LIMIT: u64 = 512;
type Result<T> = std::result::Result<T, &'static str>;
const ERROR: &str = "snapshot_ownership_failed";

fn c_name(name: &str) -> CString {
    CString::new(name).expect("fixed internal snapshot name")
}

fn open_at(dir: &File, name: &str, flags: i32, mode: u32) -> Result<File> {
    // SAFETY: valid owned directory fd and NUL-terminated fixed name. The
    // successful descriptor is transferred exactly once to File.
    let fd = unsafe {
        libc::openat(
            dir.as_raw_fd(),
            c_name(name).as_ptr(),
            flags | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            mode,
        )
    };
    if fd < 0 {
        return Err(ERROR);
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

fn absent(dir: &File, name: &str) -> Result<bool> {
    let mut value = std::mem::MaybeUninit::<libc::stat>::uninit();
    let result = unsafe {
        libc::fstatat(
            dir.as_raw_fd(),
            c_name(name).as_ptr(),
            value.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if result == 0 {
        return Ok(false);
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::ENOENT) {
        Ok(true)
    } else {
        Err(ERROR)
    }
}

fn directory(parent: &File, name: &str, create: bool, private: bool) -> Result<File> {
    if create {
        let result = unsafe { libc::mkdirat(parent.as_raw_fd(), c_name(name).as_ptr(), 0o700) };
        if result != 0 && std::io::Error::last_os_error().raw_os_error() != Some(libc::EEXIST) {
            return Err(ERROR);
        }
        if result == 0 {
            parent.sync_all().map_err(|_| ERROR)?;
        }
    }
    let file = open_at(parent, name, libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    let m = file.metadata().map_err(|_| ERROR)?;
    if !m.is_dir()
        || m.uid() != unsafe { libc::geteuid() }
        || m.mode() & 0o022 != 0
        || (private && m.mode() & 0o7777 != 0o700)
    {
        return Err(ERROR);
    }
    Ok(file)
}

fn regular(file: &File, max: u64) -> Result<fs::Metadata> {
    let m = file.metadata().map_err(|_| ERROR)?;
    if !m.is_file()
        || m.uid() != unsafe { libc::geteuid() }
        || m.nlink() != 1
        || m.mode() & 0o7177 != 0
        || m.len() > max
    {
        return Err(ERROR);
    }
    Ok(m)
}

fn identity(file: &File) -> Result<String> {
    let m = file.metadata().map_err(|_| ERROR)?;
    Ok(format!("{}:{}", m.dev(), m.ino()))
}

fn entries(dir: &File, allowed: &[&str]) -> Result<()> {
    // Independent directory description: dup() would share its enumeration
    // offset with other checks. Enumeration stops after the bounded allowlist.
    let fd = open_at(dir, ".", libc::O_RDONLY | libc::O_DIRECTORY, 0)?;
    use std::os::fd::IntoRawFd;
    let raw = fd.into_raw_fd();
    let stream = unsafe { libc::fdopendir(raw) };
    if stream.is_null() {
        unsafe {
            libc::close(raw);
        }
        return Err(ERROR);
    }
    let mut count = 0;
    let result = loop {
        // errno distinguishes EOF from enumeration failure.
        unsafe {
            *libc::__error() = 0;
        }
        let item = unsafe { libc::readdir(stream) };
        if item.is_null() {
            break if std::io::Error::last_os_error().raw_os_error() == Some(0) {
                Ok(())
            } else {
                Err(ERROR)
            };
        }
        let name = unsafe { CStr::from_ptr((*item).d_name.as_ptr()) }.to_bytes();
        if name == b"." || name == b".." {
            continue;
        }
        count += 1;
        if count > allowed.len() || !allowed.iter().any(|allowed| allowed.as_bytes() == name) {
            break Err(ERROR);
        }
    };
    unsafe {
        libc::closedir(stream);
    }
    result
}

fn read_small(file: &mut File) -> Result<Vec<u8>> {
    regular(file, JOURNAL_LIMIT)?;
    file.rewind().map_err(|_| ERROR)?;
    let mut bytes = Vec::new();
    file.take(JOURNAL_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| ERROR)?;
    if bytes.len() > JOURNAL_LIMIT as usize {
        return Err(ERROR);
    }
    Ok(bytes)
}

fn lock(file: &File) -> Result<bool> {
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        return Ok(true);
    }
    if std::io::Error::last_os_error().raw_os_error() == Some(libc::EWOULDBLOCK) {
        Ok(false)
    } else {
        Err(ERROR)
    }
}

// Permanent files are NEVER unlinked or replaced. A prefix write can only be
// completed under that very inode's lock, including after initialization crash.
fn permanent_lock(dir: &File, name: &str, binding: &str, initialize: bool) -> Result<Option<File>> {
    let mut file = open_at(
        dir,
        name,
        libc::O_RDWR | if initialize { libc::O_CREAT } else { 0 },
        0o600,
    )?;
    regular(&file, JOURNAL_LIMIT)?;
    if !lock(&file)? {
        return Ok(None);
    }
    if identity(&open_at(dir, name, libc::O_RDONLY, 0)?)? != identity(&file)? {
        return Err(ERROR);
    }
    let expected = format!("echowall-snapshot-v1 {binding} {}\n", identity(&file)?);
    let actual = read_small(&mut file)?;
    if actual != expected.as_bytes() {
        if !initialize || !expected.as_bytes().starts_with(&actual) {
            return Err(ERROR);
        }
        // An interrupted initial marker cannot already have descendants/run
        // data. In particular a replaced/truncated lock must not establish a
        // second lease inode beside a still-live original owner.
        entries(dir, &[name])?;
        file.seek(SeekFrom::End(0)).map_err(|_| ERROR)?;
        file.write_all(&expected.as_bytes()[actual.len()..])
            .map_err(|_| ERROR)?;
        file.sync_all().map_err(|_| ERROR)?;
        dir.sync_all().map_err(|_| ERROR)?;
    }
    Ok(Some(file))
}

struct Store {
    dir: File,
    _registry: File,
    binding: String,
    path: PathBuf,
}

impl Store {
    fn open(root: &Path, create: bool) -> Result<Option<Self>> {
        if !root.is_absolute()
            || root.as_os_str().len() > 4096
            || fs::canonicalize(root).map_err(|_| ERROR)? != root
        {
            return Err(ERROR);
        }
        let root_c = CString::new(root.as_os_str().as_bytes()).map_err(|_| ERROR)?;
        let fd = unsafe {
            libc::open(
                root_c.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(ERROR);
        }
        let root_dir = unsafe { File::from_raw_fd(fd) };
        let root_metadata = root_dir.metadata().map_err(|_| ERROR)?;
        if root_metadata.uid() != unsafe { libc::geteuid() } || root_metadata.mode() & 0o022 != 0 {
            return Err(ERROR);
        }
        if !create && absent(&root_dir, "processing")? {
            return Ok(None);
        }
        let processing = directory(&root_dir, "processing", create, false)?;
        if !create && absent(&processing, STORE)? {
            return Ok(None);
        }
        let dir = directory(&processing, STORE, create, true)?;
        entries(
            &dir,
            &["registry.lock", "slot-0", "slot-1", "slot-2", "slot-3"],
        )?;
        let binding = format!("{} {}", identity(&root_dir)?, identity(&dir)?);
        let Some(registry) = permanent_lock(&dir, "registry.lock", &binding, create)? else {
            return Ok(None);
        };
        Ok(Some(Self {
            dir,
            _registry: registry,
            binding,
            path: root.join("processing").join(STORE),
        }))
    }

    fn slot(&self, name: &str, create: bool) -> Result<Option<Slot>> {
        if !create && absent(&self.dir, name)? {
            return Ok(None);
        }
        let dir = directory(&self.dir, name, create, true)?;
        let binding = format!("{} {}", self.binding, identity(&dir)?);
        let Some(lease) = permanent_lock(&dir, "lease.lock", &binding, create)? else {
            return Ok(None);
        };
        entries(&dir, &["lease.lock", "run.journal", "model.gguf"])?;
        Ok(Some(Slot {
            dir,
            _lease: lease,
            path: self.path.join(name),
        }))
    }
}

struct Slot {
    dir: File,
    _lease: File,
    path: PathBuf,
}

fn unlink_bound(dir: &File, name: &str, file: &File) -> Result<()> {
    let named = open_at(dir, name, libc::O_RDONLY, 0)?;
    if identity(&named)? != identity(file)? {
        return Err(ERROR);
    }
    regular(&named, MAX_BYTES)?;
    if unsafe { libc::unlinkat(dir.as_raw_fd(), c_name(name).as_ptr(), 0) } != 0 {
        return Err(ERROR);
    }
    dir.sync_all().map_err(|_| ERROR)
}

impl Slot {
    fn cleanup(&self, expected_nonce: Option<&str>) -> Result<bool> {
        entries(&self.dir, &["lease.lock", "run.journal", "model.gguf"])?;
        if absent(&self.dir, "run.journal")? {
            return if absent(&self.dir, "model.gguf")? {
                Ok(false)
            } else {
                Err(ERROR)
            };
        }
        let mut journal = open_at(&self.dir, "run.journal", libc::O_RDONLY, 0)?;
        let bytes = read_small(&mut journal)?;
        let text = std::str::from_utf8(&bytes).map_err(|_| ERROR)?;
        let Some((header, binding)) = text.split_once('\n') else {
            // Only a partial first record is possible before model creation.
            let prefix = "run-v1 ";
            let valid = prefix.starts_with(text)
                || text.strip_prefix(prefix).is_some_and(|suffix| {
                    suffix.len() <= 32
                        && suffix
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                });
            if expected_nonce.is_some() || !valid || !absent(&self.dir, "model.gguf")? {
                return Err(ERROR);
            }
            unlink_bound(&self.dir, "run.journal", &journal)?;
            return Ok(false);
        };
        let nonce = header.strip_prefix("run-v1 ").ok_or(ERROR)?;
        if nonce.len() != 32
            || !nonce
                .bytes()
                .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
            || expected_nonce.is_some_and(|expected| expected != nonce)
        {
            return Err(ERROR);
        }
        if absent(&self.dir, "model.gguf")? {
            // Normal cleanup may have been killed between its two unlinks.
            if !binding.is_empty() && !valid_binding(binding) {
                return Err(ERROR);
            }
            unlink_bound(&self.dir, "run.journal", &journal)?;
            return Ok(false);
        }
        let model = open_at(&self.dir, "model.gguf", libc::O_RDONLY, 0)?;
        let metadata = regular(&model, MAX_BYTES)?;
        let expected = format!("model {}\n", identity(&model)?);
        if binding != expected && !(metadata.len() == 0 && expected.starts_with(binding)) {
            return Err(ERROR);
        }
        unlink_bound(&self.dir, "model.gguf", &model)?;
        unlink_bound(&self.dir, "run.journal", &journal)?;
        Ok(true)
    }
}

fn valid_binding(binding: &str) -> bool {
    binding
        .strip_prefix("model ")
        .and_then(|s| s.strip_suffix('\n'))
        .is_some_and(|s| {
            s.split_once(':').is_some_and(|(dev, ino)| {
                !dev.is_empty()
                    && !ino.is_empty()
                    && dev.len() <= 20
                    && ino.len() <= 20
                    && dev.bytes().chain(ino.bytes()).all(|b| b.is_ascii_digit())
            })
        })
}

/// Probe only the four versioned slots. Busy locks are live owners, not errors.
/// Invalid state is retained for inspection; nothing outside this store changes.
pub(crate) fn reclaim_orphans(root: &Path) -> Result<usize> {
    let Some(store) = Store::open(root, false)? else {
        return Ok(0);
    };
    let mut count = 0;
    for name in SLOTS {
        if let Some(slot) = store.slot(name, false)? {
            count += usize::from(slot.cleanup(None)?);
        }
    }
    Ok(count)
}

pub(crate) struct SnapshotLease {
    slot: Slot,
    nonce: String,
    destination: Option<File>,
}

impl SnapshotLease {
    pub(crate) fn create(root: &Path) -> Result<Self> {
        let store = Store::open(root, true)?.ok_or("snapshot_busy")?;
        for name in SLOTS {
            let Some(slot) = store.slot(name, true)? else {
                continue;
            };
            slot.cleanup(None)?;
            let mut random = [0_u8; 16];
            if unsafe { libc::getentropy(random.as_mut_ptr().cast(), random.len()) } != 0 {
                return Err(ERROR);
            }
            let nonce: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
            let mut journal = open_at(
                &slot.dir,
                "run.journal",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )?;
            journal
                .write_all(format!("run-v1 {nonce}\n").as_bytes())
                .map_err(|_| ERROR)?;
            journal.sync_all().map_err(|_| ERROR)?;
            slot.dir.sync_all().map_err(|_| ERROR)?;
            let destination = open_at(
                &slot.dir,
                "model.gguf",
                libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
                0o600,
            )?;
            journal
                .write_all(format!("model {}\n", identity(&destination)?).as_bytes())
                .map_err(|_| ERROR)?;
            journal.sync_all().map_err(|_| ERROR)?;
            slot.dir.sync_all().map_err(|_| ERROR)?;
            return Ok(Self {
                slot,
                nonce,
                destination: Some(destination),
            });
        }
        Err("snapshot_busy")
    }

    pub(crate) fn writer(&mut self) -> Result<&mut File> {
        self.destination.as_mut().ok_or(ERROR)
    }
    pub(crate) fn path(&self) -> PathBuf {
        self.slot.path.join("model.gguf")
    }
    pub(crate) fn seal(&mut self) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;
        let file = self.writer()?;
        file.sync_all().map_err(|_| ERROR)?;
        file.set_permissions(fs::Permissions::from_mode(0o400))
            .map_err(|_| ERROR)?;
        self.destination.take();
        Ok(())
    }
}

impl Drop for SnapshotLease {
    fn drop(&mut self) {
        self.destination.take();
        let _ = self.slot.cleanup(Some(&self.nonce));
    }
}

#[cfg(test)]
#[path = "scratch_tests.rs"]
pub(crate) mod tests;
