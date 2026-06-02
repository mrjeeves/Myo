# Tiered TTS — plan & handoff

Give Myo **nicer voices**, picked by hardware the same way transcription already
picks its ASR model. This is **Slice 3 (Native TTS)** from
[`native-agent.md`](native-agent.md), done the *tiered* way — and the decision
(settled with the owner) is that the **tier ladder lives in MyOwnLLM**, not Myo.

Written as a handoff: read top-to-bottom and you can build it. Two phases —
**Phase 1 is a MyOwnLLM upgrade, Phase 2 is the Myo upgrade that consumes it.**

> Work for the Myo side is on branch **`claude/laughing-meitner-AqAto`**. Phase 1
> happens in the **MyOwnLLM** repo (a separate repo — see "Where things live").

## TL;DR — where we are

- **Every spoken reply today is the browser's WebSpeech voice.** The live turn
  path [`run_turn_native`](../crates/myo-core/src/converse.rs) (`converse.rs:105`)
  hard-codes [`MyoEvent::AudioSpeak`](../crates/myo-core/src/event.rs)
  (`event.rs:148`), which the UI renders with `window.speechSynthesis`
  ([`audio-io.ts:54`](../src/lib/audio-io.ts)). That's the "Fallback TTS" — and
  it's the *only* TTS running.
- **The nicer-voice plumbing already exists and is dark.**
  [`MyoEvent::AudioReady { b64, mime }`](../crates/myo-core/src/event.rs)
  (`event.rs:140`) → [`Voice.playBase64()`](../src/lib/audio-io.ts)
  (`audio-io.ts:32`) already plays real synthesized audio; the
  [`onAudio`](../src/lib/stage.svelte.ts) reducer (`stage.svelte.ts:595`) already
  routes `kind:"ready"` → `playBase64` and `kind:"speak"` → WebSpeech. **Nothing
  feeds `AudioReady`.** Wiring it up is the whole UI-side change (≈ none).
- **The old real-TTS path is dead.** [`BrainClient::tts()`](../crates/myo-core/src/brain/mod.rs)
  (`brain/mod.rs:186`, Kokoro via Odysseus `POST /api/tts/synthesize`) still
  exists, but the native pivot ([`native-agent.md:3`](native-agent.md), dated
  2026-06-02) **stops launching Odysseus**, so that route no longer answers. Do
  not revive it.
- **The "3 tiers of transcription" live in the engine, not Myo.** They're
  MyOwnLLM's hardware-tier resolver (see
  [`myownllm-integration.md`](myownllm-integration.md) §8): `hardware::detect()`
  → `resolver::resolve("transcribe")` walks a ladder keyed on VRAM/RAM and
  returns a model tag. **TTS should mirror this exactly** — a new `speak` mode in
  the same resolver.

## Where things live (repos & scope)

- **MyOwnLLM** = the engine. It already owns the three things TTS needs:
  **onnxruntime delivery** (shared `libonnxruntime`,
  [`myownllm-integration.md`](myownllm-integration.md) §7), the **hardware-tier
  resolver** (§8), and the **model registry + HF downloader** (`models.rs`, §1).
  *Phase 1 happens here.* All MyOwnLLM file/line refs below come from
  [`myownllm-integration.md`](myownllm-integration.md), verified line-by-line
  against the real source.
- **Myo** = a thin consumer. It captures nothing TTS-specific; it POSTs text and
  plays the audio. *Phase 2 happens here.*
- **Pin:** [`.myownllm-rev`](../.myownllm-rev) is `0.2.24` today. Phase 2 bumps it
  to the speech-capable release Phase 1 cuts.

## Why the engine (decision — don't re-litigate)

The owner chose this: *MyOwnLLM already has the onnxruntime setup, so that's the
place for this.* The reasons hold up:

- **"One place owns models per device"** is an explicit decision in
  [`native-agent.md`](native-agent.md) ("Inference + ASR stay in MyOwnLLM"). ASR
  already lives there; TTS is the same shape (text/audio in, audio/text out).
- **Everything TTS needs is already in the engine:** ORT init + the
  uncancellable-`commit_from_file` watchdog (`ort_setup::load_session`), the
  per-arch dylib fetcher (`ort_install`, shares the `api-22` / `.ort-version`
  `1.24.2` pin), the tier resolver, and `models::pull_model` (HF → on-disk). Doing
  it in Myo would mean **duplicating ORT init, a hardware probe, and a model
  downloader** — none of which Myo has (no `ort`/`cpal`/`symphonia` deps today).
