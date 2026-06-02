<div align="center">

<img src="assets/logo.png" alt="Myo" width="132" height="132" />

# Myo

### Your own AI — not an app with an AI inside it.

**Voice-first** · **local-first** · and it **keeps itself up to date**.

[![CI](https://github.com/mrjeeves/Myo/actions/workflows/ci.yml/badge.svg)](https://github.com/mrjeeves/Myo/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/mrjeeves/Myo?include_prereleases&sort=semver&label=release)](https://github.com/mrjeeves/Myo/releases)
![Built with Rust + Tauri 2](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri%202-dea584)
![Platforms](https://img.shields.io/badge/platforms-Linux%20·%20macOS%20·%20Windows-555)

</div>

---

Myo is a local AI companion you **talk to**. It runs entirely on your machine
and lets the interface **dissolve into the conversation**: you mostly just talk
and watch it work. Your voice, your memory, your files — nothing leaves the
device.

**Myo *is* the agent.** The brain (its agent loop + persona), memory, the tool
loop, and the voice are built **natively** into Myo (Rust). For the heavy
lifting it shouldn't reinvent, it runs one local service —
**[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM)** — for per-device model
selection, LLM inference, and speech (ASR + TTS), reached over loopback HTTP
(`/v1/chat/completions`, `/v1/embeddings`, `/v1/audio/*`). Each install is a
self-contained **mini-myo**; installs find each other over a local mesh and
share as a **hive**.

> 🌱 **Status: early, but real — and it talks back.** A full spoken turn already
> runs end-to-end with **no brain sidecar**: open-mic **streaming dictation**
> (MyOwnLLM ASR) → Myo's **native** agent answer (streamed) → **native TTS**
> (MyOwnLLM's hardware-tiered Kokoro/Piper voice, WebSpeech fallback), with
> barge-in — all rendered as a **dissolved UI**: streamed answers, a live tool
> feed, editable document artifacts, the four capability toggles, and a memory
> surface you can review and forget. The direction + roadmap live in
> **[`docs/native-agent.md`](docs/native-agent.md)**; what's wired right now is in
> **[`docs/shell.md`](docs/shell.md)**. ⭐ Star it to watch a companion grow up.

## ✨ Why Myo

- 🗣️ **Voice-first** — talk, and interrupt by talking. Touch and type are there when you want them, never in the way.
- 🔒 **Local-first** — on-device by design. Always-on audio and memory never leave your machine.
- 🧠 **Myo *is* the agent** — the brain, memory, and tools are native Rust; MyOwnLLM provides inference, ASR, and voice. No Python brain sidecar to babysit.
- 🪟 **Dissolved UI** — surfaces *materialize* from what the agent is doing, instead of you steering fixed screens.
- 🧵 **A continuous presence** — one ongoing relationship that remembers across days, with memory you can see, pause, and forget.
- 🐝 **Hive-ready** — each install is a self-contained mini-myo; installs network over a local mesh and share as a hive (coming).
- ♻️ **Set-it-and-forget-it updates** — checks, **SHA-256-verifies**, stages, and applies on next launch. No installers to babysit. → [how it works](docs/auto-update.md)

## 🚀 Install

> 📦 Prebuilt one-line installers land with the **first tagged release**. Until
> then, [build from source](#%EF%B8%8F-build-from-source) — it's two commands.

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.ps1 | iex
```

The installer drops a single `myo` binary on your `PATH`. After that, Myo keeps
**itself** current — you never run the installer again.

## 🎛️ Usage

```sh
myo                 # open the desktop window
myo update          # update now: check → download → verify → apply
myo update status   # version, channel, install kind, pending update
myo watch           # run the background update-checker in the foreground
myo --version
```

Every launch first applies any update a previous run staged — so upgrades are
**hands-free**. Prefer to drive it yourself? `myo update`. Want it to pause?
`myo update disable` (or set `MYO_AUTOUPDATE=0`). Installed via Homebrew / apt /
MSI? Myo notices and steps aside for your package manager.

## ♻️ How it stays current

A background watcher polls GitHub releases (gated to a gentle cadence), picks
the right binary for your platform, **verifies its SHA-256**, and stages it.
The next launch atomically swaps it in — Myo never restarts itself out from
under you. It's a faithful port of MyOwnLLM's battle-tested updater, extracted
into a small, reusable, fully unit-tested crate. Full design, config knobs, and
the desktop Updates panel: **[`docs/auto-update.md`](docs/auto-update.md)**.

## 🛠️ Build from source

**Prerequisites:** [Rust](https://rustup.rs) (stable), [Node 20+](https://nodejs.org)
with [pnpm](https://pnpm.io), and — on **Linux** — the WebKitGTK stack:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev
```

```sh
pnpm install && pnpm build   # frontend → dist/ (embedded into the binary)
cargo run -p myo             # launch the desktop window
cargo test --workspace       # run the test suite
```

> Myo bundles a pinned **MyOwnLLM** engine as a sidecar (`.myownllm-rev` +
> [`src-tauri/build.rs`](src-tauri/build.rs)); in dev it picks up a built sibling
> `../MyOwnLLM` checkout or downloads the pinned release. See
> [`docs/shell.md`](docs/shell.md).

## 🧩 Layout

A Cargo workspace + Svelte 5 frontend:

| Path | What it is |
|---|---|
| [`crates/myo-core`](crates/myo-core) | The agent core — Myo's **native brain** (`llm.rs`: streams MyOwnLLM chat → a normalized `myo://` intent stream), the converse round-trip (ASR → brain → TTS), capability mapping, and engine-supervision specs. Tauri-agnostic, fully unit-tested. |
| [`crates/myo-self-update`](crates/myo-self-update) | The self-updater — release feed, SHA-256 verify, atomic swap, background watcher. Tauri-agnostic, fully unit-tested. |
| [`src-tauri`](src-tauri) | The `myo` binary — CLI **and** desktop shell (Tauri 2): the Core API commands, the engine supervisor, and the `myo://` event bridge. |
| [`src`](src) | Svelte 5 frontend — the dissolved-UI surfaces (Presence, Conversation, Activity, DocumentArtifact, Control, Memory) wired to the Core API in [`src/lib`](src/lib). |
| [`docs`](docs) | The direction ([`native-agent.md`](docs/native-agent.md)), the decisions, what's-wired-today ([`shell.md`](docs/shell.md)), and the engine-integration references. |

## 🗺️ Roadmap

The foundations are down; the companion is real and growing.

- [x] Self-update engine + `myo update` CLI + background watcher
- [x] Desktop shell (Tauri 2 + Svelte 5) + Settings → Updates panel
- [x] 5-target release pipeline + one-line installers
- [x] 🧠 **Native agent brain** — `crates/myo-core` (`llm.rs`): streams MyOwnLLM chat into the `myo://` intent protocol; persona + running history. No Odysseus.
- [x] 🪟 **Dissolved-UI surface renderer** — streamed answers, live tool feed, editable document artifacts, agent-driven panels
- [x] 🎛️ **Capability + Memory surfaces** — the four Web/Files/Code/Reach-out toggles, plus review/forget/incognito
- [x] 🗣️ **Conversation spine** — brain→voice round-trip (**native TTS** via MyOwnLLM `/v1/audio/speech`, WebSpeech fallback) with barge-in/cancel
- [x] 👂 **Open-mic voice input** — streaming dictation + clip fallback (MyOwnLLM ASR), full-duplex
- [ ] 🧠 **Native memory store** — local SQLite + embeddings recall (Slice 2)
- [ ] 🧰 **Native tool loop** — Web/Files/Code/Reach-out actions behind the toggles (Slice 4)
- [ ] ✋ Fine-grained per-action approval
- [ ] 🐝 **Hive** — multi-device over a local mesh (Slice 5)
- [ ] 📱 **iOS & Android** (mobile targets — coming)

See **[`docs/native-agent.md`](docs/native-agent.md)** for the blueprint and
**[`docs/decisions-and-rationale.md`](docs/decisions-and-rationale.md)** for the *why*.

## 🤝 Contributing

PRs and ideas welcome — start with **[CONTRIBUTING.md](CONTRIBUTING.md)**. Please
keep it kind: we follow a **[Code of Conduct](CODE_OF_CONDUCT.md)**. Found a
security issue? See **[SECURITY.md](SECURITY.md)**.

## 📄 License

[MIT](LICENSE) © Myo contributors.

<div align="center"><sub>Built to be <em>yours</em> — it listens, it remembers, and it grows up with you. 🎙️</sub></div>
