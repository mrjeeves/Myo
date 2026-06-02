# Voice loop — status & handoff

A snapshot of the voice (ears → brain → voice) work and exactly how to continue
it. Written as a handoff: read this top-to-bottom and you can pick up mid-stream.

All work is on branch **`claude/kind-lamport-AhBO3`** (Myo) and the matching
branch in MyOwnLLM. See also [`shell.md`](shell.md) (what's wired) and
[`PLAN.md`](PLAN.md) (the blueprint).

## TL;DR — where we are

Voice input works **end-to-end** and now in **two modes**:

1. **Real-time streaming dictation + full-duplex** (the primary path) — the
   WebView streams 16 kHz PCM to the engine's live WebSocket, renders **interim**
   captions as you speak, fires the brain turn on each **final**, and keeps
   listening through Myo's own reply so you can **barge in**. Built and wired on
   this branch; it lights up against a `myownllm` that serves `/v1/audio/stream`
   (**`0.2.24`+** — `.myownllm-rev` is pinned to it).
2. **Clip-at-a-time** (the fallback) — always-on mic → energy-VAD utterance →
   one-shot transcription → turn. Used automatically when the streaming socket
   can't be reached (engine too old / unreachable). Confirmed on macOS.

**Remaining:** cut/await MyOwnLLM **`0.2.24`** so the streaming engine bundles
(until then dev uses a sibling MyOwnLLM build, or falls back to the clip path),
then verify the streaming loop on-device and tune AEC/barge-in.

## What works now (this branch)

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
  brain→TTS turn as text (`spawn_text_turn`). Audio is transient (nothing kept).
- **Myo owns both engines on PRIVATE ports** — Odysseus (brain) on **`:17000`**,
  MyOwnLLM (model) on **`:11473`** — *not* the shared `:7000`/`:1473`
  ([`supervisor.rs`](../crates/myo-core/src/supervisor.rs)). This is the fix for
  the 404 (stale engine) and 403 (foreign-brain token mismatch): Myo never
  attaches to a user's own / orphaned instance.
- **Persisted internal token** at `~/.myo/internal-token`
  (`supervisor::persistent_token`) so a reused / re-attached brain authenticates
  (no per-launch mint → no 403 on reuse).
- **Bundled, pinned engine** — [`.myownllm-rev`](../.myownllm-rev) pins the
  MyOwnLLM release; [`build.rs`](../src-tauri/build.rs) stages it into
  `binaries/myownllm-<triple>` (override → sibling checkout → GitHub release
  download → stub) and Tauri ships it as an `externalBin`. Idempotent per path
  (a rebuilt sibling re-bundles; the release download is cached by tag).
- **Runtime self-heal of a stale engine** — bundling is build-time, so the
  resolved engine can still be behind the pin (classically: a stub bundle that
  fell through to an old `myownllm` on PATH). Before spawning,
  [`engine_update`](../src-tauri/src/engine_update.rs) compares the binary's
  `--version` to the pin (stamped in as `MYOWNLLM_PINNED_REV`) and, if it's
  older, fetches the pinned release into `~/.myo/engine/` **once** (cached,
  narrated on `myo://engine`) rather than 404-ing at the user — falling back to
  the local copy if the fetch can't happen. Pure version/platform logic in
  [`engine.rs`](../crates/myo-core/src/engine.rs) (unit-tested); IO in the binary.
- **Mic permission** — Windows WebView2 via `additionalBrowserArgs`
  (`--use-fake-ui-for-media-stream`) in
  [`tauri.conf.json`](../src-tauri/tauri.conf.json); macOS
  `NSMicrophoneUsageDescription` in [`Info.plist`](../src-tauri/Info.plist).

## In flight (open PRs)

- **Myo #12** (`claude/kind-lamport-AhBO3`) — everything above: bundling, private
  ports, persisted token, the HTTP ASR client, the clip mic path, **and now the
  streaming dictation client + full-duplex** (`StreamingListener`,
  `myo_asr_stream_url`, the live-caption UI) with `.myownllm-rev` pinned to
  `0.2.24`. **Merge this.**
- **MyOwnLLM #227** — `GET /v1/audio/stream` (WebSocket) live streaming ASR (the
  engine half this client talks to). **Merge, then cut `0.2.24`** so Myo's
  `build.rs` can bundle a route-capable engine.

Already landed in MyOwnLLM `0.2.23`: `#221` (the `/v1/audio/transcriptions`
route) and `#224` (symphonia `pcm` codec — PCM-WAV decode). `#226` (a
`myownllm transcribe` CLI) was **closed** — superseded by the owned-engine
approach. `#228` (diarization fix) is unrelated to the dictation surface
(diarization is off on the live stream).

## Streaming dictation + full-duplex (implemented)

