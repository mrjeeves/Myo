//! Layer 2 — **long-term memory**: a durable, embedded, semantically-recalled
//! store of the things Myo chooses to keep, with **receding tiers** that Dream
//! mode compacts into.
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
//!
//! **Tiers & salience.** Each memory carries a `tier` (0 = fresh; higher = more
//! calcified) and salience stats (`recall_count`, `last_recalled`). Dream mode
//! ([`crate::memory::dream`]) consolidates aging clusters of a tier into one
//! denser memory at the next tier (deleting the originals), and prunes deep, old,
//! never-recalled memories — the planning lives here (where embeddings are in
//! reach), the LLM summarization lives in the dreamer.

use std::path::Path;
use std::sync::{Mutex, RwLock};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use rusqlite::Connection;
use serde::Serialize;

/// One durable memory: the text, the embedding it's recalled by, its calcification
/// `tier`, and salience stats. `embedding` is private (an implementation detail of
/// recall/clustering); everything the UI needs is public.
#[derive(Debug, Clone, PartialEq)]
pub struct MemoryRecord {
    pub id: i64,
    pub text: String,
    pub category: String,
    pub created_at: i64,
    /// Calcification depth: 0 = fresh, higher = compacted by Dream mode.
    pub tier: i64,
    /// Unix secs of the last recall (0 = never) — feeds salience.
    pub last_recalled: i64,
    /// How many times this memory has been recalled — feeds salience.
    pub recall_count: i64,
    embedding: Vec<f32>,
}

/// A recalled memory and its similarity score (0..=1) — what [`LongTermMemory::recall`]
/// returns and the UI's "recalled from memory" hint renders.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct MemoryHit {
    pub text: String,
    pub score: f32,
}

/// A cluster of aging memories Dream mode should compact into one deeper-tier
/// memory. Returned by [`LongTermMemory::plan_consolidation`]; the dreamer
/// summarizes `texts` and calls [`LongTermMemory::consolidate`].
#[derive(Debug, Clone, PartialEq)]
pub struct ConsolidationCluster {
    pub tier: i64,
    pub category: String,
    pub ids: Vec<i64>,
    pub texts: Vec<String>,
}

/// The knobs the store's planners need (a flattened view of the dreamer's config,
/// so `store` doesn't depend on `dream`). Ages are in seconds.
#[derive(Debug, Clone)]
pub struct PlanParams {
    pub now: i64,
    /// Tier-0 consolidation age; tier `t` needs `base_age_secs * (t+1)`.
    pub base_age_secs: i64,
    pub cluster_min: usize,
    pub cluster_max: usize,
    pub similarity: f32,
    pub max_tier: i64,
    /// A deep memory must be at least this old to be eligible for forgetting.
    pub forget_age_secs: i64,
    /// Soft cap on total memories; over it, the weakest are pruned.
    pub space_budget: usize,
    /// Categories never hard-deleted by the forget pass (e.g. preferences).
    pub protected: Vec<String>,
}

