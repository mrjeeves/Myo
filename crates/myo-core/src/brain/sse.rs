//! Server-Sent-Events decoding + Odysseus→`MyoEvent` normalization.
//!
//! Two pure, heavily-tested pieces sit between the brain's HTTP body and the
//! shell's intent stream:
//!
//!   * [`SseDecoder`] reassembles `data: …\n\n` frames from arbitrary byte
//!     chunks (reqwest hands us whatever the socket coughs up), skipping
//!     `: heartbeat` comments and recognizing the terminal `[DONE]`.
//!   * [`normalize`] folds one decoded Odysseus event into zero-or-more
//!     [`MyoEvent`]s — the only place that knows Odysseus's wire vocabulary.
//!
//! Keeping both Tauri- and network-free means the whole translation layer is
//! testable with string fixtures, which is exactly how the contract is pinned.

use serde_json::{json, Value};

use crate::event::{MyoEvent, TurnId};

/// One item pulled off the SSE stream.
#[derive(Debug, Clone, PartialEq)]
pub enum SseItem {
    /// A complete `data:` payload (JSON text, not yet parsed).
    Data(String),
    /// The stream's terminal `data: [DONE]`.
    Done,
}

/// Incremental SSE frame reassembler. Feed it bytes; get back complete items.
///
/// Buffers as raw bytes and only splits on the ASCII `\n\n` event boundary, so a
/// multi-byte UTF-8 character straddling two network chunks is never torn (no
/// `\n` byte ever occurs inside a multi-byte sequence).
#[derive(Debug, Default)]
pub struct SseDecoder {
    buf: Vec<u8>,
}

impl SseDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a network chunk and return every event that just completed.
    pub fn feed(&mut self, bytes: &[u8]) -> Vec<SseItem> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = find(&self.buf, b"\n\n") {
            let block: Vec<u8> = self.buf.drain(..pos).collect();
            self.buf.drain(..2); // consume the "\n\n"
            if let Some(item) = parse_block(&String::from_utf8_lossy(&block)) {
                out.push(item);
            }
        }
        out
    }

    /// Flush any trailing frame the stream left without a final `\n\n`.
    pub fn finish(&mut self) -> Option<SseItem> {
        if self.buf.is_empty() {
            return None;
        }
        let block = std::mem::take(&mut self.buf);
        parse_block(&String::from_utf8_lossy(&block))
    }
}

/// Parse one event block (the text between `\n\n` boundaries) into an item.
/// Concatenates `data:` lines (per the SSE spec) and ignores `:` comments and
/// any other field lines.
fn parse_block(block: &str) -> Option<SseItem> {
    let mut data = String::new();
    for line in block.split('\n') {
        let line = line.strip_suffix('\r').unwrap_or(line);
        if let Some(rest) = line.strip_prefix("data:") {
            // A single optional leading space after the colon is part of the
            // field syntax, not the value.
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            if !data.is_empty() {
                data.push('\n');
            }
            data.push_str(rest);
        }
        // ":" comment lines (heartbeats) and unknown fields fall through.
    }
    if data.is_empty() {
        return None;
    }
    if data == "[DONE]" {
        Some(SseItem::Done)
    } else {
        Some(SseItem::Data(data))
    }
}

