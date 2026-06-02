//! The normalized `myo://` intent stream — Myo's own UI protocol.
//!
//! Odysseus emits a rich, agent-driven event stream over SSE (deltas, tool
//! activity, streamed document artifacts, `ui_control` directives, …). Rather
//! than leak Odysseus's wire shapes into the shell, [`brain::normalize`] folds
//! each one into a [`MyoEvent`] — a small, stable vocabulary the Svelte
//! surface-registry renders. Adding a new agent surface is just adding a
//! variant here and a renderer there; *that* extensibility is the "dissolved
//! UI."
//!
//! Each [`MyoEvent`] knows its own channel and JSON body via [`MyoEvent::emit`].
//! `myo-core` stays Tauri-free: the shell's job is the one-liner
//! `let Emit { channel, payload } = ev.emit(); app.emit(channel, payload);`.
//!
//! [`brain::normalize`]: crate::brain::normalize

use serde_json::{json, Value};

/// A turn is one utterance→answer round. The converse spine allocates a fresh
/// id per detected utterance (or per `say`/`feed_wav` call); every event of
/// that round carries it so the frontend can group and address them.
pub type TurnId = u64;

/// The `myo://` event channels. Stable names the frontend `listen`s on; the
/// per-turn grouping rides in the payload's `turn` field (not the channel name)
/// so the webview can subscribe once per kind.
pub mod channel {
    /// Assistant text stream (→ TTS). Payload `{turn, kind: "delta"|"done", text?}`.
    pub const ASSISTANT: &str = "myo://assistant";
    /// ASR transcript. Payload `{turn, kind: "partial"|"final", text, speaker?}`.
    pub const TRANSCRIPT: &str = "myo://transcript";
    /// Tool activity feed. Payload `{turn, phase: "start"|"progress"|"output", tool, …}`.
    pub const ACTIVITY: &str = "myo://activity";
    /// Editable document artifact. Payload `{turn, kind: "open"|"delta"|"update"|"suggestions", …}`.
    pub const ARTIFACT: &str = "myo://artifact";
    /// Agent-driven UI directive (`ui_control`). Payload `{turn, directive, data}`.
    pub const UI: &str = "myo://ui";
    /// Thinking/progress + ancillary metadata. Payload `{turn, kind, data}`.
    pub const PROGRESS: &str = "myo://progress";
    /// Synthesized speech, ready to play. Payload `{turn, kind: "ready", b64, mime}`.
    pub const AUDIO: &str = "myo://audio";
    /// Engine lifecycle (no turn). Payload `{name, status, detail?}`.
    pub const ENGINE: &str = "myo://engine";
    /// Dream-mode memory consolidation (no turn). Payload `{phase, note?}`.
    pub const DREAM: &str = "myo://dream";
}

/// A routed event: a channel name plus its JSON body, ready for `app.emit`.
#[derive(Debug, Clone, PartialEq)]
pub struct Emit {
    pub channel: &'static str,
    pub payload: Value,
}

/// One normalized event in the `myo://` intent stream.
#[derive(Debug, Clone, PartialEq)]
pub enum MyoEvent {
    /// A chunk of the assistant's spoken/written answer.
    AssistantDelta { turn: TurnId, text: String },
    /// The assistant has finished this turn's answer.
    AssistantDone { turn: TurnId },

    /// A partial (still-being-spoken) transcript of the user.
    TranscriptPartial {
        turn: TurnId,
        text: String,
        speaker: Option<String>,
    },
    /// The finalized transcript of one user utterance.
    TranscriptFinal {
        turn: TurnId,
        text: String,
        speaker: Option<String>,
    },

    /// A tool started. `command` is its headline (e.g. the shell line).
    ActivityStart {
        turn: TurnId,
        tool: String,
        command: Option<String>,
        round: Option<u64>,
    },
    /// A running tool reported progress.
    ActivityProgress {
        turn: TurnId,
        tool: String,
        progress: Option<String>,
        round: Option<u64>,
    },
    /// A tool finished. May carry output text and/or an inline image/screenshot.
    ActivityOutput {
        turn: TurnId,
        tool: String,
        output: Option<String>,
        exit_code: Option<i64>,
        image_url: Option<String>,
        round: Option<u64>,
    },

