# Myo — Documentation

> **Myo *is* the agent — not an app with an AI inside it.** A voice-first
> (touch-second, type-third) local companion whose **brain, memory, tools, and
> voice are native** (Rust), with **the interface dissolved into the
> conversation**: you mostly just talk and *watch the agent work*. It runs one
> local service — **MyOwnLLM** — for model selection, inference, and speech.
> Everything runs and stays **local**.

Myo builds the stateful, interactive parts of an agent — the agent loop, memory,
the tool loop, TTS, real-time turn-taking — **directly in Rust**, and talks to
**[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM)** over loopback HTTP for
inference, embeddings, and speech (ASR + TTS). **Odysseus** (a separate Python
agent) is a **code reference** — we read it to learn *how* to do a thing, then
reimplement it natively; it is never run. Each install is a self-contained
**mini-myo**; installs network over a local mesh and share as a **hive**.

## 📚 Docs

| File | What it is |
|---|---|
| **[`native-agent.md`](native-agent.md)** | **Start here.** The canonical direction + roadmap — what's native today (conversation, voice, TTS, memory + Dream mode, the tool loop), the Odysseus→Myo feature map, and the remaining slices (reach-out, hive). |
| **[`shell.md`](shell.md)** | What's wired **right now** — the Core API, the `myo://` event stream, how to run it, and the bundled/self-healing MyOwnLLM sidecar. |
| **[`voice-handoff.md`](voice-handoff.md)** | The voice loop in depth — open-mic streaming dictation + clip fallback, the streaming WS protocol, full-duplex/barge-in, and the open refinements. |
| **[`decisions-and-rationale.md`](decisions-and-rationale.md)** | *Why* Myo is shaped this way — the native-agent pivot and the product decisions (voice-first, four-toggle control, single evolving stage, continuous presence), plus what was rejected. |
| **[`myownllm-integration.md`](myownllm-integration.md)** | The **MyOwnLLM** engine reference — the OpenAI-compatible server Myo calls, the ASR/voice routes, the supervision + Tauri-bundling + per-arch onnxruntime patterns, and the (deferred) in-process `myo-asr` extraction. |
| **[`myownmesh-v2.md`](myownmesh-v2.md)** | The mesh reference for the **hive** (Slice 5) — embedded-lib + control-socket IPC, 6-char pairing, capability advertisement, and how MyOwnLLM embeds the daemon. |
| **[`auto-update.md`](auto-update.md)** | The self-updater — release feed, SHA-256 verify, atomic swap, the background watcher and the Updates panel. |

## The shape of it

- **Brain** — native ([`crates/myo-core/src/llm.rs`](../crates/myo-core/src/llm.rs)):
  streams MyOwnLLM `/v1/chat/completions` into a normalized `myo://` intent
  stream; persona (`MYO_PERSONA`) + running history.
- **Ears** — open-mic **streaming dictation** (and a clip fallback) over
  MyOwnLLM's ASR routes; captured in the WebView, transcribed by the engine Myo
  owns.
- **Voice** — **native TTS** via MyOwnLLM `/v1/audio/speech` (hardware-tiered
  Kokoro/Piper), WebSpeech as the last-resort fallback.
- **Memory** — native, **done** (Slice 2): a **two-layer** system — working
  memory (recent conversation) + a durable long-term store (SQLite + embeddings
  recall) — woven into every turn, plus **Dream mode** that consolidates aging
  memories into receding tiers during downtime and conservatively forgets.
- **Tools** — native, **done** (Slice 4): a streaming, parallel tool loop —
  `shell`, file read/write, keyless **web search**, and `remember`/`recall` —
  gated by the four Web/Files/Code toggles. Reach-out + deep research are next.
- **Hive** — multi-device over **MyOwnMesh** (Slice 5): mini-myos discover each
  other and share data.
- **Engine** — MyOwnLLM, **owned**: a pinned sidecar ([`.myownllm-rev`](../.myownllm-rev))
  bundled per-triple by [`build.rs`](../src-tauri/build.rs), self-healing to the
  pin at launch, on a private port.

## The product decisions (settled with the owner)

1. **Myo is the agent** — brain/memory/tools/voice are native; MyOwnLLM is the model + speech engine; Odysseus is a read-only reference.
2. **Four-category control** — Web / Files / Code / Reach-out toggles. Per-action approve/tweak/edit is the next layer.
3. **Open-mic + barge-in** — full-duplex with echo cancellation; hard-mute always one tap away.
4. **Single evolving stage + history** — Presence orb + one focal Stage + ambient ActivityStrip + recallable History; surfaces are voice-addressable.
5. **Continuous presence** — automatic local memory across days; visible, forgettable, pausable.

See [`decisions-and-rationale.md`](decisions-and-rationale.md) for the full reasoning and what was rejected.

## Platforms

Five target-triples — `windows-x86_64`, `linux-x86_64`, `linux-aarch64`,
`macos-x86_64`, `macos-aarch64` (Windows x64 only). CI is a 5-target matrix; AEC
differs per webview (CoreAudio / WebKitGTK / WebView2).
