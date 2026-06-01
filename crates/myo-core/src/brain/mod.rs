//! The brain client — a thin loopback HTTP client for the Odysseus agent.
//!
//! Odysseus *is* Myo's brain (PLAN binding decision #2): a local sidecar Myo
//! talks to over `127.0.0.1`, authenticating as admin with the internal token
//! so no user account is needed. This client speaks Odysseus's actual wire
//! shapes — multipart forms in, Server-Sent-Events out — and hands the shell
//! the normalized [`MyoEvent`] stream instead (see [`sse`]).
//!
//! Endpoint contracts are pinned in `docs/odysseus-integration.md` and verified
//! against the Odysseus source.

pub mod sse;

use anyhow::{anyhow, Result};
use reqwest::{Client, RequestBuilder, Response, StatusCode};
use serde_json::Value;
use std::time::Duration;

use crate::capabilities::Capabilities;
use crate::event::{MyoEvent, TurnId};
use sse::{normalize, SseDecoder, SseItem};

/// Where the brain lives and how to authenticate to it.
#[derive(Debug, Clone)]
pub struct BrainConfig {
    /// Base URL, e.g. `http://127.0.0.1:7000` (trailing slash trimmed).
    pub base_url: String,
    /// `ODYSSEUS_INTERNAL_TOKEN` — set by the supervisor before uvicorn boots.
    pub token: String,
    /// The `X-Odysseus-Owner` Myo impersonates (its single local user).
    pub owner: String,
}

impl BrainConfig {
    pub fn new(
        base_url: impl Into<String>,
        token: impl Into<String>,
        owner: impl Into<String>,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            token: token.into(),
            owner: owner.into(),
        }
    }
}

/// Synthesized speech for one turn (base64 + its MIME type).
#[derive(Debug, Clone, PartialEq)]
pub struct TtsAudio {
    pub b64: String,
    pub mime: String,
}

/// A loopback client for one Odysseus instance.
pub struct BrainClient {
    http: Client,
    cfg: BrainConfig,
}

