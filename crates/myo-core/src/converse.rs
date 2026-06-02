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

/// Run one full converse turn **natively** — Myo's own agent loop.
///
/// This is the native tool loop: stream the model's reply from MyOwnLLM, and if
/// it answers with `tool_calls` instead of prose, run them (concurrently,
/// streaming each tool's progress live), feed the results back, and let it
/// continue — repeating until it produces a final spoken answer, which is then
/// voiced and returned for the history.
///
/// There is no Odysseus in this path: `convo` is the whole context (persona +
/// history + this user turn) the caller assembled, owned so the loop can append
/// the intermediate tool-call / tool-result messages without touching long-term
/// history (only the returned final answer is recorded by the caller). `caps`
/// gates which tools are offered, and `web` is the shared search client a
/// `web_search` call uses.
///
/// Voicing matches the rest of the stack: try the engine's own synthesis
/// ([`TtsClient::synthesize`] → [`MyoEvent::AudioReady`]) and fall back to
/// WebSpeech ([`MyoEvent::AudioSpeak`]) when it can't.
pub async fn run_turn_native(
    llm: &LlmClient,
    tts: &TtsClient,
    web: Arc<WebSearch>,
    caps: Capabilities,
    mut convo: Vec<ChatMessage>,
    turn: TurnId,
    emit: &mut (dyn FnMut(MyoEvent) + Send),
) -> Result<String> {
    let tool_kit = tools::registry(caps);
    let schemas = tools::tool_schemas(&tool_kit);

    // When any tool is on, slip a tool-aware note in right after the persona so
    // the model knows the tools are real and that tool arguments are exempt from
    // the voice-only writing rules.
    if !tool_kit.is_empty() {
        let at = usize::from(convo.first().map(|m| m.role == "system").unwrap_or(false));
        convo.insert(at, ChatMessage::system(TOOL_PREAMBLE));
    }

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
                for (id, content) in
                    run_calls(&tool_kit, web.clone(), turn, round, calls, emit).await
                {
                    convo.push(ChatMessage::tool(id, content));
                }
            }
        }
    }
}

/// Run one round's tool calls **concurrently**, streaming each tool's
/// intermittent output live, and return `(tool_call_id, result_text)` per call
/// in the model's original call order.
///
/// Concurrency seam: tools can't share the loop's single `&mut emit` closure, so
/// each runs as a task that streams progress over an mpsc channel; this function
/// multiplexes that channel onto `emit` while the tasks run, and emits the
/// `ActivityStart`/`ActivityOutput` pills itself so their ordering is stable.
async fn run_calls(
    tool_kit: &[Arc<dyn Tool>],
    web: Arc<WebSearch>,
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
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    /// Loopback server that answers a `/v1/models` lookup once, then replies to
    /// each subsequent chat request with the next scripted stream body — enough
    /// to drive a multi-round tool loop end to end.
    async fn serve_script(bodies: Vec<&'static str>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            let mut buf = [0u8; 8192];
            // 1) /v1/models
            let (mut sock, _) = listener.accept().await.unwrap();
            let _ = sock.read(&mut buf).await;
            let models = "{\"data\":[{\"id\":\"m\"}]}";
            let _ = sock
                .write_all(
                    format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{models}",
                        models.len()
                    )
                    .as_bytes(),
                )
                .await;
            let _ = sock.flush().await;
            // 2..) one chat stream per scripted body.
            for body in bodies {
                let (mut sock, _) = listener.accept().await.unwrap();
                let _ = sock.read(&mut buf).await;
                let _ = sock
                    .write_all(
                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/event-stream\r\nConnection: close\r\n\r\n{body}"
                        )
                        .as_bytes(),
                    )
                    .await;
                let _ = sock.flush().await;
            }
        });
        format!("http://{addr}")
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn tool_loop_runs_a_call_then_answers() {
        // Round 1: the model asks to run `echo myo`. Round 2: it answers.
        let base = serve_script(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"echo myo\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"All set.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ])
        .await;

        let llm = LlmClient::new(base).unwrap();
        // TTS points at a dead port so synthesis fails fast → WebSpeech fallback.
        let tts = TtsClient::new("http://127.0.0.1:9").unwrap();
        let web = Arc::new(WebSearch::new(WebSearchConfig::Ddg).unwrap());
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
                &llm,
                &tts,
                web,
                caps,
                vec![ChatMessage::user("say myo")],
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
    }

    #[tokio::test]
    async fn disabled_tool_is_refused_not_run() {
        // The model calls `shell` but Code is off → it's refused, and the model
        // gets a tool result it can read, then answers.
        let base = serve_script(vec![
            "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"c1\",\"function\":{\"name\":\"shell\",\"arguments\":\"{\\\"command\\\":\\\"rm -rf /\\\"}\"}}]}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"tool_calls\"}]}\n\n\
             data: [DONE]\n\n",
            "data: {\"choices\":[{\"delta\":{\"content\":\"I can't run that.\"}}]}\n\n\
             data: {\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n\
             data: [DONE]\n\n",
        ])
        .await;

        let llm = LlmClient::new(base).unwrap();
        let tts = TtsClient::new("http://127.0.0.1:9").unwrap();
        let web = Arc::new(WebSearch::new(WebSearchConfig::Ddg).unwrap());
        // All tools off.
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
                &llm,
                &tts,
                web,
                caps,
                vec![ChatMessage::user("delete everything")],
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
}
