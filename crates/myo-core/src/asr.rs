//! The ears client — a loopback HTTP client for MyOwnLLM's transcription.
//!
//! Myo is a character who's always listening. Open-mic audio is captured in
//! the WebView (so the browser's echo-cancellation sees the same context as
//! TTS playback), and each detected utterance is handed to the backend,
//! which forwards it here. We POST the raw audio bytes to MyOwnLLM's `serve`
//! ASR route — `POST /v1/audio/transcriptions` on `:1473` — and get back the
//! recognized text. MyOwnLLM owns the actual Moonshine/Parakeet inference;
//! this is the thin seam to it (the model-engine sidecar Myo already spawns,
//! so there's no second engine to bundle).
//!
//! Audio is transcribed transiently: MyOwnLLM deletes the upload the moment
//! it finishes. That's Myo's privacy default — she's always listening, but
//! nothing is kept (unlike MyOwnLLM's opt-in "keep audio").

use std::time::Duration;

use anyhow::{anyhow, Result};
use reqwest::{Client, StatusCode};
use serde_json::Value;

/// A loopback client for MyOwnLLM's HTTP transcription route.
pub struct AsrClient {
    http: Client,
    base_url: String,
}

impl AsrClient {
    /// Build a client pointed at MyOwnLLM's base URL (e.g.
    /// `http://127.0.0.1:1473`). The per-request timeout is deliberately
    /// generous: the *first* transcription on a cold machine downloads the
    /// onnxruntime dylib and the ASR model before it can answer (see
    /// [`AsrClient::warm_up`], which pre-pays that cost at startup so live
    /// utterances stay snappy).
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

    /// POST one utterance's audio and return the recognized text. The text
    /// may be empty (silence / noise / non-speech) — callers should treat an
    /// empty transcript as "nothing was said" and not open a turn. `mime` is
    /// best-effort metadata; MyOwnLLM probes the container by content anyway.
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
            // Engine still warming (downloading runtime / model). Surface a
            // clear, retriable error rather than a generic failure.
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

    /// Best-effort warm-up: send a short clip of silence so MyOwnLLM pulls
    /// the onnxruntime dylib + ASR model and loads it *now*, rather than on
    /// the user's first real words. Fire-and-forget; the empty transcript is
    /// discarded and any error is just logged (the live path will retry).
    pub async fn warm_up(&self) -> Result<()> {
        eprintln!("asr: warming up transcription engine (silent probe)…");
        let wav = silent_wav_16k(300);
        match self.transcribe(wav, "audio/wav").await {
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
/// just enough of a valid container to force the engine to initialize.
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
            "{\"text\":\"hello world\",\"model\":\"moonshine-small-q8\"}",
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
            "{\"error\":{\"message\":\"model moonshine is being pulled\",\"code\":\"warming_up\"}}",
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
        // 16 kHz * 0.1 s * 2 bytes = 3200 bytes of samples + 44 header.
        assert_eq!(w.len(), 44 + 3200);
    }
}
