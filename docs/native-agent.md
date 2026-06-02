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
| Short-term context (session) | `MyoState::history` (+ `chat_context`) | ✅ done |
| Long-term memory + recall (SQLite + embeddings; ChromaDB degrades) | `memory` module (SQLite + `/v1/embeddings`) | ⏳ Slice 2 |
| Tool loop (web/files/code/reach-out) + capability gating | `tools` module + the 4 toggles | ⏳ Slice 4 |
| TTS provider | native TTS → `AudioReady` (today: WebSpeech `AudioSpeak`) | ⏳ Slice 3 |
| Scheduling / proactive ("reach out") | later | ⏳ |

## Roadmap (slices)

1. **Native conversation — DONE.** `LlmClient` streams `/v1/chat/completions`
   (OpenAI SSE → `AssistantDelta`/`AssistantDone`); `run_turn_native` voices the
   reply (WebSpeech fallback for now). `MyoState` keeps the running history
   (persona prepended per turn). The Tauri turn path (`spawn_text_turn`) uses it
   — **a conversation now needs only MyOwnLLM, no Odysseus.**
2. **Memory.** A local store (SQLite under `~/.myo`) of durable memories + recall
   via MyOwnLLM `/v1/embeddings`; inject the top hits into `chat_context`.
   Incognito pauses writes. (Reference: Odysseus `services/` memory + RAG.)
3. **Native TTS.** Replace the WebSpeech fallback with on-device synthesis →
   emit `AudioReady{b64,mime}` (the UI already plays it). Keep WebSpeech as the
   last-resort fallback.
4. **Tools + capability gating.** A native tool-call loop (the model proposes a
   tool, Myo runs it, feeds the result back), wired to the existing four toggles
   (`web`/`files`/`code`/`reach_out`). Emit `activity`/`artifact` events (the UI
   renders them already). (Reference: Odysseus `mcp_servers/` + `routes/`.)
5. **Hive.** `myownmesh-core`: installs discover each other and share data
   (memories, presence) as a hive. Per-device identity + roster already exist in
   the substrate.

## Decisions
- **Inference + ASR stay in MyOwnLLM** (one place owns models per device); Myo
  talks to it over loopback HTTP/WS on the private port.
- **The UI contract is unchanged** — the native brain emits the same `myo://`
  events the Odysseus client did, so surfaces didn't move.
- **History is in-process for now** (Slice 1); it becomes durable + embedded in
  Slice 2 (memory).
- **`BrainClient`/Odysseus startup is not yet removed** — the conversation no
  longer uses it; capabilities/memory panels still call it (and degrade) until
  Slices 2/4 port them. Removing the Odysseus supervisor + client is a cleanup
  once those land.

## File map (new/changed this slice)
| Area | File |
|---|---|
| Native brain (chat streaming + persona) | `crates/myo-core/src/llm.rs` |
| Native turn (stream → voice) | `crates/myo-core/src/converse.rs` (`run_turn_native`) |
| History + context assembly | `src-tauri/src/state.rs` (`chat_context`/`record_reply`) |
| Turn command (native path) | `src-tauri/src/core_api.rs` (`spawn_text_turn`) |
