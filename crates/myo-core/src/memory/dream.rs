//! Dream mode — sleep-style memory consolidation that runs only during downtime.
//!
//! Myo is meant to be on 24/7, and a memory that only ever grows eventually
//! overloads recall. Dream mode is the counter-pressure: during **total
//! downtime** (no inference and no other activity for a while — the shell gates
//! this, see `src-tauri/src/dream.rs`), a slow, resumable background thread
//! reaches back to *aging* memories and compacts them into progressively denser
//! **calcifications** in the receding tiers of the store.
//!
//! Each call to [`step`] does at most **one** small, atomic unit of work and
//! returns — so it's interruptible (the moment activity resumes, the shell stops
//! calling `step`) and resumable (the DB is the cursor; the next downtime simply
//! continues). Two kinds of work, cheapest first:
//!
//!   1. **Forget** — prune deep, old, never-recalled, unprotected memories (and,
//!      only if over the space budget, the weakest deep ones). No model needed.
//!   2. **Consolidate** — take an aging cluster of same-category, mutually-similar
//!      memories, summarize them with the model into one durable memory at the
//!      next tier, and delete the originals. The gist survives; detail fades.
//!
//! The *selection* (which memories, which cluster) lives in
//! [`LongTermMemory`](super::store::LongTermMemory) where the embeddings are in
//! reach; this module adds the model summarization and the downtime-only policy.

use anyhow::{anyhow, Result};
use serde::{Deserialize, Serialize};

use crate::event::MyoEvent;
use crate::llm::{ChatMessage, LlmClient};
use crate::memory::store::PlanParams;
use crate::memory::Memory;

const SECS_PER_DAY: f64 = 86_400.0;

/// Dream-mode policy — all tunable, persisted in `~/.myo/config.json`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DreamConfig {
    /// Master switch.
    pub enabled: bool,
    /// Downtime (no inference / activity) required before dreaming begins.
    pub idle_secs: u64,
    /// Pause between dream steps while still idle — keeps it slow and gentle.
    pub step_interval_secs: u64,
    /// How old (days) a tier-0 memory must be to be eligible for consolidation;
    /// each deeper tier requires proportionally more age.
    pub base_age_days: f64,
    /// Cluster sizing for a single calcification.
    pub cluster_min: usize,
    pub cluster_max: usize,
    /// Minimum cosine similarity for memories to calcify together.
    pub similarity: f32,
    /// The deepest tier; memories here are candidates for forgetting.
    pub max_tier: i64,
    /// A deep memory must be at least this old (days) to be forgotten.
    pub forget_age_days: f64,
    /// Soft cap on total stored memories; over it, the weakest are pruned.
    pub space_budget: usize,
    /// Categories never hard-deleted by the forget pass (e.g. preferences).
    pub protected_categories: Vec<String>,
}

impl Default for DreamConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            idle_secs: 60,
            step_interval_secs: 8,
            base_age_days: 3.0,
            cluster_min: 3,
            cluster_max: 8,
            similarity: 0.6,
            max_tier: 3,
            forget_age_days: 30.0,
            space_budget: 5_000,
            protected_categories: vec!["preference".into()],
        }
    }
}

impl DreamConfig {
    /// Flatten into the store's planning params at time `now` (unix secs).
    pub fn plan_params(&self, now: i64) -> PlanParams {
        PlanParams {
            now,
            base_age_secs: (self.base_age_days * SECS_PER_DAY) as i64,
            cluster_min: self.cluster_min,
            cluster_max: self.cluster_max,
            similarity: self.similarity,
            max_tier: self.max_tier,
            forget_age_secs: (self.forget_age_days * SECS_PER_DAY) as i64,
            space_budget: self.space_budget,
            protected: self.protected_categories.clone(),
        }
    }
}

/// What one [`step`] did, so the shell knows whether to keep dreaming (more work
/// remains) or stand down (nothing left to do this downtime).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StepOutcome {
    /// Did a unit of work; call again (after the gentle pause) if still idle.
    Worked,
    /// Nothing to do right now.
    Idle,
}

