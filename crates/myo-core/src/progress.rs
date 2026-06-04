//! Watch the model engine's force-load progress and surface it as a `myo://`
//! event the shell renders as an inline bar.
//!
//! Myo force-loads a model by sending `X-MyOwnLLM-Wait: true` (chat/embed) or by
//! hitting the speak/transcribe routes; the engine then blocks that request
//! while it downloads + loads the model, and the caller would otherwise see
//! nothing but a hung connection. MyOwnLLM parks the structured progress at
//! `GET /v1/myownllm/progress` (see its `progress` module); we poll that and
//! re-emit it so the WebView can draw a real bar with a live percentage and
//! status text. Tauri-free: the shell drives the poll loop (see
//! `supervisor::run_progress_poller`) and forwards [`MyoEvent`]s via its emit
//! bridge.

use std::time::Duration;

use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::event::MyoEvent;

/// One model the engine is currently acquiring. Mirrors MyOwnLLM's
/// `progress::ModelProgress` wire shape; every field past `model`/`phase`
/// defaults so a future engine field (or an older engine omitting one) never
/// breaks the poll.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct EngineProgress {
    /// Tag / logical name being acquired (e.g. `gemma4:e2b`).
    pub model: String,
    /// What it's for — `chat`, `embed`, `speak`, `transcribe`, … (status copy).
    #[serde(default)]
    pub kind: String,
    /// `downloading` | `loading` | `ready` | `error`.
    pub phase: String,
    /// 0.0–1.0 when known (a byte-counted download); absent while loading.
    #[serde(default)]
    pub percent: Option<f64>,
    /// Bytes fetched so far (0 when not a byte-counted phase).
    #[serde(default)]
    pub completed: u64,
    /// Total bytes (0 when unknown).
    #[serde(default)]
    pub total: u64,
    /// Human-readable status line, shown verbatim under the bar.
    #[serde(default)]
    pub detail: String,
}

/// GET `{base}/v1/myownllm/progress` and return the active acquisitions.
///
/// Returns an empty vec on *any* failure — engine down, the route absent on an
/// older engine (404), a parse error — so the caller treats "can't tell" as
/// "nothing loading" and the bar simply stays hidden rather than flapping.
pub async fn fetch_active(http: &Client, base_url: &str) -> Vec<EngineProgress> {
    let url = format!("{}/v1/myownllm/progress", base_url.trim_end_matches('/'));
    let Ok(resp) = http.get(&url).send().await else {
        return Vec::new();
    };
    if !resp.status().is_success() {
        return Vec::new();
    }
    match resp.json::<serde_json::Value>().await {
        Ok(v) => parse_active(&v),
        Err(_) => Vec::new(),
    }
}

/// Pull the `active` array out of the endpoint's JSON body, skipping any entry
/// that doesn't deserialize (defensive against a partial/garbled row).
pub fn parse_active(v: &serde_json::Value) -> Vec<EngineProgress> {
    v.get("active")
        .and_then(|a| a.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|e| serde_json::from_value(e.clone()).ok())
                .collect()
        })
        .unwrap_or_default()
}

/// Wrap the active set as a `myo://progress` event (`kind: "model_load"`) the
/// shell renders as an inline bar. An empty set clears the bar. Engine-level,
/// so it carries the sentinel `turn: 0` — the renderer keys on `kind`, not the
/// turn.
pub fn active_event(active: Vec<EngineProgress>) -> MyoEvent {
    MyoEvent::Progress {
        turn: 0,
        kind: "model_load".into(),
        data: json!({ "active": active }),
    }
}

/// Poll the engine's progress endpoint forever, calling `emit` with a
/// `model_load` event while a load is in flight and once more with an empty set
/// when it finishes (to clear the bar). Polls fast while active, idles slowly
/// otherwise. Tolerant of the engine being down or too old for the route — it
/// just sees an empty set and stays quiet — so the shell can fire-and-forget
/// this for the app's lifetime.
pub async fn run_loop<F>(base_url: String, mut emit: F)
where
    F: FnMut(MyoEvent) + Send,
{
    // Fast enough that a download bar moves smoothly; the idle cadence keeps
    // the endpoint barely touched when nothing is loading.
    const ACTIVE_MS: u64 = 450;
    const IDLE_MS: u64 = 1_500;

    let http = Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
        .unwrap_or_else(|_| Client::new());
    let mut was_active = false;
    loop {
        let active = fetch_active(&http, &base_url).await;
        if active.is_empty() {
            // One trailing empty emit dismisses the bar after a run completes.
            if was_active {
                emit(active_event(Vec::new()));
                was_active = false;
            }
            tokio::time::sleep(Duration::from_millis(IDLE_MS)).await;
        } else {
            emit(active_event(active));
            was_active = true;
            tokio::time::sleep(Duration::from_millis(ACTIVE_MS)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_active_reads_entries_and_skips_garbage() {
        let v = json!({"active": [
            {"model":"gemma4:e2b","kind":"chat","phase":"downloading",
             "percent":0.42,"completed":100,"total":238,"detail":"pulling 42%"},
            {"phase":"loading"},               // no `model` → skipped
            {"model":"embed-x","phase":"loading"} // minimal valid (defaults fill in)
        ]});
        let active = parse_active(&v);
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].model, "gemma4:e2b");
        assert_eq!(active[0].kind, "chat");
        assert_eq!(active[0].percent, Some(0.42));
        assert_eq!(active[0].total, 238);
        // The minimal entry takes defaults for everything past model/phase.
        assert_eq!(active[1].model, "embed-x");
        assert_eq!(active[1].phase, "loading");
        assert_eq!(active[1].percent, None);
        assert_eq!(active[1].completed, 0);
        assert_eq!(active[1].detail, "");
    }

    #[test]
    fn parse_active_tolerates_missing_or_wrong_shape() {
        assert!(parse_active(&json!({})).is_empty());
        assert!(parse_active(&json!({ "active": "nope" })).is_empty());
        assert!(parse_active(&json!({ "active": [] })).is_empty());
    }

    #[test]
    fn active_event_routes_to_progress_with_model_load_kind() {
        let ev = active_event(vec![EngineProgress {
            model: "m".into(),
            kind: "chat".into(),
            phase: "downloading".into(),
            percent: Some(0.5),
            completed: 1,
            total: 2,
            detail: "d".into(),
        }]);
        let emitted = ev.emit();
        assert_eq!(emitted.channel, crate::event::channel::PROGRESS);
        assert_eq!(emitted.payload["kind"], json!("model_load"));
        assert_eq!(emitted.payload["data"]["active"][0]["model"], json!("m"));
        assert_eq!(emitted.payload["data"]["active"][0]["percent"], json!(0.5));
    }

    #[test]
    fn empty_active_event_clears_the_bar() {
        let emitted = active_event(Vec::new()).emit();
        assert_eq!(emitted.payload["data"]["active"], json!([]));
    }
}
