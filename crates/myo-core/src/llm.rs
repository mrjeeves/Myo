//! The native brain — Myo's own agent loop, talking straight to MyOwnLLM.
//!
//! The pivot (see `docs/native-agent.md`): rather than run Odysseus as a brain
//! sidecar, **Myo is the agent**. This is the first slice — a streaming chat
//! turn against MyOwnLLM's OpenAI-compatible `/v1/chat/completions`, so a
//! conversation needs nothing but the model engine Myo already owns (no
//! Odysseus, no `:17000`). Memory, tools, and native TTS layer onto this loop in
//! later slices; Odysseus is now a *reference*, not a runtime.
//!
//! The wire shape is plain OpenAI streaming, so the same generic [`SseDecoder`]
//! the Odysseus client used drives it; we just read `choices[].delta.content`
//! and emit the identical [`MyoEvent`] stream the UI already renders.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::Client;
use serde::Serialize;
use serde_json::{json, Value};

use crate::brain::sse::{SseDecoder, SseItem};
use crate::event::{MyoEvent, TurnId};

/// Myo's character — the system prompt that opens every conversation. Written
/// for the ear: a TTS voice reads every reply aloud, so it leans on real
/// sentences and natural punctuation for rhythm — not markdown or lists.
pub const MYO_PERSONA: &str = "\
You are Myo, a warm and present companion that lives entirely on the user's own \
machine. You are a voice — everything you say is read aloud by a text-to-speech \
engine, so write for the ear, not the page. Speak in real, well-formed \
sentences, and let punctuation carry the performance: commas and periods for \
breath and pacing, question marks and dashes for the natural rise and fall of \
talk. That rhythm is the difference between sounding like a friend and sounding \
like a robot reading a wall of text. Leave out whatever the ear can't hear — no \
markdown, headings, bullet or numbered lists, code blocks, emoji, or \
spelled-out URLs; if you wouldn't say it out loud, don't write it. Keep replies \
short and easy to follow by ear, going deeper only when you're asked. You're \
always listening, and you genuinely remember across conversations: when someone \
tells you something worth keeping — a preference, a detail about their life, the \
people and projects they care about — hold onto it, and let it shape how you show \
up next time, drawing on it naturally rather than announcing that you saved it. \
When you're unsure, you say so plainly.";

/// The virtual model ID MyOwnLLM serves embeddings under. Resolving it is the
/// engine's job: per device it maps to the hardware-appropriate Ollama
/// embedding model (EmbeddingGemma / Nomic / all-MiniLM), which MyOwnLLM keeps
/// pulled + warm because it's a tracked mode. Myo's memory system names this
/// stable id and never has to know the underlying tag.
pub const EMBED_MODEL: &str = "myownllm-embed";

/// The chat model Myo pins for the live conversational loop. MyOwnLLM serves a
/// hardware-tiered default, but a voice companion lives or dies on latency, so
/// Myo deliberately asks for the small, fast Gemma 4 `e2b` build on every
/// machine — even a big GPU — trading a little quality for near-real-time
/// replies (and leaving the engine room to keep other models resident). The
/// engine pulls it on demand and bare tags pass straight through its resolver.
/// A deliberate pin "for now"; revisit once parallel model streams land.
pub const MYO_CHAT_MODEL: &str = "gemma4:e2b";

