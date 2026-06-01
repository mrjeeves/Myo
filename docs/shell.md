# The Myo shell — what's wired today

[`docs/PLAN.md`](PLAN.md) is the full blueprint. This page is the smaller,
honest map of what the **v1 orchestration shell** actually does right now, how
to run it against the real engines, and where the seams are.

Myo is **the AI, not an app with an AI inside it**: a thin voice-first shell
that composes three local engines as swappable senses behind one internal API
and renders the agent's intent as a "dissolved" UI. v1 builds that shell
end-to-end. The heavy native engine *packaging* (freezing the Python brain,
bundling onnxruntime, extracting the on-device ASR) is deployment work the PLAN
brackets separately; the shell is the part that turns Myo from "an updater with
a window" into "a companion you talk to."

## The pieces

| Layer | Crate / dir | Role |
|---|---|---|
| **Orchestration core** | [`crates/myo-core`](../crates/myo-core) | Tauri-free, fully unit-tested. The Odysseus **brain client** (multipart in, SSE out → normalized [`MyoEvent`](../crates/myo-core/src/event.rs) stream), the **capability** mapping (4 toggles ⇄ Odysseus's `allow_*` + `disabled_tools`), engine **supervision specs**, and the **converse** round-trip. |
| **Shell binary** | [`src-tauri`](../src-tauri) | The Core API `#[tauri::command]`s, the OS-facing **process supervisor** (spawn + health-poll + kill-on-Drop), and the `myo://` event bridge to the WebView. |
| **Dissolved UI** | [`src`](../src) | The Svelte 5 surfaces — Presence, Conversation, Activity, DocumentArtifact, Control, Memory — driven by one reactive store ([`src/lib/stage.svelte.ts`](../src/lib/stage.svelte.ts)) that folds the `myo://` stream into state. |

Both `myo-core` and `myo-self-update` are deliberately Tauri-agnostic, so all
the load-bearing logic compiles and is tested without a webview.

## The Core API (the stable seam)

Tauri commands the frontend invokes ([`src-tauri/src/core_api.rs`](../src-tauri/src/core_api.rs)):

| Command | Does |
|---|---|
| `myo_engines_status` / `myo_engines_ensure_ready` | health of the brain + model engine; launch and auto-wire them |
| `myo_converse_say{text}` → `turnId` | the **text path**: brain answer (streamed) → voice |
| `myo_converse_cancel{turn}` | barge-in / stop an in-flight turn |
| `myo_converse_feed_wav{path}` | reserved for `myo-asr` (returns a clear "not bundled yet") |
| `myo_capabilities_get` / `myo_capabilities_set{caps}` | the four Web/Files/Code/Reach-out toggles |
| `myo_converse_incognito{on}` | pause memory (privacy) |
| `myo_memory_list{query?}` / `myo_memory_forget{id}` | review / forget what Myo remembers |
| `myo_settings_get` / `myo_tts_speak{text}` | persisted settings; one-off speech |

The normalized **`myo://` event stream** ([`event.rs`](../crates/myo-core/src/event.rs))
the UI renders: `assistant`, `transcript`, `activity`, `artifact`, `ui`
(agent-driven `ui_control`), `progress` (incl. `memories_used`, `thinking`,
errors), `audio` (server TTS or WebSpeech fallback), and `engine`. Adding a new
agent surface is one variant here plus one renderer there — *that* extensibility
is the dissolved UI.

## Running it against the real engines (dev)

The shell talks to two loopback sidecars: **Odysseus** (`:7000`, the brain) and
**MyOwnLLM** (`:1473`, the model engine). The supervisor will try to start them;
point it at your checkouts with env vars, or start them yourself first (the
supervisor attaches to whatever's already healthy):

```sh
# Tell Myo where the engines live (dev): 
export MYO_ODYSSEUS_DIR=/path/to/odysseus          # contains app.py
export MYO_UVICORN=/path/to/odysseus-venv/bin/uvicorn   # default: ~/.myo/odysseus-venv
export MYO_MYOWNLLM_BIN=/path/to/myownllm           # default: PATH / bundled

cd Myo && pnpm install && cargo run -p myo          # launches the desktop shell
```

On launch the shell mints an `ODYSSEUS_INTERNAL_TOKEN`, injects it into the
Odysseus child (so it authenticates as admin over loopback — no account needed),
polls both engines healthy on `myo://engine`, registers MyOwnLLM as Odysseus's
default model endpoint, and applies the persisted capability allowlist. Then:
type in the composer → watch the answer stream, the tool feed run, documents
materialize, and the reply speak back.

**CI / no-audio:** the text path (`myo_converse_say`) needs no microphone, which
is exactly how the round-trip is exercised without hardware.

## Seams left open (deliberately)

These are real, scoped follow-ups — the shell is built *around* each seam so the
engine drops in without reshaping the shell:

- **👂 On-device ASR — the remaining "ear".** Open-mic capture → VAD →
  transcription. The brain→voice half already runs; `myo_converse_feed_wav` and
  the mic button are the placeholders, and `myo://transcript` is already in the
  vocabulary. MyOwnLLM's STT is now **always-on, recorded, and diarized**
  (persistent speaker IDs) — but it's still **in-process only**: it has no
  HTTP/WS/CLI surface, every entry point needs a live `WebviewWindow`, and its
  output goes solely to that window's event bus. So there are two ways in:
  - **(recommended) a loopback bridge in MyOwnLLM.** Its transcription is built
    on a `FrameSink` trait (`src-tauri/src/frame_sink.rs`); the internal workers
    already take `Arc<dyn FrameSink>`. Adding a `FrameSink` impl that forwards
    `TranscribeFrame`s over a small WS/loopback route (axum's `ws` feature) turns
    the "ear" into a thin client Myo connects to — mirroring the brain. Small,
    well-localized change *there*; then `engines/asr_local` here is a WS client
    that re-emits on `myo://transcript`.
  - **(heavy) extract the engine** into `crates/myo-asr` — the `transcribe.rs` +
    `asr/` + `diarize/` pipeline plus onnxruntime (`load-dynamic`) and model
    management. Tractable via the same `FrameSink` seam, but it drags the whole
    onnx stack into Myo. See [`myownllm-integration.md`](myownllm-integration.md) §1.
- **✋ Fine-grained approval (tier-b).** Coarse control (the four toggles) ships
  now; per-action approve/tweak/edit needs the upstreamable Odysseus hook plus an
  ApprovalCard surface. Reserved in the event vocabulary.
- **📦 End-user packaging.** v1 runs against engine checkouts (env vars above);
  freezing the Python brain and bundling sidecars per target-triple is the
  installer work in PLAN's "Odysseus packaging" section.