/// Perform at most one small, atomic dream step. Cheapest work first (forgetting),
/// then one consolidation. Returns whether it did anything.
pub async fn step(
    memory: &Memory,
    llm: &LlmClient,
    cfg: &DreamConfig,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Result<StepOutcome> {
    let lt = memory.long_term();
    let params = cfg.plan_params(now_secs());

    // 1) Forget pass — cheap, no model.
    let forget = lt.plan_forget(&params);
    if !forget.is_empty() {
        let n = lt.forget_batch(&forget)?;
        emit(MyoEvent::Dream {
            phase: "forget".into(),
            note: Some(format!(
                "let go of {n} aging {}",
                plural(n, "memory", "memories")
            )),
        });
        return Ok(StepOutcome::Worked);
    }

    // 2) Consolidation pass — summarize an aging cluster into the next tier.
    if let Some(cluster) = lt.plan_consolidation(&params) {
        emit(MyoEvent::Dream {
            phase: "consolidate".into(),
            note: Some(format!(
                "compacting {} {} memories",
                cluster.ids.len(),
                cluster.category
            )),
        });
        let summary = summarize(llm, &cluster.category, &cluster.texts)
            .await?
            .trim()
            .to_string();
        if summary.is_empty() {
            return Err(anyhow!("consolidation produced an empty summary"));
        }
        let mut vectors = llm.embed(std::slice::from_ref(&summary)).await?;
        let embedding = vectors
            .drain(..)
            .next()
            .ok_or_else(|| anyhow!("no embedding for the calcified memory"))?;
        lt.consolidate(
            &cluster.ids,
            &summary,
            &cluster.category,
            embedding,
            cluster.tier + 1,
        )?;
        return Ok(StepOutcome::Worked);
    }

    Ok(StepOutcome::Idle)
}

/// Ask the model to compact a cluster of related memories into one durable line.
async fn summarize(llm: &LlmClient, category: &str, texts: &[String]) -> Result<String> {
    let mut joined = String::new();
    for t in texts {
        joined.push_str("- ");
        joined.push_str(t);
        joined.push('\n');
    }
    let system = ChatMessage::system(
        "You are consolidating a person's long-term memories during downtime. Merge the related \
         memories below into a single durable memory of one or two sentences that preserves the \
         lasting gist and drops incidental detail. Keep it concrete and write it the same plain \
         way the originals are written. Output only the consolidated memory — no preamble, no \
         quotes, no list.",
    );
    let user = ChatMessage::user(format!("Category: {category}\nMemories:\n{joined}"));
    llm.complete(&[system, user]).await
}

fn plural<'a>(n: usize, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 {
        one
    } else {
        many
    }
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Path-aware engine: `/v1/models`, a non-streaming `/v1/chat/completions`
    /// returning `summary`, and `/v1/embeddings` returning a fixed 2-dim vector.
    async fn serve_engine(summary: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let body = if req.contains("/v1/models") {
                    "{\"data\":[{\"id\":\"m\"}]}".to_string()
                } else if req.contains("/v1/embeddings") {
                    "{\"data\":[{\"index\":0,\"embedding\":[1.0,0.0]}]}".to_string()
                } else {
                    format!("{{\"choices\":[{{\"message\":{{\"content\":\"{summary}\"}}}}]}}")
                };
                let resp = format!(
                    "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn eager_cfg() -> DreamConfig {
        // Age thresholds at zero so fresh seeded memories are immediately eligible.
        DreamConfig {
            base_age_days: 0.0,
            cluster_min: 3,
            cluster_max: 8,
            similarity: 0.5,
            max_tier: 3,
            ..DreamConfig::default()
        }
    }

    #[tokio::test]
    async fn step_consolidates_an_aging_cluster() {
        let base = serve_engine("the user went sailing across several days").await;
        let llm = LlmClient::new(base).unwrap();
        let memory = Memory::in_memory().unwrap();
        // Three similar, same-category memories → one cluster.
        memory
            .remember("sailed monday", "event", vec![1.0, 0.0])
            .unwrap();
        memory
            .remember("sailed tuesday", "event", vec![0.98, 0.02])
            .unwrap();
        memory
            .remember("sailed friday", "event", vec![0.95, 0.05])
            .unwrap();
        assert_eq!(memory.long_term_len(), 3);

        let mut events = Vec::new();
        let outcome = {
            let mut emit = |e: MyoEvent| events.push(e);
            step(&memory, &llm, &eager_cfg(), &mut emit).await.unwrap()
        };

        assert_eq!(outcome, StepOutcome::Worked);
        // Three originals → one calcified memory at tier 1.
        assert_eq!(memory.long_term_len(), 1);
        let rec = &memory.long_term().all()[0];
        assert_eq!(rec.tier, 1);
        assert!(rec.text.contains("sailing"));
        assert!(events
            .iter()
            .any(|e| matches!(e, MyoEvent::Dream { phase, .. } if phase == "consolidate")));
    }

    #[tokio::test]
    async fn step_is_idle_with_nothing_to_do() {
        let base = serve_engine("unused").await;
        let llm = LlmClient::new(base).unwrap();
        let memory = Memory::in_memory().unwrap();
        memory
            .remember("just one memory", "fact", vec![1.0, 0.0])
            .unwrap();
        let mut emit = |_e: MyoEvent| {};
        assert_eq!(
            step(&memory, &llm, &eager_cfg(), &mut emit).await.unwrap(),
            StepOutcome::Idle
        );
    }

    #[test]
    fn config_translates_days_to_seconds() {
        let cfg = DreamConfig {
            base_age_days: 2.0,
            forget_age_days: 10.0,
            ..DreamConfig::default()
        };
        let p = cfg.plan_params(1_000_000);
        assert_eq!(p.base_age_secs, 2 * 86_400);
        assert_eq!(p.forget_age_secs, 10 * 86_400);
        assert_eq!(p.now, 1_000_000);
    }
}
