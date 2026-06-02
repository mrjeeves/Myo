//! The voice — a loopback HTTP client for the engine Myo *owns*.
//!
//! The synthesis mirror of [`crate::asr`]: where the ears POST captured audio
//! to MyOwnLLM's `/v1/audio/transcriptions` and get text back, the voice POSTs
//! text to the engine's `/v1/audio/speech` and gets synthesized audio back.
//! Same private port Myo controls (`:11473`, not the shared `:1473`), same
//! warming/503 shape, same "the engine owns the model" split — the engine
//! picks the voice tier by hardware (Kokoro on capable machines, Piper on the
//! lower rungs), Myo never does.
//!
//! The engine returns raw audio bytes; this client base64-encodes them into
//! [`TtsAudio`] for [`crate::event::MyoEvent::AudioReady`], which the WebView
//! plays directly. When the engine can't synthesize — too old for the route
//! (404), still warming (503), or synthesis otherwise unavailable — the call
//! errors and the converse path degrades to WebSpeech (the tier-4 fallback).

use std::time::Duration;

use anyhow::{anyhow, Result};
use base64::Engine;
use reqwest::{Client, StatusCode};

/// Synthesized speech for one turn (base64 + its MIME type). Lives here, not in
/// the (Odysseus) [`crate::brain`] module, so the native voice path doesn't
/// depend on the dead brain client; `brain::tts` reuses it for parity.
#[derive(Debug, Clone, PartialEq)]
pub struct TtsAudio {
    pub b64: String,
    pub mime: String,
}

/// A loopback client for Myo's owned `myownllm` speech endpoint.
pub struct TtsClient {
    http: Client,
    base_url: String,
}

impl TtsClient {
    /// `base_url` is Myo's private engine URL (e.g. `http://127.0.0.1:11473`).
    /// The per-request timeout is generous: the first synthesis on a cold
    /// machine downloads the onnxruntime dylib + voice model before answering
    /// (the same reason [`crate::asr::AsrClient::new`] is patient).
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self { http, base_url })
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/audio/speech", self.base_url)
    }

    /// POST one line of text and return the synthesized audio, base64-encoded
    /// and tagged with its container MIME. `voice` is an optional voice id for
    /// multi-voice tiers (Kokoro); omit it (`None`) to let the engine choose.
    ///
    /// Any non-success is an `Err` the caller treats as the cue to fall back to
    /// WebSpeech: `404` (engine too old for the route), `503` (warming), or a
    /// `500` (synthesis unavailable on this engine yet).
    pub async fn synthesize(&self, text: &str, voice: Option<&str>) -> Result<TtsAudio> {
        let url = self.endpoint();
        let mut body = serde_json::json!({ "input": text });
        if let Some(v) = voice {
            body["voice"] = serde_json::json!(v);
        }
        let resp = self
            .http
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| anyhow!("TTS request to {url} failed: {e}"))?;
        let status = resp.status();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            // Engine still warming (downloading runtime / voice model) — retriable.
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "speech engine warming up: {}",
                first_chars(&body, 200)
            ));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "speech synthesis failed (HTTP {status}): {}",
                first_chars(&body, 300)
            ));
        }

        // Success body is the raw audio bytes; the container type rides in the
        // Content-Type header (the engine returns `audio/wav` for v1).
        let mime = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("audio/wav")
            .to_string();
        let bytes = resp
            .bytes()
            .await
            .map_err(|e| anyhow!("reading speech audio: {e}"))?;
        if bytes.is_empty() {
            return Err(anyhow!("speech engine returned no audio"));
        }
        let b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);
        eprintln!(
            "tts: {} chars → {} bytes synthesized ({mime})",
            text.len(),
            bytes.len()
        );
        Ok(TtsAudio { b64, mime })
    }

    /// Best-effort warm-up: synthesize a short probe so the engine pulls the
    /// onnxruntime dylib + voice model now, before the user's first reply.
    /// Fire-and-forget; a failure is logged, not fatal — and on an engine whose
    /// synthesis isn't available yet it still pre-pays the model download the
    /// route does before synthesizing. Mirrors [`crate::asr::AsrClient::warm_up`].
    pub async fn warm_up(&self) -> Result<()> {
        eprintln!("tts: warming up speech engine (probe)…");
        match self.synthesize("Ready.", None).await {
            Ok(_) => {
                eprintln!("tts: speech engine warm");
                Ok(())
            }
            Err(e) => {
                eprintln!("tts: warm-up skipped ({e})");
                Err(e)
            }
        }
    }
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot loopback server that drains the request and replies with a
    /// fixed body + content type (mirrors the ASR client's test harness).
    async fn serve_once(
        status_line: &'static str,
        content_type: &'static str,
        body: &'static [u8],
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let mut resp = format!(
                "{status_line}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            )
            .into_bytes();
            resp.extend_from_slice(body);
            sock.write_all(&resp).await.unwrap();
            let _ = sock.flush().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn synthesize_base64s_the_audio_bytes() {
        let base = serve_once("HTTP/1.1 200 OK", "audio/wav", b"RIFFfakeWAVdata").await;
        let client = TtsClient::new(base).unwrap();
        let audio = client.synthesize("hello", None).await.unwrap();
        assert_eq!(audio.mime, "audio/wav");
        assert_eq!(
            audio.b64,
            base64::engine::general_purpose::STANDARD.encode(b"RIFFfakeWAVdata")
        );
    }

    #[tokio::test]
    async fn warming_up_503_is_a_clear_error() {
        let base = serve_once(
            "HTTP/1.1 503 Service Unavailable",
            "application/json",
            b"{\"error\":{\"message\":\"voice model is being pulled\",\"code\":\"warming_up\"}}",
        )
        .await;
        let client = TtsClient::new(base).unwrap();
        let err = client.synthesize("hi", None).await.unwrap_err();
        assert!(err.to_string().contains("warming up"), "got: {err}");
    }

    #[tokio::test]
    async fn route_absent_404_errors_so_caller_falls_back() {
        // An engine too old for the route → 404 → the converse path degrades to
        // WebSpeech. The client just needs to surface it as an error.
        let base = serve_once("HTTP/1.1 404 Not Found", "text/plain", b"not found").await;
        let client = TtsClient::new(base).unwrap();
        assert!(client.synthesize("hi", None).await.is_err());
    }
}
