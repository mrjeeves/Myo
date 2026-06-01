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

Myo is a local AI companion you **talk to**. It runs entirely on your machine,
composes a few best-in-class local-AI projects as swappable *senses* — a brain,
ears, a voice — behind one thin shell, and lets the interface **dissolve into
the conversation**: you mostly just talk and watch it work. Your voice, your
memory, your files. Nothing leaves the device.

> 🌱 **Status: early, but real.** The foundations are in — a rock-solid
> **self-updater** (so Myo stays current with zero fuss) and the **desktop
> shell**. The voice spine, on-device speech, and the agent brain are next; the
> whole blueprint lives in **[`docs/PLAN.md`](docs/PLAN.md)**. ⭐ Star it to
> watch a companion grow up.

## ✨ Why Myo

- 🗣️ **Voice-first** — talk, and interrupt by talking. Touch and type are there when you want them, never in the way.
- 🔒 **Local-first** — on-device by design. Always-on audio and memory never leave your machine.
- 🧠 **Composes, doesn't reinvent** — a real agent brain (tools, RAG, memory), excellent ASR, and a voice — orchestrated, not rebuilt.
- 🪟 **Dissolved UI** — surfaces *materialize* from what the agent is doing, instead of you steering fixed screens.
- 🧵 **A continuous presence** — one ongoing relationship that remembers across days, with memory you can see, pause, and forget.
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

## 🧩 Layout

A Cargo workspace + Svelte 5 frontend (the seed of the orchestrator in `docs/PLAN.md`):

| Path | What it is |
|---|---|
| [`crates/myo-self-update`](crates/myo-self-update) | The self-updater — release feed, SHA-256 verify, atomic swap, background watcher. Tauri-agnostic, fully unit-tested. |
| [`src-tauri`](src-tauri) | The `myo` binary — CLI **and** desktop shell (Tauri 2). |
| [`src`](src) | Svelte 5 frontend; the Settings → Updates panel is [`src/surfaces/UpdatesSection.svelte`](src/surfaces/UpdatesSection.svelte). |
| [`docs`](docs) | The plan, the decisions, and the engine-integration contracts. |

## 🗺️ Roadmap

The foundations are down; the companion is being built on top.

- [x] Self-update engine + `myo update` CLI + background watcher
- [x] Desktop shell (Tauri 2 + Svelte 5) + Settings → Updates panel
- [x] 5-target release pipeline + one-line installers
- [ ] 🗣️ Voice spine — open-mic, barge-in, full-duplex
- [ ] 👂 On-device ASR / diarization (`myo-asr`)
- [ ] 🧠 Agent brain — tools, RAG, continuous memory
- [ ] 🪟 Dissolved-UI surface renderer
- [ ] 📱 **iOS & Android** (mobile targets — coming)
- [ ] 🌐 Multi-device over a local mesh

See **[`docs/PLAN.md`](docs/PLAN.md)** for the full blueprint and
**[`docs/decisions-and-rationale.md`](docs/decisions-and-rationale.md)** for the *why*.

## 🤝 Contributing

PRs and ideas welcome — start with **[CONTRIBUTING.md](CONTRIBUTING.md)**. Please
keep it kind: we follow a **[Code of Conduct](CODE_OF_CONDUCT.md)**. Found a
security issue? See **[SECURITY.md](SECURITY.md)**.

## 📄 License

[MIT](LICENSE) © Myo contributors.

<div align="center"><sub>Built to be <em>yours</em> — it listens, it remembers, and it grows up with you. 🎙️</sub></div>
