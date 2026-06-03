//! The `remember` / `recall` tools — Myo's deliberate hand on its own memory.
//!
//! Recall also happens automatically every turn (the loop injects relevant
//! memories before the model even runs), but these tools let Myo act on its
//! memory *on purpose*: save something it just learned, or look something up
//! mid-task. Writing is intentional and visible (a `remember` shows as an
//! activity pill), and it honors **incognito** — when memory is paused, nothing
//! is saved. Both gate under [`Category::Memory`], which is always available:
//! memory is part of the companion, not one of the four opt-in capabilities.

use anyhow::{anyhow, Result};
use serde_json::{json, Value};

use crate::memory::{RECALL_K, RECALL_MIN_SCORE};

use super::{Category, Tool, ToolCtx, ToolResult};

/// Embed one string via the engine, returning its vector.
async fn embed_one(ctx: &ToolCtx, text: &str) -> Result<Vec<f32>> {
    let vectors = ctx
        .llm
        .embed(std::slice::from_ref(&text.to_string()))
        .await
        .map_err(|e| anyhow!("couldn't reach the embedding model: {e}"))?;
    vectors
        .into_iter()
        .next()
        .ok_or_else(|| anyhow!("the embedding model returned no vector"))
}

pub struct RememberTool;

#[async_trait::async_trait]
impl Tool for RememberTool {
    fn name(&self) -> &str {
        "remember"
    }

    fn category(&self) -> Category {
        Category::Memory
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("text").and_then(Value::as_str).map(str::to_string)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "remember",
                "description": "Save a durable memory about the user or the world that's worth \
                                keeping for future conversations — a preference, a fact, an \
                                ongoing project, an important detail. Write one clear, \
                                self-contained sentence. Recall is automatic later, so remember \
                                things you'd want to know next time.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "text": {
                            "type": "string",
                            "description": "The memory, as one self-contained sentence."
                        },
                        "category": {
                            "type": "string",
                            "description": "A short label, e.g. 'preference', 'fact', 'project', 'event'. Defaults to 'fact'."
                        }
                    },
                    "required": ["text"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult> {
        let text = args
            .get("text")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("remember requires a 'text' string"))?
            .trim()
            .to_string();
        if text.is_empty() {
            return Err(anyhow!("remember requires non-empty 'text'"));
        }
        // Responsible: incognito means nothing is written down.
        if ctx.incognito {
            return Ok(ToolResult::text(
                "Incognito is on, so I didn't save that to long-term memory.",
            ));
        }
        let category = args
            .get("category")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .unwrap_or("fact");

        let vector = embed_one(ctx, &text).await?;
        let rec = ctx.memory.remember(&text, category, vector)?;
        ctx.progress(self.name(), format!("Remembered: {text}"));
        Ok(ToolResult::text(format!(
            "Saved to long-term memory (id {}, category {category}).",
            rec.id
        )))
    }
}

pub struct RecallTool;

#[async_trait::async_trait]
impl Tool for RecallTool {
    fn name(&self) -> &str {
        "recall"
    }

    fn category(&self) -> Category {
        Category::Memory
    }

    fn headline(&self, args: &Value) -> Option<String> {
        args.get("query")
            .and_then(Value::as_str)
            .map(str::to_string)
    }

    fn schema(&self) -> Value {
        json!({
            "type": "function",
            "function": {
                "name": "recall",
                "description": "Search your long-term memory for things you remember about a \
                                topic. Relevant memories are already surfaced automatically each \
                                turn; use this when you want to look something specific up on \
                                purpose.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "What to look up in memory." },
                        "limit": {
                            "type": "integer",
                            "description": "Max memories to return (default 5)."
                        }
                    },
                    "required": ["query"]
                }
            }
        })
    }

    async fn execute(&self, args: Value, ctx: &ToolCtx) -> Result<ToolResult> {
        let query = args
            .get("query")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("recall requires a 'query' string"))?
            .to_string();
        let limit = args
            .get("limit")
            .and_then(Value::as_u64)
            .map(|n| n.clamp(1, 10) as usize)
            .unwrap_or(RECALL_K);

        let vector = embed_one(ctx, &query).await?;
        let hits = ctx.memory.recall(&vector, limit, RECALL_MIN_SCORE);
        if hits.is_empty() {
            return Ok(ToolResult::text(format!(
                "Nothing in long-term memory about \"{query}\"."
            )));
        }
        let mut text = format!("From long-term memory about \"{query}\":\n");
        for h in &hits {
            text.push_str(&format!("- {}\n", h.text));
        }
        Ok(ToolResult::text(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::sync::mpsc;

    #[tokio::test]
    async fn remember_is_paused_in_incognito() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let mut ctx = ToolCtx::test(tx);
        ctx.incognito = true;
        let out = RememberTool
            .execute(json!({ "text": "secret" }), &ctx)
            .await
            .unwrap();
        assert!(out.text.contains("Incognito"));
        // Nothing was written.
        assert_eq!(ctx.memory.long_term_len(), 0);
    }

    #[tokio::test]
    async fn remember_requires_text() {
        let (tx, _rx) = mpsc::unbounded_channel();
        let ctx = ToolCtx::test(tx);
        assert!(RememberTool.execute(json!({}), &ctx).await.is_err());
        assert!(RememberTool
            .execute(json!({ "text": "   " }), &ctx)
            .await
            .is_err());
    }
}
