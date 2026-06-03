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
use std::sync::Arc;

use anyhow::Result;
use serde_json::{json, Value};
use tokio::sync::mpsc;
use tokio::task::JoinSet;

use crate::brain::BrainClient;
use crate::capabilities::Capabilities;
use crate::event::{MyoEvent, TurnId};
use crate::llm::{ChatMessage, LlmClient, ToolCall, TurnOutcome};
use crate::memory::{Memory, MemoryHit, RECALL_K, RECALL_MIN_SCORE};
use crate::tools::{self, Tool, ToolCtx, ToolResult, WebSearch, TOOL_PREAMBLE};
use crate::tts::TtsClient;

/// How many tool rounds a single turn may take before we force a final answer.
/// Generous — real agent chains (search → read → run → check) are expected — but
/// bounded so a loop that keeps calling tools always terminates with speech.
const MAX_TOOL_ROUNDS: u64 = 16;

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

/// Run one full converse turn **natively** — Myo's own memory-aware agent loop.
///
/// The turn weaves both memory layers together before the model runs: it embeds
/// the user's message, **recalls** the most relevant long-term memories (Layer 2)
/// and surfaces them to the UI, then assembles the context as persona + those
/// recalled memories + the working-memory window (Layer 1) + this turn. From
/// there it's the native tool loop: stream the reply, and if the model answers
/// with `tool_calls`, run them (concurrently, streaming each tool's progress
/// live — including `remember`/`recall`, which act on memory), feed the results
/// back, and continue until it produces a final spoken answer. The user turn and
/// the final reply are recorded into working memory; durable writes happen only
/// through the `remember` tool (paused under `incognito`).
///
/// There is no Odysseus in this path. `caps` gates which capability tools are
/// offered; `web`, `llm`, and `memory` are the shared clients the tools use.
/// Voicing matches the rest of the stack: engine synthesis
/// ([`MyoEvent::AudioReady`]) with a WebSpeech fallback ([`MyoEvent::AudioSpeak`]).
#[allow(clippy::too_many_arguments)]
pub async fn run_turn_native(
    llm: Arc<LlmClient>,
    tts: &TtsClient,
    web: Arc<WebSearch>,
    memory: Arc<Memory>,
    caps: Capabilities,
    incognito: bool,
    persona: String,
    user_text: String,
    turn: TurnId,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Result<String> {
    // ── Layer 2: recall the long-term memories most relevant to this turn, and
    // surface them (the UI's "recalled from memory" hint). Best-effort — a cold
    // engine just means no recall, never a failed turn.
    let recalled = recall_relevant(&llm, &memory, &user_text).await;
    if !recalled.is_empty() {
        emit(MyoEvent::Progress {
            turn,
            kind: "memories_used".into(),
            data: json!(recalled
                .iter()
                .map(|h| json!({ "text": h.text }))
                .collect::<Vec<_>>()),
        });
    }

    // ── Assemble the context from both layers: persona, recalled long-term
    // memories, the working-memory window (Layer 1), then this user turn.
    let mut convo: Vec<ChatMessage> = vec![ChatMessage::system(persona)];
    if !recalled.is_empty() {
        convo.push(ChatMessage::system(memory_note(&recalled)));
    }
    convo.extend(memory.working_window());
    convo.push(ChatMessage::user(&user_text));
    memory.record_user(&user_text); // working memory now holds this turn

    // ── Tools: offer the enabled kit, and tell the model they're real.
    let tool_kit = tools::registry(caps);
    let schemas = tools::tool_schemas(&tool_kit);
    if !tool_kit.is_empty() {
        let at = usize::from(convo.first().map(|m| m.role == "system").unwrap_or(false));
        convo.insert(at, ChatMessage::system(TOOL_PREAMBLE));
    }

    // ── The loop (recall/assembly above run once; this only iterates on tools).
    let mut round: u64 = 0;
    loop {
        // Offer tools only while rounds remain; on the final round force a plain
        // answer (no tools) so the turn always terminates with speech.
        let offer: &[Value] = if round < MAX_TOOL_ROUNDS {
            &schemas
        } else {
            &[]
        };
        match llm.chat_stream_tools(&convo, offer, turn, emit).await? {
            TurnOutcome::Message(text) => {
                emit(MyoEvent::AssistantDone { turn });
                let spoken = text.trim().to_string();
                memory.record_assistant(&spoken); // working memory keeps the reply
                if !spoken.is_empty() {
                    match tts.synthesize(&spoken, None).await {
                        Ok(audio) => emit(MyoEvent::AudioReady {
                            turn,
                            b64: audio.b64,
                            mime: audio.mime,
                        }),
                        Err(_) => emit(MyoEvent::AudioSpeak {
                            turn,
                            text: spoken.clone(),
                        }),
                    }
                }
                return Ok(spoken);
            }
            TurnOutcome::ToolCalls(calls) => {
                round += 1;
                // Record the assistant's tool-call turn so the model sees its own
                // request alongside the results we're about to append.
                convo.push(ChatMessage::assistant_calls(&calls));
                for (id, content) in run_calls(
                    &tool_kit, &llm, &web, &memory, incognito, turn, round, calls, emit,
                )
                .await
                {
                    convo.push(ChatMessage::tool(id, content));
                }
            }
        }
    }
}

/// Recall the long-term memories most relevant to `user_text` (embedding it via
/// the engine). Empty when the store is empty, the text is blank, or the embed
/// call fails — recall never breaks a turn.
async fn recall_relevant(llm: &LlmClient, memory: &Memory, user_text: &str) -> Vec<MemoryHit> {
    if user_text.trim().is_empty() || memory.long_term_len() == 0 {
        return Vec::new();
    }
    match llm
        .embed(std::slice::from_ref(&user_text.to_string()))
        .await
    {
        Ok(mut vectors) if !vectors.is_empty() => {
            memory.recall(&vectors.remove(0), RECALL_K, RECALL_MIN_SCORE)
        }
        _ => Vec::new(),
    }
}

/// The system note that folds recalled memories into a turn's context.
fn memory_note(hits: &[MemoryHit]) -> String {
    let mut s = String::from(
        "Here are some things you remember that may be relevant. Draw on them only when they \
         genuinely help, and don't announce that you're recalling them:\n",
    );
    for h in hits {
        s.push_str("- ");
        s.push_str(&h.text);
        s.push('\n');
    }
    s
}

/// Run one round's tool calls **concurrently**, streaming each tool's
/// intermittent output live, and return `(tool_call_id, result_text)` per call
/// in the model's original call order.
///
/// Concurrency seam: tools can't share the loop's single `&mut emit` closure, so
/// each runs as a task that streams progress over an mpsc channel; this function
/// multiplexes that channel onto `emit` while the tasks run, and emits the
/// `ActivityStart`/`ActivityOutput` pills itself so their ordering is stable.
#[allow(clippy::too_many_arguments)]
async fn run_calls(
    tool_kit: &[Arc<dyn Tool>],
    llm: &Arc<LlmClient>,
    web: &Arc<WebSearch>,
    memory: &Arc<Memory>,
    incognito: bool,
    turn: TurnId,
    round: u64,
    calls: Vec<ToolCall>,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Vec<(String, String)> {
    let (tx, mut rx) = mpsc::unbounded_channel::<MyoEvent>();
    let mut results: Vec<Option<(String, String)>> = vec![None; calls.len()];
    let mut set: JoinSet<(usize, String, String, ToolResult)> = JoinSet::new();

    for (i, call) in calls.into_iter().enumerate() {
        let args: Value = serde_json::from_str(&call.arguments).unwrap_or_else(|_| json!({}));
        match tools::find(tool_kit, &call.name) {
            // Disabled or unknown tool: refuse cleanly with a result the model can
            // read and recover from, rather than executing anything.
            None => {
                emit(MyoEvent::ActivityStart {
                    turn,
                    tool: call.name.clone(),
                    command: None,
                    round: Some(round),
                });
                let msg = format!(
                    "The tool '{}' is not available — its capability is turned off.",
                    call.name
                );
                emit(MyoEvent::ActivityOutput {
                    turn,
                    tool: call.name.clone(),
                    output: Some(msg.clone()),
                    exit_code: None,
                    image_url: None,
                    round: Some(round),
                });
                results[i] = Some((call.id, msg));
            }
            Some(tool) => {
                let args_for_headline = args.clone();
                emit(MyoEvent::ActivityStart {
                    turn,
                    tool: call.name.clone(),
                    command: tool.headline(&args_for_headline),
                    round: Some(round),
                });
                let ctx = ToolCtx {
                    turn,
                    round,
                    web: web.clone(),
                    llm: llm.clone(),
                    memory: memory.clone(),
                    incognito,
                    events: tx.clone(),
                };
                let name = call.name.clone();
                let id = call.id.clone();
                set.spawn(async move {
                    let result = match tool.execute(args, &ctx).await {
                        Ok(r) => r,
                        Err(e) => ToolResult {
                            text: format!("tool error: {e}"),
                            exit_code: None,
                        },
                    };
                    (i, id, name, result)
                });
            }
        }
    }
    // Drop our own sender so the channel closes once every task (which holds a
    // clone) has finished and dropped its `ToolCtx`.
    drop(tx);

    // Multiplex: forward progress as it streams, collect results as tasks finish.
    loop {
        tokio::select! {
            biased;
            joined = set.join_next(), if !set.is_empty() => {
                if let Some(Ok((i, id, name, result))) = joined {
                    emit(MyoEvent::ActivityOutput {
                        turn,
                        tool: name,
                        output: Some(result.text.clone()),
                        exit_code: result.exit_code,
                        image_url: None,
                        round: Some(round),
                    });
                    results[i] = Some((id, result.text));
                }
            }
            ev = rx.recv() => {
                match ev {
                    Some(ev) => emit(ev),
                    None if set.is_empty() => break,
                    None => {}
                }
            }
        }
    }
    // Belt-and-suspenders: drain any progress buffered right before close.
    while let Ok(ev) = rx.try_recv() {
        emit(ev);
    }

    results.into_iter().map(|r| r.unwrap_or_default()).collect()
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

    use crate::config::WebSearchConfig;
    use crate::memory::Memory;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// A path-aware loopback engine: answers `/v1/models` and `/v1/embeddings`
    /// automatically (the embedding is a fixed 3-dim unit vector), and serves the
    /// scripted SSE bodies in order for each `/v1/chat/completions` — enough to
    /// drive a multi-round, memory-aware tool loop end to end.
    async fn serve_engine(chat_bodies: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut chats = chat_bodies.into_iter();
            loop {
                let (mut sock, _) = match listener.accept().await {
                    Ok(x) => x,
                    Err(_) => break,
                };
                let mut buf = vec![0u8; 16384];
                let n = sock.read(&mut buf).await.unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]).to_string();
                let resp = if req.contains("/v1/models") {
                    let body = "{\"data\":[{\"id\":\"m\"}]}";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else if req.contains("/v1/embeddings") {
                    let body = "{\"data\":[{\"index\":0,\"embedding\":[1.0,0.0,0.0]}]}";
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                        body.len()
                    )
                } else {
                    let body = chats.next().unwrap_or("data: [DONE]\n\n");
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                    )
                };
                let _ = sock.write_all(resp.as_bytes()).await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    fn deps(base: String) -> (Arc<LlmClient>, TtsClient, Arc<WebSearch>, Arc<Memory>) {
        (
            Arc::new(LlmClient::new(base).unwrap()),
            // TTS points at a dead port so synthesis fails fast → WebSpeech fallback.
            TtsClient::new("http://127.0.0.1:9").unwrap(),
            Arc::new(WebSearch::new(WebSearchConfig::Ddg).unwrap()),
            Arc::new(Memory::in_memory().unwrap()),
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_loop_runs_a_call_then_answers() {
        // Round 1: the model asks to run `echo myo`. Round 2: it answers.
        let base = serve_engine(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"echo myo\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"All set.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ])
        .await;
        let (llm, tts, web, memory) = deps(base);
        let caps = Capabilities {
            web: false,
            files: false,
            code: true,
            reach_out: false,
        };

        let mut events = Vec::new();
        let reply = {
            let mut emit = |ev: MyoEvent| events.push(ev);
            run_turn_native(
                llm.clone(),
                &tts,
                web,
                memory.clone(),
                caps,
                false,
                "You are Myo.".into(),
                "say myo".into(),
                1,
                &mut emit,
            )
            .await
            .unwrap()
        };

        assert_eq!(reply, "All set.");

        // The tool ran: a start, live progress carrying the output, and an output.
        assert!(events.iter().any(|e| matches!(
            e,
            MyoEvent::ActivityStart { tool, .. } if tool == "shell"
        )));
        let progress_has_output = events.iter().any(|e| {
            matches!(
                e,
                MyoEvent::ActivityProgress { progress: Some(p), .. } if p.contains("myo")
            )
        });
        assert!(
            progress_has_output,
            "shell output should stream as progress"
        );
        assert!(events.iter().any(|e| matches!(
            e,
            MyoEvent::ActivityOutput { tool, output: Some(o), .. }
                if tool == "shell" && o.contains("myo")
        )));
        // The turn terminated with exactly one AssistantDone (the final round).
        let dones = events
            .iter()
            .filter(|e| matches!(e, MyoEvent::AssistantDone { .. }))
            .count();
        assert_eq!(dones, 1);

        // Working memory (Layer 1) captured both sides of the turn.
        assert_eq!(memory.working_window().len(), 2);
    }

    #[tokio::test]
    async fn disabled_tool_is_refused_not_run() {
        // The model calls `shell` but Code is off → it's refused, and the model
        // gets a tool result it can read, then answers.
        let base = serve_engine(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"rm -rf /\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"I can't run that.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ])
        .await;
        let (llm, tts, web, memory) = deps(base);
        // All four toggles off (memory tools are still offered, but the model
        // calls shell, which isn't in the kit).
        let caps = Capabilities {
            web: false,
            files: false,
            code: false,
            reach_out: false,
        };

        let mut events = Vec::new();
        let reply = {
            let mut emit = |ev: MyoEvent| events.push(ev);
            run_turn_native(
                llm.clone(),
                &tts,
                web,
                memory,
                caps,
                false,
                "You are Myo.".into(),
                "delete everything".into(),
                1,
                &mut emit,
            )
            .await
            .unwrap()
        };

        assert_eq!(reply, "I can't run that.");
        // It was refused: the output says "not available", never a real exit code.
        assert!(events.iter().any(|e| matches!(
            e,
            MyoEvent::ActivityOutput { output: Some(o), exit_code: None, .. }
                if o.contains("not available")
        )));
    }

    #[tokio::test]
    async fn recalls_relevant_memory_and_emits_hint() {
        // One scripted answer; recall runs first (the engine auto-answers the
        // embedding as the unit vector [1,0,0]).
        let base = serve_engine(vec![
            "data: {\"choices\":[{\"delta\":{\"content\":\"Noted.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ])
        .await;
        let (llm, tts, web, memory) = deps(base);
        // Seed a long-term memory whose embedding matches what the engine returns.
        memory
            .remember("the user loves sailing", "preference", vec![1.0, 0.0, 0.0])
            .unwrap();

        let caps = Capabilities::default();
        let mut events = Vec::new();
        {
            let mut emit = |ev: MyoEvent| events.push(ev);
            run_turn_native(
                llm.clone(),
                &tts,
                web,
                memory,
                caps,
                false,
                "You are Myo.".into(),
                "what do you know about me?".into(),
                1,
                &mut emit,
            )
            .await
            .unwrap();
        }

        // The recall surfaced as a `memories_used` progress event carrying the hit.
        let recalled = events.iter().any(|e| {
            matches!(
                e,
                MyoEvent::Progress { kind, data, .. }
                    if kind == "memories_used"
                        && data.to_string().contains("loves sailing")
            )
        });
        assert!(recalled, "relevant memory should be recalled and surfaced");
    }
}
