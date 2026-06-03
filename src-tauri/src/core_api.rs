//! The Myo Core API — the stable seam between the Svelte shell and the engines.
//!
//! Every command is a thin adapter over `myo-core`: it snapshots a little state,
//! calls the core, and (for streaming work) spawns a turn task that pumps the
//! normalized [`MyoEvent`] stream to the WebView via [`emit`]. The intelligence
//! lives in `myo-core`; this file is wiring.
//!
//! Argument names are single words so the JS↔Rust casing convention never bites
//! (multi-word state crosses the boundary inside serde structs like
//! [`Capabilities`], whose `snake_case` fields the frontend sends verbatim).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use serde_json::{json, Value};
use tauri::{AppHandle, State};

use myo_core::{Capabilities, MyoEvent, ShellSettings, TurnId};

use crate::events::emit;
use crate::state::{MyoState, TurnTask};

type Shared<'a> = State<'a, Arc<MyoState>>;

// ─── Engines ────────────────────────────────────────────────────────────────

/// Are the brain and model engine up right now?
#[tauri::command]
pub async fn myo_engines_status(state: Shared<'_>) -> Result<Value, String> {
    let state = state.inner().clone();
    let odysseus = state.brain.health().await.unwrap_or(false);
    let health_url = format!("{}/healthz", myo_core::supervisor::myownllm_base_url());
    let myownllm = myo_core::supervisor::endpoint_reachable(&health_url).await;
    Ok(json!({ "odysseus": odysseus, "myownllm": myownllm }))
}

/// Launch + health-check + auto-configure the engines (idempotent). Progress is
/// narrated on `myo://engine`.
#[tauri::command]
pub async fn myo_engines_ensure_ready(app: AppHandle, state: Shared<'_>) -> Result<(), String> {
    let state = state.inner().clone();
    crate::supervisor::ensure_ready(app, state).await;
    Ok(())
}

/// The engine's live streaming-transcription WebSocket URL
/// (`ws://127.0.0.1:11473/v1/audio/stream`). The WebView connects here for
/// real-time dictation: it streams 16 kHz mono i16-LE PCM up and reads interim
/// + final caption frames down. Myo owns this engine on a private loopback port
/// and runs it tokenless, so the socket needs no auth.
#[tauri::command]
pub fn myo_asr_stream_url() -> String {
    myo_core::supervisor::myownllm_stream_url()
}

// ─── Converse ───────────────────────────────────────────────────────────────

/// Run one converse turn for already-known text: echo it as the final
/// transcript, then spawn the **native** brain→voice round-trip (Myo streams the
/// reply straight from MyOwnLLM — no Odysseus) and return the turn id
/// immediately so the UI can follow the `myo://` stream. Shared by the text path
/// ([`myo_converse_say`]) and the voice paths ([`myo_converse_feed_audio`] /
/// [`myo_converse_feed_wav`], which prepend ASR).
async fn spawn_text_turn(
    app: AppHandle,
    state: Arc<MyoState>,
    text: String,
) -> Result<TurnId, String> {
    let turn = state.turns.allocate();
    // A turn is activity — push back Dream mode's downtime clock.
    state.mark_activity();

    // Show what the user said straight away.
    emit(
        &app,
        MyoEvent::TranscriptFinal {
            turn,
            text: text.clone(),
            speaker: None,
        },
    );

    // Snapshot the turn's settings; the memory-aware loop assembles the context
    // itself (recall + working window) from `state.memory`. No Odysseus involved.
    let caps = state.capabilities();
    let incognito = state.incognito();
    let persona = state.persona();

    let app_task = app.clone();
    let st = state.clone();
    let done = Arc::new(AtomicBool::new(false));
    let done_task = done.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut sink = |ev: MyoEvent| emit(&app_task, ev);
        // Keep the user's words for automatic memory capture below (the loop
        // consumes `text`).
        let user_text = text.clone();
        // The loop records the user turn and the reply into working memory itself.
        match myo_core::run_turn_native(
            st.llm.clone(),
            &st.tts,
            st.web.clone(),
            st.memory.clone(),
            caps,
            incognito,
            persona,
            text,
            turn,
            &mut sink,
        )
        .await
        {
            Ok(reply) => {
                // Capture any durable memory from this exchange — model-independent,
                // so memory fills even when the served model can't call the
                // `remember` tool. Detached so it never delays the conversation.
                let llm = st.llm.clone();
                let mem = st.memory.clone();
                tauri::async_runtime::spawn(async move {
                    myo_core::memory::ingest_turn(&llm, &mem, &user_text, &reply, incognito).await;
                });
            }
            Err(e) => {
                // Surface the failure and unblock the turn so the UI doesn't hang.
                emit(
                    &app_task,
                    MyoEvent::Progress {
                        turn,
                        kind: "error".into(),
                        data: json!({ "message": e.to_string() }),
                    },
                );
                emit(&app_task, MyoEvent::AssistantDone { turn });
            }
        }
        // The reply is done — restart the downtime clock from now, so Dream mode
        // waits the full idle window after the conversation actually settles.
        st.mark_activity();
        done_task.store(true, Ordering::Relaxed);
    });
    state.track_task(turn, TurnTask { handle, done });
    Ok(turn)
}

