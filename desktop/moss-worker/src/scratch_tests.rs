use super::*;
use std::{
    os::unix::fs::{symlink, PermissionsExt},
    process::{Command, Stdio},
    time::{Duration, Instant},
};

fn root() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = fs::canonicalize(temp.path()).unwrap();
    (temp, root)
}

#[test]
fn live_leases_protect_exact_models_and_reuse_fresh_nonce() {
    let (_temp, root) = root();
    fs::create_dir(root.join(".moss-model-legacy")).unwrap();
    fs::write(root.join(".moss-model-legacy/keep"), b"legacy").unwrap();
    fs::write(root.join("unrelated"), b"keep").unwrap();
    let mut first = SnapshotLease::create(&root).unwrap();
    first.writer().unwrap().write_all(b"synthetic").unwrap();
    first.seal().unwrap();
    assert_eq!(reclaim_orphans(&root).unwrap(), 0);
    let path = first.path();
    let nonce = first.nonce.clone();
    let lease_inode = identity(&first.slot._lease).unwrap();
    assert_eq!(fs::read(&path).unwrap(), b"synthetic");
    let second = SnapshotLease::create(&root).unwrap();
    assert_ne!(second.path(), path);
    drop(first);
    assert!(!path.exists());
    let third = SnapshotLease::create(&root).unwrap();
    assert_eq!(third.path(), path);
    assert_ne!(third.nonce, nonce);
    assert_eq!(identity(&third.slot._lease).unwrap(), lease_inode);
    assert!(third.slot.cleanup(Some(&nonce)).is_err());
    assert!(third.path().exists());
    drop((second, third));
    assert_eq!(fs::read(root.join("unrelated")).unwrap(), b"keep");
    assert_eq!(
        fs::read(root.join(".moss-model-legacy/keep")).unwrap(),
        b"legacy"
    );
}

#[test]
fn four_slots_bound_live_copies() {
    let (_temp, root) = root();
    let leases: Vec<_> = (0..4)
        .map(|_| SnapshotLease::create(&root).unwrap())
        .collect();
    assert!(matches!(SnapshotLease::create(&root), Err("snapshot_busy")));
    assert_eq!(reclaim_orphans(&root).unwrap(), 0);
    drop(leases);
    assert!(SnapshotLease::create(&root).is_ok());
}

#[test]
fn replaced_lock_cannot_reinitialize_beside_a_live_model() {
    let (_temp, root) = root();
    let mut snapshot = SnapshotLease::create(&root).unwrap();
    snapshot.writer().unwrap().write_all(b"live").unwrap();
    snapshot.seal().unwrap();
    let model = snapshot.path();
    fs::rename(
        snapshot.slot.path.join("lease.lock"),
        root.join("displaced.lock"),
    )
    .unwrap();
    assert!(SnapshotLease::create(&root).is_err());
    assert!(reclaim_orphans(&root).is_err());
    assert_eq!(fs::read(&model).unwrap(), b"live");
    // The original guard still owns its exact model and can clean it normally;
    // the malformed replacement lock remains quarantined for inspection.
    drop(snapshot);
    assert!(!model.exists());
}

