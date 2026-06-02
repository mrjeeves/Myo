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

use std::sync::Mutex;
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
always listening, you remember across conversations, and when you're unsure you \
say so plainly.";

/// The virtual model ID MyOwnLLM serves embeddings under. Resolving it is the
/// engine's job: per device it maps to the hardware-appropriate Ollama
/// embedding model (EmbeddingGemma / Nomic / all-MiniLM), which MyOwnLLM keeps
/// pulled + warm because it's a tracked mode. Myo's memory system names this
/// stable id and never has to know the underlying tag.
pub const EMBED_MODEL: &str = "myownllm-embed";

/// One message in the OpenAI `messages` array.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct ChatMessage {
    pub role: String,
    pub content: String,
}

impl ChatMessage {
    pub fn system(content: impl Into<String>) -> Self {
        Self {
            role: "system".into(),
            content: content.into(),
        }
    }
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: "user".into(),
            content: content.into(),
        }
    }
    pub fn assistant(content: impl Into<String>) -> Self {
        Self {
            role: "assistant".into(),
            content: content.into(),
        }
    }
}

/// A loopback client for MyOwnLLM's OpenAI-compatible chat endpoint — Myo's
/// inference backend, running on the private port the supervisor owns.
pub struct LlmClient {
    http: Client,
    base_url: String,
    /// The resolved model id, cached after the first `/v1/models` lookup.
    model: Mutex<Option<String>>,
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
        Ok(Self {
            http,
            base_url,
            model: Mutex::new(None),
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    /// Resolve (and cache) a model id from `/v1/models`. MyOwnLLM owns model
    /// *selection* per device, so Myo just uses whatever it's serving.
    async fn model(&self) -> Result<String> {
        if let Some(m) = self.model.lock().unwrap().clone() {
            return Ok(m);
        }
        let v: Value = self
            .http
            .get(self.url("/v1/models"))
            .send()
            .await?
            .json()
            .await?;
        let id = v
            .get("data")
            .and_then(Value::as_array)
            .and_then(|a| a.first())
            .and_then(|m| m.get("id"))
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("the model engine has no model available yet"))?
            .to_string();
        *self.model.lock().unwrap() = Some(id.clone());
        Ok(id)
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
        let base =
            serve_once("{\"data\":[{\"index\":0,\"embedding\":[0.5,0.25]}]}").await;
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
}
