//! The shell's managed state — one `Arc<MyoState>` Tauri hands to every command.
//!
//! Following MyOwnLLM's pattern: the managed value is an `Arc<T>` whose `T`
//! carries its own locks, so it's `Send + Sync + 'static` for Tauri while the
//! commands mutate pieces of it. Guards are always dropped before any `.await`
//! (a held `std::sync::Mutex` guard would make a command future non-`Send`).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use myo_core::{
    AsrClient, BrainClient, Capabilities, ChatMessage, LlmClient, ShellSettings, TtsClient,
    TurnAllocator, TurnId, WebSearch, MYO_PERSONA,
};

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

/// One line of the running transcript, tagged with the turn it belongs to.
///
/// The tag is what keeps the transcript coherent now that turns *overlap*: a new
/// utterance no longer cancels the one before it, so several turns can be in
/// flight at once and finish out of order. Recording each reply against its turn
/// lets us slot it back in turn order regardless of which model call returns
/// first.
struct HistoryEntry {
    turn: TurnId,
    msg: ChatMessage,
}

/// The running conversation Myo keeps for context — an ordered, turn-tagged
/// transcript with a bounded length. Split out from [`MyoState`] (behind its own
/// lock there) so the ordering logic is unit-testable without standing up every
/// engine client.
#[derive(Default)]
struct History {
    entries: Vec<HistoryEntry>,
}

impl History {
    /// Keep the transcript bounded so the context sent each turn stays small.
    const MAX: usize = 40;

    /// Open a new turn: append its user message and return the full context to
    /// send — persona first, then the transcript so far (this turn included).
    /// The transcript is capped here (dropping the oldest entries), so context
    /// never grows without bound.
    fn context(&mut self, turn: TurnId, persona: String, user_text: &str) -> Vec<ChatMessage> {
        self.entries.push(HistoryEntry {
            turn,
            msg: ChatMessage::user(user_text),
        });
        if self.entries.len() > Self::MAX {
            let cut = self.entries.len() - Self::MAX;
            self.entries.drain(0..cut);
        }
        let mut messages = Vec::with_capacity(self.entries.len() + 1);
        messages.push(ChatMessage::system(persona));
        messages.extend(self.entries.iter().map(|e| e.msg.clone()));
        messages
    }

    /// Record a finished turn's reply, slotting it **after the last entry of its
    /// own turn** (its user message) and before any later turn — so the
    /// transcript stays in turn order even when a turn finishes before an earlier
    /// one that's still generating. Empty replies are dropped.
    fn record_reply(&mut self, turn: TurnId, text: String) {
        if text.trim().is_empty() {
            return;
        }
        let pos = self
            .entries
            .iter()
            .rposition(|e| e.turn <= turn)
            .map_or(0, |i| i + 1);
        self.entries.insert(
            pos,
            HistoryEntry {
                turn,
                msg: ChatMessage::assistant(text),
            },
        );
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
    /// `docs/native-agent.md`).
    pub llm: LlmClient,
    /// The shared web-search client the native `web_search` tool uses (keyless
    /// DuckDuckGo by default; configurable to a SearXNG instance).
    pub web: Arc<WebSearch>,
    /// The running conversation (user/assistant turns) Myo keeps itself — the
    /// seed of native memory. The persona is prepended per turn, not stored here.
    history: Mutex<History>,
    /// Allocates a fresh turn id per utterance.
    pub turns: TurnAllocator,
    /// The persisted shell settings (capabilities + incognito).
    pub settings: Mutex<ShellSettings>,
    /// Spawned engine children, kept alive (and killed on app exit) here.
    pub children: Mutex<Vec<EngineChild>>,
    /// In-flight turn tasks, kept so a turn can be hard-cancelled on an explicit
    /// "force stop" / teardown. The conversational flow never cancels — turns run
    /// to completion and overlap freely; talking over Myo only hushes her voice.
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
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        token: String,
        brain: BrainClient,
        asr: AsrClient,
        tts: TtsClient,
        llm: LlmClient,
        web: Arc<WebSearch>,
        settings: ShellSettings,
    ) -> Self {
        Self {
            token,
            brain,
            asr,
            tts,
            llm,
            web,
            history: Mutex::new(History::default()),
            turns: TurnAllocator::new(),
            settings: Mutex::new(settings),
            children: Mutex::new(Vec::new()),
            tasks: Mutex::new(HashMap::new()),
        }
    }