- **Myo's contract stays tiny** and matches ASR: `POST /v1/audio/speech` → audio
  bytes, exactly like `POST /v1/audio/transcriptions` → `{text}`.

## The tier ladder (the "3 tiers + WebSpeech fallback")

Mirror the transcribe ladder (`resolver.rs:1151-1152`, which is Parakeet on
capable HW → Moonshine on Pi/low-end). A proposed `speak` ladder:

| Tier | Hardware (engine-picked) | Voice (suggested) | Runtime |
|---|---|---|---|
| 1 | capable (≈ ≥4 GB VRAM / ≥8 GB RAM / ≥16 GB unified) | **Kokoro-82M** — expressive, multi-voice | `kokoro` |
| 2 | mid | **Piper** *medium* | `piper` |
| 3 | Pi / low-end | **Piper** *low / x_low* | `piper` |
| 4 | *(Myo-side)* engine has no speech route / unreachable | **WebSpeech** (no model, always works) | — |

Tiers 1–3 are engine-side (chosen by `resolver.resolve("speak")`); **tier 4 is
Myo's existing WebSpeech fallback** — the graceful degrade when the engine is too
old or down, exactly like the ASR streaming→clip fallback
([`voice-handoff.md`](voice-handoff.md) gotchas). So "3 tiers, technically 4 with
the default fallback" maps 1:1.

> **Engine choices are the implementer's call.** Suggested because both are ONNX
> (slot into `ort_setup` like the ASR backends), permissively licensed (Kokoro
> Apache-2.0, Piper MIT), and Piper ships voices in `x_low/low/medium/high` — a
> *ready-made* quality ladder. The transcribe ladder is only 2 rungs today;
> "3 tiers" is the intent, not gospel — pick rungs that map to real voice models
> and tune thresholds empirically.

---

## Phase 1 — MyOwnLLM (the engine upgrade) — **start here**

Mirror the existing `asr/` structure. References are MyOwnLLM paths from
[`myownllm-integration.md`](myownllm-integration.md).

