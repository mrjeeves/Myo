# The Myo shell — what's wired today

[`native-agent.md`](native-agent.md) is the direction; this page is the smaller,
honest map of what **actually runs right now**, how to run it against the engine,
and where the seams are.

> **Picking up the voice work?** Start at [`voice-handoff.md`](voice-handoff.md) —
> the streaming-dictation path, the WS protocol, and the open refinements.

**Myo *is* the agent.** The brain (agent loop + persona), the turn lifecycle,
the memory/capability surfaces, and the voice are native (Rust + Svelte); Myo
runs one local service — **MyOwnLLM** — for inference and speech, and renders the
agent's work as a **dissolved UI**. There is **no Odysseus sidecar**: a full
spoken turn needs only Myo + its bundled MyOwnLLM.

## The pieces

| Layer | Crate / dir | Role |
|---|---|---|
| **Agent core** | [`crates/myo-core`](../crates/myo-core) | Tauri-free, fully unit-tested. The **native brain** (`llm.rs`: MyOwnLLM chat SSE → normalized [`MyoEvent`](../crates/myo-core/src/event.rs) stream), the **converse** round-trip (`converse.rs`: ASR → brain → TTS), the **ASR**/**TTS** clients (`asr.rs`/`tts.rs` — the engine's `/v1/audio/*` routes), the **capability** mapping (the 4 toggles), and the engine **supervision specs**. |
| **Shell binary** | [`src-tauri`](../src-tauri) | The Core API `#[tauri::command]`s, the OS-facing **engine supervisor** (spawn + health-poll + kill-on-Drop), the in-process turn state + layered memory ([`state.rs`](../src-tauri/src/state.rs) — persona + context), and the `myo://` event bridge to the WebView. |
| **Dissolved UI** | [`src`](../src) | The Svelte 5 surfaces — Presence, Conversation, Activity, DocumentArtifact, Control, Memory — driven by one reactive store ([`src/lib/stage.svelte.ts`](../src/lib/stage.svelte.ts)) that folds the `myo://` stream into state. |

Both `myo-core` and `myo-self-update` are deliberately Tauri-agnostic, so the
load-bearing logic compiles and is tested without a webview.

## The Core API (the stable seam)

Tauri commands the frontend invokes ([`src-tauri/src/core_api.rs`](../src-tauri/src/core_api.rs)):

| Command | Does |
|---|---|
| `myo_engines_status` / `myo_engines_ensure_ready` | health of the model engine; launch + warm it |
| `myo_asr_stream_url` → `ws://…/v1/audio/stream` | the engine's **live dictation** WebSocket (the streaming voice path connects here) |
| `myo_converse_say{text}` → `turnId` | the **text path**: native brain answer (streamed) → voice. Also how the streaming voice path runs a finalized utterance. |
| `myo_converse_feed_audio{audio,mime}` → `turnId?` | the **clip voice path** (fallback): base64 WAV → MyOwnLLM transcription → turn (`null` = silence/empty) |
| `myo_converse_cancel{turn}` | explicit **hard-stop** of an in-flight turn (force-stop / teardown). The conversational flow never cancels — replies generate one at a time (a draining accumulator) and talking over Myo only hushes her voice (frontend); see [`voice-handoff.md`](voice-handoff.md). |
| `myo_converse_feed_wav{path}` → `turnId?` | WAV-**file** bypass (CI / "transcribe this file") → turn |
| `myo_capabilities_get` / `myo_capabilities_set{caps}` | the four Web/Files/Code/Reach-out toggles |
| `myo_converse_incognito{on}` | pause memory (privacy) |
| `myo_memory_list{query?}` / `myo_memory_forget{id}` | review / forget what Myo remembers |
| `myo_settings_get` / `myo_tts_speak{text}` | persisted settings; one-off speech |

The normalized **`myo://` event stream** ([`event.rs`](../crates/myo-core/src/event.rs))
the UI renders: `assistant`, `transcript`, `activity`, `artifact`, `ui`
(agent-driven `ui_control`), `progress` (incl. `memories_used`, `thinking`,
errors), `audio` (native TTS or WebSpeech fallback), and `engine`. Adding a new
agent surface is one variant here plus one renderer there — *that* extensibility
is the dissolved UI. The native brain emits the **same** `myo://` events the
earlier Odysseus client did, so the surfaces didn't move.

## Running it (dev)

The shell talks to one loopback sidecar: **MyOwnLLM** (the model + speech
engine), which it **owns** on a private port. The supervisor starts it (or
attaches to a healthy one it spawned); point it at a checkout with an env var,
or let the bundled sidecar resolve:

```sh
export MYO_MYOWNLLM_BIN=/path/to/myownllm   # optional — default: bundled sidecar / PATH

cd Myo && pnpm install && cargo run -p myo  # launches the desktop shell
```

**The model engine is a pinned, bundled sidecar.** Just as MyOwnLLM bundles a
pinned `myownmesh` daemon, Myo bundles a pinned `myownllm`: `.myownllm-rev` names
the required release tag, [`src-tauri/build.rs`](../src-tauri/build.rs) stages
that binary into `binaries/myownllm-<triple>`, and Tauri ships it as an
`externalBin` next to the Myo executable. This guarantees the engine is **new
enough to serve the routes Myo needs** (chat, embeddings, `/v1/audio/*`) — pinned
the way the rest of the stack pins its dependencies. Build-time resolution:
`MYO_MYOWNLLM_BIN` override → **sibling MyOwnLLM checkout** (a built
`../MyOwnLLM/src-tauri/target/<profile>/myownllm`) → GitHub release download for
the pinned tag → zero-byte stub (runtime then falls back to `MYO_MYOWNLLM_BIN` /
`PATH`). So in dev: `cargo build` your sibling MyOwnLLM once and Myo bundles it;
bump `.myownllm-rev` when the engine cuts a release Myo needs.

**The engine self-heals at launch if the local copy is stale.** Bundling is
build-time, so the resolved engine can still be behind the pin — most often when
the build *couldn't* download the release (stub sidecar) and falls through to an
older `myownllm` on `PATH`. Before spawning, [`engine_update`](../src-tauri/src/engine_update.rs)
checks the resolved binary's `--version` against the pin (stamped in by
`build.rs` as `MYOWNLLM_PINNED_REV`); if it's older, Myo fetches the pinned
release into a copy it owns (`~/.myo/engine/`, cached across launches) **once** —
narrated on `myo://engine` (`updating`/`updated`) — instead of letting a stale
engine 404 at the user. If that fetch can't happen (offline, tag unreleased) it
falls back to the resolved copy (`update-failed`) and the normal error path
applies. A nice consequence: once a needed release ships, Myo picks it up at
runtime without a rebuild.

On launch the shell polls the engine healthy on `myo://engine`, warms it, and
applies the persisted capability allowlist. Then: type in the composer (or just
talk) → watch the answer stream, the tool feed run, documents materialize, and
the reply speak back — all from Myo's own brain, no second process.

**Myo owns its engine on a private port — no `:1473` collisions.** Myo runs *its*
pinned `myownllm serve` on **`:11473`**, not MyOwnLLM's shared `:1473`, so a
user's own MyOwnLLM / desktop app — or an engine orphaned by a Ctrl-C'd `just
dev` that didn't run kill-on-drop — can't be attached to. Chat, embeddings, and
transcription all use Myo's own engine. (An orphan of Myo's *own* engine left on
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
native piece drops in without reshaping it:

- **🧠 Native memory + tools (Slices 2 & 4).** The conversation, voice, and the
  four-toggle Control surface are wired; the **native memory store** (local
  SQLite + embeddings recall) and the **native tool loop** (the actions behind
  Web/Files/Code/Reach-out) are the next slices. The Memory/Control *surfaces*
  exist and currently call a lingering brain client that **degrades gracefully**
  until those land — see [`native-agent.md`](native-agent.md).
- **✋ Fine-grained approval.** Coarse control (the four toggles) is wired now;
  per-action approve/tweak/edit (pause a mutating tool → approve/tweak/edit →
  resume) is the next control layer. Reserved in the `myo://` event vocabulary.
- **🐝 Hive (Slice 5).** Single-device today; multi-device over MyOwnMesh —
  mini-myos discovering each other and sharing data — is the mesh milestone
  ([`myownmesh-v2.md`](myownmesh-v2.md)).