/// One message in the OpenAI `messages` array.
///
/// Beyond the plain `role`/`content` a conversation needs two extra wire fields
/// for a **tool loop**: an assistant turn can carry the `tool_calls` it wants
/// run, and a `tool` turn carries the `tool_call_id` it answers. Both are
/// omitted from the wire when empty (`skip_serializing_if`), so an ordinary
/// chat message serializes exactly as before.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
    /// The tool calls an assistant turn is requesting (empty for normal turns).
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCallWire>,
    /// The id of the tool call a `role: "tool"` result answers.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl ChatMessage {
    fn bare(role: &str, content: impl Into<String>) -> Self {
        Self {
            role: role.into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }
    pub fn system(content: impl Into<String>) -> Self {
        Self::bare("system", content)
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self::bare("user", content)
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self::bare("assistant", content)
    }
    /// An assistant turn that requests tool calls (content is empty — the model
    /// speaks the answer on a later round, after it sees the results).
    pub fn assistant_calls(calls: &[ToolCall]) -> Self {
        Self {
            role: "assistant".into(),
            content: String::new(),
            tool_calls: calls.iter().map(ToolCallWire::from).collect(),
            tool_call_id: None,
        }
    }
    /// The result of one tool call, fed back to the model as a `tool` turn.
    pub fn tool(tool_call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: "tool".into(),
            content: content.into(),
            tool_calls: Vec::new(),
            tool_call_id: Some(tool_call_id.into()),
        }
    }
}

/// One tool call the model requested, fully assembled from the stream — the unit
/// the tool loop dispatches on. `arguments` is the raw JSON string the model
/// emitted (parsed by the tool at execution time).
#[derive(Debug, Clone, PartialEq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// The OpenAI wire shape of a tool call, used both when *sending* an assistant
/// turn back (so the model sees its own prior calls) and as the structure the
/// stream assembles into.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ToolCallWire {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub function: FunctionWire,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct FunctionWire {
    pub name: String,
    pub arguments: String,
}

impl From<&ToolCall> for ToolCallWire {
    fn from(c: &ToolCall) -> Self {
        Self {
            id: c.id.clone(),
            kind: "function".into(),
            function: FunctionWire {
                name: c.name.clone(),
                arguments: c.arguments.clone(),
            },
        }
    }
}

/// How a streamed turn ended: with the model's final spoken answer, or with a
/// batch of tool calls it wants the loop to run before it continues.
#[derive(Debug, Clone, PartialEq)]
pub enum TurnOutcome {
    /// The model produced a final answer — its text, for TTS + history.
    Message(String),
    /// The model requested one or more tool calls; the turn isn't over.
    ToolCalls(Vec<ToolCall>),
}

/// A loopback client for MyOwnLLM's OpenAI-compatible chat endpoint — Myo's
/// inference backend, running on the private port the supervisor owns.
pub struct LlmClient {
    http: Client,
    base_url: String,
}

