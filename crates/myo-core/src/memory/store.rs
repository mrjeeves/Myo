//! Layer 2 — **long-term memory**: a durable, embedded, semantically-recalled
//! store of the things Myo chooses to keep.
//!
//! Durability lives in a single-file SQLite DB under `~/.myo` (roadmap Slice 2);
//! recall is a cosine search done in Rust over an in-RAM cache of the rows, so a
//! turn's lookup never touches disk and scales fine at personal volume (a
//! brute-force dot product over a few thousand normalized vectors is trivial).
//! Writes are incremental single-row SQLite ops — no full-file rewrites — and the
//! cache is kept in lock-step so reads and writes never disagree.
//!
//! Embeddings are stored normalized (so recall is a plain dot product) as a BLOB
//! of little-endian `f32`s. A record whose vector length doesn't match the query
//! (the embedding model changed underfoot) is skipped rather than mis-scored, so
//! swapping models degrades gracefully instead of crashing.

use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

/// One durable memory: a short piece of text Myo kept, plus the embedding it's
/// recalled by. `embedding` is private (it's an implementation detail of recall);
/// everything the UI needs is public.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRecord {
    pub id: i64,
    pub text: String,
    pub category: String,
    pub created_at: i64,
    embedding: Vec<f32>,
}

/// A recalled memory and its similarity score (0..=1) — what [`LongTermMemory::recall`]
/// returns and the UI's "recalled from memory" hint renders.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryHit {
    pub text: String,
    pub score: f32,
}

/// The durable store: a SQLite connection for persistence and an in-RAM cache
/// (newest first) for fast recall/list. Both are kept in sync on every write.
pub struct LongTermMemory {
    conn: Mutex<Connection>,
    cache: RwLock<Vec<MemoryRecord>>,
}

impl LongTermMemory {
    /// Open (creating if needed) the store at `path`, loading every memory into
    /// the cache.
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)
            .with_context(|| format!("open memory db at {}", path.display()))?;
        Self::from_conn(conn)
    }

    /// An ephemeral in-RAM store — for tests.
    #[cfg(test)]
    pub fn open_in_memory() -> Result<Self> {
        Self::from_conn(Connection::open_in_memory()?)
    }

    fn from_conn(conn: Connection) -> Result<Self> {
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS memories (
                id         INTEGER PRIMARY KEY AUTOINCREMENT,
                text       TEXT    NOT NULL,
                category   TEXT    NOT NULL DEFAULT 'fact',
                embedding  BLOB    NOT NULL,
                created_at INTEGER NOT NULL
            );",
        )
        .context("create memories table")?;
        let cache = load_all(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: RwLock::new(cache),
        })
    }

    /// Persist a new memory (embedding normalized on the way in) and return it.
    pub fn insert(&self, text: &str, category: &str, embedding: Vec<f32>) -> Result<MemoryRecord> {
        let embedding = normalize(embedding);
        let created_at = now_secs();
        let blob = f32_to_bytes(&embedding);
        let id = {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO memories (text, category, embedding, created_at) VALUES (?1, ?2, ?3, ?4)",
                rusqlite::params![text, category, blob, created_at],
            )
            .context("insert memory")?;
            conn.last_insert_rowid()
        };
        let rec = MemoryRecord {
            id,
            text: text.to_string(),
            category: category.to_string(),
            created_at,
            embedding,
        };
        self.cache.write().unwrap().insert(0, rec.clone()); // newest first
        Ok(rec)
    }

    /// Forget one memory by id. Returns whether a row was actually removed.
    pub fn remove(&self, id: i64) -> Result<bool> {
        let n = {
            let conn = self.conn.lock().unwrap();
            conn.execute("DELETE FROM memories WHERE id = ?1", [id])
                .context("delete memory")?
        };
        if n > 0 {
            self.cache.write().unwrap().retain(|m| m.id != id);
        }
        Ok(n > 0)
    }

    /// Every memory, newest first.
    pub fn all(&self) -> Vec<MemoryRecord> {
        self.cache.read().unwrap().clone()
    }

    /// Memories whose text contains `query` (case-insensitive) — the Memory
    /// panel's search.
    pub fn search(&self, query: &str) -> Vec<MemoryRecord> {
        let q = query.to_lowercase();
        self.cache
            .read()
            .unwrap()
            .iter()
            .filter(|m| m.text.to_lowercase().contains(&q))
            .cloned()
            .collect()
    }

    /// The `k` memories most similar to `query_embedding` scoring at least
    /// `min_score` — Myo's semantic recall. Pure read, no side effects.
    pub fn recall(&self, query_embedding: &[f32], k: usize, min_score: f32) -> Vec<MemoryHit> {
        let q = normalize(query_embedding.to_vec());
        let cache = self.cache.read().unwrap();
        let mut scored: Vec<MemoryHit> = cache
            .iter()
            .filter(|m| m.embedding.len() == q.len()) // skip dim mismatches (model changed)
            .map(|m| MemoryHit {
                text: m.text.clone(),
                score: dot(&q, &m.embedding),
            })
            .filter(|h| h.score >= min_score)
            .collect();
        scored.sort_by(|a, b| {
            b.score
                .partial_cmp(&a.score)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        scored.truncate(k);
        scored
    }

    /// How many memories are stored — lets a turn skip the embed call entirely
    /// when there's nothing to recall.
    pub fn len(&self) -> usize {
        self.cache.read().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

fn load_all(conn: &Connection) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, category, embedding, created_at FROM memories ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let blob: Vec<u8> = r.get(3)?;
        Ok(MemoryRecord {
            id: r.get(0)?,
            text: r.get(1)?,
            category: r.get(2)?,
            embedding: bytes_to_f32(&blob),
            created_at: r.get(4)?,
        })
    })?;
    let mut out = Vec::new();
    for row in rows {
        out.push(row?);
    }
    Ok(out)
}

