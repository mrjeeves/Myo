//! Layer 1 — **working memory**: the live conversation, bounded and volatile.
//!
//! This is the short-term layer: the recent back-and-forth that gives a turn its
//! immediate context, kept in RAM and capped so the prompt stays bounded. It's
//! deliberately *not* durable — it's "what we're talking about right now," not
//! "what Myo knows about you" (that's [`super::store`], Layer 2). Restarting Myo
//! starts a fresh working memory; the long-term layer is what carries across.

use std::sync::Mutex;

use crate::llm::ChatMessage;

/// A bounded ring of the most recent conversation turns.
pub struct WorkingMemory {
    turns: Mutex<Vec<ChatMessage>>,
    max: usize,
}

impl WorkingMemory {
    pub fn new(max: usize) -> Self {
        Self {
            turns: Mutex::new(Vec::new()),
            max: max.max(1),
        }
    }

    /// Record the user's turn.
    pub fn record_user(&self, text: &str) {
        self.push(ChatMessage::user(text));
    }

    /// Record Myo's reply (empty replies are skipped).
    pub fn record_assistant(&self, text: &str) {
        if !text.trim().is_empty() {
            self.push(ChatMessage::assistant(text));
        }
    }

    fn push(&self, m: ChatMessage) {
        let mut turns = self.turns.lock().unwrap();
        turns.push(m);
        if turns.len() > self.max {
            let cut = turns.len() - self.max;
            turns.drain(0..cut);
        }
    }

    /// A snapshot of the recent window, oldest-first, for prompt assembly.
    pub fn window(&self) -> Vec<ChatMessage> {
        self.turns.lock().unwrap().clone()
    }

    /// Drop everything (e.g. a "start fresh" action).
    pub fn clear(&self) {
        self.turns.lock().unwrap().clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keeps_order_and_caps_at_max() {
        let w = WorkingMemory::new(4);
        w.record_user("u1");
        w.record_assistant("a1");
        w.record_user("u2");
        w.record_assistant("a2");
        w.record_user("u3"); // overflows → oldest ("u1") drops

        let win = w.window();
        assert_eq!(win.len(), 4);
        assert_eq!(win.first().unwrap().content, "a1");
        assert_eq!(win.last().unwrap().content, "u3");
    }

    #[test]
    fn empty_assistant_reply_is_skipped() {
        let w = WorkingMemory::new(10);
        w.record_user("hi");
        w.record_assistant("   ");
        assert_eq!(w.window().len(), 1);
    }
}
