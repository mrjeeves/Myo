# The voice loop — how it works

How the voice path (ears → native brain → voice) is wired, the streaming
protocol, and the refinements still open. For the broader shell see
[`shell.md`](shell.md); for the direction, [`native-agent.md`](native-agent.md).

## TL;DR

Voice runs **end-to-end** in **two modes**:

1. **Real-time streaming dictation + full-duplex** (the primary path) — the
   WebView streams 16 kHz PCM to the engine's live WebSocket, renders **interim**
   captions as you speak, fires the **native brain** turn on each **final**, and
   keeps listening through Myo's own reply so you can **barge in**. It lights up
   against a `myownllm` that serves `/v1/audio/stream` — guaranteed by the pin
   (`.myownllm-rev`).
2. **Clip-at-a-time** (the fallback) — always-on mic → energy-VAD utterance →
   one-shot transcription → turn. Used automatically when the streaming socket
   can't be reached (engine too old / unreachable).

Both transcribe via the **MyOwnLLM engine Myo owns** on a private port, then run
the same native brain → TTS turn.

## What works now

- **Streaming dictation (primary)** — [`StreamingListener`](../src/lib/audio-io.ts)
  opens the engine's live WebSocket (URL from the `myo_asr_stream_url` command →
  [`supervisor::myownllm_stream_url`](../crates/myo-core/src/supervisor.rs)),
  captures the mic via Web Audio, **resamples to 16 kHz mono** and streams i16-LE
  PCM continuously. Down-frames drive a live **interim** caption (a ghost bubble
  in [`Conversation.svelte`](../src/surfaces/Conversation.svelte) via
  `myo.liveTranscript`); each **final** runs the brain turn through the proven
  `api.say` text path ([`stage.svelte.ts`](../src/lib/stage.svelte.ts)
  `onInterim`/`onFinal`). **Full-duplex:** the mic is never gated during a reply,
  so a final mid-reply **barges in** (cancels + restarts). Falls back to the clip
  path if the socket can't be reached.
- **Always-on open-mic capture** in the WebView — `getUserMedia`
  (echo-cancellation). The clip fallback ([`audio-io.ts`](../src/lib/audio-io.ts)
  `Listener`) segments utterances with a lightweight energy VAD and is
  half-duplex (mic **gated** while Myo thinks/speaks). Mic button = one-tap hard
  mute ([`Composer.svelte`](../src/surfaces/Composer.svelte)).
- **Clip transcription (fallback)** — each finished utterance is base64'd to
  `myo_converse_feed_audio` ([`core_api.rs`](../src-tauri/src/core_api.rs)),
  which POSTs it to the engine's `POST /v1/audio/transcriptions`
  ([`AsrClient`](../crates/myo-core/src/asr.rs)); the transcript runs the same
  native brain → TTS turn as text (`spawn_text_turn`). Audio is transient
  (nothing kept).
- **Myo owns its engine on a PRIVATE port** — MyOwnLLM on **`:11473`**, *not* the
  shared `:1473` ([`supervisor.rs`](../crates/myo-core/src/supervisor.rs)). The
  fix for the 404 (stale engine): Myo never attaches to a user's own / orphaned
  instance.
- **Bundled, pinned engine + runtime self-heal** — [`.myownllm-rev`](../.myownllm-rev)
  pins the MyOwnLLM release; [`build.rs`](../src-tauri/build.rs) stages it into
  `binaries/myownllm-<triple>` (override → sibling checkout → GitHub release
  download → stub), and [`engine_update`](../src-tauri/src/engine_update.rs)
  fetches the pinned release at launch if the resolved binary is older than the
  pin (`MYOWNLLM_PINNED_REV`). Pure version/platform logic in
  [`engine.rs`](../crates/myo-core/src/engine.rs) (unit-tested). Full resolution
  order: [`shell.md`](shell.md).
- **Mic permission** — Windows WebView2 via `additionalBrowserArgs`
  (`--use-fake-ui-for-media-stream`) in
  [`tauri.conf.json`](../src-tauri/tauri.conf.json); macOS
  `NSMicrophoneUsageDescription` in [`Info.plist`](../src-tauri/Info.plist).

## Streaming dictation + full-duplex

The engine's `GET /v1/audio/stream` drives the same live pipeline the MyOwnLLM
app uses (Silero VAD + LocalAgreement-2), the model kept **warm** for the
connection, emitting **interim** captions as you speak + a **final** per
utterance. The Myo client (`StreamingListener` + the store wiring) talks to it.