The engine (MyOwnLLM #227) `GET /v1/audio/stream` drives the same live pipeline
the MyOwnLLM app uses (Silero VAD + LocalAgreement-2), model kept **warm** for
the connection, emitting **interim** captions as you speak + a **final** per
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

**What's done (this branch):** the pin bump (`.myownllm-rev` → `0.2.24`); the
`myo_asr_stream_url` command + `supervisor::myownllm_stream_url`;
`StreamingListener` (mic → Web Audio → stateful 16 kHz resampler → LE PCM →
WS, with bounded auto-reconnect); the store integration (streaming-first with
clip fallback, `liveTranscript`/`asrStatus`, `onInterim`/`onFinal`/`onStreamError`,
full-duplex `runUserTurn`, barge-in on final); and the live-caption UI. Warm
model (the session loads once) also removes the per-utterance reload latency the
clip path has.

**What's left:**
1. **Cut/await MyOwnLLM `0.2.24`** so `build.rs` bundles a route-capable engine
   (until then dev needs a built sibling `../MyOwnLLM`, or you get the clip
   fallback). The pin is already `0.2.24`.
2. **Verify on-device:** the streaming loop end-to-end (interim captions firming
   into turns), and **tune full-duplex** — echo-cancellation is supposed to keep
   Myo from transcribing her own TTS while she speaks. If AEC proves weak (she
   barges in on herself), the one-line degrade is to gate sends during playback:
   call `streamer.setGated(true)` on `speaking` / `false` on idle (the lever is
   already on `StreamingListener`; the store deliberately doesn't pull it).
3. **AudioWorklet** instead of the deprecated `ScriptProcessorNode` (both capture
   paths still use it), and **Silero VAD** in the browser for the clip path.

## Decisions already made (don't re-litigate)

- **Transcription rides the engine's HTTP/WS, not a bundled `myo-asr` crate, not
  a CLI, not a raw IPC socket.** The brain already needs MyOwnLLM as an OpenAI
  HTTP endpoint, so the engine serve exists regardless; a loopback **WebSocket**
  is the right "IPC bridge" because the audio is captured in the WebView, which
  speaks WS natively (a Unix socket would force a pointless Rust audio
  round-trip). PLAN Step 2's in-process `myo-asr` extraction is **deferred** (see
  [`myownllm-integration.md`](myownllm-integration.md) §1) — only needed if Myo
  ever wants ASR fully decoupled from the sidecar.
- **Myo owns its engines on private ports** (never attach to a foreign instance).
- **`keep_audio` is off** — always listening, nothing stored.

## Gotchas / known follow-ups

- **Orphans from a Ctrl-C'd `just dev`.** On macOS the `myo` GUI + its child
  engines can survive Ctrl-C (kill-on-Drop doesn't fire on SIGINT), leaving
  processes on `:17000`/`:11473`. Private ports + the persisted token make these
  **benign** (same version, same token → reattach just works), but they leak and
  cause `Address already in use` noise. **Follow-up:** a SIGINT handler that
  tears down `EngineChild`ren, and/or reap stale engines on startup.
- **Linux WebKitGTK mic** needs an `enable-media-stream` + permission-grant hook
  in the setup (Windows/macOS handled). Small follow-up.
- **One-shot `/v1/audio/transcriptions` rebuilds the model per call** — fine for
  the clip path, but caching the warm backend in `serve` would help; streaming
  (the WS) avoids it entirely.
- **Full-duplex relies on echo-cancellation.** The streaming path never gates
  the mic during a reply (that's the point — barge-in). If a machine's AEC is
  weak, Myo could transcribe her own TTS and barge in on herself; the documented
  degrade is `streamer.setGated(true)` while `speaking` (see "What's left" #2).
- **Streaming falls back to clip, not the other way.** `startListening` tries the
  WS first; on a persistent connect failure `StreamingListener` gives up
  (bounded reconnect) and the store switches to the clip `Listener`. A too-old
  engine (no `/v1/audio/stream`) therefore degrades gracefully — but the pin
  (`0.2.24`) is what guarantees the route exists in the bundled engine.
- **Dev engine source:** `build.rs` prefers a built sibling `../MyOwnLLM`
  checkout over the release download — `cargo build` it once and Myo bundles it;
  otherwise it downloads the pinned release.

## File map

| Area | File |
|---|---|
| ASR client (HTTP `/v1/audio/transcriptions`) | `crates/myo-core/src/asr.rs` |
| Ports, specs, token, stream URL, brain↔model wiring | `crates/myo-core/src/supervisor.rs` |
| One turn (ASR→brain→TTS) | `crates/myo-core/src/converse.rs` |
| Tauri commands (`asr_stream_url`, `feed_audio`, `say`, …) | `src-tauri/src/core_api.rs` |
| Process supervisor (spawn/own engines, warm-up) | `src-tauri/src/supervisor.rs` |
| Engine bundling | `src-tauri/build.rs`, `.myownllm-rev` |
| Mic capture — `StreamingListener` (WS) + `Listener` (clip) + TTS playback | `src/lib/audio-io.ts` |
| The reactive store / turn lifecycle / streaming handlers | `src/lib/stage.svelte.ts` |
| Command + `myo://` event wrappers | `src/lib/core-api.ts` |
| Live-caption ghost bubble + warming status | `src/surfaces/Conversation.svelte`, `Presence.svelte` |
| Mic mute toggle | `src/surfaces/Composer.svelte` |
| Engine streaming WS (MyOwnLLM) | `MyOwnLLM/src-tauri/src/api.rs` (`audio_stream_ws`), `transcribe.rs` (`start_remote_session_with_sink`) |
