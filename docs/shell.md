# The Myo shell — what's wired today

[`docs/PLAN.md`](PLAN.md) is the full blueprint. This page is the smaller,
honest map of what the **v1 orchestration shell** actually does right now, how
to run it against the real engines, and where the seams are.

> **Picking up the voice work?** Start at **[`voice-handoff.md`](voice-handoff.md)** —
> current state, open PRs, and the exact next steps (real-time streaming
> dictation + full-duplex).

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
| `myo_converse_feed_audio{audio,mime}` → `turnId?` | the **voice path**: base64 WAV → MyOwnLLM transcription → turn (`null` = silence/empty) |
| `myo_converse_cancel{turn}` | barge-in / stop an in-flight turn |
| `myo_converse_feed_wav{path}` → `turnId?` | WAV-**file** bypass (CI / "transcribe this file") → turn |
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
export MYO_MYOWNLLM_BIN=/path/to/myownllm           # default: bundled sidecar / PATH

cd Myo && pnpm install && cargo run -p myo          # launches the desktop shell
```

**The model engine is a pinned, bundled sidecar.** Just as MyOwnLLM bundles a
pinned `myownmesh` daemon, Myo bundles a pinned `myownllm`: `.myownllm-rev` names
the required release tag, [`src-tauri/build.rs`](../src-tauri/build.rs) stages
that binary into `binaries/myownllm-<triple>`, and Tauri ships it as an
`externalBin` next to the Myo executable. This is what guarantees the engine is
**new enough to have the transcription route** — pinning it the way the rest of
the stack pins its dependencies. Build-time resolution: `MYO_MYOWNLLM_BIN`
override → **sibling MyOwnLLM checkout** (a built `../MyOwnLLM/src-tauri/target/<profile>/myownllm`)
→ GitHub release download for the pinned tag → zero-byte stub (runtime then
falls back to `MYO_MYOWNLLM_BIN`/`PATH`). So in dev: `cargo build` your sibling
MyOwnLLM once and Myo bundles it; bump `.myownllm-rev` when the engine cuts a
release Myo needs.

On launch the shell mints an `ODYSSEUS_INTERNAL_TOKEN`, injects it into the
Odysseus child (so it authenticates as admin over loopback — no account needed),
polls both engines healthy on `myo://engine`, registers MyOwnLLM as Odysseus's
default model endpoint, and applies the persisted capability allowlist. Then:
type in the composer → watch the answer stream, the tool feed run, documents
materialize, and the reply speak back.

**Myo owns its engine on a private port — no more `:1473` collisions.** Myo runs
*its* pinned `myownllm serve` on `:11473`, not MyOwnLLM's shared `:1473`, so a
user's own MyOwnLLM / desktop app — or an engine orphaned by a Ctrl-C'd `just
dev` that didn't run kill-on-drop — can't be attached to. Both chat and
transcription use Myo's own engine. (An orphan of Myo's *own* engine left on
`:11473` is harmless: same pinned, route-capable build, so reattaching just
works.)

**CI / no-audio:** the text path (`myo_converse_say`) needs no microphone, and
`myo_converse_feed_wav{path}` runs a fixture file through the same transcription
engine — the two no-mic hooks that exercise the round-trip without hardware.

**Mic permission (per platform).** The always-on capture needs the WebView to
allow `getUserMedia`. Windows (WebView2) is granted declaratively via
`additionalBrowserArgs` (`--use-fake-ui-for-media-stream`) in
[`tauri.conf.json`](../src-tauri/tauri.conf.json); macOS reads
`NSMicrophoneUsageDescription` from [`src-tauri/Info.plist`](../src-tauri/Info.plist).
Linux WebKitGTK still needs an `enable-media-stream` + permission-grant hook in
the setup (small follow-up). If capture can't start, the listener logs the exact
error (`[myo-listen]` / `[myo]` in the devtools console) and the shell falls back
to the text composer.

## Seams left open (deliberately)

These are real, scoped follow-ups — the shell is built *around* each seam so the
engine drops in without reshaping the shell:

- **👂 Voice input — wired (open-mic).** Myo is always listening: the WebView
  captures the mic (`getUserMedia` with echo-cancellation), a lightweight energy
  VAD segments utterances ([`src/lib/audio-io.ts`](../src/lib/audio-io.ts)), and
  each finished utterance is base64'd to `myo_converse_feed_audio`, which POSTs it
  to **Myo's own engine** — `/v1/audio/transcriptions` on the private `:11473`
  (`AsrClient`). Because Myo owns that engine, no stale/foreign instance can
  hijack ASR. The transcript drives the same brain→voice turn as text
  (`myo://transcript` → `assistant` → `audio`); the engine deletes the upload
  immediately and the mic button is the one-tap hard mute. **Refinements still
  open:** **warm** transcription (the engine rebuilds the ASR backend per
  utterance — fine beside a multi-second agent turn, but caching the loaded
  backend in `serve` would make turns snappier), full-duplex **barge-in** (today
  half-duplex — the mic gates while Myo thinks/speaks so she never transcribes
  herself), and **Silero VAD** (+ AudioWorklet) in place of the energy gate.
- **✋ Fine-grained approval (tier-b).** Coarse control (the four toggles) ships
  now; per-action approve/tweak/edit needs the upstreamable Odysseus hook plus an
  ApprovalCard surface. Reserved in the event vocabulary.
- **📦 End-user packaging.** v1 runs against engine checkouts (env vars above);
  freezing the Python brain and bundling sidecars per target-triple is the
  installer work in PLAN's "Odysseus packaging" section.