    /// The agent began streaming a document — materialize an editable artifact.
    ArtifactOpen {
        turn: TurnId,
        title: Option<String>,
        language: Option<String>,
    },
    /// A chunk of the streaming document body.
    ArtifactDelta { turn: TurnId, content: String },
    /// A whole-document update (id, final content, version) — replaces the body.
    ArtifactUpdate {
        turn: TurnId,
        doc_id: Option<String>,
        title: Option<String>,
        language: Option<String>,
        content: Option<String>,
        version: Option<i64>,
    },
    /// Edit suggestions for an open document.
    ArtifactSuggestions {
        turn: TurnId,
        doc_id: Option<String>,
        suggestions: Value,
    },

    /// An agent-driven UI directive (`ui_control`): open_panel, toggle, set_mode,
    /// switch_model, set_theme, highlight, open_email_reply, …
    Ui {
        turn: TurnId,
        directive: String,
        data: Value,
    },

    /// Progress + ancillary metadata (agent_step, budget, research_*,
    /// memories_used, rag_sources, model_info, metrics, …) under one channel,
    /// discriminated by `kind`.
    Progress {
        turn: TurnId,
        kind: String,
        data: Value,
    },

    /// Synthesized speech for this turn, base64-encoded and ready to play.
    AudioReady {
        turn: TurnId,
        b64: String,
        mime: String,
    },
    /// No server-side TTS (the provider is disabled): the shell should voice
    /// this text with the browser's speech engine instead. Carrying it as its
    /// own event means the frontend never has to guess whether audio is coming.
    AudioSpeak { turn: TurnId, text: String },

    /// An engine changed state (starting / healthy / failed / stopped).
    Engine {
        name: String,
        status: String,
        detail: Option<String>,
    },

    /// Dream mode is consolidating memory during downtime. `phase` is
    /// `"forget"` or `"consolidate"`; `note` is a short human description.
    Dream { phase: String, note: Option<String> },
}