/// Find the first occurrence of `needle` in `hay`.
fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Fold one decoded Odysseus event into the normalized intent stream.
///
/// Dispatch order matches Odysseus (`agent_loop.py:1528`): a `type` field wins;
/// only its absence means the event is a bare text `delta`. Unknown `type`s are
/// dropped silently so new agent events never break the shell (PLAN risk #4).
///
/// `tool_progress`, `agent_prep`, and `budget_exceeded` are handled defensively
/// even though Odysseus's chat-route wrapper currently filters them out of
/// `/api/chat_stream` — other endpoints forward them, and tolerating them costs
/// nothing.
pub fn normalize(v: &Value, turn: TurnId) -> Vec<MyoEvent> {
    let Some(ty) = v.get("type").and_then(Value::as_str) else {
        return normalize_delta(v, turn);
    };

    match ty {
        "tool_start" => vec![MyoEvent::ActivityStart {
            turn,
            tool: str_field(v, "tool"),
            command: opt_str(v, "command"),
            round: opt_u64(v, "round"),
        }],
        "tool_progress" => vec![MyoEvent::ActivityProgress {
            turn,
            tool: str_field(v, "tool"),
            progress: opt_str(v, "tail").or_else(|| opt_str(v, "progress")),
            round: opt_u64(v, "round"),
        }],
        "tool_output" => normalize_tool_output(v, turn),

        "doc_stream_open" => vec![MyoEvent::ArtifactOpen {
            turn,
            title: opt_str(v, "title"),
            language: opt_str(v, "language"),
        }],
        "doc_stream_delta" => vec![MyoEvent::ArtifactDelta {
            // Odysseus sends the *full* body so far — the renderer replaces.
            turn,
            content: str_field(v, "content"),
        }],
        "doc_update" => vec![MyoEvent::ArtifactUpdate {
            turn,
            doc_id: opt_str(v, "doc_id"),
            title: opt_str(v, "title"),
            language: opt_str(v, "language"),
            content: opt_str(v, "content"),
            version: opt_i64(v, "version"),
        }],
        "doc_suggestions" => vec![MyoEvent::ArtifactSuggestions {
            turn,
            doc_id: opt_str(v, "doc_id"),
            suggestions: v.get("suggestions").cloned().unwrap_or(Value::Null),
        }],

        // Agent-driven UI: the directive is `data.ui_event`. A `ui_control`
        // with no (or empty) `ui_event` is malformed — drop it rather than emit
        // a directive the frontend can't dispatch (same posture as unknown types).
        "ui_control" => match v
            .get("data")
            .and_then(|d| d.get("ui_event"))
            .and_then(Value::as_str)
        {
            Some(directive) if !directive.is_empty() => vec![MyoEvent::Ui {
                turn,
                directive: directive.to_string(),
                data: v.get("data").cloned().unwrap_or(Value::Null),
            }],
            _ => vec![],
        },

        "agent_step" => vec![MyoEvent::Progress {
            turn,
            kind: "agent_step".into(),
            data: json!({ "round": opt_u64(v, "round") }),
        }],

        // Everything else is progress/ancillary metadata under one channel,
        // discriminated by `kind`. We keep the richest payload available: the
        // event's own `data` object, or the whole event when it inlines its
        // fields (model_info, compacted, message_saved, budget_exceeded, …).
        "agent_prep" | "budget_exceeded" | "metrics" | "model_info" | "compacted"
        | "message_saved" | "attachments" | "web_sources" | "rag_sources" | "memories_used"
        | "research_progress" | "research_sources" | "research_findings" | "research_done" => {
            vec![MyoEvent::Progress {
                turn,
                kind: ty.into(),
                data: v.get("data").cloned().unwrap_or_else(|| v.clone()),
            }]
        }

        _ => vec![], // unknown → ignore (forward-compatible)
    }
}

/// A bare `{delta: …}` chunk: spoken answer text, unless it's a `thinking` token
/// (reasoning), which is internal and routed to progress instead of TTS.
fn normalize_delta(v: &Value, turn: TurnId) -> Vec<MyoEvent> {
    let Some(text) = v.get("delta").and_then(Value::as_str) else {
        return vec![];
    };
    if v.get("thinking").and_then(Value::as_bool).unwrap_or(false) {
        vec![MyoEvent::Progress {
            turn,
            kind: "thinking".into(),
            data: json!({ "text": text }),
        }]
    } else {
        vec![MyoEvent::AssistantDelta {
            turn,
            text: text.to_string(),
        }]
    }
}

