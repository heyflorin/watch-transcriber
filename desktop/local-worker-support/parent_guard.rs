//! Optional App-owner lifetime binding for macOS one-shot workers.
//! No PID is ever signaled: a worker exits itself if its real kernel parent
//! changes. Comparing getppid, not kill(pid,0), cannot attach to a reused PID.
//! Standalone tools omit this opt-in; the App supplies its own PID after
//! clearing the environment. Bind before reading input or loading any model.

use std::{
    sync::{Arc, Condvar, Mutex},
    thread::{self, JoinHandle},
    time::Duration,
};

pub const APP_PARENT_ENV: &str = "ECHOWALL_WORKER_PARENT_PID";
pub const PARENT_EXIT_CODE: i32 = 74;
const POLL: Duration = Duration::from_millis(100);

pub struct ParentGuard {
    stop: Arc<(Mutex<bool>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

fn parse_parent(value: &str) -> Result<libc::pid_t, &'static str> {
    let pid: libc::pid_t = value.parse().map_err(|_| "invalid_parent_identity")?;
    if pid <= 1 || pid.to_string() != value {
        return Err("invalid_parent_identity");
    }
    Ok(pid)
}

pub fn bind_from_environment() -> Result<Option<ParentGuard>, &'static str> {
    let Some(value) = std::env::var_os(APP_PARENT_ENV) else {
        return Ok(None);
    };
    let parent = parse_parent(value.to_str().ok_or("invalid_parent_identity")?)?;
    ParentGuard::bind(parent).map(Some)
}

impl ParentGuard {
    fn bind(parent: libc::pid_t) -> Result<Self, &'static str> {
        // The supplied owner must be the current direct parent. sandbox-exec
        // execs the worker in place; it does not introduce another parent.
        if unsafe { libc::getppid() } != parent {
            return Err("parent_identity_mismatch");
        }
        let stop = Arc::new((Mutex::new(false), Condvar::new()));
        let watcher = Arc::clone(&stop);
        let thread = thread::Builder::new()
            .name("worker-parent-lifetime".into())
            .spawn(move || {
                let (lock, changed) = &*watcher;
                loop {
                    let stopped = lock
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner);
                    if *stopped {
                        return;
                    }
                    if unsafe { libc::getppid() } != parent {
                        // Model calls may be non-interruptible. OS process exit
                        // releases GPU mappings/descriptors and snapshot locks;
                        // the next App safely recovers the persisted local job.
                        unsafe {
                            libc::_exit(PARENT_EXIT_CODE);
                        }
                    }
                    drop(
                        changed
                            .wait_timeout(stopped, POLL)
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                    );
                }
            })
            .map_err(|_| "parent_monitor_unavailable")?;
        Ok(Self {
            stop,
            thread: Some(thread),
        })
    }
}

impl Drop for ParentGuard {
    fn drop(&mut self) {
        let (lock, changed) = &*self.stop;
        *lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = true;
        changed.notify_all();
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
#[path = "parent_guard_tests.rs"]
mod tests;