1. **TTS backends** — new `tts/` module, parallel to `asr/`:
   - `tts/mod.rs` — a `TtsBackend` trait (`synthesize(text, voice) -> PCM/WAV`),
     `TtsCaps`, and `make_backend(runtime, model_name)` dispatching
     `"kokoro"` / `"piper"`. (Mirror `asr/mod.rs`'s `make_backend`, §1.)
   - `tts/kokoro.rs` — Kokoro ONNX backend (model + voice embeddings) loaded via
     `ort_setup::load_session(label, 90, …)` (the watchdog, §7). Mirror
     `asr/moonshine.rs`.
   - `tts/piper.rs` — Piper VITS ONNX backend. Mirror `asr/parakeet.rs`.
   - **Phonemization is the hard part** (see Gotchas): Kokoro and Piper both need
     a grapheme→phoneme front-end (espeak-ng / misaki) plus its data files,
     bundled per-platform. Scope this first — it's the riskiest bit, not the ORT
     inference.
2. **Model registry** (`models.rs`, §1): add `ModelKind::Tts`; register the
   Kokoro + Piper voice artifacts (HF repos) so `pull_model` streams them to
   `~/.myownllm/models/tts/<name>/`. Add `is_installed` coverage. Mirror the ASR
   `ModelSpec`/`Artifact`.
3. **Resolver `speak` mode** (`resolver.rs`, §8): add `"speak"` to `KNOWN_MODES`
   (currently `["text","transcribe","diarize"]`); add a `speak` ladder to
   `manifests/default.json` **and** the embedded fallback manifest
   (`resolver.rs:1151-1152`/`:1184`), with per-tier `min_vram_gb` /
   `min_ram_gb` / `min_unified_ram_gb` + a per-tier `runtime`
   (`kokoro` on top, `piper` on the low rungs). Extend `default_runtime_for`
   (`resolver.rs:121-129`): `speak` → `kokoro`.
4. **HTTP route** (`api.rs`): add **`POST /v1/audio/speech`** to the router
   (`api.rs:69-77`) — OpenAI-compatible (see "The contract"). It's a headless
   wrapper over the in-process TTS pipeline, **exactly how
   `/v1/audio/transcriptions` wraps the upload-ASR path** (§4): resolve
   `(runtime, model)` via `resolver.resolve("speak")`, pull-on-demand if missing
   (503 + `retry-after`, like chat/ASR), synthesize, return audio bytes. Add a
   `myownllm-speak` virtual ID to `/v1/models` (`PUBLIC_VIRTUAL_IDS`,
   `resolver.rs:38-39`) so Myo can show the resolved voice tier, like
   `myownllm-transcribe` reports `resolved_to`.
5. **Preload/CLI** (`main.rs`): teach `myownllm preload speak` to warm the voice
   model (mirror the transcribe/diarize preload) so the first spoken reply isn't
   cold.
6. **Cut a release** (≥ `0.2.25`) so Myo's `build.rs` can bundle a
   speech-capable engine. Phase 2 bumps `.myownllm-rev` to it.

---

## Phase 2 — Myo (the consumer upgrade)

After the engine ships the route and a release tag exists.

1. **Bump the pin** — [`.myownllm-rev`](../.myownllm-rev) → the speech-capable
   release. `build.rs` then bundles a route-capable engine (mirrors the `0.2.24`
   streaming pin).
2. **A TTS client** — new `crates/myo-core/src/tts.rs`, parallel to
   [`asr.rs`](../crates/myo-core/src/asr.rs): `TtsClient::new(base_url)`;
   `synthesize(text, voice?) -> TtsAudio { b64, mime }` POSTing to
   `{base}/v1/audio/speech`. Reuse `AsrClient`'s warming/503-retry shape
   (`asr.rs`) and add a `warm_up()` (mirror `asr.rs:85`). Reuse the existing
   [`TtsAudio`](../crates/myo-core/src/brain/mod.rs) struct (`brain/mod.rs:50`) —
   move it to `tts.rs` or `event.rs` so it doesn't drag in the dead `BrainClient`.
   Export from [`lib.rs`](../crates/myo-core/src/lib.rs) and construct it on
   `MyoState` next to `asr` ([`src-tauri/src/state.rs`](../src-tauri/src/state.rs)).
3. **Wire `run_turn_native`** ([`converse.rs:105`](../crates/myo-core/src/converse.rs))
   — the one behavior change. After accumulating `spoken`, try synthesis:
   ```rust
   match tts.synthesize(spoken, voice).await {
       Ok(a)  => emit(MyoEvent::AudioReady { turn, b64: a.b64, mime: a.mime }),
       Err(_) => emit(MyoEvent::AudioSpeak  { turn, text: spoken.into() }), // tier-4 fallback
   }
   ```
   A too-old engine (no `/v1/audio/speech` → 404) or an unreachable one just
   degrades to WebSpeech — same graceful-fallback shape as streaming→clip ASR.
   Note: `run_turn` (the Odysseus path, `converse.rs:53`) already has this
   `AudioReady`/`AudioSpeak` branch via `brain.tts()`; this brings the *native*
   path to parity using the engine instead of dead Odysseus.
4. **Warm-up** — in [`ensure_ready`](../src-tauri/src/supervisor.rs)
   (`supervisor.rs:42`), call `st.tts.warm_up()` alongside `st.asr.warm_up()` so
   the first reply doesn't wait on a cold voice model.
5. **`myo_tts_speak`** ([`core_api.rs:262`](../src-tauri/src/core_api.rs)) — point
   the one-off speak command at the new `TtsClient` (it currently calls the dead
   `brain.tts()`).
6. **Optional polish:** a voice/tier indicator (read `myownllm-speak`'s
   `resolved_to` from `/v1/models`, surface like `asrStatus`); a settings knob to
   pick a voice. Defer if it bloats the slice.
7. **Docs:** flip Slice 3 → done in [`native-agent.md`](native-agent.md); update
   the README roadmap line (`README.md:128`, "server TTS, WebSpeech fallback").

---

## The contract (keep both sides in sync)

**`POST /v1/audio/speech`** — loopback, tokenless on Myo's private port (Myo owns
the engine; same as the streaming ASR socket). OpenAI-shaped so any client works:

- **Request** (JSON): `{ "input": "<text>", "voice": "<optional id>",
  "response_format": "wav" | "mp3", "model": "<optional>" }`. Omitting `model`
  lets `resolver.resolve("speak")` pick the hardware tier — **the client never
  picks the tier**, exactly like ASR.