impl LlmClient {
    /// `base_url` is Myo's private engine URL (e.g. `http://127.0.0.1:11473`).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        // No overall timeout — a turn streams for as long as it generates; a
        // connect timeout still fails fast when the engine isn't up yet.
        let http = Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, base_url })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// The chat model Myo asks the engine for. Pinned to [`MYO_CHAT_MODEL`]
    /// rather than read from `/v1/models`: Myo deliberately overrides the
    /// engine's hardware-tiered default to keep the conversational loop on the
    /// small, fast build (see the constant). Bare tags resolve straight through
    /// MyOwnLLM, and the `X-MyOwnLLM-Wait` header on each call lets the engine
    /// pull it on first use instead of bouncing the request.
    async fn model(&self) -> Result<String> {
        Ok(MYO_CHAT_MODEL.to_string())
    }

    /// Stream a chat completion: emit one [`MyoEvent::AssistantDelta`] per token,
    /// then exactly one [`MyoEvent::AssistantDone`]. Same contract the Odysseus
    /// client had, so the UI's reducer is unchanged.
    pub async fn chat_stream(
        &self,
        messages: &[ChatMessage],
        turn: TurnId,
        emit: &mut (dyn FnMut(MyoEvent) + Send),
    ) -> Result<()> {
        let model = self.model().await?;
        self.stream(&model, messages, turn, emit).await
    }

    /// Embed one or more strings into vectors via MyOwnLLM's OpenAI-compatible
    /// `/v1/embeddings`, against the [`EMBED_MODEL`] virtual ID — the building
    /// block for Myo's native memory system (store + cosine-search recalled
    /// context locally, no cloud).
    ///
    /// Sends `X-MyOwnLLM-Wait: true` so the very first call on a cold machine
    /// blocks while the engine pulls the embedding model rather than bouncing
    /// with a 503; later calls return immediately. Returns one vector per input,
    /// in request order.
    pub async fn embed(&self, inputs: &[String]) -> Result<Vec<Vec<f32>>> {
        if inputs.is_empty() {
            return Ok(Vec::new());
        }
        let body = json!({ "model": EMBED_MODEL, "input": inputs });
        let resp = self
            .http
            .post(self.url("/v1/embeddings"))
            .header("X-MyOwnLLM-Wait", "true")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "embeddings failed (HTTP {status}): {}",
                txt.chars().take(300).collect::<String>()
            ));
        }
        let v: Value = resp.json().await?;
        parse_embeddings(&v, inputs.len())
    }

    /// A **non-streaming** chat completion that just returns the text — used for
    /// background work (Dream-mode memory consolidation) where there's no turn,
    /// no UI stream, and we only want the model's answer.
    pub async fn complete(&self, messages: &[ChatMessage]) -> Result<String> {
        let model = self.model().await?;
        let body = json!({ "model": model, "messages": messages, "stream": false });
        let resp = self
            .http
            .post(self.url("/v1/chat/completions"))
            .header("X-MyOwnLLM-Wait", "true")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "completion failed (HTTP {status}): {}",
                txt.chars().take(300).collect::<String>()
            ));
        }
        let v: Value = resp.json().await?;
        let text = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string();
        Ok(text)
    }

    async fn stream(
        &self,
        model: &str,
        messages: &[ChatMessage],
        turn: TurnId,
        emit: &mut (dyn FnMut(MyoEvent) + Send),
    ) -> Result<()> {
        let body = json!({ "model": model, "messages": messages, "stream": true });
        let mut resp = self
            .http
            .post(self.url("/v1/chat/completions"))
            .header("X-MyOwnLLM-Wait", "true")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "chat failed (HTTP {status}): {}",
                txt.chars().take(300).collect::<String>()
            ));
        }

        let mut decoder = SseDecoder::new();
        'stream: while let Some(chunk) = resp.chunk().await? {
            for item in decoder.feed(&chunk) {
                match item {
                    SseItem::Data(s) => {
                        if let Some(text) = openai_delta(&s) {
                            if !text.is_empty() {
                                emit(MyoEvent::AssistantDelta { turn, text });
                            }
                        }
                    }
                    SseItem::Done => break 'stream,
                }
            }
        }
        emit(MyoEvent::AssistantDone { turn });
        Ok(())
    }

    /// Stream a chat completion **with tools** — the building block of the native
    /// tool loop. `tools` is the OpenAI function-schema array; when non-empty it
    /// rides on the request so the model can answer with `tool_calls` instead of
    /// (or alongside) prose.
    ///
    /// Content deltas are emitted as [`MyoEvent::AssistantDelta`] exactly as the
    /// plain path does, but this method **does not** emit [`MyoEvent::AssistantDone`]:
    /// the loop owns turn termination, since a `ToolCalls` outcome means the turn
    /// continues for another round. It returns whether the model wants tools run
    /// ([`TurnOutcome::ToolCalls`]) or produced its final answer
    /// ([`TurnOutcome::Message`]).
    pub async fn chat_stream_tools(
        &self,
        messages: &[ChatMessage],
        tools: &[Value],
        turn: TurnId,
        emit: &mut (dyn FnMut(MyoEvent) + Send),
    ) -> Result<TurnOutcome> {
        let model = self.model().await?;
        let mut body = json!({ "model": model, "messages": messages, "stream": true });
        if !tools.is_empty() {
            body["tools"] = json!(tools);
        }
        let mut resp = self
            .http
            .post(self.url("/v1/chat/completions"))
            .header("X-MyOwnLLM-Wait", "true")
            .json(&body)
            .send()
            .await?;
        if !resp.status().is_success() {
            let status = resp.status();
            let txt = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "chat failed (HTTP {status}): {}",
                txt.chars().take(300).collect::<String>()
            ));
        }

        let mut content = String::new();
        let mut calls = ToolCallAccumulator::new();
        let mut decoder = SseDecoder::new();
        'stream: while let Some(chunk) = resp.chunk().await? {
            for item in decoder.feed(&chunk) {
                match item {
                    SseItem::Data(s) => {
                        let frame = parse_frame(&s);
                        if let Some(text) = frame.content {
                            if !text.is_empty() {
                                content.push_str(&text);
                                emit(MyoEvent::AssistantDelta { turn, text });
                            }
                        }
                        for d in frame.tool_calls {
                            calls.feed(d);
                        }
                    }
                    SseItem::Done => break 'stream,
                }
            }
        }

        let assembled = calls.finish();
        if assembled.is_empty() {
            Ok(TurnOutcome::Message(content))
        } else {
            Ok(TurnOutcome::ToolCalls(assembled))
        }
    }
}

