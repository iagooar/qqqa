//! Ctrl-C for the whole process.
//!
//! CLI providers run in their own process group (so a timeout can kill
//! everything they started), which means the terminal's Ctrl-C does not reach
//! them. One handler, installed at startup, covers every phase of qq and qa:
//! it kills the process groups of providers that are still running and exits
//! with status 130. A per-call `tokio::signal::ctrl_c()` would not do: once
//! tokio installs a SIGINT handler it stays installed, so Ctrl-C would stop
//! working in the phases that follow (for example qa's `Execute? [y/N]`).

use std::sync::Mutex;

/// Process groups (by leader pid) of CLI providers that are running now.
static GROUPS: Mutex<Vec<u32>> = Mutex::new(Vec::new());

/// Exit status after Ctrl-C, as shells report it (128 + SIGINT).
pub const INTERRUPTED_EXIT_CODE: i32 = 130;

/// Install the handler. Call once, at the start of `main`, inside the tokio runtime.
/// On Unix the SIGINT listener is registered before this returns, so there is
/// no window in which Ctrl-C still gets the default action.
pub fn install() {
    #[cfg(unix)]
    {
        use tokio::signal::unix::{SignalKind, signal};
        let Ok(mut sigint) = signal(SignalKind::interrupt()) else {
            return;
        };
        tokio::spawn(async move {
            if sigint.recv().await.is_some() {
                on_interrupt();
            }
        });
    }
    #[cfg(not(unix))]
    tokio::spawn(async {
        if tokio::signal::ctrl_c().await.is_ok() {
            on_interrupt();
        }
    });
}

fn on_interrupt() -> ! {
    kill_tracked_groups();
    eprintln!();
    std::process::exit(INTERRUPTED_EXIT_CODE);
}

/// Removes its group from the kill list when dropped.
pub struct GroupGuard(Option<u32>);

impl Drop for GroupGuard {
    fn drop(&mut self) {
        if let Some(pid) = self.0 {
            let mut groups = GROUPS.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(pos) = groups.iter().position(|p| *p == pid) {
                groups.remove(pos);
            }
        }
    }
}

/// Kill the process group led by `pid` on Ctrl-C, until the guard is dropped.
pub fn track_group(pid: Option<u32>) -> GroupGuard {
    if let Some(pid) = pid {
        GROUPS.lock().unwrap_or_else(|e| e.into_inner()).push(pid);
    }
    GroupGuard(pid)
}

/// The process groups currently tracked.
pub fn tracked_groups() -> Vec<u32> {
    GROUPS.lock().unwrap_or_else(|e| e.into_inner()).clone()
}

fn kill_tracked_groups() {
    #[cfg(unix)]
    for pid in tracked_groups() {
        // SAFETY: plain kill(2) on a process group qq started.
        unsafe {
            libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guards_add_and_remove_groups() {
        let a = track_group(Some(4_000_001));
        let none = track_group(None);
        assert!(tracked_groups().contains(&4_000_001));
        drop(a);
        drop(none);
        assert!(!tracked_groups().contains(&4_000_001));
    }
}
