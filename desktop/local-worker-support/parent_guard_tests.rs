use super::*;
use std::{
    fs,
    os::unix::process::CommandExt,
    path::Path,
    process::{Child, Command, Stdio},
    time::Instant,
};

const ROOT_ENV: &str = "ECHOWALL_PARENT_GUARD_TEST_ROOT";
const MODE_ENV: &str = "ECHOWALL_PARENT_GUARD_TEST_MODE";

fn test_name() -> String {
    format!(
        "{}::process_host_probe",
        module_path!().split_once("::").unwrap().1
    )
}

fn probe_command(root: &Path, mode: &str) -> Command {
    let mut command = Command::new(std::env::current_exe().unwrap());
    command
        .args(["--exact", &test_name(), "--ignored", "--nocapture"])
        .env_clear()
        .env(ROOT_ENV, root)
        .env(MODE_ENV, mode)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    command
}

#[test]
fn canonical_parent_identity_is_closed_and_normal_drop_joins() {
    for invalid in ["", "0", "1", "-2", "+3", "03", " 3", "3 ", "2147483648"] {
        assert!(parse_parent(invalid).is_err());
    }
    let parent = unsafe { libc::getppid() };
    assert!(ParentGuard::bind(1).is_err());
    let started = Instant::now();
    drop(ParentGuard::bind(parent).unwrap());
    assert!(started.elapsed() < Duration::from_secs(1));
}

// Only this test's process group is killed. The root never reaps the host
// before group cleanup, so the host PID/PGID cannot be reused meanwhile.
struct HostGroup(Child);
impl Drop for HostGroup {
    fn drop(&mut self) {
        unsafe {
            libc::kill(-(self.0.id() as libc::pid_t), libc::SIGKILL);
        }
        let _ = self.0.wait();
    }
}

#[test]
fn killed_host_stops_guarded_worker_but_unguarded_control_survives() {
    for guarded in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let mut command = probe_command(
            temp.path(),
            if guarded {
                "host-guarded"
            } else {
                "host-control"
            },
        );
        command.process_group(0);
        let mut host = HostGroup(command.spawn().unwrap());
        let started = Instant::now();
        let worker_pid: libc::pid_t = loop {
            if let Ok(value) = fs::read_to_string(temp.path().join("ready")) {
                if let Ok(pid) = value.parse() {
                    break pid;
                }
            }
            assert!(
                started.elapsed() < Duration::from_secs(5),
                "fixture did not start"
            );
            thread::sleep(Duration::from_millis(10));
        };
        host.0.kill().unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        if guarded {
            while unsafe { libc::kill(worker_pid, 0) } == 0 {
                assert!(Instant::now() < deadline, "guarded orphan survived");
                thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                std::io::Error::last_os_error().raw_os_error(),
                Some(libc::ESRCH)
            );
        } else {
            thread::sleep(Duration::from_millis(350));
            assert_eq!(
                unsafe { libc::kill(worker_pid, 0) },
                0,
                "control did not reproduce orphan"
            );
        }
        drop(host);
        while unsafe { libc::kill(worker_pid, 0) } == 0 {
            assert!(Instant::now() < deadline, "fixture group was not reaped");
            thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
#[ignore = "exact synthetic process-tree fixture, invoked by parent lifetime tests"]
fn process_host_probe() {
    let root = std::path::PathBuf::from(std::env::var_os(ROOT_ENV).expect("fixture root"));
    assert!(root.is_absolute() && root.is_dir());
    let mode = std::env::var(MODE_ENV).unwrap();
    if mode.starts_with("host-") {
        let mut command = if mode == "host-swift" {
            let mut command = Command::new("/usr/bin/sandbox-exec");
            command
                .args(["-p", "(version 1) (allow default) (deny network*)"])
                .arg(root.join("swift-worker"))
                .env_clear()
                .env(ROOT_ENV, &root)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null());
            command
        } else {
            probe_command(
                &root,
                if mode == "host-guarded" {
                    "worker-guarded"
                } else {
                    "worker-control"
                },
            )
        };
        let mut child = command
            .env(APP_PARENT_ENV, std::process::id().to_string())
            .spawn()
            .unwrap();
        let _ = child.wait();
    } else {
        let _guard = if mode == "worker-guarded" {
            bind_from_environment().unwrap()
        } else {
            None
        };
        fs::write(root.join("ready"), std::process::id().to_string()).unwrap();
        loop {
            thread::sleep(Duration::from_millis(20));
        }
    }
}

#[test]
fn swift_guard_uses_the_same_owner_contract_under_real_sandbox() {
    let temp = tempfile::tempdir().unwrap();
    let fixture = temp.path().join("Fixture.swift");
    fs::write(
        &fixture,
        r#"
import Darwin
import Foundation
@main struct Fixture {
  static func main() throws {
    let guardOwner = try ParentLifetimeGuard.bindFromEnvironment()
    defer { guardOwner?.stop() }
    guard guardOwner != nil else { exit(2) }
    let root = ProcessInfo.processInfo.environment["ECHOWALL_PARENT_GUARD_TEST_ROOT"]!
    try String(getpid()).write(toFile: root + "/ready", atomically: false, encoding: .utf8)
    while true { Thread.sleep(forTimeInterval: 0.02) }
  }
}
"#,
    )
    .unwrap();
    let desktop = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let output =
        Command::new("/usr/bin/xcrun")
            .args(["swiftc", "-parse-as-library"])
            .arg(desktop.join(
                "diarization-worker/Sources/EchoWallDiarizationWorker/ParentLifetimeGuard.swift",
            ))
            .arg(&fixture)
            .arg("-o")
            .arg(temp.path().join("swift-worker"))
            .output()
            .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let mut command = probe_command(temp.path(), "host-swift");
    command.process_group(0);
    let mut host = HostGroup(command.spawn().unwrap());
    let started = Instant::now();
    let pid: libc::pid_t = loop {
        if let Ok(value) = fs::read_to_string(temp.path().join("ready")) {
            if let Ok(pid) = value.parse() {
                break pid;
            }
        }
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "Swift fixture did not start"
        );
        thread::sleep(Duration::from_millis(10));
    };
    host.0.kill().unwrap();
    let killed = Instant::now();
    while unsafe { libc::kill(pid, 0) } == 0 {
        assert!(
            killed.elapsed() < Duration::from_secs(3),
            "Swift orphan survived"
        );
        thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(
        std::io::Error::last_os_error().raw_os_error(),
        Some(libc::ESRCH)
    );
}