**Streaming WS protocol** (the contract the client implements — keep in sync if
the engine changes):
- Connect: `ws://127.0.0.1:11473/v1/audio/stream` (loopback, tokenless — Myo
  owns the engine; `check_auth` is a no-op with no bearer token, and browsers
  can't set WS handshake headers anyway).
- Up: **binary** frames of **16 kHz mono i16 LE PCM**. End with a text `"end"`
  (or just close). The client resamples capture-rate → 16 kHz and packs LE.
- Down: JSON `TranscribeFrame` per frame. **`segments[].partial: true` =
  interim** (live caption); **absent/false = finalized** (serde skips `false`).
  `seg_id` is stable per utterance (interim→final replaces in place). The
  top-level **`final`** key (serde-renamed from `is_final`) is the *session* end,
  not a per-utterance final. `status` carries subtitles; `{ "error": … }` frames
  signal a fatal engine error (it then closes).

The warm model (the session loads once) removes the per-utterance reload latency
the clip path has.

## Refinements still open

1. **Verify on-device** — the streaming loop end-to-end (interim captions firming
   into turns), and **tune full-duplex**: echo-cancellation is supposed to keep
   Myo from transcribing her own TTS while she speaks. If AEC proves weak (she
   barges in on herself), the one-line degrade is to gate sends during playback —
   `streamer.setGated(true)` on `speaking` / `false` on idle (the lever is
   already on `StreamingListener`; the store deliberately doesn't pull it).
2. **AudioWorklet** instead of the deprecated `ScriptProcessorNode` (both capture
   paths still use it), and a browser **Silero VAD** for the clip path.
3. **Linux WebKitGTK mic** — an `enable-media-stream` + permission-grant hook in
   the setup (Windows/macOS handled). Small follow-up.

## Decisions already made (don't re-litigate)

- **Transcription rides the engine's HTTP/WS, not an in-process `myo-asr` crate.**
  The native brain already needs MyOwnLLM as an OpenAI HTTP endpoint, so the
  engine `serve` exists regardless; a loopback **WebSocket** is the right "IPC
  bridge" because the audio is captured in the WebView, which speaks WS natively
  (a Unix socket would force a pointless Rust audio round-trip). The in-process
  `myo-asr` extraction stays **deferred** (see
  [`myownllm-integration.md`](myownllm-integration.md) §1) — only needed if Myo
  ever wants ASR fully decoupled from the sidecar.
- **Myo owns its engine on a private port** (never attach to a foreign instance).
- **`keep_audio` is off** — always listening, nothing stored.

## Gotchas / known follow-ups

- **Closing Myo closes the engine it started.** A Tauri `RunEvent::Exit` hook
  tears down the spawned engine (`MyoState::shutdown` → kill-on-Drop), so closing
  the window brings the stack down with it. On **Windows** the engine is assigned
  to a kill-on-close **Job Object** (a crash/taskkill still reaps it, and the
  chain cascades — `myownllm` taking its own `myownmesh` down), plus a
  **parent-PID watchdog** so a `just dev` Ctrl-C — which a GUI app never receives
  — makes Myo exit and clean up ([`src-tauri/src/windows.rs`](../src-tauri/src/windows.rs)).
  Myo only ever closes an engine it *spawned*, never one it attached to (so a
  user's own MyOwnLLM is left alone — multi-instance friendly). *Remaining:* the
  macOS/Linux dev-Ctrl-C path still leans on the terminal's process group (no
  watchdog there yet).
- **One-shot `/v1/audio/transcriptions` rebuilds the model per call** — fine for
  the clip path, but the streaming WS avoids it entirely (warm session).
- **Full-duplex relies on echo-cancellation.** The streaming path never gates the
  mic during a reply (that's the point — barge-in). If a machine's AEC is weak,
  Myo could transcribe her own TTS and barge in on herself; the documented degrade
  is `streamer.setGated(true)` while `speaking` (see "Refinements" #1).
- **Streaming falls back to clip, not the other way.** `startListening` tries the
  WS first; on a persistent connect failure `StreamingListener` gives up (bounded
  reconnect) and the store switches to the clip `Listener`. A too-old engine (no
  `/v1/audio/stream`) degrades gracefully — but the pin is what guarantees the
  route exists in the bundled engine.

## File map

| Area | File |
|---|---|
| Native brain (chat SSE → `myo://`) | `crates/myo-core/src/llm.rs` |
| ASR client (HTTP `/v1/audio/transcriptions`) | `crates/myo-core/src/asr.rs` |
| TTS client (`/v1/audio/speech`) | `crates/myo-core/src/tts.rs` |
| Ports, specs, stream URL, supervision | `crates/myo-core/src/supervisor.rs` |
| One turn (ASR → brain → TTS) | `crates/myo-core/src/converse.rs` |
| Tauri commands (`asr_stream_url`, `feed_audio`, `say`, …) | `src-tauri/src/core_api.rs` |
| Turn / history state (persona + context) | `src-tauri/src/state.rs` |
| Process supervisor (spawn/own engine, warm-up) | `src-tauri/src/supervisor.rs` |
| Engine bundling + self-heal | `src-tauri/build.rs`, `src-tauri/src/engine_update.rs`, `.myownllm-rev` |
| Mic capture — `StreamingListener` (WS) + `Listener` (clip) + TTS playback | `src/lib/audio-io.ts` |
| The reactive store / turn lifecycle / streaming handlers | `src/lib/stage.svelte.ts` |
| Live-caption ghost bubble + warming status | `src/surfaces/Conversation.svelte`, `Presence.svelte` |
| Engine streaming WS (MyOwnLLM) | `MyOwnLLM/src-tauri/src/api.rs` (`audio_stream_ws`), `transcribe.rs` |