impl BrainClient {
    pub fn new(mut cfg: BrainConfig) -> Result<Self> {
        cfg.base_url = cfg.base_url.trim_end_matches('/').to_string();
        let http = Client::builder()
            // No overall timeout: an agent turn streams for as long as it
            // thinks. A connect timeout still fails fast when the brain is down.
            .connect_timeout(Duration::from_secs(10))
            .build()?;
        Ok(Self { http, cfg })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.cfg.base_url, path)
    }

    /// Stamp the internal-tool + owner headers onto a request. Loopback-only;
    /// Odysseus rejects these headers from any non-127.0.0.1 client.
    fn auth(&self, rb: RequestBuilder) -> RequestBuilder {
        rb.header("X-Odysseus-Internal-Token", &self.cfg.token)
            .header("X-Odysseus-Owner", &self.cfg.owner)
    }

    /// `GET /api/health` → is the brain up and healthy? (Auth-exempt.)
    pub async fn health(&self) -> Result<bool> {
        let resp = self.http.get(self.url("/api/health")).send().await?;
        if !resp.status().is_success() {
            return Ok(false);
        }
        let v: Value = resp.json().await?;
        Ok(v.get("status").and_then(Value::as_str) == Some("healthy"))
    }

    /// `GET /api/version` → the brain's version string. (Auth-exempt.)
    pub async fn version(&self) -> Result<String> {
        let v: Value = self
            .http
            .get(self.url("/api/version"))
            .send()
            .await?
            .json()
            .await?;
        Ok(v.get("version")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// `POST /api/session` → a fresh session id. `skip_validation=true` so we
    /// don't have to name an endpoint here; the default endpoint (MyOwnLLM,
    /// registered at startup) applies.
    pub async fn create_session(&self) -> Result<String> {
        let form = reqwest::multipart::Form::new().text("skip_validation", "true");
        let resp = self
            .auth(self.http.post(self.url("/api/session")))
            .multipart(form)
            .send()
            .await?;
        let v: Value = ok_json(resp).await?;
        v.get("id")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or_else(|| anyhow!("session response missing `id`"))
    }

    /// `POST /api/chat_stream` (multipart) → stream SSE, normalize each event,
    /// and push it through `emit`. Emits [`MyoEvent::AssistantDone`] on `[DONE]`.
    ///
    /// The four capability toggles ride along as the per-turn `allow_bash` /
    /// `allow_web_search` flags (the persistent `disabled_tools` half is written
    /// separately, see [`BrainClient::set_disabled_tools`]). Memory + RAG run on
    /// (`use_rag=true`) unless `incognito` pauses them — Myo's continuous
    /// presence (PLAN binding decision #5).
    pub async fn chat_stream(
        &self,
        session: &str,
        message: &str,
        caps: Capabilities,
        incognito: bool,
        turn: TurnId,
        emit: &mut (dyn FnMut(MyoEvent) + Send),
    ) -> Result<()> {
        let form = reqwest::multipart::Form::new()
            .text("message", message.to_string())
            .text("session", session.to_string())
            .text("mode", "agent")
            .text("use_rag", "true")
            .text("allow_bash", bstr(caps.allow_bash()))
            .text("allow_web_search", bstr(caps.allow_web_search()))
            .text("incognito", bstr(incognito));

        let resp = self
            .auth(self.http.post(self.url("/api/chat_stream")))
            .multipart(form)
            .send()
            .await?;
        let mut resp = ok_resp(resp).await?;

        let mut decoder = SseDecoder::new();
        let mut saw_done = false;
        'stream: while let Some(chunk) = resp.chunk().await? {
            for item in decoder.feed(&chunk) {
                match item {
                    SseItem::Data(s) => emit_frame(&s, turn, emit),
                    SseItem::Done => {
                        saw_done = true;
                        break 'stream;
                    }
                }
            }
        }
        // Flush a trailing frame the server may have left without its final
        // blank line (truncation / disconnect), then close the turn exactly
        // once — whether it ended with `[DONE]` or just EOF'd — so the UI never
        // hangs on an unterminated turn.
        if !saw_done {
            if let Some(SseItem::Data(s)) = decoder.finish() {
                emit_frame(&s, turn, emit);
            }
        }
        emit(MyoEvent::AssistantDone { turn });
        Ok(())
    }

    /// `POST /api/tts/synthesize` → base64 audio, or `None` when the provider is
    /// disabled (503) so the caller can fall back to the browser's speech engine.
    pub async fn tts(&self, text: &str) -> Result<Option<TtsAudio>> {
        let body = serde_json::json!({ "text": text, "format": "base64" });
        let resp = self
            .auth(self.http.post(self.url("/api/tts/synthesize")))
            .json(&body)
            .send()
            .await?;
        if resp.status() == StatusCode::SERVICE_UNAVAILABLE {
            return Ok(None);
        }
        let v: Value = ok_json(resp).await?;
        let b64 = v
            .get("audio")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("tts response missing `audio`"))?;
        Ok(Some(TtsAudio {
            b64: b64.to_string(),
            // The base64 path doesn't carry a MIME; Kokoro/OpenAI default to MP3.
            mime: "audio/mpeg".to_string(),
        }))
    }

    /// `POST /api/model-endpoints` (multipart) → register an OpenAI-compatible
    /// endpoint. The first one registered auto-becomes Odysseus's default, so
    /// pointing the brain at MyOwnLLM is a single call.
    pub async fn register_model_endpoint(&self, base_url: &str) -> Result<Value> {
        let form = reqwest::multipart::Form::new()
            .text("base_url", base_url.to_string())
            .text("model_type", "llm")
            .text("supports_tools", "true");
        let resp = self
            .auth(self.http.post(self.url("/api/model-endpoints")))
            .multipart(form)
            .send()
            .await?;
        ok_json(resp).await
    }

    /// `GET /api/model-endpoints` → the registered endpoints (used to avoid
    /// re-registering MyOwnLLM on every launch).
    pub async fn list_model_endpoints(&self) -> Result<Value> {
        let resp = self
            .auth(self.http.get(self.url("/api/model-endpoints")))
            .send()
            .await?;
        ok_json(resp).await
    }

    /// `POST /api/tools` → persist the `disabled_tools` allowlist (the toggles'
    /// persistent half). Field name is `disabled`.
    pub async fn set_disabled_tools(&self, disabled: &[String]) -> Result<()> {
        let body = serde_json::json!({ "disabled": disabled });
        let resp = self
            .auth(self.http.post(self.url("/api/tools")))
            .json(&body)
            .send()
            .await?;
        ok_resp(resp).await.map(|_| ())
    }

    /// `GET /api/tools` → the tools currently disabled (those with `enabled:false`).
    pub async fn get_disabled_tools(&self) -> Result<Vec<String>> {
        let v: Value = ok_json(
            self.auth(self.http.get(self.url("/api/tools")))
                .send()
                .await?,
        )
        .await?;
        let mut out = Vec::new();
        if let Some(tools) = v.get("tools").and_then(Value::as_array) {
            for t in tools {
                if t.get("enabled").and_then(Value::as_bool) == Some(false) {
                    if let Some(id) = t.get("id").and_then(Value::as_str) {
                        out.push(id.to_string());
                    }
                }
            }
        }
        Ok(out)
    }

    /// `GET /api/memory` (or `POST /api/memory/search` when filtered) → what Myo
    /// remembers, for the Memory surface.
    pub async fn list_memories(&self, query: Option<&str>) -> Result<Value> {
        let resp = match query {
            Some(q) => {
                let form = reqwest::multipart::Form::new().text("query", q.to_string());
                self.auth(self.http.post(self.url("/api/memory/search")))
                    .multipart(form)
                    .send()
                    .await?
            }
            None => {
                self.auth(self.http.get(self.url("/api/memory")))
                    .send()
                    .await?
            }
        };
        ok_json(resp).await
    }

    /// `DELETE /api/memory/{id}` → forget one memory.
    pub async fn forget_memory(&self, id: &str) -> Result<()> {
        let resp = self
            .auth(self.http.delete(self.url(&format!("/api/memory/{id}"))))
            .send()
            .await?;
        ok_resp(resp).await.map(|_| ())
    }
}