/// Pull the assistant text delta out of one OpenAI streaming frame
/// (`choices[0].delta.content`). `None` for the non-content frames a stream also
/// carries — the role-only opener, the finish marker, keepalives.
fn openai_delta(data: &str) -> Option<String> {
    let v: Value = serde_json::from_str(data).ok()?;
    v.get("choices")?
        .as_array()?
        .first()?
        .get("delta")?
        .get("content")?
        .as_str()
        .map(str::to_string)
}

/// The pieces of interest in one streaming frame: a content delta, the finish
/// reason (when the frame carries it), and any tool-call fragments.
#[derive(Debug, Default, PartialEq)]
struct Frame {
    content: Option<String>,
    finish: Option<String>,
    tool_calls: Vec<ToolCallDelta>,
}

/// One frame's worth of a single tool call, keyed by its stream `index`. The
/// fields trickle in across frames: the opener carries `index`+`id`+`name`, the
/// rest append `arguments`. A whole-object `arguments` (some engines, notably
/// Ollama, send the object in one shot rather than as string fragments) is
/// normalized to its JSON string here so the accumulator only ever concatenates.
#[derive(Debug, Clone, PartialEq)]
struct ToolCallDelta {
    index: usize,
    id: Option<String>,
    name: Option<String>,
    arguments: Option<String>,
}