#[test]
fn crash_prefix_journals_and_empty_model_are_recoverable() {
    let (_temp, root) = root();
    let store = Store::open(&root, true).unwrap().unwrap();
    let slot = store.slot(SLOTS[0], true).unwrap().unwrap();
    for prefix in ["", "run", "run-v1 ", "run-v1 abc"] {
        fs::write(slot.path.join("run.journal"), prefix).unwrap();
        fs::set_permissions(
            slot.path.join("run.journal"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        assert!(!slot.cleanup(None).unwrap());
    }
    let header = "run-v1 0123456789abcdef0123456789abcdef\n";
    for suffix in ["", "model "] {
        fs::write(slot.path.join("run.journal"), format!("{header}{suffix}")).unwrap();
        fs::set_permissions(
            slot.path.join("run.journal"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
        let model = open_at(
            &slot.dir,
            "model.gguf",
            libc::O_WRONLY | libc::O_CREAT | libc::O_EXCL,
            0o600,
        )
        .unwrap();
        drop(model);
        assert!(slot.cleanup(None).unwrap());
    }
    drop((slot, store));
    // Simulate interruption writing the permanent lock's initial header.
    let store = Store::open(&root, true).unwrap().unwrap();
    let slot = store.slot(SLOTS[0], true).unwrap().unwrap();
    let inode = identity(&slot._lease).unwrap();
    slot._lease.set_len(7).unwrap();
    drop((slot, store));
    let snapshot = SnapshotLease::create(&root).unwrap();
    assert_eq!(identity(&snapshot.slot._lease).unwrap(), inode);
}

#[test]
fn unexpected_layout_symlink_hardlink_and_inode_replacement_are_not_deleted() {
    for damage in ["extra", "symlink", "hardlink", "replacement", "marker"] {
        let (_temp, root) = root();
        let mut snapshot = SnapshotLease::create(&root).unwrap();
        snapshot.writer().unwrap().write_all(b"owned").unwrap();
        snapshot.seal().unwrap();
        let path = snapshot.path();
        let foreign = root.join("foreign");
        fs::write(&foreign, b"foreign").unwrap();
        match damage {
            "extra" => fs::write(snapshot.slot.path.join("unexpected"), b"keep").unwrap(),
            "symlink" => {
                fs::remove_file(&path).unwrap();
                symlink(&foreign, &path).unwrap();
            }
            "hardlink" => fs::hard_link(&path, root.join("hardlink")).unwrap(),
            "replacement" => {
                fs::rename(&path, root.join("moved-owned")).unwrap();
                fs::write(&path, b"replacement").unwrap();
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
            }
            "marker" => {
                fs::write(snapshot.slot.path.join("run.journal"), b"foreign marker").unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            snapshot.slot.cleanup(Some(&snapshot.nonce)).is_err(),
            "{damage}"
        );
        drop(snapshot);
        // Another parallel test may fork while our CLOEXEC lease exists.
        // Its inherited descriptor can briefly retain the flock until exec;
        // a busy skip is safe, but deleting anything is never acceptable.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            match reclaim_orphans(&root) {
                Err(_) => break,
                Ok(0) => {
                    assert!(fs::symlink_metadata(&path).is_ok(), "{damage}");
                    assert!(
                        std::time::Instant::now() < deadline,
                        "{damage}: lease did not release"
                    );
                    std::thread::sleep(std::time::Duration::from_millis(5));
                }
                Ok(_) => panic!("{damage}: unexpected cleanup"),
            }
        }
        assert!(fs::symlink_metadata(&path).is_ok(), "{damage}");
        assert_eq!(fs::read(&foreign).unwrap(), b"foreign");
    }
}

#[test]
fn a_shared_lease_descriptor_defers_cleanup_without_hiding_invalid_layout() {
    let (_temp, root) = root();
    let mut snapshot = SnapshotLease::create(&root).unwrap();
    snapshot.writer().unwrap().write_all(b"owned").unwrap();
    snapshot.seal().unwrap();
    let path = snapshot.path();
    fs::write(snapshot.slot.path.join("unexpected"), b"keep").unwrap();
    // Same open-file description, as with a temporarily inherited pre-exec fd.
    let held_descriptor = snapshot.slot._lease.try_clone().unwrap();
    drop(snapshot);
    assert_eq!(reclaim_orphans(&root).unwrap(), 0);
    assert_eq!(fs::read(&path).unwrap(), b"owned");
    drop(held_descriptor);
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    loop {
        match reclaim_orphans(&root) {
            Err(_) => break,
            Ok(0) => {
                assert_eq!(fs::read(&path).unwrap(), b"owned");
                assert!(
                    std::time::Instant::now() < deadline,
                    "lease did not release"
                );
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Ok(_) => panic!("invalid layout was cleaned"),
        }
    }
    assert_eq!(fs::read(path).unwrap(), b"owned");
}

pub(crate) fn child_test_name() -> String {
    format!(
        "{}::snapshot_child_process",
        module_path!().split_once("::").unwrap().1
    )
}

// A fabricated process fixture. It never loads a model, decodes audio, or makes
// an inference. The App launcher tests invoke this same test through a script.
#[test]
#[ignore = "subprocess-only synthetic snapshot holder"]
fn snapshot_child_process() {
    let Some(root) = std::env::var_os("ECHOWALL_APP_DATA_ROOT") else {
        return;
    };
    let root = PathBuf::from(root);
    let mut snapshot = SnapshotLease::create(&root).unwrap();
    snapshot
        .writer()
        .unwrap()
        .write_all(b"synthetic owned copy")
        .unwrap();
    snapshot.seal().unwrap();
    fs::write(
        root.join("scratch-ready"),
        format!("{}", std::process::id()),
    )
    .unwrap();
    loop {
        std::thread::park_timeout(Duration::from_secs(60));
    }
}

#[test]
fn killed_process_releases_kernel_lease_and_restart_reclaims_only_its_copy() {
    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let (_temp, root) = root();
    let mut child = ChildGuard(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &child_test_name(), "--ignored"])
            .env("ECHOWALL_APP_DATA_ROOT", &root)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap(),
    );
    let deadline = Instant::now() + Duration::from_secs(10);
    while !root.join("scratch-ready").exists() {
        assert!(Instant::now() < deadline);
        assert!(child.0.try_wait().unwrap().is_none());
        std::thread::sleep(Duration::from_millis(10));
    }
    let path = root
        .join("processing")
        .join(STORE)
        .join(SLOTS[0])
        .join("model.gguf");
    assert!(path.exists());
    assert_eq!(reclaim_orphans(&root).unwrap(), 0);
    child.0.kill().unwrap();
    child.0.wait().unwrap();
    assert!(path.exists(), "SIGKILL bypasses worker Drop");
    assert_eq!(reclaim_orphans(&root).unwrap(), 1);
    assert!(!path.exists());
    assert_eq!(reclaim_orphans(&root).unwrap(), 0);
    let replacement = SnapshotLease::create(&root).unwrap();
    assert_eq!(replacement.path(), path);
}
