//! `myo-core` — the Myo shell's orchestration core.
//!
//! Myo *is* the AI, not an app with an AI inside it: a thin voice-first shell
//! that composes three local engines as swappable senses — Odysseus (the
//! brain), MyOwnLLM (the model engine), and an on-device ASR (the ears) —
//! behind one internal API, and renders the agent's intent as a "dissolved" UI.
//!
//! This crate is the Tauri-free heart of that shell, so it compiles and is
//! unit-tested without a webview (mirroring `myo-self-update`). The `myo` binary
//! wraps it in `#[tauri::command]`s and pumps its [`MyoEvent`] stream to the
//! WebView via `app.emit`; everything load-bearing — the brain's wire protocol,
//! the capability mapping, the converse round-trip — lives and is tested here.
//!
//! Map of the pieces:
//!
//! | Module | Role |
//! |---|---|
//! | [`event`] | the normalized `myo://` intent stream the UI renders |
//! | [`asr`] | the ears: POST captured audio to Myo's own engine (private `:11473`) |
//! | [`tts`] | the voice: POST reply text to that same engine → synthesized audio |
//! | [`engine`] | pinned-engine version/platform logic (self-heal a stale `myownllm`) |
//! | [`llm`] | the native brain: streaming chat straight from MyOwnLLM (no Odysseus) |
//! | [`brain`] | the Odysseus loopback client (multipart in, SSE → [`MyoEvent`] out) |
//! | [`capabilities`] | the 4 toggles ⇄ Odysseus's `allow_*` + `disabled_tools` |
//! | [`converse`] | one utterance→answer→voice turn |
//! | [`supervisor`] | engine launch specs + health + brain↔model wiring |
//! | [`config`] | the persisted `shell` section of `~/.myo/config.json` |

pub mod asr;
pub mod brain;
pub mod capabilities;
pub mod config;
pub mod converse;
pub mod engine;
pub mod event;
pub mod llm;
pub mod paths;
pub mod supervisor;
pub mod tools;
pub mod tts;

pub use asr::AsrClient;
pub use brain::{BrainClient, BrainConfig};
pub use capabilities::Capabilities;
pub use config::{ShellSettings, WebSearchConfig};
pub use converse::{run_turn, run_turn_native, TurnAllocator};
pub use event::{channel, Emit, MyoEvent, TurnId};
pub use llm::{ChatMessage, LlmClient, ToolCall, TurnOutcome, EMBED_MODEL, MYO_PERSONA};
pub use tools::WebSearch;
pub use tts::{TtsAudio, TtsClient};
