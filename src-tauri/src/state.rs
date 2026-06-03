//! The shell's managed state — one `Arc<MyoState>` Tauri hands to every command.
//!
//! Following MyOwnLLM's pattern: the managed value is an `Arc<T>` whose `T`
//! carries its own locks, so it's `Send + Sync + 'static` for Tauri while the
//! commands mutate pieces of it. Guards are always dropped before any `.await`
//! (a held `std::sync::Mutex` guard would make a command future non-`Send`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use myo_core::{
    AsrClient, BrainClient, Capabilities, DreamConfig, LlmClient, Memory, ShellSettings, TtsClient,
    TurnAllocator, TurnId, WebSearch, MYO_PERSONA,
};

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

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
    /// The voice: POSTs reply text to MyOwnLLM's `/v1/audio/speech` for a
    /// hardware-tiered synthesized voice, falling back to WebSpeech when the
    /// engine can't synthesize. Built up front like the ears.
    pub tts: TtsClient,
    /// Myo's **native brain**: streams replies straight from MyOwnLLM's
    /// OpenAI-compatible endpoint, so a conversation needs no Odysseus (see
    /// `docs/native-agent.md`). Shared (`Arc`) so memory tools can embed through
    /// it from inside a tool task.
    pub llm: Arc<LlmClient>,
    /// The shared web-search client the native `web_search` tool uses (keyless
    /// DuckDuckGo by default; configurable to a SearXNG instance).
    pub web: Arc<WebSearch>,
    /// Myo's memory — working (recent conversation) + long-term (durable,
    /// embedded) layers. Owns what was the in-process history; the persona is
    /// prepended per turn, not stored here.
    pub memory: Arc<Memory>,
    /// Allocates a fresh turn id per utterance.
    pub turns: TurnAllocator,
    /// The persisted shell settings (capabilities + incognito).
    pub settings: Mutex<ShellSettings>,
    /// Spawned engine children, kept alive (and killed on app exit) here.
    pub children: Mutex<Vec<EngineChild>>,
    /// In-flight turn tasks, so a turn can be cancelled (barge-in / user stop).
    pub tasks: Mutex<HashMap<TurnId, TurnTask>>,
    /// Unix-millis of the last user-facing activity — the clock Dream mode waits
    /// on so it only ever runs during genuine downtime.
    last_activity_ms: AtomicI64,
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: String,
        brain: BrainClient,
        asr: AsrClient,
        tts: TtsClient,
        llm: Arc<LlmClient>,
        web: Arc<WebSearch>,
        memory: Arc<Memory>,
        settings: ShellSettings,
    ) -> Self {
        Self {
            token,
            brain,
            asr,
            tts,
            llm,
            web,
            memory,
            turns: TurnAllocator::new(),
            settings: Mutex::new(settings),
            children: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
            last_activity_ms: AtomicI64::new(now_ms()),
        }
    }

    /// The current capability toggles (which tools the agent loop may offer).
    pub fn capabilities(&self) -> Capabilities {
        self.settings.lock().unwrap().capabilities
    }

    /// Whether memory is paused for this conversation (incognito).
    pub fn incognito(&self) -> bool {
        self.settings.lock().unwrap().incognito
    }

    /// The Dream-mode policy (a snapshot of the persisted settings).
    pub fn dream_config(&self) -> DreamConfig {
        self.settings.lock().unwrap().dream.clone()
    }

    /// Mark "something just happened" — resets the downtime clock Dream mode
    /// waits on. Called at the start and end of every turn.
    pub fn mark_activity(&self) {
        self.last_activity_ms.store(now_ms(), Ordering::Relaxed);
    }

    /// Is the system in genuine downtime — no turn running, and at least
    /// `idle_secs` since the last activity? The gate Dream mode runs behind.
    pub fn is_idle(&self, idle_secs: u64) -> bool {
        let running = self
            .tasks
            .lock()
            .unwrap()
            .values()
            .any(|t| !t.done.load(Ordering::Relaxed));
        if running {
            return false;
        }
        let idle_for = now_ms() - self.last_activity_ms.load(Ordering::Relaxed);
        idle_for >= (idle_secs as i64) * 1000
    }

    /// The system prompt every turn opens with: the user's custom persona when
    /// set (and non-empty), otherwise the built-in [`MYO_PERSONA`].
    pub fn persona(&self) -> String {
        let s = self.settings.lock().unwrap();
        match s.persona.as_deref().map(str::trim) {
            Some(p) if !p.is_empty() => p.to_string(),
            _ => MYO_PERSONA.to_string(),
        }
    }

    /// Whether a non-empty custom persona override is in force (vs. the default).
    pub fn persona_is_custom(&self) -> bool {
        let s = self.settings.lock().unwrap();
        s.persona
            .as_deref()
            .map(str::trim)
            .is_some_and(|p| !p.is_empty())
    }

    /// Tear down every engine Myo *spawned* — each [`EngineChild`]'s `Drop` kills
    /// and reaps its process, so closing Myo closes the stack it started (which
    /// closes theirs in turn). Engines Myo merely *attached* to were never
    /// tracked here, so this only ever closes what Myo itself opened. Called from
    /// the Tauri `RunEvent::Exit` hook, since Tauri may `process::exit` on close
    /// without unwinding (so `Drop` alone wouldn't fire).
    pub fn shutdown(&self) {
        let mut children = self.children.lock().unwrap();
        if !children.is_empty() {
            eprintln!("myo: closing {} spawned engine(s)", children.len());
            children.clear();
        }
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