fn bstr(b: bool) -> &'static str {
    if b {
        "true"
    } else {
        "false"
    }
}

/// Parse one SSE `data:` frame and push every normalized event through `emit`.
/// A single malformed frame is skipped rather than aborting the turn.
fn emit_frame(data: &str, turn: TurnId, emit: &mut (dyn FnMut(MyoEvent) + Send)) {
    if let Ok(v) = serde_json::from_str::<Value>(data) {
        for ev in normalize(&v, turn) {
            emit(ev);
        }
    }
}

/// Turn a non-2xx response into an error that carries a slice of the body —
/// far more debuggable than a bare status code.
async fn ok_resp(resp: Response) -> Result<Response> {
    let status = resp.status();
    if status.is_success() {
        return Ok(resp);
    }
    let body = resp.text().await.unwrap_or_default();
    Err(anyhow!(
        "HTTP {status}: {}",
        body.chars().take(500).collect::<String>()
    ))
}

async fn ok_json(resp: Response) -> Result<Value> {
    Ok(ok_resp(resp).await?.json().await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot loopback HTTP server that ignores the request and replies with
    /// the given raw body. `Connection: close` makes reqwest stream to EOF.
    async fn serve_once(body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            // Drain the request enough that the client finishes writing.
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

    fn client_for(base: String) -> BrainClient {
        BrainClient::new(BrainConfig::new(base, "tok", "myo")).unwrap()
    }

    #[tokio::test]
    async fn chat_stream_normalizes_and_finishes_with_done() {
        let base = serve_once(
            "data: {\"delta\":\"Hel\"}\n\n\
             data: {\"delta\":\"lo\"}\n\n\
             data: {\"type\":\"tool_start\",\"tool\":\"bash\",\"command\":\"ls\"}\n\n\
             data: [DONE]\n\n",
        )
        .await;
        let client = client_for(base);

        let mut events = Vec::new();
        {
            let mut emit = |ev: MyoEvent| events.push(ev);
            client
                .chat_stream("s1", "hi", Capabilities::default(), false, 1, &mut emit)
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
                MyoEvent::ActivityStart {
                    turn: 1,
                    tool: "bash".into(),
                    command: Some("ls".into()),
                    round: None,
                },
                MyoEvent::AssistantDone { turn: 1 },
            ]
        );
    }

    #[tokio::test]
    async fn chat_stream_flushes_trailing_frame_and_closes_on_eof() {
        // No `[DONE]`, and the final frame has no closing blank line — exactly
        // the truncation/disconnect case that used to drop the frame and leave
        // the turn hanging open. The frame must still be emitted, and the turn
        // must still close.
        let base = serve_once("data: {\"delta\":\"bye\"}").await;
        let client = client_for(base);
        let mut events = Vec::new();
        {
            let mut emit = |ev: MyoEvent| events.push(ev);
            client
                .chat_stream("s", "hi", Capabilities::default(), false, 1, &mut emit)
                .await
                .unwrap();
        }
        assert_eq!(
            events,
            vec![
                MyoEvent::AssistantDelta {
                    turn: 1,
                    text: "bye".into()
                },
                MyoEvent::AssistantDone { turn: 1 },
            ]
        );
    }

    #[tokio::test]
    async fn tts_503_is_none_not_error() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let resp = "HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"detail\":{\"message\":\"TTS service not available\"}}";
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.flush().await;
        });
        let client = client_for(format!("http://{addr}"));
        assert_eq!(client.tts("hi").await.unwrap(), None);
    }

    #[tokio::test]
    async fn create_session_extracts_id() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let body = "{\"id\":\"abc-123\",\"name\":\"\",\"model\":\"m\",\"rag\":false,\"archived\":false}";
            let resp = format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.flush().await;
        });
        let client = client_for(format!("http://{addr}"));
        assert_eq!(client.create_session().await.unwrap(), "abc-123");
    }
}
