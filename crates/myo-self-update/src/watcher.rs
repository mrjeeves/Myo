//! Background update watcher — the "set-it-and-forget-it" loop.
//!
//! Ticks on an interval and calls [`crate::update::tick`], which is itself
//! gated by `auto_update.check_interval_hours` so hitting the network stays
//! cheap even though the loop wakes more often. A single advisory file lock at
//! `~/.myo/watcher.lock` keeps two Myo processes from both watching (first
//! wins).

use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How often the loop wakes. The real network cadence is governed by
/// `auto_update.check_interval_hours` inside `update::tick`; this just bounds
/// how soon a freshly-due check fires.
const TICK_INTERVAL: Duration = Duration::from_secs(30 * 60);

static STARTED: OnceLock<Mutex<bool>> = OnceLock::new();

/// Spawn the watcher exactly once per process (idempotent across calls).
/// Returns true if this call started it; false if it was already running or
/// another Myo process holds the lock. Requires a Tokio runtime.
pub fn spawn_background() -> bool {
    let lock = STARTED.get_or_init(|| Mutex::new(false));
    let Ok(mut guard) = lock.try_lock() else {
        return false;
    };
    if *guard {
        return false;
    }
    *guard = true;
    drop(guard);

    if !acquire_process_lock() {
        eprintln!("watcher: another myo process holds the watcher lock; skipping.");
        return false;
    }

    tokio::spawn(async move {
        loop {
            if let Err(e) = crate::update::tick().await {
                eprintln!("watcher: self-update tick error: {e}");
            }
            tokio::time::sleep(TICK_INTERVAL).await;
        }
    });
    true
}

/// Foreground variant for `myo watch`: acquire the lock and loop forever,
/// ticking immediately and then every [`TICK_INTERVAL`]. Never returns under
/// normal operation.
pub async fn watch_forever() {
    if !acquire_process_lock() {
        eprintln!("watcher: another myo process holds the watcher lock; exiting.");
        return;
    }
    eprintln!(
        "myo: watching for updates (every {} min; network checks gated by check_interval_hours).",
        TICK_INTERVAL.as_secs() / 60
    );
    loop {
        if let Err(e) = crate::update::tick().await {
            eprintln!("watcher: self-update tick error: {e}");
        }
        tokio::time::sleep(TICK_INTERVAL).await;
    }
}

// ---------------------------------------------------------------------------
// Process-wide advisory lock so two `myo` processes don't both watch.
// ---------------------------------------------------------------------------

fn acquire_process_lock() -> bool {
    let path = match crate::paths::myo_dir() {
        Ok(d) => d.join("watcher.lock"),
        Err(_) => return true,
    };
    acquire_process_lock_at(&path)
}

fn acquire_process_lock_at(path: &std::path::Path) -> bool {
    use std::fs::OpenOptions;
    use std::io::Write;

    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    if let Ok(s) = std::fs::read_to_string(path) {
        if let Ok(pid) = s.trim().parse::<u32>() {
            if process_alive(pid) {
                return false;
            }
        }
    }

    match OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(path)
    {
        Ok(mut f) => {
            let _ = writeln!(f, "{}", std::process::id());
            true
        }
        Err(_) => true, // Best-effort: if we can't write the lock, still run.
    }
}

/// Liveness check without FFI. On Linux, `/proc/<pid>` is authoritative. On
/// other platforms we conservatively assume the recorded PID is alive (so we
/// don't steal a lock that may still be held) — matching MyOwnLLM's
/// best-effort Windows behavior.
#[cfg(target_os = "linux")]
fn process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(not(target_os = "linux"))]
fn process_alive(_pid: u32) -> bool {
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lock_is_held_by_a_live_pid_then_reclaimable_when_dead() {
        let dir = std::env::temp_dir().join(format!("myo-lock-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("watcher.lock");

        // First acquire writes our (live) pid and wins.
        assert!(acquire_process_lock_at(&path));
        // Second acquire sees our own live pid → refuses.
        assert!(!acquire_process_lock_at(&path));

        // Simulate a dead holder: a pid that doesn't exist. On Linux this is
        // detected and the lock is reclaimable; elsewhere process_alive is
        // conservative, so only assert the Linux behavior.
        #[cfg(target_os = "linux")]
        {
            std::fs::write(&path, "4294967294\n").unwrap(); // implausible pid
            assert!(!process_alive(4_294_967_294));
            assert!(acquire_process_lock_at(&path));
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