/// Parse one OpenAI streaming `choices[0]` frame into the bits the tool loop
/// needs. Tolerant by design: anything missing is simply absent, so a keepalive
/// or role-only opener yields an empty [`Frame`].
fn parse_frame(data: &str) -> Frame {
    let v: Value = match serde_json::from_str(data) {
        Ok(v) => v,
        Err(_) => return Frame::default(),
    };
    let Some(choice) = v
        .get("choices")
        .and_then(Value::as_array)
        .and_then(|a| a.first())
    else {
        return Frame::default();
    };
    let mut frame = Frame {
        finish: choice
            .get("finish_reason")
            .and_then(Value::as_str)
            .map(str::to_string),
        ..Frame::default()
    };
    let delta = choice.get("delta").unwrap_or(&Value::Null);
    frame.content = delta
        .get("content")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(tcs) = delta.get("tool_calls").and_then(Value::as_array) {
        for (fallback, tc) in tcs.iter().enumerate() {
            let index = tc
                .get("index")
                .and_then(Value::as_u64)
                .map(|i| i as usize)
                .unwrap_or(fallback);
            let func = tc.get("function");
            let arguments = func
                .and_then(|f| f.get("arguments"))
                .and_then(arg_to_string);
            frame.tool_calls.push(ToolCallDelta {
                index,
                id: tc.get("id").and_then(Value::as_str).map(str::to_string),
                name: func
                    .and_then(|f| f.get("name"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                arguments,
            });
        }
    }
    frame
}

/// Normalize a `function.arguments` value to a string: a JSON string is taken
/// verbatim (the fragment case); a non-null object/array/number is re-serialized
/// (the whole-object case); null/absent yields `None`.
fn arg_to_string(v: &Value) -> Option<String> {
    match v {
        Value::Null => None,
        Value::String(s) => Some(s.clone()),
        other => serde_json::to_string(other).ok(),
    }
}

/// Reassembles streamed [`ToolCallDelta`]s into whole [`ToolCall`]s, preserving
/// the order each call's index was first seen so parallel calls come back in a
/// stable order.
#[derive(Default)]
struct ToolCallAccumulator {
    // (index, id, name, arguments) — built up in first-seen order.
    rows: Vec<(usize, String, String, String)>,
}

impl ToolCallAccumulator {
    fn new() -> Self {
        Self::default()
    }

    fn feed(&mut self, d: ToolCallDelta) {
        let slot = match self.rows.iter_mut().find(|(i, ..)| *i == d.index) {
            Some(s) => s,
            None => {
                self.rows
                    .push((d.index, String::new(), String::new(), String::new()));
                self.rows.last_mut().unwrap()
            }
        };
        if let Some(id) = d.id {
            slot.1 = id;
        }
        if let Some(name) = d.name {
            slot.2 = name;
        }
        if let Some(args) = d.arguments {
            slot.3.push_str(&args);
        }
    }

    /// The assembled calls. Drops any rows that never got a name (a stray index
    /// with only keepalive noise) so the loop never dispatches a nameless call.
    fn finish(self) -> Vec<ToolCall> {
        self.rows
            .into_iter()
            .filter(|(_, _, name, _)| !name.is_empty())
            .map(|(idx, id, name, arguments)| ToolCall {
                // A missing id is rare but legal mid-stream; synthesize a stable
                // one from the index so the tool result can still be addressed.
                id: if id.is_empty() {
                    format!("call_{idx}")
                } else {
                    id
                },
                name,
                arguments,
            })
            .collect()
    }
}

/// Pull the embedding vectors out of an OpenAI `/v1/embeddings` response body
/// (`data[].embedding`). The OpenAI shape doesn't guarantee `data` arrives in
/// input order, so we sort by `index` when present before collecting. Errors if
/// the count doesn't match what we asked for, so a partial/garbled response
/// fails loudly rather than silently dropping a memory.
fn parse_embeddings(v: &Value, expected: usize) -> Result<Vec<Vec<f32>>> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("embeddings response had no 'data' array"))?;
    let mut rows: Vec<(u64, Vec<f32>)> = Vec::with_capacity(data.len());
    for (fallback_idx, item) in data.iter().enumerate() {
        let embedding = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow!("embeddings response item had no 'embedding' array"))?
            .iter()
            .map(|n| n.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| anyhow!("embedding contained a non-numeric value"))?;
        let idx = item
            .get("index")
            .and_then(Value::as_u64)
            .unwrap_or(fallback_idx as u64);
        rows.push((idx, embedding));
    }
    rows.sort_by_key(|(idx, _)| *idx);
    let out: Vec<Vec<f32>> = rows.into_iter().map(|(_, e)| e).collect();
    if out.len() != expected {
        return Err(anyhow!(
            "embeddings response returned {} vectors, expected {expected}",
            out.len()
        ));
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn openai_delta_extracts_content_only() {
        assert_eq!(
            openai_delta(r#"{"choices":[{"delta":{"content":"Hi"}}]}"#),
            Some("Hi".into())
        );
        // role-only opener, finish frame, and garbage → no text
        assert_eq!(
            openai_delta(r#"{"choices":[{"delta":{"role":"assistant"}}]}"#),
            None
        );
        assert_eq!(
            openai_delta(r#"{"choices":[{"delta":{},"finish_reason":"stop"}]}"#),
            None
        );
        assert_eq!(openai_delta("not json at all"), None);
    }

    #[test]
    fn parse_embeddings_sorts_by_index_and_checks_count() {
        // Out-of-order `index` fields are reordered back to input order.
        let v = serde_json::json!({
            "data": [
                { "index": 1, "embedding": [0.3, 0.4] },
                { "index": 0, "embedding": [0.1, 0.2] }
            ]
        });
        assert_eq!(
            parse_embeddings(&v, 2).unwrap(),
            vec![vec![0.1, 0.2], vec![0.3, 0.4]]
        );
        // A short count fails loudly rather than dropping a vector.
        assert!(parse_embeddings(&v, 3).is_err());
        // Missing `data` is an error, not an empty result.
        assert!(parse_embeddings(&serde_json::json!({}), 0).is_err());
    }

    #[tokio::test]
    async fn embed_empty_input_short_circuits() {
        // No network call for an empty batch — base URL is bogus on purpose.
        let client = LlmClient::new("http://127.0.0.1:1").unwrap();
        assert_eq!(client.embed(&[]).await.unwrap(), Vec::<Vec<f32>>::new());
    }

    #[tokio::test]
    async fn embed_parses_vectors_from_loopback() {
        let base = serve_once("{\"data\":[{\"index\":0,\"embedding\":[0.5,0.25]}]}").await;
        let client = LlmClient::new(base).unwrap();
        let out = client.embed(&["hello".to_string()]).await.unwrap();
        assert_eq!(out, vec![vec![0.5, 0.25]]);
    }

    /// One-shot loopback server that replies with a fixed body and closes (so
    /// reqwest streams to EOF). Mirrors the brain client's test harness.
    async fn serve_once(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.flush().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn stream_emits_deltas_then_done() {
        let base = serve_once(
            "data: {\"choices\":[{\"delta\":{\"role\":\"assistant\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"Hel\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"lo\"}}]}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let client = LlmClient::new(base).unwrap();
        let mut events = Vec::new();
        {
            let mut emit = |ev: MyoEvent| events.push(ev);
            client
                .stream("m", &[ChatMessage::user("hi")], 1, &mut emit)
                .await
                .unwrap();
        }
        assert_eq!(
            events,
            vec![
                MyoEvent::AssistantDelta {
                    turn: 1,
                    text: "Hel".into()
                },
                MyoEvent::AssistantDelta {
                    turn: 1,
                    text: "lo".into()
                },
                MyoEvent::AssistantDone { turn: 1 },
            ]
        );
    }

    // ── Tool-call streaming ──────────────────────────────────────────────────

    #[test]
    fn parse_frame_pulls_content_finish_and_tool_fragments() {
        // Plain content frame.
        let f = parse_frame(r#"{"choices":[{"delta":{"content":"hi"}}]}"#);
        assert_eq!(f.content.as_deref(), Some("hi"));
        assert!(f.tool_calls.is_empty());

        // A tool-call opener: index + id + name, no args yet.
        let f = parse_frame(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"shell"}}]}}]}"#,
        );
        assert_eq!(f.tool_calls.len(), 1);
        assert_eq!(f.tool_calls[0].index, 0);
        assert_eq!(f.tool_calls[0].id.as_deref(), Some("c1"));
        assert_eq!(f.tool_calls[0].name.as_deref(), Some("shell"));
        assert_eq!(f.tool_calls[0].arguments, None);

        // finish_reason frame.
        let f = parse_frame(r#"{"choices":[{"delta":{},"finish_reason":"tool_calls"}]}"#);
        assert_eq!(f.finish.as_deref(), Some("tool_calls"));
    }

    #[test]
    fn arguments_assemble_from_string_fragments() {
        // Fragmented arguments across frames reassemble into one JSON string.
        let mut acc = ToolCallAccumulator::new();
        acc.feed(ToolCallDelta {
            index: 0,
            id: Some("c1".into()),
            name: Some("shell".into()),
            arguments: Some("{\"comm".into()),
        });
        acc.feed(ToolCallDelta {
            index: 0,
            id: None,
            name: None,
            arguments: Some("and\":\"ls\"}".into()),
        });
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "c1");
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[0].arguments, r#"{"command":"ls"}"#);
    }

    #[test]
    fn arguments_as_whole_object_are_normalized_to_string() {
        // Some engines (Ollama) send `arguments` as an object in one shot.
        let f = parse_frame(
            r#"{"choices":[{"delta":{"tool_calls":[{"index":0,"id":"c1","function":{"name":"web_search","arguments":{"query":"rust"}}}]}}]}"#,
        );
        let mut acc = ToolCallAccumulator::new();
        for d in f.tool_calls {
            acc.feed(d);
        }
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].name, "web_search");
        assert_eq!(calls[0].arguments, r#"{"query":"rust"}"#);
    }

    #[test]
    fn parallel_tool_calls_stay_in_first_seen_order() {
        let mut acc = ToolCallAccumulator::new();
        // Two calls interleaved across frames.
        acc.feed(ToolCallDelta {
            index: 0,
            id: Some("a".into()),
            name: Some("shell".into()),
            arguments: Some("{}".into()),
        });
        acc.feed(ToolCallDelta {
            index: 1,
            id: Some("b".into()),
            name: Some("read_file".into()),
            arguments: Some("{".into()),
        });
        acc.feed(ToolCallDelta {
            index: 1,
            id: None,
            name: None,
            arguments: Some("}".into()),
        });
        let calls = acc.finish();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0].name, "shell");
        assert_eq!(calls[1].name, "read_file");
    }

    #[test]
    fn accumulator_synthesizes_id_and_drops_nameless_rows() {
        let mut acc = ToolCallAccumulator::new();
        // A named call with no id → id synthesized from index.
        acc.feed(ToolCallDelta {
            index: 2,
            id: None,
            name: Some("shell".into()),
            arguments: Some("{}".into()),
        });
        // A stray index that never got a name → dropped.
        acc.feed(ToolCallDelta {
            index: 9,
            id: Some("x".into()),
            name: None,
            arguments: Some("noise".into()),
        });
        let calls = acc.finish();
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].id, "call_2");
    }

    #[tokio::test]
    async fn chat_stream_tools_returns_tool_calls() {
        let base = serve_once(
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"ls\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let client = LlmClient::new(base).unwrap();
        let mut events = Vec::new();
        let outcome = {
            let mut emit = |ev: MyoEvent| events.push(ev);
            client
                .chat_stream_tools(&[ChatMessage::user("list files")], &[], 1, &mut emit)
                .await
                .unwrap()
        };
        match outcome {
            TurnOutcome::ToolCalls(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].name, "shell");
                assert_eq!(calls[0].arguments, r#"{"command":"ls"}"#);
            }
            other => panic!("expected ToolCalls, got {other:?}"),
        }
        // No AssistantDone — the loop owns termination, not this method.
        assert!(!events
            .iter()
            .any(|e| matches!(e, MyoEvent::AssistantDone { .. })));
    }

    #[tokio::test]
    async fn chat_stream_tools_returns_final_message() {
        let base = serve_once(
            "data: {\"choices\":[{\"delta\":{\"content\":\"All \"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{\"content\":\"done.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let client = LlmClient::new(base).unwrap();
        let mut events = Vec::new();
        let outcome = {
            let mut emit = |ev: MyoEvent| events.push(ev);
            client
                .chat_stream_tools(&[ChatMessage::user("hi")], &[], 1, &mut emit)
                .await
                .unwrap()
        };
        assert_eq!(outcome, TurnOutcome::Message("All done.".into()));
    }
}