- **Response:** raw audio bytes, `Content-Type: audio/wav` (or `audio/mpeg`).
  Myo base64's them into `AudioReady`. *(Whole-utterance WAV is fine for v1; see
  "streaming TTS" in Gotchas.)*
- **`503` + `retry-after`** while a voice model pulls (mirror chat/ASR warming).
- **`404`** if the route doesn't exist (old engine) → Myo falls back to WebSpeech.

## Decisions already made (don't re-litigate)

- **TTS lives in the engine (MyOwnLLM)** — it already owns ORT + the tier resolver
  + the model registry. Myo is a thin consumer that emits `AudioReady` (UI
  plumbing already exists and is unused).
- **WebSpeech stays the last-resort 4th tier** — no model, always works; the
  degrade path for an old/unreachable engine (the ASR streaming→clip pattern).
- **Tier selection is hardware-driven, engine-side**, via the existing resolver
  ladder — never a Myo concern.
- **OpenAI-compatible route** (`/v1/audio/speech`) so it's a drop-in.
- **Do not revive Odysseus `brain.tts()`** — the native pivot removed it on
  purpose.

## Gotchas / open questions for the implementer

- **Phonemization is the real work**, not the ONNX inference. Kokoro (misaki /
  espeak) and Piper (espeak-ng) need a g2p front-end + data files bundled across
  the 5 target triples. Decide pure-Rust bindings vs shipping the espeak-ng data
  dir, and prove it on Pi/aarch64 early.
- **One ORT dylib, shared.** TTS backends `dlopen` the same `libonnxruntime` as
  ASR (the `api-22` pin, `.ort-version` `1.24.2`). Reuse `ort_setup`; **do not**
  add a second runtime (a version mismatch is UB/hang, not a clean error — §7).
- **Licenses:** Kokoro Apache-2.0, Piper MIT; **individual voices vary** — check
  each before bundling/downloading.
- **Streaming TTS is a later slice.** v1 synthesizes the whole reply at once and
  plays one blob (`playBase64`); replies are short. Sentence-chunked/streamed
  audio (lower latency-to-first-word, friendlier to barge-in) can mirror the ASR
  stream WS later. Barge-in already works: `Voice.stop()` (`audio-io.ts:65`)
  cancels the blob.
- **Tier count isn't fixed.** The transcribe ladder is 2 rungs today; land
  whatever maps to real voice models (Kokoro / Piper-medium / Piper-low) +
  WebSpeech and tune thresholds on real hardware.

## File map

**Phase 1 — MyOwnLLM** (mirror the `asr/` structure; refs from
[`myownllm-integration.md`](myownllm-integration.md)):

| Area | File (MyOwnLLM) |
|---|---|
| TTS backend trait + factory | `src-tauri/src/tts/mod.rs` *(new, mirror `asr/mod.rs`)* |
| Kokoro / Piper ONNX backends | `src-tauri/src/tts/kokoro.rs`, `tts/piper.rs` *(new)* |
| Voice models in the registry/downloader | `src-tauri/src/models.rs` (`ModelKind::Tts`) |
| `speak` mode + tier ladder | `src-tauri/src/resolver.rs`, `manifests/default.json` |
| `POST /v1/audio/speech` route + `/v1/models` ID | `src-tauri/src/api.rs` |
| Preload / CLI | `src-tauri/src/main.rs` |
| Shared ORT (reuse, don't duplicate) | `src-tauri/src/ort_setup.rs`, `ort_install.rs` |

**Phase 2 — Myo** (this repo):

| Area | File |
|---|---|
| TTS client (`POST /v1/audio/speech`) | `crates/myo-core/src/tts.rs` *(new, mirror `asr.rs`)* |
| Native turn → `AudioReady` | `crates/myo-core/src/converse.rs` (`run_turn_native`) |
| Client construction + warm-up | `src-tauri/src/state.rs`, `src-tauri/src/supervisor.rs` |
| One-off speak command | `src-tauri/src/core_api.rs` (`myo_tts_speak`) |
| Engine pin | `.myownllm-rev` |
| Audio playback + event (already done) | `src/lib/audio-io.ts` (`Voice.playBase64`), `crates/myo-core/src/event.rs` (`AudioReady`) |
| Roadmap docs | `docs/native-agent.md` (Slice 3), `README.md` |