    /// The current capability toggles (which tools the agent loop may offer).
    pub fn capabilities(&self) -> Capabilities {
        self.settings.lock().unwrap().capabilities
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

    /// Assemble the chat context for a new user turn — Myo's persona, the
    /// running history, and this message — recording the user message in history
    /// as we go (so the turn is on the record even before its reply lands, and
    /// even if the user keeps talking and opens more turns over the top of it).
    /// History is capped so the context stays bounded.
    pub fn chat_context(&self, turn: TurnId, user_text: &str) -> Vec<ChatMessage> {
        // Resolve the system prompt first (locks `settings`, then drops it) so we
        // never hold the `history` and `settings` locks simultaneously.
        let persona = self.persona();
        self.history
            .lock()
            .unwrap()
            .context(turn, persona, user_text)
    }

    /// Record a turn's reply in the running history, in turn order (an empty
    /// reply is skipped). Turns can finish out of order — Myo lets several run at
    /// once rather than cancelling — so the reply is slotted by its `turn`, not
    /// merely appended.
    pub fn record_reply(&self, turn: TurnId, text: String) {
        self.history.lock().unwrap().record_reply(turn, text);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Flatten a context into `(role, content)` pairs for terse assertions.
    fn shape(ctx: &[ChatMessage]) -> Vec<(&str, &str)> {
        ctx.iter()
            .map(|m| (m.role.as_str(), m.content.as_str()))
            .collect()
    }

    #[test]
    fn context_opens_with_persona_then_the_turn() {
        let mut h = History::default();
        let ctx = h.context(1, "PERSONA".into(), "hello");
        assert_eq!(shape(&ctx), vec![("system", "PERSONA"), ("user", "hello")]);
    }

    #[test]
    fn a_reply_lands_after_its_own_user_turn() {
        let mut h = History::default();
        h.context(1, "P".into(), "u1");
        h.record_reply(1, "r1".into());
        // The next turn sees the full prior exchange, in order.
        let ctx = h.context(2, "P".into(), "u2");
        assert_eq!(
            shape(&ctx),
            vec![
                ("system", "P"),
                ("user", "u1"),
                ("assistant", "r1"),
                ("user", "u2"),
            ]
        );
    }

    #[test]
    fn overlapping_turns_stay_in_turn_order_when_replies_finish_out_of_order() {
        // The whole point of the change: the user keeps talking, so turns 1, 2 and
        // 3 are all open at once (no cancellation). Their replies then come back
        // in a different order than they were asked.
        let mut h = History::default();
        h.context(1, "P".into(), "u1");
        h.context(2, "P".into(), "u2");
        h.context(3, "P".into(), "u3");
        // Replies land 2, then 3, then 1 — yet each slots behind its own user line.
        h.record_reply(2, "r2".into());
        h.record_reply(3, "r3".into());
        h.record_reply(1, "r1".into());

        let ctx = h.context(4, "P".into(), "u4");
        assert_eq!(
            shape(&ctx),
            vec![
                ("system", "P"),
                ("user", "u1"),
                ("assistant", "r1"),
                ("user", "u2"),
                ("assistant", "r2"),
                ("user", "u3"),
                ("assistant", "r3"),
                ("user", "u4"),
            ]
        );
    }

    #[test]
    fn empty_replies_are_not_recorded() {
        let mut h = History::default();
        h.context(1, "P".into(), "u1");
        h.record_reply(1, "   ".into());
        let ctx = h.context(2, "P".into(), "u2");
        assert_eq!(
            shape(&ctx),
            vec![("system", "P"), ("user", "u1"), ("user", "u2")]
        );
    }

    #[test]
    fn history_is_capped_to_the_most_recent_entries() {
        let mut h = History::default();
        // Drive well past the cap; only the newest MAX entries survive.
        for i in 0..(History::MAX as u64 + 25) {
            h.context(i + 1, "P".into(), "u");
        }
        let ctx = h.context(9999, "P".into(), "last");
        // persona + exactly MAX transcript entries (the just-added "last" included).
        assert_eq!(ctx.len(), History::MAX + 1);
        assert_eq!(ctx[0].role, "system");
        assert_eq!(ctx.last().unwrap().content, "last");
    }
}
