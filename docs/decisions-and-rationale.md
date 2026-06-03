# Myo — Decisions & Rationale

This file records **why** Myo is shaped the way it is — the product decisions and
the alternatives that were rejected. [`native-agent.md`](native-agent.md) says
*what* to build; this says *why*. These were deliberate calls made with the
project owner, not defaults. If you think one is wrong, raise it explicitly;
don't quietly drift.

> **The north star.** *Myo **is** the AI — not an app with an AI inside it.* One
> voice-first (touch-second, type-third) full-service agent with its own brain,
> ears, voice, memory, networking, and files. **The interface dissolves into the
> conversation:** the user mostly just talks and *watches the agent work*. UI is
> an *output of the agent*, materialized on the fly — never a fixed frame the
> agent lives inside. A thin mid-layer lets the user step in to approve, tweak,
> or edit at any moment. Everything runs and stays **local**.

---

## The architectural decision: Myo *is* the agent

**Myo builds the brain natively rather than running someone else's.** The agent
loop, persona, memory, the tool loop, and TTS live **in Myo** (Rust), talking to
**MyOwnLLM** over loopback HTTP for inference, embeddings, and speech. Each
install is a self-contained **mini-myo**; installs network over MyOwnMesh as a
**hive**.

- **MyOwnLLM = model engine + speech.** Per-device model selection, LLM
  inference, ASR, and TTS. *Owned:* a pinned sidecar, bundled per-triple,
  self-healing to the pin at launch, on a private port. The one service Myo runs.
- **Odysseus = a code *reference*, not a dependency.** Odysseus is a deep Python
  agent (agent loop, tools, RAG, memory, TTS). Myo **reads** it to learn *how* it
  does a thing, then reimplements the parts it wants natively. It is **never
  launched** — no freezing, no venv, no sideload, no loopback brain port.
- **MyOwnMesh = networking** for the hive (Slice 5).

**Why not run Odysseus as a brain sidecar (the earlier plan)?** Freezing a
Python/FastAPI app with its own service stack (ChromaDB, SearXNG, ntfy)
per-platform — or shipping a venv — is fragile and heavy, and it keeps Myo as
"an app with an AI inside it." Building the features we actually want directly
into Myo makes each install small, owned, and hive-ready — and it *is* the north
star: Myo is the AI, not a shell around someone else's brain. (See
[`native-agent.md`](native-agent.md) for the Odysseus→Myo feature map and the
slice-by-slice plan.)

- *Rejected:* running/freezing Odysseus as a sidecar (heavy, fragile packaging;
  keeps Myo a shell), and forking it (permanent maintenance divergence).
- *Rejected:* reinventing the **model/speech** layer too — MyOwnLLM already does
  per-device model selection, inference, ASR, and TTS well; Myo owns and bundles
  it rather than rebuilding it.

---

## The product decisions (decided with the owner)

These are about the *experience*, and they hold regardless of where the brain
runs.

### Fork 1 — How does the user control the agent? → **Four category toggles: Web / Files / Code / Reach-out**

- **Chosen:** a Control surface with **four coarse capability toggles** — **Web**,
  **Files**, **Code**, **Reach-out** — gating the native tool loop. Default
  posture: **Web on, others off.** Per-action approve/tweak/edit is the **next**
  layer, not v1.
- **Why:** voice-first means control must be *glanceable and calm*, not a
  spreadsheet of switches. Four plain-language categories map cleanly onto the
  real tool families. The agent can even flip them itself via a `ui_control`
  toggle.
- **Rejected:** a granular per-tool allowlist (too fiddly; wrong altitude for
  voice); a single "autonomy" slider (too coarse/opaque); no visible control
  ("just trust it" — loses the trust a full-service local agent needs).
- **Consequence:** coarse control ships first; **fine-grained per-action
  approval** (pause a mutating tool → approve/tweak/edit → resume) is the
  immediate next layer, reserved in the `myo://` event vocabulary.

### Fork 2 — How does the user talk to Myo? → **Open-mic + barge-in (full-duplex)**

- **Chosen:** an **always-listening open mic** with **barge-in** (interrupt by
  speaking) and an always-visible **hard-mute**. The home surface is an **ambient
  presence indicator** (listening / thinking / speaking), *not* a push-to-talk
  button.
- **Why:** this is the most "dissolved," calmest interaction — you just talk, like
  to a person, and interrupt naturally. The strongest expression of "Myo is the
  AI."
- **Rejected:** push-to-talk (reintroduces a button/frame; kept only as the
  **degraded** mode where a webview lacks usable echo cancellation, and as the
  CI/headless path); a wake-word ("Hey Myo" — adds friction and a false-trigger
  surface).
