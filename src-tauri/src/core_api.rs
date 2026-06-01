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

// ─── Converse ───────────────────────────────────────────────────────────────

/// Speak (well, type) to Myo: the text bypass path. Allocates a turn, echoes the
/// user's words as a final transcript, and spawns the brain→TTS round-trip,
/// returning the turn id immediately so the UI can follow the stream.
#[tauri::command]
pub async fn myo_converse_say(
    text: String,
    app: AppHandle,
    state: Shared<'_>,
) -> Result<TurnId, String> {
    let state = state.inner().clone();
    let session = state.ensure_session().await.map_err(|e| e.to_string())?;
    let (caps, incognito) = state.turn_context();
    let turn = state.turns.allocate();

    // Show what the user said straight away.
    emit(
        &app,
        MyoEvent::TranscriptFinal {
            turn,
            text: text.clone(),
            speaker: None,
        },
    );

    let app_task = app.clone();
    let st = state.clone();
    let done = Arc::new(AtomicBool::new(false));
    let done_task = done.clone();
    let handle = tauri::async_runtime::spawn(async move {
        let mut sink = |ev: MyoEvent| emit(&app_task, ev);
        if let Err(e) =
            myo_core::run_turn(&st.brain, &session, &text, caps, incognito, turn, &mut sink).await
        {
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
        done_task.store(true, Ordering::Relaxed);
    });
    state.track_task(turn, TurnTask { handle, done });
    Ok(turn)
}

/// Cancel an in-flight turn (barge-in or an explicit stop). Returns whether a
/// turn was actually running.
#[tauri::command]
pub fn myo_converse_cancel(turn: TurnId, app: AppHandle, state: Shared<'_>) -> bool {
    let cancelled = state.cancel_task(turn);
    if cancelled {
        // Close the turn out on the UI side too.
        emit(&app, MyoEvent::AssistantDone { turn });
    }
    cancelled
}

/// The WAV bypass path — reserved for the on-device ASR engine (`myo-asr`),
/// which isn't bundled in this build. Until it lands, transcription has no local
/// engine; use [`myo_converse_say`] for the working text path.
#[tauri::command]
pub fn myo_converse_feed_wav(path: String) -> Result<TurnId, String> {
    Err(format!(
        "on-device ASR (myo-asr) isn't bundled in this build yet — \
         the WAV path will transcribe {path} once the engine is extracted. \
         Use myo_converse_say for the text path."
    ))
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

/// What Myo remembers (optionally filtered by a query).
#[tauri::command]
pub async fn myo_memory_list(query: Option<String>, state: Shared<'_>) -> Result<Value, String> {
    let state = state.inner().clone();
    state
        .brain
        .list_memories(query.as_deref())
        .await
        .map_err(|e| e.to_string())
}

/// Forget one memory by id.
#[tauri::command]
pub async fn myo_memory_forget(id: String, state: Shared<'_>) -> Result<(), String> {
    let state = state.inner().clone();
    state
        .brain
        .forget_memory(&id)
        .await
        .map_err(|e| e.to_string())
}

// ─── Settings & misc ────────────────────────────────────────────────────────

/// The whole persisted shell-settings document.
#[tauri::command]
pub fn myo_settings_get(state: Shared<'_>) -> ShellSettings {
    state.settings.lock().unwrap().clone()
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
    match state.brain.tts(&text).await.map_err(|e| e.to_string())? {
        Some(a) => emit(
            &app,
            MyoEvent::AudioReady {
                turn,
                b64: a.b64,
                mime: a.mime,
            },
        ),
        None => emit(&app, MyoEvent::AudioSpeak { turn, text }),
    }
    Ok(turn)
}
