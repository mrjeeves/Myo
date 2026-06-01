//! The ears — a loopback HTTP client for the engine Myo *owns*.
//!
//! Myo spawns its own bundled `myownllm serve` on a **private** port (not the
//! shared `:1473` that a user's own MyOwnLLM / desktop app uses), so
//! transcription talks to an engine Myo controls and pinned — never a foreign
//! or stale instance. Each captured utterance is POSTed to that engine's
//! `/v1/audio/transcriptions`; the engine owns the Moonshine/Parakeet inference.
//!
//! Audio is transcribed transiently: MyOwnLLM deletes the upload the moment it
//! finishes. That's Myo's privacy default — always listening, nothing kept.

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;

/// A loopback client for Myo's owned `myownllm` transcription endpoint.
pub struct AsrClient {
    http: Client,
    base_url: String,
}

impl AsrClient {
    /// `base_url` is Myo's private engine URL (e.g. `http://127.0.0.1:11473`).
    /// The per-request timeout is generous: the first transcription on a cold
    /// machine downloads the onnxruntime dylib + ASR model before answering.
    pub fn new(base_url: impl Into<String>) -> Result<Self> {
        let base_url = base_url.into().trim_end_matches('/').to_string();
        let http = Client::builder()
            .timeout(Duration::from_secs(180))
            .build()?;
        Ok(Self { http, base_url })
    }

    fn endpoint(&self) -> String {
        format!("{}/v1/audio/transcriptions", self.base_url)
    }

    /// POST one utterance's audio and return the recognized text. May be empty
    /// (silence / noise / non-speech) — callers treat that as "nothing said".
    /// `mime` is best-effort metadata; the engine probes the container anyway.
    pub async fn transcribe(&self, audio: Vec<u8>, mime: &str) -> Result<String> {
        let url = self.endpoint();
        let bytes = audio.len();
        let resp = self
            .http
            .post(&url)
            .header(reqwest::header::CONTENT_TYPE, mime.to_string())
            .body(audio)
            .send()
            .await
            .map_err(|e| anyhow!("ASR request to {url} failed: {e}"))?;
        let status = resp.status();
        if status == StatusCode::SERVICE_UNAVAILABLE {
            // Engine still warming (downloading runtime / model) — retriable.
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transcription engine warming up: {}",
                first_chars(&body, 200)
            ));
        }
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            return Err(anyhow!(
                "transcription failed (HTTP {status}): {}",
                first_chars(&body, 300)
            ));
        }
        let v: Value = resp
            .json()
            .await
            .map_err(|e| anyhow!("bad transcription response: {e}"))?;
        let text = v
            .get("text")
            .and_then(Value::as_str)
            .unwrap_or("")
            .trim()
            .to_string();
        eprintln!("asr: {bytes} bytes → {} chars transcribed", text.len());
        Ok(text)
    }

    /// Best-effort warm-up: send a short silent clip so the engine pulls the
    /// onnxruntime dylib + ASR model now, before the user's first real words.
    /// Fire-and-forget; a failure is logged, not fatal.
    pub async fn warm_up(&self) -> Result<()> {
        eprintln!("asr: warming up transcription engine (silent probe)…");
        match self.transcribe(silent_wav_16k(300), "audio/wav").await {
            Ok(_) => {
                eprintln!("asr: transcription engine warm");
                Ok(())
            }
            Err(e) => {
                eprintln!("asr: warm-up skipped ({e})");
                Err(e)
            }
        }
    }
}

fn first_chars(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

/// A minimal mono 16-bit PCM WAV of `ms` milliseconds of silence at 16 kHz —
/// just enough of a valid container to warm the engine.
fn silent_wav_16k(ms: u32) -> Vec<u8> {
    const SR: u32 = 16_000;
    let samples = (SR as u64 * ms as u64 / 1000) as u32;
    let data_len = samples * 2; // 16-bit mono
    let mut w = Vec::with_capacity(44 + data_len as usize);
    w.extend_from_slice(b"RIFF");
    w.extend_from_slice(&(36 + data_len).to_le_bytes());
    w.extend_from_slice(b"WAVE");
    w.extend_from_slice(b"fmt ");
    w.extend_from_slice(&16u32.to_le_bytes()); // PCM fmt chunk size
    w.extend_from_slice(&1u16.to_le_bytes()); // PCM
    w.extend_from_slice(&1u16.to_le_bytes()); // mono
    w.extend_from_slice(&SR.to_le_bytes());
    w.extend_from_slice(&(SR * 2).to_le_bytes()); // byte rate
    w.extend_from_slice(&2u16.to_le_bytes()); // block align
    w.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    w.extend_from_slice(b"data");
    w.extend_from_slice(&data_len.to_le_bytes());
    w.resize(44 + data_len as usize, 0); // silence
    w
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A one-shot loopback server that drains the request and replies with a
    /// fixed JSON body (mirrors the brain client's test harness).
    async fn serve_once(status_line: &'static str, body: &'static str) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut sock, _) = listener.accept().await.unwrap();
            let mut buf = [0u8; 8192];
            let _ = sock.read(&mut buf).await;
            let resp = format!(
                "{status_line}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            sock.write_all(resp.as_bytes()).await.unwrap();
            let _ = sock.flush().await;
        });
        format!("http://{addr}")
    }

    #[tokio::test]
    async fn transcribe_extracts_text() {
        let base = serve_once(
            "HTTP/1.1 200 OK",
            "{\"text\":\"hello world\",\"model\":\"moonshine\"}",
        )
        .await;
        let client = AsrClient::new(base).unwrap();
        let text = client
            .transcribe(vec![1, 2, 3, 4], "audio/wav")
            .await
            .unwrap();
        assert_eq!(text, "hello world");
    }

    #[tokio::test]
    async fn empty_transcript_is_ok_not_error() {
        let base = serve_once("HTTP/1.1 200 OK", "{\"text\":\"\"}").await;
        let client = AsrClient::new(base).unwrap();
        assert_eq!(
            client.transcribe(vec![0; 16], "audio/wav").await.unwrap(),
            ""
        );
    }

    #[tokio::test]
    async fn warming_up_503_is_a_clear_error() {
        let base = serve_once(
            "HTTP/1.1 503 Service Unavailable",
            "{\"error\":{\"message\":\"model is being pulled\",\"code\":\"warming_up\"}}",
        )
        .await;
        let client = AsrClient::new(base).unwrap();
        let err = client
            .transcribe(vec![0; 16], "audio/wav")
            .await
            .unwrap_err();
        assert!(err.to_string().contains("warming up"), "got: {err}");
    }

    #[test]
    fn silent_wav_has_a_valid_riff_header() {
        let w = silent_wav_16k(100);
        assert_eq!(&w[0..4], b"RIFF");
        assert_eq!(&w[8..12], b"WAVE");
        assert_eq!(w.len(), 44 + 3200);
    }
}