/// L2-normalize a vector so similarity is a plain dot product. A zero vector is
/// left as-is (its dot products are all zero, which is the right "no signal").
fn normalize(mut v: Vec<f32>) -> Vec<f32> {
    let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        for x in &mut v {
            *x /= norm;
        }
    }
    v
}

fn dot(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn f32_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut b = Vec::with_capacity(v.len() * 4);
    for x in v {
        b.extend_from_slice(&x.to_le_bytes());
    }
    b
}

fn bytes_to_f32(b: &[u8]) -> Vec<f32> {
    b.chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}

fn now_secs() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_blob_roundtrips() {
        let v = vec![0.5_f32, -1.0, 2.25, 0.0];
        assert_eq!(bytes_to_f32(&f32_to_bytes(&v)), v);
    }

    #[test]
    fn insert_then_recall_ranks_by_similarity() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("the cat is grey", "fact", vec![1.0, 0.0, 0.0])
            .unwrap();
        m.insert("the sky is blue", "fact", vec![0.0, 1.0, 0.0])
            .unwrap();
        m.insert("the grass is green", "fact", vec![0.0, 0.0, 1.0])
            .unwrap();

        // A query closest to the first vector recalls it on top.
        let hits = m.recall(&[0.9, 0.1, 0.0], 2, 0.0);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "the cat is grey");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn recall_respects_threshold_and_k() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("a", "fact", vec![1.0, 0.0]).unwrap();
        m.insert("b", "fact", vec![0.0, 1.0]).unwrap();
        // Orthogonal query → score 0 for both; a positive threshold excludes them.
        assert!(m.recall(&[1.0, 0.0], 5, 0.5).iter().all(|h| h.text == "a"));
        // k caps the count.
        assert_eq!(m.recall(&[1.0, 1.0], 1, 0.0).len(), 1);
    }

    #[test]
    fn recall_skips_dimension_mismatches() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("old model", "fact", vec![1.0, 0.0, 0.0, 0.0])
            .unwrap();
        m.insert("new model", "fact", vec![1.0, 0.0]).unwrap();
        // A 2-dim query only matches the 2-dim record; the 4-dim one is skipped.
        let hits = m.recall(&[1.0, 0.0], 5, 0.1);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].text, "new model");
    }

    #[test]
    fn list_search_and_forget() {
        let m = LongTermMemory::open_in_memory().unwrap();
        let a = m
            .insert("likes oat milk", "preference", vec![1.0, 0.0])
            .unwrap();
        m.insert("works at a bakery", "fact", vec![0.0, 1.0])
            .unwrap();

        assert_eq!(m.all().len(), 2);
        assert_eq!(m.search("OAT").len(), 1);
        assert_eq!(m.search("oat")[0].text, "likes oat milk");

        assert!(m.remove(a.id).unwrap());
        assert!(!m.remove(a.id).unwrap()); // already gone
        assert_eq!(m.all().len(), 1);
    }

    #[test]
    fn persists_across_reopen() {
        let dir = std::env::temp_dir().join(format!("myo-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("memory.db");
        {
            let m = LongTermMemory::open(&path).unwrap();
            m.insert("durable fact", "fact", vec![0.3, 0.4]).unwrap();
        }
        // Reopen: the row (and its embedding) come back.
        let m = LongTermMemory::open(&path).unwrap();
        assert_eq!(m.len(), 1);
        let hits = m.recall(&[0.3, 0.4], 1, 0.0);
        assert_eq!(hits[0].text, "durable fact");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