/// Speak (well, type) to Myo: the text bypass path. Allocates a turn, echoes the
/// user's words as a final transcript, and spawns the brain→TTS round-trip,
/// returning the turn id immediately so the UI can follow the stream.
#[tauri::command]
pub async fn myo_converse_say(
    text: String,
    app: AppHandle,
    state: Shared<'_>,
) -> Result<TurnId, String> {
    spawn_text_turn(app, state.inner().clone(), text).await
}

/// Hard-cancel an in-flight turn: abort its generation outright. Returns whether
/// a turn was actually running.
///
/// This is the explicit, last-resort stop — *not* the conversational barge-in.
/// Talking over Myo never lands here: she lets every turn finish (each one
/// carries the whole conversation, so there's nothing to gain by killing one)
/// and the shell simply hushes her voice when you take the floor. Kept for an
/// explicit "force stop" and teardown.
#[tauri::command]
pub fn myo_converse_cancel(turn: TurnId, app: AppHandle, state: Shared<'_>) -> bool {
    let cancelled = state.cancel_task(turn);
    if cancelled {
        // Close the turn out on the UI side too.
        emit(&app, MyoEvent::AssistantDone { turn });
    }
    cancelled
}

/// The voice path: transcribe one captured utterance, then run it as a turn.
///
/// `audio` is base64-encoded raw audio bytes — a WAV from the WebView's
/// always-on open-mic capture. We hand it to MyOwnLLM's transcription route,
/// then (for a non-empty transcript) drive the same brain→TTS turn as text.
/// Returns the new turn id, or `None` when the utterance transcribed to
/// nothing (silence / background noise) so the always-listening loop never
/// opens an empty turn.
#[tauri::command]
pub async fn myo_converse_feed_audio(
    audio: String,
    mime: String,
    app: AppHandle,
    state: Shared<'_>,
) -> Result<Option<TurnId>, String> {
    use base64::Engine;
    let state = state.inner().clone();
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(audio.as_bytes())
        .map_err(|e| format!("could not decode audio (expected base64): {e}"))?;
    eprintln!("converse: feed_audio {} bytes ({mime})", bytes.len());
    let text = state
        .asr
        .transcribe(bytes, &mime)
        .await
        .map_err(|e| e.to_string())?;
    if text.is_empty() {
        eprintln!("converse: empty transcript — utterance ignored");
        return Ok(None);
    }
    spawn_text_turn(app, state, text).await.map(Some)
}

/// The WAV-file bypass (CI / "transcribe this file"): read an audio file from
/// disk, transcribe it via MyOwnLLM, and run the result as a turn — the mic is
/// never touched. Returns the turn id, or `None` for an empty transcript. This
/// is the non-audio test hook the PLAN's CI uses to exercise the voice spine
/// without a microphone.
#[tauri::command]
pub async fn myo_converse_feed_wav(
    path: String,
    app: AppHandle,
    state: Shared<'_>,
) -> Result<Option<TurnId>, String> {
    let state = state.inner().clone();
    let bytes = std::fs::read(&path).map_err(|e| format!("reading {path}: {e}"))?;
    eprintln!("converse: feed_wav {path} ({} bytes)", bytes.len());
    let text = state
        .asr
        .transcribe(bytes, "audio/wav")
        .await
        .map_err(|e| e.to_string())?;
    if text.is_empty() {
        return Ok(None);
    }
    spawn_text_turn(app, state, text).await.map(Some)
}

/// Pause/resume memory for the conversation (privacy / incognito). Persisted.
#[tauri::command]
pub fn myo_converse_incognito(on: bool, state: Shared<'_>) -> Result<bool, String> {
    let mut s = state.settings.lock().unwrap();
    s.incognito = on;
    s.save().map_err(|e| e.to_string())?;
    Ok(on)
}