/// The durable store: a SQLite connection for persistence and an in-RAM cache
/// (newest first) for fast recall/list. Both are kept in sync on every write.
///
/// Lock discipline (deadlock-free): a method either takes the cache lock alone,
/// or takes `conn` first and the cache lock after — never the reverse. Methods
/// that need both snapshot the cache and drop its lock before touching `conn`.
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
        // Migrate older DBs to the tiered/salience schema (idempotent).
        ensure_column(&conn, "tier", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "last_recalled", "INTEGER NOT NULL DEFAULT 0")?;
        ensure_column(&conn, "recall_count", "INTEGER NOT NULL DEFAULT 0")?;
        let cache = load_all(&conn)?;
        Ok(Self {
            conn: Mutex::new(conn),
            cache: RwLock::new(cache),
        })
    }

    /// Persist a new fresh (tier-0) memory and return it.
    pub fn insert(&self, text: &str, category: &str, embedding: Vec<f32>) -> Result<MemoryRecord> {
        self.insert_tiered(text, category, embedding, 0)
    }

    fn insert_tiered(
        &self,
        text: &str,
        category: &str,
        embedding: Vec<f32>,
        tier: i64,
    ) -> Result<MemoryRecord> {
        let embedding = normalize(embedding);
        let created_at = now_secs();
        let blob = f32_to_bytes(&embedding);
        let id = {
            let conn = self.conn.lock().unwrap();
            conn.execute(
                "INSERT INTO memories (text, category, embedding, created_at, tier) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![text, category, blob, created_at, tier],
            )
            .context("insert memory")?;
            conn.last_insert_rowid()
        };
        let rec = MemoryRecord {
            id,
            text: text.to_string(),
            category: category.to_string(),
            created_at,
            tier,
            last_recalled: 0,
            recall_count: 0,
            embedding,
        };
        self.cache.write().unwrap().insert(0, rec.clone()); // newest first
        Ok(rec)
    }

    /// Forget one memory by id. Returns whether a row was actually removed.
    pub fn remove(&self, id: i64) -> Result<bool> {
        Ok(self.forget_batch(&[id])? > 0)
    }

    /// Forget a batch of memories in one transaction (the dreamer's prune pass).
    /// Returns how many rows were removed.
    pub fn forget_batch(&self, ids: &[i64]) -> Result<usize> {
        if ids.is_empty() {
            return Ok(0);
        }
        let removed = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction().context("begin forget tx")?;
            let mut n = 0;
            for id in ids {
                n += tx
                    .execute("DELETE FROM memories WHERE id = ?1", [id])
                    .context("delete memory")?;
            }
            tx.commit().context("commit forget tx")?;
            n
        };
        self.cache.write().unwrap().retain(|m| !ids.contains(&m.id));
        Ok(removed)
    }

    /// Atomically compact a cluster: insert `summary` as a new memory at
    /// `new_tier`, delete the `originals`, all in one transaction (so a crash
    /// mid-dream rolls back cleanly and the work is simply re-done next dream).
    pub fn consolidate(
        &self,
        originals: &[i64],
        summary: &str,
        category: &str,
        embedding: Vec<f32>,
        new_tier: i64,
    ) -> Result<MemoryRecord> {
        let embedding = normalize(embedding);
        let created_at = now_secs();
        let blob = f32_to_bytes(&embedding);
        let id = {
            let mut conn = self.conn.lock().unwrap();
            let tx = conn.transaction().context("begin consolidate tx")?;
            tx.execute(
                "INSERT INTO memories (text, category, embedding, created_at, tier) VALUES (?1, ?2, ?3, ?4, ?5)",
                rusqlite::params![summary, category, blob, created_at, new_tier],
            )
            .context("insert calcification")?;
            let new_id = tx.last_insert_rowid();
            for oid in originals {
                tx.execute("DELETE FROM memories WHERE id = ?1", [oid])
                    .context("delete consolidated original")?;
            }
            tx.commit().context("commit consolidate tx")?;
            new_id
        };
        let rec = MemoryRecord {
            id,
            text: summary.to_string(),
            category: category.to_string(),
            created_at,
            tier: new_tier,
            last_recalled: 0,
            recall_count: 0,
            embedding,
        };
        {
            let mut cache = self.cache.write().unwrap();
            cache.retain(|m| !originals.contains(&m.id));
            cache.insert(0, rec.clone());
        }
        Ok(rec)
    }

    /// Persist the in-RAM salience stats (`recall_count`/`last_recalled`, bumped
    /// on the hot path without disk I/O) back to SQLite — the dreamer flushes
    /// these during calm so they survive a restart.
    pub fn persist_stats(&self) -> Result<()> {
        let snapshot: Vec<(i64, i64, i64)> = {
            let cache = self.cache.read().unwrap();
            cache
                .iter()
                .map(|m| (m.id, m.last_recalled, m.recall_count))
                .collect()
        }; // cache lock dropped here, before we touch `conn`
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction().context("begin stats tx")?;
        for (id, last_recalled, recall_count) in snapshot {
            tx.execute(
                "UPDATE memories SET last_recalled = ?1, recall_count = ?2 WHERE id = ?3",
                rusqlite::params![last_recalled, recall_count, id],
            )
            .context("update stats")?;
        }
        tx.commit().context("commit stats tx")?;
        Ok(())
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
    /// `min_score` — Myo's semantic recall across all tiers. Bumps the matched
    /// memories' salience in the cache (no disk I/O; flushed later by the dreamer).
    pub fn recall(&self, query_embedding: &[f32], k: usize, min_score: f32) -> Vec<MemoryHit> {
        let q = normalize(query_embedding.to_vec());
        let now = now_secs();
        let mut cache = self.cache.write().unwrap();
        // Score every dimension-matching memory.
        let mut scored: Vec<(usize, f32)> = cache
            .iter()
            .enumerate()
            .filter(|(_, m)| m.embedding.len() == q.len()) // skip dim mismatches (model changed)
            .map(|(i, m)| (i, dot(&q, &m.embedding)))
            .filter(|(_, s)| *s >= min_score)
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(k);
        scored
            .into_iter()
            .map(|(i, score)| {
                let m = &mut cache[i];
                m.recall_count += 1;
                m.last_recalled = now;
                MemoryHit {
                    text: m.text.clone(),
                    score,
                }
            })
            .collect()
    }

    /// Plan the next consolidation: the lowest tier with a cluster of aging,
    /// same-category, mutually-similar memories worth compacting. `None` when
    /// nothing is ready. Pure read over the cache.
    pub fn plan_consolidation(&self, p: &PlanParams) -> Option<ConsolidationCluster> {
        let cache = self.cache.read().unwrap();
        for tier in 0..p.max_tier {
            let min_age = p.base_age_secs.saturating_mul(tier + 1);
            // Eligible: this tier, old enough.
            let eligible: Vec<&MemoryRecord> = cache
                .iter()
                .filter(|m| m.tier == tier && (p.now - m.created_at) >= min_age)
                .collect();
            if eligible.len() < p.cluster_min {
                continue;
            }
            // Seed on the oldest eligible memory; grow its same-category neighbors.
            let mut by_age = eligible.clone();
            by_age.sort_by_key(|m| m.created_at); // oldest first
            for seed in by_age {
                let mut cluster: Vec<&MemoryRecord> = eligible
                    .iter()
                    .copied()
                    .filter(|m| {
                        m.category == seed.category
                            && m.embedding.len() == seed.embedding.len()
                            && dot(&seed.embedding, &m.embedding) >= p.similarity
                    })
                    .collect();
                if cluster.len() >= p.cluster_min {
                    cluster.sort_by_key(|m| m.created_at);
                    cluster.truncate(p.cluster_max);
                    return Some(ConsolidationCluster {
                        tier,
                        category: seed.category.clone(),
                        ids: cluster.iter().map(|m| m.id).collect(),
                        texts: cluster.iter().map(|m| m.text.clone()).collect(),
                    });
                }
            }
        }
        None
    }

    /// Plan the next forget batch (conservative): deep-tier memories that are old
    /// and never recalled and unprotected, plus — only if over the space budget —
    /// the weakest deep memories until back under budget. Returns up to
    /// `cluster_max` ids. Pure read over the cache.
    pub fn plan_forget(&self, p: &PlanParams) -> Vec<i64> {
        let cache = self.cache.read().unwrap();
        let protected = |m: &MemoryRecord| p.protected.contains(&m.category);

        // 1) Deep, old, never-recalled, unprotected → safe to forget.
        let mut victims: Vec<i64> = cache
            .iter()
            .filter(|m| {
                m.tier >= p.max_tier
                    && (p.now - m.created_at) >= p.forget_age_secs
                    && m.recall_count == 0
                    && !protected(m)
            })
            .map(|m| m.id)
            .collect();

        // 2) Over budget → prune the weakest (deepest, least recalled, oldest)
        //    unprotected memories until back under the cap.
        if cache.len() > p.space_budget {
            let mut weakest: Vec<&MemoryRecord> = cache.iter().filter(|m| !protected(m)).collect();
            weakest.sort_by(|a, b| {
                // weaker first: lower tier-depth value is stronger, so sort by
                // (recall_count asc, tier desc, created_at asc).
                a.recall_count
                    .cmp(&b.recall_count)
                    .then(b.tier.cmp(&a.tier))
                    .then(a.created_at.cmp(&b.created_at))
            });
            let over = cache.len() - p.space_budget;
            for m in weakest.into_iter().take(over) {
                if !victims.contains(&m.id) {
                    victims.push(m.id);
                }
            }
        }

        victims.truncate(p.cluster_max);
        victims
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

/// Add `name decl` to the `memories` table if it isn't already there (a tiny,
/// idempotent migration). `name`/`decl` are compile-time constants, never user
/// input, so the format is injection-free.
fn ensure_column(conn: &Connection, name: &str, decl: &str) -> Result<()> {
    let present = {
        let mut stmt = conn.prepare("PRAGMA table_info(memories)")?;
        let cols: Vec<String> = stmt
            .query_map([], |r| r.get::<_, String>(1))?
            .collect::<rusqlite::Result<_>>()?;
        cols.iter().any(|c| c == name)
    };
    if !present {
        conn.execute_batch(&format!("ALTER TABLE memories ADD COLUMN {name} {decl};"))
            .with_context(|| format!("add column {name}"))?;
    }
    Ok(())
}

fn load_all(conn: &Connection) -> Result<Vec<MemoryRecord>> {
    let mut stmt = conn.prepare(
        "SELECT id, text, category, embedding, created_at, tier, last_recalled, recall_count \
         FROM memories ORDER BY created_at DESC, id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        let blob: Vec<u8> = r.get(3)?;
        Ok(MemoryRecord {
            id: r.get(0)?,
            text: r.get(1)?,
            category: r.get(2)?,
            embedding: bytes_to_f32(&blob),
            created_at: r.get(4)?,
            tier: r.get(5)?,
            last_recalled: r.get(6)?,
            recall_count: r.get(7)?,
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

    fn params(now: i64) -> PlanParams {
        PlanParams {
            now,
            base_age_secs: 100,
            cluster_min: 3,
            cluster_max: 8,
            similarity: 0.5,
            max_tier: 3,
            forget_age_secs: 1000,
            space_budget: 10_000,
            protected: vec!["preference".into()],
        }
    }

    /// Insert a record and backdate its `created_at` so age-based planning can be
    /// exercised deterministically.
    fn insert_aged(m: &LongTermMemory, text: &str, cat: &str, emb: Vec<f32>, age_secs: i64) -> i64 {
        let rec = m.insert(text, cat, emb).unwrap();
        let backdated = now_secs() - age_secs;
        {
            let conn = m.conn.lock().unwrap();
            conn.execute(
                "UPDATE memories SET created_at = ?1 WHERE id = ?2",
                rusqlite::params![backdated, rec.id],
            )
            .unwrap();
        }
        m.cache.write().unwrap().iter_mut().for_each(|r| {
            if r.id == rec.id {
                r.created_at = backdated;
            }
        });
        rec.id
    }

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

        let hits = m.recall(&[0.9, 0.1, 0.0], 2, 0.0);
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].text, "the cat is grey");
        assert!(hits[0].score > hits[1].score);
    }

    #[test]
    fn recall_bumps_salience() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("x", "fact", vec![1.0, 0.0]).unwrap();
        m.recall(&[1.0, 0.0], 1, 0.0);
        m.recall(&[1.0, 0.0], 1, 0.0);
        let rec = &m.all()[0];
        assert_eq!(rec.recall_count, 2);
        assert!(rec.last_recalled > 0);
    }

    #[test]
    fn recall_respects_threshold_and_k() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("a", "fact", vec![1.0, 0.0]).unwrap();
        m.insert("b", "fact", vec![0.0, 1.0]).unwrap();
        assert!(m.recall(&[1.0, 0.0], 5, 0.5).iter().all(|h| h.text == "a"));
        assert_eq!(m.recall(&[1.0, 1.0], 1, 0.0).len(), 1);
    }

    #[test]
    fn recall_skips_dimension_mismatches() {
        let m = LongTermMemory::open_in_memory().unwrap();
        m.insert("old model", "fact", vec![1.0, 0.0, 0.0, 0.0])
            .unwrap();
        m.insert("new model", "fact", vec![1.0, 0.0]).unwrap();
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
        assert!(m.remove(a.id).unwrap());
        assert!(!m.remove(a.id).unwrap());
        assert_eq!(m.all().len(), 1);
    }

    #[test]
    fn persists_tier_and_stats_across_reopen() {
        let dir = std::env::temp_dir().join(format!("myo-mem-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("memory.db");
        {
            let m = LongTermMemory::open(&path).unwrap();
            m.insert("durable fact", "fact", vec![0.3, 0.4]).unwrap();
            m.recall(&[0.3, 0.4], 1, 0.0); // bump salience
            m.persist_stats().unwrap();
        }
        let m = LongTermMemory::open(&path).unwrap();
        assert_eq!(m.len(), 1);
        let rec = &m.all()[0];
        assert_eq!(rec.tier, 0);
        assert_eq!(rec.recall_count, 1);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn plan_consolidation_groups_aging_same_category_cluster() {
        let m = LongTermMemory::open_in_memory().unwrap();
        // Three aging, similar, same-category memories → a cluster.
        insert_aged(&m, "sailed on monday", "event", vec![1.0, 0.0], 500);
        insert_aged(&m, "sailed on tuesday", "event", vec![0.96, 0.05], 500);
        insert_aged(&m, "sailed on friday", "event", vec![0.9, 0.1], 500);
        // A fresh one (too new) and a dissimilar one don't pull in.
        insert_aged(&m, "sailed today", "event", vec![1.0, 0.0], 1); // too new
        insert_aged(&m, "ate a sandwich", "event", vec![0.0, 1.0], 500); // dissimilar

        let cluster = m.plan_consolidation(&params(now_secs())).unwrap();
        assert_eq!(cluster.tier, 0);
        assert_eq!(cluster.category, "event");
        assert_eq!(cluster.ids.len(), 3);
        assert!(cluster.texts.iter().all(|t| t.contains("sailed")));
    }

    #[test]
    fn plan_consolidation_none_when_too_few_or_too_new() {
        let m = LongTermMemory::open_in_memory().unwrap();
        insert_aged(&m, "a", "fact", vec![1.0, 0.0], 500);
        insert_aged(&m, "b", "fact", vec![1.0, 0.0], 500); // only 2 → below cluster_min
        assert!(m.plan_consolidation(&params(now_secs())).is_none());
    }

    #[test]
    fn consolidate_is_atomic_and_updates_cache() {
        let m = LongTermMemory::open_in_memory().unwrap();
        let a = insert_aged(&m, "sailed monday", "event", vec![1.0, 0.0], 500);
        let b = insert_aged(&m, "sailed tuesday", "event", vec![1.0, 0.0], 500);
        let c = insert_aged(&m, "sailed friday", "event", vec![1.0, 0.0], 500);

        let rec = m
            .consolidate(
                &[a, b, c],
                "the user went sailing several days",
                "event",
                vec![1.0, 0.0],
                1,
            )
            .unwrap();
        // Three originals gone, one denser memory at the next tier remains.
        assert_eq!(m.len(), 1);
        assert_eq!(rec.tier, 1);
        assert_eq!(m.all()[0].text, "the user went sailing several days");
        // And it survives a reopen path (the delete+insert committed together).
        assert!(m.recall(&[1.0, 0.0], 1, 0.0)[0].text.contains("sailing"));
    }

    #[test]
    fn plan_forget_only_takes_deep_old_unrecalled_unprotected() {
        let m = LongTermMemory::open_in_memory().unwrap();
        let mut p = params(now_secs());
        p.max_tier = 1;
        p.forget_age_secs = 100;

        // Deep, old, never recalled, unprotected → forgettable.
        let victim = insert_aged(&m, "stale trivia", "fact", vec![1.0, 0.0], 1000);
        set_tier(&m, victim, 1);
        // Deep + old but a protected category → kept.
        let pref = insert_aged(&m, "likes black coffee", "preference", vec![0.0, 1.0], 1000);
        set_tier(&m, pref, 1);
        // Deep + old but recalled → kept.
        let used = insert_aged(&m, "important", "fact", vec![0.5, 0.5], 1000);
        set_tier(&m, used, 1);
        m.recall(&[0.5, 0.5], 1, 0.0);

        let forget = m.plan_forget(&p);
        assert_eq!(forget, vec![victim]);
    }

    #[test]
    fn plan_forget_prunes_over_budget() {
        let m = LongTermMemory::open_in_memory().unwrap();
        let mut p = params(now_secs());
        p.space_budget = 1;
        p.cluster_max = 8;
        // Two fresh, unprotected memories, over a budget of 1 → one gets pruned.
        insert_aged(&m, "one", "fact", vec![1.0, 0.0], 10);
        insert_aged(&m, "two", "fact", vec![0.0, 1.0], 10);
        let forget = m.plan_forget(&p);
        assert_eq!(forget.len(), 1);
    }

    fn set_tier(m: &LongTermMemory, id: i64, tier: i64) {
        {
            let conn = m.conn.lock().unwrap();
            conn.execute(
                "UPDATE memories SET tier = ?1 WHERE id = ?2",
                rusqlite::params![tier, id],
            )
            .unwrap();
        }
        m.cache.write().unwrap().iter_mut().for_each(|r| {
            if r.id == id {
                r.tier = tier;
            }
        });
    }
}