- **Consequence (the hard engineering bit):** full-duplex demands **echo
  cancellation so the ASR never hears the TTS.** So **audio capture lives in the
  WebView** (`getUserMedia({echoCancellation,noiseSuppression,autoGainControl})`),
  a VAD does endpointing, PCM streams to **MyOwnLLM's live ASR** over a loopback
  WebSocket, and **TTS plays in the same audio context as the AEC reference.**
  Barge-in = *sustained* talk-over during playback → hush her voice (the
  generation isn't cancelled; replies queue and drain — see `voice-handoff.md`).
  **ASR is local, so always-on
  audio never leaves the device.** AEC quality must be validated *per-webview*
  (CoreAudio / WebKitGTK / WebView2).

### Fork 3 — How does the user *watch the agent work*? → **A single evolving stage + recallable history**

- **Chosen:** one focal **Stage** at a time, with finished surfaces collapsing
  into a **History** you can recall (by voice or touch), plus an **ambient
  ActivityStrip** for the live tool feed and a persistent **Presence** orb for
  voice state. So the screen is: **Presence orb + Stage + ActivityStrip +
  HistoryRail.**
- **Why:** voice-first means *looking*, not window-managing. A single evolving
  stage is calm and never makes the user arrange panes. History keeps everything
  recoverable; the ambient strip keeps "watch it work" always glanceable.
- **Focus policy (important):** a streaming artifact or an agent-opened panel
  takes the Stage; the **live tool feed *is* the Stage when nothing else is
  live**, otherwise it demotes to the ambient strip; the agent (`ui_control`) or
  the user can redirect focus; **blurred surfaces park in History with their
  state intact** and are recallable by voice ("bring back that doc") via a
  per-session surface referent registry (stable surfaceId + kind + title) that
  makes deixis like "that document" resolvable.
- **Rejected:** a tabbed / multi-window workspace (window management is exactly
  what voice-first should dissolve); ephemeral surfaces that vanish on blur
  (breaks "watch it work" continuity and voice recall); a dashboard grid (a fixed
  frame; contradicts "surfaces materialize from agent intent").

### Fork 4 — How much does Myo remember? → **Continuous presence (automatic local memory)**

- **Chosen:** Myo is **one ongoing relationship, not sessions.** Memory + recall
  run on by default, so Myo recalls across turns and days ("last time you…") with
  no thread management. A subtle "recalled from memory" cue shows when it draws on
  the past.
- **Why:** the most companion-like, most "Myo is the AI" option.
- **How (native):** a local **SQLite** store of durable memories with embeddings
  recall via MyOwnLLM `/v1/embeddings`, injected into each turn's context
  (Slice 2). *(Reference: how Odysseus does memory + RAG — reimplemented
  natively.)*
- **Counterweight (mandatory):** a thing that remembers everything must be
  **visible, forgettable, and pausable** — a **Memory surface** to review and
  forget, and **one-tap incognito** to stop recording. All **local, nothing
  leaves the device.**
- **Rejected:** topic/session threads (organized but tool-like, less "eternal
  companion"); ephemeral + opt-in memory (max privacy but least magical and most
  user effort; wrong default for a companion).

---

## The shape (decided)

- **One fresh Tauri 2 + Svelte 5 app** that is a **renderer of agent intent** — a
  registry of surface-kind → component, not a set of fixed screens. Adding a new
  agent-emitted surface = registering a renderer. That extensibility *is* the
  dissolved/generative UI.
  - *Rejected:* extending MyOwnLLM's UI in place (model-management-shaped) or
    Odysseus's PWA (web chat, not voice-first); a loose multi-app federation (the
    opposite of "Myo is the AI").
- **The engine is owned, not assumed.** MyOwnLLM is pinned (`.myownllm-rev`),
  bundled per-triple, and self-heals to the pin at launch — Myo never depends on
  a system-installed engine.
- **Platforms → all 5 targets from day one:** `windows-x86_64`, `linux-x86_64`,
  `linux-aarch64`, `macos-x86_64`, `macos-aarch64` (Windows x64 only — no
  Windows-on-ARM). The owner's test fleet spans all five. CI = 5-target matrix;
  mic-permission + WebRTC/AEC wired for macOS (CoreAudio), Linux (WebKitGTK), and
  Windows (WebView2).

---

## Out of scope for now (don't build these yet)

- **Fine-grained per-action approval** — the *next* control layer; the coarse
  four toggles ship first.
- **Multi-device / mesh (the hive)** — Slice 5.
- **Native renderers for *every* agent panel target** — render what we have and
  **gracefully ignore unknown event kinds** (log + skip); new ones are added by
  registering a renderer.
