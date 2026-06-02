# Myo, the native agent — direction & roadmap

**The pivot (2026-06-02):** Myo stops *running* Odysseus as a brain sidecar.
Instead, **Myo is the agent** — memory, the tool loop, TTS, and real-time
interactivity are built directly into Myo (Rust). MyOwnLLM stays a service
(per-device model selection, inference hosting, transcription/ASR). **Odysseus
becomes a reference** — we read its code to learn *how* it does a thing, then
reimplement the parts we want natively. No freezing, no sideloading, no `:17000`.

Each install is its own self-contained **mini-myo**; installs network over the
mesh and share data as a **hive-myo** (the `myownmesh-core` substrate MyOwnLLM
already uses).

## Why (not freeze / not sideload)
Odysseus is a Python/FastAPI app with its own service stack (ChromaDB, SearXNG,
ntfy). Freezing it per-platform or shipping a venv is fragile and heavy, and it
keeps Myo as "an app with an AI inside it." Building the features we actually
want into Myo makes each install small, owned, and hive-ready — and it's the
PLAN's own thesis: *Myo is the AI, not a shell around someone else's brain.*

## Architecture now

```
        ┌─────────────────────────── Myo (Rust + Svelte) ──────────────────────────┐
        │  native brain (llm.rs)   memory   tools   TTS   real-time   hive (mesh)   │
        └───────────────┬───────────────────────────────────────────────┬──────────┘
                        │ OpenAI HTTP (chat, embeddings)                  │ mesh
                        ▼                                                 ▼
                 MyOwnLLM  (model selection · inference · ASR)      other mini-myos
```

- **MyOwnLLM** — owned already (pinned bundle + runtime self-heal + private
  `:11473`). Provides `/v1/chat/completions`, `/v1/embeddings`, `/v1/audio/*`.
- **Myo** — the agent loop and everything stateful/interactive, in-process.
- **Odysseus** — reference only. Not launched. (The `BrainClient` lingers for the
  memory/capabilities panels until those are ported, then it's deleted.)

## Odysseus → Myo feature map (what to harvest, and where it lands)

| Odysseus feature (reference) | Myo native home | Status |
|---|---|---|
| Agent turn / chat streaming | `crates/myo-core/src/llm.rs` (`LlmClient`) | ✅ **done (Slice 1)** |
| Persona / system prompt | `llm::MYO_PERSONA` | ✅ done |
| Short-term context (session) | working memory (`memory::WorkingMemory`) | ✅ **done (Slice 2)** |
| Long-term memory + recall (SQLite + embeddings) | `memory` module (`LongTermMemory`: SQLite + `/v1/embeddings`) | ✅ **done (Slice 2)** |
| Tool loop (web/files/code) + capability gating | `tools` module + the 4 toggles | ✅ **done (Slice 4)** — reach-out + deep research still ⏳ |
| TTS provider | engine TTS via `/v1/audio/speech` → `AudioReady`, WebSpeech fallback | ✅ **done (Slice 3)** |
| Scheduling / proactive ("reach out") | later | ⏳ |

## Roadmap (slices)

1. **Native conversation — DONE.** `LlmClient` streams `/v1/chat/completions`
   (OpenAI SSE → `AssistantDelta`/`AssistantDone`); `run_turn_native` voices the
   reply (WebSpeech fallback for now). `MyoState` keeps the running history
   (persona prepended per turn). The Tauri turn path (`spawn_text_turn`) uses it
   — **a conversation now needs only MyOwnLLM, no Odysseus.**
2. **Memory — DONE.** A layered system in `crates/myo-core/src/memory/`: **Layer 1
   — working memory** (`WorkingMemory`, the recent conversation, volatile and
   bounded) and **Layer 2 — long-term memory** (`LongTermMemory`, durable
   facts/preferences in SQLite under `~/.myo/memory.db`, recalled by cosine over
   the local `/v1/embeddings`). Each turn (`run_turn_native`) embeds the user's
   message, recalls the top hits (emitting `memories_used`), and assembles
   persona + recalled memories + the working window + the turn. Myo writes
   durably through the `remember` tool (and can `recall` on purpose); **incognito
   pauses writes**; the Memory panel (`myo_memory_list`/`myo_memory_forget`) lists
   and forgets. Automatic salience-based capture is a future extension.
3. **Native TTS — DONE.** `TtsClient` POSTs reply text to MyOwnLLM's
   `/v1/audio/speech` (the hardware-tiered voice — Kokoro/Piper, picked
   engine-side); `run_turn_native` emits `AudioReady{b64,mime}` (the UI plays
   it) on success and degrades to WebSpeech `AudioSpeak` otherwise. Pinned to
   `.myownllm-rev` 0.2.27 — the engine that ships real synthesis (Kokoro/Piper
   ONNX forward) plus a self-installing espeak-ng phonemizer, so end-to-end
   audio is live with no system dependency. WebSpeech stays the permanent
   last-resort tier for when the engine errors or is unreachable.
4. **Tools + capability gating — DONE (web/files/code).** A native tool-call loop
   lives in `crates/myo-core/src/tools/` + `converse::run_turn_native`: the model
   proposes tools, Myo runs them (a round's calls run **concurrently**, each
   streaming `ActivityProgress` live), feeds results back, and repeats until it
   answers. Tools — `shell` (Code), `read_file`/`write_file` (Files), `web_search`
   (Web, keyless DuckDuckGo by default, SearXNG-configurable via
   `ShellSettings::web_search`) — are gated by the existing toggles: `registry`
   only offers enabled categories and a `find` backstop refuses anything off.
   `LlmClient::chat_stream_tools` assembles streamed OpenAI `tool_calls` (handling
   fragmented and whole-object arguments, and parallel calls). Reach-out and a
   multi-step deep-research tool are the remaining ⏳ extensions (the registry is
   built to slot them in). Needs a tool-capable served model (e.g. Qwen3.x);
   otherwise the loop degrades cleanly to plain chat.
5. **Hive.** `myownmesh-core`: installs discover each other and share data
   (memories, presence) as a hive. Per-device identity + roster already exist in
   the substrate.

## Decisions
- **Inference + ASR stay in MyOwnLLM** (one place owns models per device); Myo
  talks to it over loopback HTTP/WS on the private port.
- **The UI contract is unchanged** — the native brain emits the same `myo://`
  events the Odysseus client did, so surfaces didn't move.
- **Memory is layered** (Slice 2): working memory (volatile, recent) +
  long-term memory (durable SQLite + embedded recall). The conversation history
  is no longer an ad-hoc `Vec` on `MyoState` — it's `memory::WorkingMemory`, and
  the turn assembles context from both layers.
- **`BrainClient`/Odysseus startup is not yet removed** — the conversation,
  tools, and memory panels are all native now; the lingering `BrainClient`
  (engine status, capability `disabled_tools`) is the last thing to retire.
  Removing the Odysseus supervisor + client is a remaining cleanup.

## File map (new/changed this slice)
| Area | File |
|---|---|
| Native brain (chat streaming + persona) | `crates/myo-core/src/llm.rs` |
| Native turn (stream → voice) | `crates/myo-core/src/converse.rs` (`run_turn_native`) |
| Memory (working + long-term layers) | `crates/myo-core/src/memory/` |
| Turn context assembly + recall | `crates/myo-core/src/converse.rs` (`run_turn_native`) |
| Turn command (native path) | `src-tauri/src/core_api.rs` (`spawn_text_turn`) |
