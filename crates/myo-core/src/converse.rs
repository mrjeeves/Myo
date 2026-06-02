//! The converse spine — one utterance→answer→voice round-trip.
//!
//! This is the orchestration the PLAN's voice loop runs (ASR→brain→TTS),
//! minus the audio I/O (which lives in the WebView for echo-cancellation). It
//! allocates a turn id, streams the brain's answer through the normalized
//! intent stream, then voices the spoken text — synthesized server-side when a
//! TTS provider is available, or handed to the browser's speech engine when not.
//!
//! `myo_converse_say` (text in) is exactly [`run_turn`] with the text already
//! known; the mic/WAV paths prepend ASR (which finalizes a transcript and then
//! calls the same [`run_turn`]).

use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::Result;

use crate::brain::BrainClient;
use crate::capabilities::Capabilities;
use crate::event::{MyoEvent, TurnId};
use crate::llm::{ChatMessage, LlmClient};

/// Hands out a fresh [`TurnId`] per detected utterance / `say` / `feed_wav`.
#[derive(Debug)]
pub struct TurnAllocator {
    next: AtomicU64,
}

impl Default for TurnAllocator {
    fn default() -> Self {
        // Start at 1 so 0 can mean "no turn" on the frontend.
        Self {
            next: AtomicU64::new(1),
        }
    }
}

impl TurnAllocator {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn allocate(&self) -> TurnId {
        self.next.fetch_add(1, Ordering::Relaxed)
    }
}

/// Run one full converse turn for already-known text.
///
/// Streams `chat_stream` (emitting deltas, activity, artifacts, ui directives …
/// through `emit`), accumulates the spoken answer, then emits exactly one audio
/// event for the turn: [`MyoEvent::AudioReady`] when the brain synthesized
/// speech, or [`MyoEvent::AudioSpeak`] as the WebSpeech fallback.
pub async fn run_turn(
    brain: &BrainClient,
    session: &str,
    message: &str,
    caps: Capabilities,
    incognito: bool,
    turn: TurnId,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Result<()> {
    let mut spoken = String::new();
    {
        // Tee the stream: forward every event, and keep the spoken text for TTS.
        let mut capture = |ev: MyoEvent| {
            if let MyoEvent::AssistantDelta { text, .. } = &ev {
                spoken.push_str(text);
            }
            emit(ev);
        };
        brain
            .chat_stream(session, message, caps, incognito, turn, &mut capture)
            .await?;
    }

    let spoken = spoken.trim();
    if spoken.is_empty() {
        return Ok(());
    }
    match brain.tts(spoken).await? {
        Some(audio) => emit(MyoEvent::AudioReady {
            turn,
            b64: audio.b64,
            mime: audio.mime,
        }),
        None => emit(MyoEvent::AudioSpeak {
            turn,
            text: spoken.to_string(),
        }),
    }
    Ok(())
}

/// Run one full converse turn **natively** — Myo's own brain.
///
/// Streams the reply straight from MyOwnLLM ([`LlmClient::chat_stream`] emits the
/// deltas + closes the turn), accumulates the spoken text, and voices it. There
/// is no Odysseus in this path: `messages` is the whole context (persona +
/// history + this user turn) the caller assembled. Returns the spoken text so
/// the caller can append it to the conversation history.
///
/// TTS is the WebSpeech fallback for now ([`MyoEvent::AudioSpeak`]); native
/// synthesis ([`MyoEvent::AudioReady`]) is a later slice (see
/// `docs/native-agent.md`).
pub async fn run_turn_native(
    llm: &LlmClient,
    messages: &[ChatMessage],
    turn: TurnId,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Result<String> {
    let mut spoken = String::new();
    {
        // Tee the stream: forward every event, keep the spoken text for TTS + history.
        let mut capture = |ev: MyoEvent| {
            if let MyoEvent::AssistantDelta { text, .. } = &ev {
                spoken.push_str(text);
            }
            emit(ev);
        };
        llm.chat_stream(messages, turn, &mut capture).await?;
    }

    let spoken = spoken.trim().to_string();
    if !spoken.is_empty() {
        emit(MyoEvent::AudioSpeak {
            turn,
            text: spoken.clone(),
        });
    }
    Ok(spoken)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn turn_ids_start_at_one_and_increase() {
        let a = TurnAllocator::new();
        assert_eq!(a.allocate(), 1);
        assert_eq!(a.allocate(), 2);
        assert_eq!(a.allocate(), 3);
    }

    #[test]
    fn allocator_is_shareable_across_threads() {
        use std::sync::Arc;
        let a = Arc::new(TurnAllocator::new());
        let mut handles = Vec::new();
        for _ in 0..8 {
            let a = a.clone();
            handles.push(std::thread::spawn(move || a.allocate()));
        }
        let mut ids: Vec<TurnId> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), 8, "every allocated turn id must be unique");
    }
}
