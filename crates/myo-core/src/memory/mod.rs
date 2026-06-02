//! Myo's memory — a layered, local-first system that lets Myo actually remember.
//!
//! Two integrated layers, brought together at turn time:
//!
//!   * **Layer 1 — working memory** ([`WorkingMemory`]): the live conversation,
//!     volatile and bounded. Immediate continuity — "what we're talking about
//!     right now."
//!   * **Layer 2 — long-term memory** ([`LongTermMemory`]): durable, embedded
//!     facts/preferences/events recalled by meaning (cosine over the local
//!     embeddings MyOwnLLM serves). Persistent — "what Myo knows about you."
//!
//! How they come together each turn (see [`crate::converse::run_turn_native`]):
//! the user's message is embedded and used to **recall** the most relevant
//! long-term memories, which are injected as a system note above the **working**
//! window; Myo answers with that combined context. Myo writes to the long-term
//! layer deliberately, through the `remember` tool — intentional, not a silent
//! recording of everything — and the user stays in control via the Memory panel
//! (list / forget) and **incognito**, which pauses writes.
//!
//! [`Memory`] is the facade the shell holds; it owns both layers and keeps them
//! free of any LLM dependency (embedding happens at the call site, where the
//! engine client lives), so this module stays small and unit-testable.

mod store;
mod working;

pub use store::{LongTermMemory, MemoryHit, MemoryRecord};
pub use working::WorkingMemory;

use std::path::Path;

use anyhow::Result;

use crate::llm::ChatMessage;

/// How many long-term memories a turn recalls into context.
pub const RECALL_K: usize = 5;
/// Minimum cosine score for a recalled memory to be injected — keeps weakly
/// related memories out of the prompt.
pub const RECALL_MIN_SCORE: f32 = 0.35;
/// How many recent messages working memory keeps.
const WORKING_MAX: usize = 40;

/// The composed memory system the shell holds — both layers behind one door.
pub struct Memory {
    working: WorkingMemory,
    long_term: LongTermMemory,
}

impl Memory {
    /// Open Myo's memory rooted at `dir` (e.g. `~/.myo`): a fresh working layer
    /// and the durable store at `dir/memory.db`.
    pub fn open(dir: &Path) -> Result<Self> {
        std::fs::create_dir_all(dir).ok();
        Ok(Self {
            working: WorkingMemory::new(WORKING_MAX),
            long_term: LongTermMemory::open(&dir.join("memory.db"))?,
        })
    }

    /// An entirely in-RAM memory — for tests.
    #[cfg(test)]
    pub fn in_memory() -> Result<Self> {
        Ok(Self {
            working: WorkingMemory::new(WORKING_MAX),
            long_term: LongTermMemory::open_in_memory()?,
        })
    }

    // ── Layer 1: working memory ──────────────────────────────────────────────

    /// The recent conversation window, for prompt assembly.
    pub fn working_window(&self) -> Vec<ChatMessage> {
        self.working.window()
    }

    /// Record the user's turn in working memory.
    pub fn record_user(&self, text: &str) {
        self.working.record_user(text);
    }

    /// Record Myo's reply in working memory.
    pub fn record_assistant(&self, text: &str) {
        self.working.record_assistant(text);
    }

    // ── Layer 2: long-term memory ────────────────────────────────────────────

    /// Save a durable memory (the caller supplies the embedding).
    pub fn remember(
        &self,
        text: &str,
        category: &str,
        embedding: Vec<f32>,
    ) -> Result<MemoryRecord> {
        self.long_term.insert(text, category, embedding)
    }

    /// Recall the memories most relevant to `query_embedding`.
    pub fn recall(&self, query_embedding: &[f32], k: usize, min_score: f32) -> Vec<MemoryHit> {
        self.long_term.recall(query_embedding, k, min_score)
    }

    /// List durable memories (optionally filtered by a text query) — the Memory
    /// panel's source.
    pub fn list(&self, query: Option<&str>) -> Vec<MemoryRecord> {
        match query {
            Some(q) if !q.trim().is_empty() => self.long_term.search(q),
            _ => self.long_term.all(),
        }
    }

    /// Forget one durable memory by id.
    pub fn forget(&self, id: i64) -> Result<bool> {
        self.long_term.remove(id)
    }

    /// How many durable memories exist (lets a turn skip recall when empty).
    pub fn long_term_len(&self) -> usize {
        self.long_term.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn facade_composes_both_layers() {
        let m = Memory::in_memory().unwrap();

        // Layer 1: working memory accumulates the conversation.
        m.record_user("hi");
        m.record_assistant("hello");
        assert_eq!(m.working_window().len(), 2);

        // Layer 2: a remembered fact is recalled by a similar embedding.
        assert_eq!(m.long_term_len(), 0);
        m.remember("the user loves sailing", "preference", vec![1.0, 0.0, 0.0])
            .unwrap();
        assert_eq!(m.long_term_len(), 1);
        let hits = m.recall(&[0.95, 0.05, 0.0], RECALL_K, RECALL_MIN_SCORE);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "the user loves sailing");

        // list + forget round-trip.
        let id = m.list(None)[0].id;
        assert!(m.forget(id).unwrap());
        assert_eq!(m.long_term_len(), 0);
    }
}