impl MyoEvent {
    /// Route this event to its channel and serialize its body. The single seam
    /// between the Tauri-free core and the shell's `app.emit`.
    pub fn emit(self) -> Emit {
        use channel as ch;
        match self {
            MyoEvent::AssistantDelta { turn, text } => Emit {
                channel: ch::ASSISTANT,
                payload: json!({ "turn": turn, "kind": "delta", "text": text }),
            },
            MyoEvent::AssistantDone { turn } => Emit {
                channel: ch::ASSISTANT,
                payload: json!({ "turn": turn, "kind": "done" }),
            },
            MyoEvent::TranscriptPartial {
                turn,
                text,
                speaker,
            } => Emit {
                channel: ch::TRANSCRIPT,
                payload: json!({ "turn": turn, "kind": "partial", "text": text, "speaker": speaker }),
            },
            MyoEvent::TranscriptFinal {
                turn,
                text,
                speaker,
            } => Emit {
                channel: ch::TRANSCRIPT,
                payload: json!({ "turn": turn, "kind": "final", "text": text, "speaker": speaker }),
            },
            MyoEvent::ActivityStart {
                turn,
                tool,
                command,
                round,
            } => Emit {
                channel: ch::ACTIVITY,
                payload: json!({ "turn": turn, "phase": "start", "tool": tool, "command": command, "round": round }),
            },
            MyoEvent::ActivityProgress {
                turn,
                tool,
                progress,
                round,
            } => Emit {
                channel: ch::ACTIVITY,
                payload: json!({ "turn": turn, "phase": "progress", "tool": tool, "progress": progress, "round": round }),
            },
            MyoEvent::ActivityOutput {
                turn,
                tool,
                output,
                exit_code,
                image_url,
                round,
            } => Emit {
                channel: ch::ACTIVITY,
                payload: json!({
                    "turn": turn, "phase": "output", "tool": tool,
                    "output": output, "exit_code": exit_code,
                    "image_url": image_url, "round": round
                }),
            },
            MyoEvent::ArtifactOpen {
                turn,
                title,
                language,
            } => Emit {
                channel: ch::ARTIFACT,
                payload: json!({ "turn": turn, "kind": "open", "title": title, "language": language }),
            },
            MyoEvent::ArtifactDelta { turn, content } => Emit {
                channel: ch::ARTIFACT,
                payload: json!({ "turn": turn, "kind": "delta", "content": content }),
            },
            MyoEvent::ArtifactUpdate {
                turn,
                doc_id,
                title,
                language,
                content,
                version,
            } => Emit {
                channel: ch::ARTIFACT,
                payload: json!({
                    "turn": turn, "kind": "update", "doc_id": doc_id,
                    "title": title, "language": language,
                    "content": content, "version": version
                }),
            },
            MyoEvent::ArtifactSuggestions {
                turn,
                doc_id,
                suggestions,
            } => Emit {
                channel: ch::ARTIFACT,
                payload: json!({ "turn": turn, "kind": "suggestions", "doc_id": doc_id, "suggestions": suggestions }),
            },
            MyoEvent::Ui {
                turn,
                directive,
                data,
            } => Emit {
                channel: ch::UI,
                payload: json!({ "turn": turn, "directive": directive, "data": data }),
            },
            MyoEvent::Progress { turn, kind, data } => Emit {
                channel: ch::PROGRESS,
                payload: json!({ "turn": turn, "kind": kind, "data": data }),
            },
            MyoEvent::AudioReady { turn, b64, mime } => Emit {
                channel: ch::AUDIO,
                payload: json!({ "turn": turn, "kind": "ready", "b64": b64, "mime": mime }),
            },
            MyoEvent::AudioSpeak { turn, text } => Emit {
                channel: ch::AUDIO,
                payload: json!({ "turn": turn, "kind": "speak", "text": text }),
            },
            MyoEvent::Engine {
                name,
                status,
                detail,
            } => Emit {
                channel: ch::ENGINE,
                payload: json!({ "name": name, "status": status, "detail": detail }),
            },
            MyoEvent::Dream { phase, note } => Emit {
                channel: ch::DREAM,
                payload: json!({ "phase": phase, "note": note }),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn assistant_delta_routes_and_serializes() {
        let e = MyoEvent::AssistantDelta {
            turn: 7,
            text: "hi".into(),
        }
        .emit();
        assert_eq!(e.channel, channel::ASSISTANT);
        assert_eq!(e.payload["turn"], json!(7));
        assert_eq!(e.payload["kind"], json!("delta"));
        assert_eq!(e.payload["text"], json!("hi"));
    }

    #[test]
    fn activity_output_carries_image() {
        let e = MyoEvent::ActivityOutput {
            turn: 1,
            tool: "bash".into(),
            output: Some("done".into()),
            exit_code: Some(0),
            image_url: Some("data:image/png;base64,AAA".into()),
            round: Some(2),
        }
        .emit();
        assert_eq!(e.channel, channel::ACTIVITY);
        assert_eq!(e.payload["phase"], json!("output"));
        assert_eq!(e.payload["exit_code"], json!(0));
        assert_eq!(e.payload["image_url"], json!("data:image/png;base64,AAA"));
    }

    #[test]
    fn engine_event_has_no_turn() {
        let e = MyoEvent::Engine {
            name: "odysseus".into(),
            status: "healthy".into(),
            detail: None,
        }
        .emit();
        assert_eq!(e.channel, channel::ENGINE);
        assert_eq!(e.payload["name"], json!("odysseus"));
        assert_eq!(e.payload["status"], json!("healthy"));
        assert!(e.payload.get("turn").is_none());
    }

    #[test]
    fn channels_use_the_myo_scheme() {
        // The shell emits these verbatim as Tauri event names.
        for c in [
            channel::ASSISTANT,
            channel::TRANSCRIPT,
            channel::ACTIVITY,
            channel::ARTIFACT,
            channel::UI,
            channel::PROGRESS,
            channel::AUDIO,
            channel::ENGINE,
            channel::DREAM,
        ] {
            assert!(c.starts_with("myo://"));
        }
    }
}
