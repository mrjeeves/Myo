//! The shell's managed state — one `Arc<MyoState>` Tauri hands to every command.
//!
//! Following MyOwnLLM's pattern: the managed value is an `Arc<T>` whose `T`
//! carries its own locks, so it's `Send + Sync + 'static` for Tauri while the
//! commands mutate pieces of it. Guards are always dropped before any `.await`
//! (a held `std::sync::Mutex` guard would make a command future non-`Send`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use myo_core::{AsrClient, BrainClient, ShellSettings, TurnAllocator, TurnId};

/// A spawned engine sidecar that is killed when this handle drops — so closing
/// Myo tears the brain/model engine down with it (ported from MyOwnLLM's
/// `DaemonChild`).
pub struct EngineChild {
    #[allow(dead_code)] // kept for diagnostics / future targeted restarts
    pub name: String,
    child: Option<std::process::Child>,
}

impl EngineChild {
    pub fn new(name: impl Into<String>, child: std::process::Child) -> Self {
        Self {
            name: name.into(),
            child: Some(child),
        }
    }
}

impl Drop for EngineChild {
    fn drop(&mut self) {
        if let Some(mut c) = self.child.take() {
            let _ = c.kill();
            let _ = c.wait();
        }
    }
}

/// Everything the commands share.
pub struct MyoState {
    /// The minted `ODYSSEUS_INTERNAL_TOKEN` — used both to build [`Self::brain`]
    /// and to inject into the Odysseus child's environment before it boots.
    pub token: String,
    /// The loopback client for the brain (created at startup with the minted
    /// internal token; its calls simply fail until the brain is healthy).
    pub brain: BrainClient,
    /// The ears: posts open-mic utterances to MyOwnLLM's transcription route.
    /// Like the brain client, it's built up front and simply fails until the
    /// model engine is serving.
    pub asr: AsrClient,
    /// The current conversation's session id (one per run, created lazily).
    pub session: Mutex<Option<String>>,
    /// Allocates a fresh turn id per utterance.
    pub turns: TurnAllocator,
    /// The persisted shell settings (capabilities + incognito).
    pub settings: Mutex<ShellSettings>,
    /// Spawned engine children, kept alive (and killed on app exit) here.
    pub children: Mutex<Vec<EngineChild>>,
    /// In-flight turn tasks, so a turn can be cancelled (barge-in / user stop).
    pub tasks: Mutex<HashMap<TurnId, TurnTask>>,
}

/// A tracked turn task: its abort handle plus a flag the task sets when it's
/// finished. The flag lets `cancel` distinguish "still running" from "already
/// done" without racing the task's own teardown, and lets `track` prune
/// completed entries — so a turn that finishes before it's even registered
/// can't leak its handle or be spuriously "cancelled".
pub struct TurnTask {
    pub handle: tauri::async_runtime::JoinHandle<()>,
    pub done: Arc<AtomicBool>,
}

impl MyoState {
    pub fn new(
        token: String,
        brain: BrainClient,
        asr: AsrClient,
        settings: ShellSettings,
    ) -> Self {
        Self {
            token,
            brain,
            asr,
            session: Mutex::new(None),
            turns: TurnAllocator::new(),
            settings: Mutex::new(settings),
            children: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// Get-or-create this run's session id. The lock is released before the
    /// network call so the future stays `Send`; if two turns race the very first
    /// utterance, both create a session but both then agree on whichever id was
    /// stored first — so the conversation never splits across two sessions (the
    /// loser's extra session is simply left unused).
    pub async fn ensure_session(&self) -> anyhow::Result<String> {
        if let Some(s) = self.session.lock().unwrap().clone() {
            return Ok(s);
        }
        let created = self.brain.create_session().await?;
        let mut guard = self.session.lock().unwrap();
        Ok(guard.get_or_insert(created).clone())
    }

    /// Snapshot the capability + incognito state for a turn (a cheap copy taken
    /// under the lock, never held across `.await`).
    pub fn turn_context(&self) -> (myo_core::Capabilities, bool) {
        let s = self.settings.lock().unwrap();
        (s.capabilities, s.incognito)
    }

    /// Register an in-flight turn task. Prunes any already-finished tasks first
    /// so the map stays bounded by what's actually running (the task sets its
    /// own `done` flag when it completes, rather than racing to remove itself).
    pub fn track_task(&self, turn: TurnId, task: TurnTask) {
        let mut map = self.tasks.lock().unwrap();
        map.retain(|_, t| !t.done.load(Ordering::Relaxed));
        if let Some(prev) = map.insert(turn, task) {
            prev.handle.abort();
        }
    }

    /// Cancel an in-flight turn; returns whether one was actually *running*
    /// (a turn that already finished is reported as not-cancelled, so cancel
    /// never fabricates a turn-closed signal for a turn that ended on its own).
    pub fn cancel_task(&self, turn: TurnId) -> bool {
        match self.tasks.lock().unwrap().remove(&turn) {
            Some(t) if !t.done.load(Ordering::Relaxed) => {
                t.handle.abort();
                true
            }
            _ => false,
        }
    }
}
