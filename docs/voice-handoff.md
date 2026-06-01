# Voice loop — status & handoff

A snapshot of the voice (ears → brain → voice) work and exactly how to continue
it. Written as a handoff: read this top-to-bottom and you can pick up mid-stream.

All work is on branch **`claude/kind-lamport-AhBO3`** (Myo) and the matching
branch in MyOwnLLM. See also [`shell.md`](shell.md) (what's wired) and
[`PLAN.md`](PLAN.md) (the blueprint).

## TL;DR — where we are

Voice input works **end-to-end today** (clip-at-a-time): always-on mic →
energy-VAD utterance → MyOwnLLM transcription → Odysseus turn → TTS. Confirmed on
macOS. The **next milestone is real-time streaming dictation + full-duplex**
(barge-in), for which the engine half is built and waiting on a MyOwnLLM release.

## What works now (this branch)

- **Always-on open-mic capture** in the WebView — `getUserMedia`
  (echo-cancellation) + a lightweight energy VAD that segments utterances
  ([`src/lib/audio-io.ts`](../src/lib/audio-io.ts) `Listener`). Mic button =
  one-tap hard mute ([`Composer.svelte`](../src/surfaces/Composer.svelte)).
  Half-duplex for now: the mic is **gated** while Myo thinks/speaks
  ([`stage.svelte.ts`](../src/lib/stage.svelte.ts)).
- **Clip transcription** — each finished utterance is base64'd to
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
- **Mic permission** — Windows WebView2 via `additionalBrowserArgs`
  (`--use-fake-ui-for-media-stream`) in
  [`tauri.conf.json`](../src-tauri/tauri.conf.json); macOS
  `NSMicrophoneUsageDescription` in [`Info.plist`](../src-tauri/Info.plist).

## In flight (open PRs)

- **Myo #12** (`claude/kind-lamport-AhBO3`) — everything above (bundling, private
  ports, persisted token, HTTP ASR client, mic capture). **Merge this.**
- **MyOwnLLM #227** — `GET /v1/audio/stream` (WebSocket) live streaming ASR.
  **Merge, then cut `0.2.24`.**

Already landed in MyOwnLLM `0.2.23`: `#221` (the `/v1/audio/transcriptions`
route) and `#224` (symphonia `pcm` codec — PCM-WAV decode). `#226` (a
`myownllm transcribe` CLI) was **closed** — superseded by the owned-engine
approach.

## Next milestone: streaming dictation + full-duplex

The engine half is done (MyOwnLLM #227): `GET /v1/audio/stream` drives the same
live pipeline the MyOwnLLM app uses (Silero VAD + LocalAgreement-2), model kept
**warm** for the connection, emitting **interim** captions as you speak +
**final** per utterance.

**Streaming WS protocol** (client = Myo WebView):
- Connect: `ws://127.0.0.1:11473/v1/audio/stream`.
- Up: **binary** frames of **16 kHz mono i16 LE PCM**. End with a text `"end"`
  (or just close).
- Down: JSON `TranscribeFrame` per frame; `segments[]` carries
  `{ text, start_ms, end_ms, partial, seg_id, … }`. **`partial: true` = interim**
  (live "typing" caption), `partial: false` = finalized utterance.

**The remaining work (Myo frontend + a pin bump):**
1. Bump [`.myownllm-rev`](../.myownllm-rev) → `0.2.24` once it's released.
2. Add a streaming capture path in [`audio-io.ts`](../src/lib/audio-io.ts): open
   the WS, capture mic via Web Audio, **downsample to 16 kHz mono** and send
   i16 LE PCM frames continuously. (The current `Listener` does energy-VAD +
   WAV-clip encoding; streaming hands endpointing to the engine instead, so this
   is a sibling capture mode — keep the clip path as a fallback.)
3. In [`stage.svelte.ts`](../src/lib/stage.svelte.ts): render **interim**
   segments live (the `turn.partial` slot already exists and `myo://transcript`
   `partial` is already in the reducer), and on a **final** segment run the brain
   turn (reuse `api.say(text)` — the proven text path — rather than re-POSTing
   audio).
4. **Full-duplex / barge-in**: keep the WS + mic open while Myo replies; a final
   utterance arriving during `thinking`/`speaking` should `cancel()` the current
   turn and start a new one. (Today the mic is gated during reply; streaming
   removes that gate.) Echo-cancellation + her TTS as the AEC reference keep her
   from transcribing herself; degrade to gated/half-duplex if AEC is weak.
5. The frontend needs the engine URL — expose it (e.g. a tiny
   `myo_asr_stream_url` command returning `myo_core::supervisor::myownllm_base_url()`
   with `ws://` + `/v1/audio/stream`, or fold into `enginesStatus`).

Warm model (the streaming session loads once) also removes the per-utterance
reload latency you see in the clip path.

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
- **Dev engine source:** `build.rs` prefers a built sibling `../MyOwnLLM`
  checkout over the release download — `cargo build` it once and Myo bundles it;
  otherwise it downloads the pinned release.

## File map

| Area | File |
|---|---|
| ASR client (HTTP `/v1/audio/transcriptions`) | `crates/myo-core/src/asr.rs` |
| Ports, specs, token, brain↔model wiring | `crates/myo-core/src/supervisor.rs` |
| One turn (ASR→brain→TTS) | `crates/myo-core/src/converse.rs` |
| Tauri commands (`feed_audio`, `say`, …) | `src-tauri/src/core_api.rs` |
| Process supervisor (spawn/own engines, warm-up) | `src-tauri/src/supervisor.rs` |
| Engine bundling | `src-tauri/build.rs`, `.myownllm-rev` |
| Mic capture + VAD + TTS playback | `src/lib/audio-io.ts` |
| The reactive store / turn lifecycle | `src/lib/stage.svelte.ts` |
| Command + `myo://` event wrappers | `src/lib/core-api.ts` |
| Mic mute toggle | `src/surfaces/Composer.svelte` |
| Engine streaming WS (MyOwnLLM) | `MyOwnLLM/src-tauri/src/api.rs` (`audio_stream_ws`), `transcribe.rs` (`start_remote_session_with_sink`) |
