//! Automatic memory capture — the model-independent write path.
//!
//! Myo's deliberate `remember` tool only fires when the served model supports
//! tool-calling (Qwen3 yes; the default Gemma family, no). So that long-term
//! memory actually fills on any model, this runs after each turn: a **plain,
//! no-tools completion** ([`LlmClient::complete`]) reads the just-finished
//! exchange and extracts any durable memory worth keeping, which is embedded and
//! stored. It's best-effort and runs off the conversation's hot path, so it never
//! delays a reply; incognito turns are skipped (nothing is written down).
//!
//! Extraction is conservative — only stable, reusable facts/preferences, capped
//! per turn, and de-duplicated against what's already stored — so memory grows
//! with signal, not chatter. Dream mode ([`super::dream`]) later compacts it.

use anyhow::Result;

use crate::llm::{ChatMessage, LlmClient};
use crate::memory::Memory;

/// Don't store a new memory that's essentially a restatement of an existing one.
const DEDUP_SIMILARITY: f32 = 0.92;
/// At most this many memories captured from a single turn.
const MAX_PER_TURN: usize = 3;

/// Inspect one completed exchange and store any durable memories from it. Skips
/// incognito and empty turns. Never returns an error to the caller — memory
/// capture must never disturb the conversation.
pub async fn ingest_turn(
    llm: &LlmClient,
    memory: &Memory,
    user: &str,
    assistant: &str,
    incognito: bool,
) {
    if incognito || user.trim().is_empty() {
        return;
    }
    let items = match extract(llm, user, assistant).await {
        Ok(items) => items,
        Err(_) => return, // engine hiccup — try again next turn
    };
    for (category, text) in items {
        // Embed, dedup, store. Any single failure just skips that memory.
        let embedding = match llm.embed(std::slice::from_ref(&text)).await {
            Ok(mut v) => match v.drain(..).next() {
                Some(e) => e,
                None => continue,
            },
            Err(_) => continue,
        };
        if memory.max_similarity(&embedding) >= DEDUP_SIMILARITY {
            continue; // already remember something equivalent
        }
        let _ = memory.remember(&text, &category, embedding);
    }
}

/// Ask the model (no tools) to pull durable memories out of the exchange.
async fn extract(llm: &LlmClient, user: &str, assistant: &str) -> Result<Vec<(String, String)>> {
    let system = ChatMessage::system(
        "You curate a person's long-term memory. From the exchange below, extract only durable, \
         reusable things worth remembering for future conversations — stable preferences, personal \
         facts, ongoing projects, important dates, relationships. Ignore small talk, one-off \
         context, questions, and anything about you (the assistant). Output each memory on its own \
         line as `category: memory`, where category is a single lowercase word like preference, \
         fact, project, event, or person, and memory is one self-contained sentence written about \
         the user. If there is nothing worth keeping, output exactly NONE.",
    );
    let user_msg = ChatMessage::user(format!("User: {user}\nAssistant: {assistant}"));
    let out = llm.complete(&[system, user_msg]).await?;
    Ok(parse_extracted(&out))
}

/// Parse the model's extraction output into `(category, text)` pairs. Tolerant of
/// the shapes small models actually produce (bullets, missing categories, a
/// trailing NONE) and capped at [`MAX_PER_TURN`].
fn parse_extracted(s: &str) -> Vec<(String, String)> {
    let trimmed = s.trim();
    if trimmed.is_empty() || trimmed.eq_ignore_ascii_case("none") {
        return Vec::new();
    }
    let mut out = Vec::new();
    for raw in trimmed.lines() {
        let line = raw.trim().trim_start_matches(['-', '*', '•', ' ']).trim();
        if line.is_empty() || line.eq_ignore_ascii_case("none") {
            continue;
        }
        let (category, text) = match line.split_once(':') {
            Some((c, t)) => {
                let c = c.trim().to_lowercase();
                // A bare "http" or sentence with a colon isn't a category — keep
                // only short, single-word-ish labels.
                if c.is_empty() || c.len() > 20 || c.contains(' ') {
                    ("fact".to_string(), line.to_string())
                } else {
                    (c, t.trim().to_string())
                }
            }
            None => ("fact".to_string(), line.to_string()),
        };
        if text.is_empty() {
            continue;
        }
        out.push((category, text));
        if out.len() >= MAX_PER_TURN {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_none_yields_nothing() {
        assert!(parse_extracted("NONE").is_empty());
        assert!(parse_extracted("  none  ").is_empty());
        assert!(parse_extracted("").is_empty());
    }

    #[test]
    fn parse_categorized_lines() {
        let out = parse_extracted(
            "preference: the user takes coffee black\nfact: the user lives in Berlin",
        );
        assert_eq!(out.len(), 2);
        assert_eq!(
            out[0],
            (
                "preference".to_string(),
                "the user takes coffee black".to_string()
            )
        );
        assert_eq!(out[1].0, "fact");
    }

    #[test]
    fn parse_tolerates_bullets_and_missing_category() {
        let out = parse_extracted("- the user has a dog named Pip\n* project: writing a novel");
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].0, "fact"); // no category → default
        assert!(out[0].1.contains("Pip"));
        assert_eq!(out[1].0, "project");
    }

    #[test]
    fn parse_caps_per_turn_and_skips_trailing_none() {
        let many = "fact: a\nfact: b\nfact: c\nfact: d\nNONE";
        assert_eq!(parse_extracted(many).len(), MAX_PER_TURN);
    }

    #[test]
    fn parse_does_not_treat_a_sentence_colon_as_category() {
        // A colon mid-sentence shouldn't be mistaken for a category label.
        let out = parse_extracted("the user said: I love hiking on weekends");
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, "fact");
        assert!(out[0].1.contains("I love hiking"));
    }
}