/// `tool_output` may carry an inline image (`image_url` from generate_image, or
/// `screenshot` — a `data:` URL — from the browser) and may piggy-back a
/// `ui_event` directive; we surface both.
fn normalize_tool_output(v: &Value, turn: TurnId) -> Vec<MyoEvent> {
    let mut out = vec![MyoEvent::ActivityOutput {
        turn,
        tool: str_field(v, "tool"),
        output: opt_str(v, "output"),
        exit_code: opt_i64(v, "exit_code"),
        image_url: opt_str(v, "image_url").or_else(|| opt_str(v, "screenshot")),
        round: opt_u64(v, "round"),
    }];
    if let Some(directive) = opt_str(v, "ui_event") {
        out.push(MyoEvent::Ui {
            turn,
            directive,
            data: v.clone(),
        });
    }
    out
}

fn str_field(v: &Value, key: &str) -> String {
    v.get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}
fn opt_str(v: &Value, key: &str) -> Option<String> {
    v.get(key).and_then(Value::as_str).map(str::to_string)
}
fn opt_u64(v: &Value, key: &str) -> Option<u64> {
    v.get(key).and_then(Value::as_u64)
}
fn opt_i64(v: &Value, key: &str) -> Option<i64> {
    v.get(key).and_then(Value::as_i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn data_strings(items: Vec<SseItem>) -> Vec<String> {
        items
            .into_iter()
            .filter_map(|i| match i {
                SseItem::Data(s) => Some(s),
                SseItem::Done => None,
            })
            .collect()
    }

    #[test]
    fn decodes_single_frame() {
        let mut d = SseDecoder::new();
        let items = d.feed(b"data: {\"delta\":\"hi\"}\n\n");
        assert_eq!(items, vec![SseItem::Data("{\"delta\":\"hi\"}".into())]);
    }

    #[test]
    fn reassembles_across_chunk_boundaries() {
        let mut d = SseDecoder::new();
        assert!(d.feed(b"data: {\"de").is_empty()); // mid-frame: nothing yet
        let items = d.feed(b"lta\":\"hi\"}\n\n");
        assert_eq!(data_strings(items), vec!["{\"delta\":\"hi\"}"]);
    }

    #[test]
    fn skips_heartbeat_comments() {
        let mut d = SseDecoder::new();
        let items = d.feed(b": heartbeat 3\n\ndata: {\"type\":\"agent_step\",\"round\":1}\n\n");
        assert_eq!(data_strings(items).len(), 1);
    }

    #[test]
    fn recognizes_done() {
        let mut d = SseDecoder::new();
        let items = d.feed(b"data: [DONE]\n\n");
        assert_eq!(items, vec![SseItem::Done]);
    }

    #[test]
    fn multibyte_split_across_chunks_is_intact() {
        // "café" — the é (2 bytes 0xC3 0xA9) is split across two feeds.
        let mut d = SseDecoder::new();
        let payload = "data: {\"delta\":\"café\"}\n\n".as_bytes();
        let split = 19; // lands between the two bytes of é
        assert!(d.feed(&payload[..split]).is_empty());
        let items = d.feed(&payload[split..]);
        let s = &data_strings(items)[0];
        assert!(s.contains("café"));
    }

    #[test]
    fn normalize_text_delta() {
        let v = json!({ "delta": "hello" });
        assert_eq!(
            normalize(&v, 5),
            vec![MyoEvent::AssistantDelta {
                turn: 5,
                text: "hello".into()
            }]
        );
    }

    #[test]
    fn normalize_thinking_delta_goes_to_progress_not_tts() {
        let v = json!({ "delta": "let me think", "thinking": true });
        match &normalize(&v, 1)[0] {
            MyoEvent::Progress { kind, .. } => assert_eq!(kind, "thinking"),
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn normalize_tool_start() {
        let v = json!({ "type": "tool_start", "tool": "bash", "command": "ls -la", "round": 2 });
        assert_eq!(
            normalize(&v, 9),
            vec![MyoEvent::ActivityStart {
                turn: 9,
                tool: "bash".into(),
                command: Some("ls -la".into()),
                round: Some(2),
            }]
        );
    }

    #[test]
    fn normalize_tool_output_with_screenshot_and_ui_event() {
        let v = json!({
            "type": "tool_output", "tool": "builtin_browser", "command": "open",
            "output": "ok", "exit_code": 0,
            "screenshot": "data:image/png;base64,AAAA",
            "ui_event": "open_panel", "panel": "gallery"
        });
        let out = normalize(&v, 3);
        // First an activity-output carrying the screenshot as the image…
        match &out[0] {
            MyoEvent::ActivityOutput {
                image_url, tool, ..
            } => {
                assert_eq!(tool, "builtin_browser");
                assert_eq!(image_url.as_deref(), Some("data:image/png;base64,AAAA"));
            }
            other => panic!("expected activity output, got {other:?}"),
        }
        // …then the piggy-backed UI directive.
        match &out[1] {
            MyoEvent::Ui { directive, .. } => assert_eq!(directive, "open_panel"),
            other => panic!("expected ui, got {other:?}"),
        }
    }

    #[test]
    fn normalize_ui_control_extracts_directive() {
        let v = json!({ "type": "ui_control", "data": { "ui_event": "toggle", "toggle_name": "web", "state": true } });
        match &normalize(&v, 1)[0] {
            MyoEvent::Ui {
                directive, data, ..
            } => {
                assert_eq!(directive, "toggle");
                assert_eq!(data["toggle_name"], json!("web"));
                assert_eq!(data["state"], json!(true));
            }
            other => panic!("expected ui, got {other:?}"),
        }
    }

    #[test]
    fn malformed_ui_control_without_event_is_ignored() {
        let v = json!({ "type": "ui_control", "data": { "foo": 1 } });
        assert!(normalize(&v, 1).is_empty());
        let v2 = json!({ "type": "ui_control", "data": { "ui_event": "" } });
        assert!(normalize(&v2, 1).is_empty());
    }

    #[test]
    fn normalize_doc_stream() {
        let open = json!({ "type": "doc_stream_open", "title": "Budget", "language": "markdown" });
        assert_eq!(
            normalize(&open, 1),
            vec![MyoEvent::ArtifactOpen {
                turn: 1,
                title: Some("Budget".into()),
                language: Some("markdown".into()),
            }]
        );
        let delta = json!({ "type": "doc_stream_delta", "content": "# Budget\n" });
        assert_eq!(
            normalize(&delta, 1),
            vec![MyoEvent::ArtifactDelta {
                turn: 1,
                content: "# Budget\n".into()
            }]
        );
    }

    #[test]
    fn normalize_doc_update_keeps_id_and_version() {
        let v = json!({ "type": "doc_update", "doc_id": "d1", "content": "body", "version": 4, "title": "T", "language": "md" });
        assert_eq!(
            normalize(&v, 1),
            vec![MyoEvent::ArtifactUpdate {
                turn: 1,
                doc_id: Some("d1".into()),
                title: Some("T".into()),
                language: Some("md".into()),
                content: Some("body".into()),
                version: Some(4),
            }]
        );
    }

    #[test]
    fn normalize_memories_used_becomes_progress() {
        let v = json!({ "type": "memories_used", "data": [{ "text": "ships Friday", "category": "fact", "type": "recalled" }] });
        match &normalize(&v, 2)[0] {
            MyoEvent::Progress { kind, data, .. } => {
                assert_eq!(kind, "memories_used");
                assert_eq!(data[0]["text"], json!("ships Friday"));
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn normalize_inline_metadata_keeps_whole_event() {
        // model_info inlines its fields (no `data` object) — keep them all.
        let v = json!({ "type": "model_info", "model": "myownllm", "suffix": "Research" });
        match &normalize(&v, 1)[0] {
            MyoEvent::Progress { kind, data, .. } => {
                assert_eq!(kind, "model_info");
                assert_eq!(data["model"], json!("myownllm"));
                assert_eq!(data["suffix"], json!("Research"));
            }
            other => panic!("expected progress, got {other:?}"),
        }
    }

    #[test]
    fn unknown_type_is_ignored() {
        let v = json!({ "type": "some_future_event", "whatever": 1 });
        assert!(normalize(&v, 1).is_empty());
    }

    #[test]
    fn empty_object_is_ignored() {
        assert!(normalize(&json!({}), 1).is_empty());
    }
}