// ─── Capabilities ───────────────────────────────────────────────────────────

/// The current four toggles.
#[tauri::command]
pub fn myo_capabilities_get(state: Shared<'_>) -> Capabilities {
    state.settings.lock().unwrap().capabilities
}

/// Set the four toggles: persist them and push the composed `disabled_tools`
/// allowlist to the brain (best-effort — it's re-applied on the next
/// `ensure_ready` if the brain is currently down).
#[tauri::command]
pub async fn myo_capabilities_set(
    caps: Capabilities,
    state: Shared<'_>,
) -> Result<Capabilities, String> {
    let state = state.inner().clone();
    {
        let mut s = state.settings.lock().unwrap();
        s.capabilities = caps;
        s.save().map_err(|e| e.to_string())?;
    }
    let _ = state.brain.set_disabled_tools(&caps.disabled_tools()).await;
    Ok(caps)
}

// ─── Memory ─────────────────────────────────────────────────────────────────

/// What Myo remembers (optionally filtered by a text query) — the native
/// long-term store, in the `{ memory: [...] }` shape the Memory panel renders.
#[tauri::command]
pub fn myo_memory_list(query: Option<String>, state: Shared<'_>) -> Result<Value, String> {
    let records = state.memory.list(query.as_deref());
    let memory: Vec<Value> = records
        .iter()
        .map(|m| {
            json!({
                "id": m.id.to_string(),
                "text": m.text,
                "category": m.category,
                "created_at": m.created_at,
            })
        })
        .collect();
    Ok(json!({ "memory": memory }))
}

/// Forget one durable memory by id.
#[tauri::command]
pub fn myo_memory_forget(id: String, state: Shared<'_>) -> Result<(), String> {
    let id: i64 = id.parse().map_err(|_| format!("invalid memory id: {id}"))?;
    state.memory.forget(id).map_err(|e| e.to_string())?;
    Ok(())
}

/// Embed text into vectors via the local model engine (`/v1/embeddings`
/// against the `myownllm-embed` virtual ID). The primitive Myo's native
/// memory system builds on — store these vectors and cosine-search them to
/// recall context, all on the user's own machine. Returns one vector per
/// input string, in order.
#[tauri::command]
pub async fn myo_embed(texts: Vec<String>, state: Shared<'_>) -> Result<Vec<Vec<f32>>, String> {
    let state = state.inner().clone();
    state.llm.embed(&texts).await.map_err(|e| e.to_string())
}

// ─── Settings & misc ────────────────────────────────────────────────────────

/// The whole persisted shell-settings document.
#[tauri::command]
pub fn myo_settings_get(state: Shared<'_>) -> ShellSettings {
    state.settings.lock().unwrap().clone()
}

/// Myo's persona — the system prompt that opens every conversation: the
/// effective text in force, the built-in default (for "reset to default"), and
/// whether a custom override is set.
#[tauri::command]
pub fn myo_persona_get(state: Shared<'_>) -> Value {
    json!({
        "effective": state.persona(),
        "default": myo_core::MYO_PERSONA,
        "custom": state.persona_is_custom(),
    })
}

/// Set (or clear) Myo's custom persona. An empty / whitespace-only value clears
/// the override and restores the built-in default. Persisted to `~/.myo`.
#[tauri::command]
pub fn myo_persona_set(persona: String, state: Shared<'_>) -> Result<Value, String> {
    {
        let mut s = state.settings.lock().unwrap();
        s.persona = if persona.trim().is_empty() {
            None
        } else {
            Some(persona)
        };
        s.save().map_err(|e| e.to_string())?;
    }
    Ok(json!({
        "effective": state.persona(),
        "default": myo_core::MYO_PERSONA,
        "custom": state.persona_is_custom(),
    }))
}

/// Synthesize (or fall back to WebSpeech for) a one-off line of speech.
#[tauri::command]
pub async fn myo_tts_speak(
    text: String,
    app: AppHandle,
    state: Shared<'_>,
) -> Result<TurnId, String> {
    let state = state.inner().clone();
    let turn = state.turns.allocate();
    // Synthesize via the engine Myo owns; degrade to WebSpeech on any failure
    // (route absent, warming, or synthesis unavailable) — the tier-4 fallback.
    match state.tts.synthesize(&text, None).await {
        Ok(a) => emit(
            &app,
            MyoEvent::AudioReady {
                turn,
                b64: a.b64,
                mime: a.mime,
            },
        ),
        Err(_) => emit(&app, MyoEvent::AudioSpeak { turn, text }),
    }
    Ok(turn)
}
