<div align="center">

<img src="assets/logo.png" alt="Myo" width="120" height="120" />

# Myo

**Your own AI — not an app with an AI inside it.**

Voice-first, local-first, and it keeps itself up to date.

[![CI](https://github.com/mrjeeves/Myo/actions/workflows/ci.yml/badge.svg)](https://github.com/mrjeeves/Myo/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)
[![Release](https://img.shields.io/github/v/release/mrjeeves/Myo?include_prereleases&sort=semver&label=release)](https://github.com/mrjeeves/Myo/releases)
![Built with Rust + Tauri 2](https://img.shields.io/badge/built%20with-Rust%20%2B%20Tauri%202-dea584)
![Platforms](https://img.shields.io/badge/platforms-Linux%20·%20macOS%20·%20Windows-555)

</div>

---

Myo is an AI companion you **talk to**. Open-mic voice in, a streamed spoken
answer back, and a UI that gets out of the way — surfaces appear from what the
agent is doing instead of screens you steer. Your voice, your memory, and your
files never leave the device.

**Myo is the agent.** Its agent loop, persona, memory, and tools are built
natively in Rust. For the work it shouldn't reinvent — per-device model
selection, inference, and speech — it runs one local service,
[MyOwnLLM](https://github.com/mrjeeves/MyOwnLLM), over loopback HTTP. No Python
brain to babysit, and nothing in the cloud.

> **Status: early, but real.** A full spoken turn already runs end to end:
> streaming dictation → a native, tool-using answer → native voice, with
> barge-in and memory that carries across days. See the [roadmap](#roadmap).

## Features

- **Voice-first** — talk, and interrupt by talking. Touch and type are there when you want them, never in the way.
- **Local-first** — always-on audio, memory, and files stay on device by design.
- **Myo is the agent** — the brain, memory, and the tool loop (`shell`, file read/write, keyless web search) are native Rust, not a sidecar to manage.
- **Dissolved UI** — streamed answers, a live tool feed, and editable document artifacts materialize from the work itself.
- **Memory that lasts** — a working + long-term store (SQLite + embeddings) recalled into every turn, with **Dream mode** consolidating aging memories during downtime. Review, pause, or forget any of it.
- **Honest model progress** — the first time a model is fetched, you get a live, inline download-and-load bar instead of a frozen wait.
- **Self-updating** — checks, SHA-256-verifies, and applies on the next launch. → [how it works](docs/auto-update.md)

## Install

> Prebuilt one-line installers ship with the first tagged release. Until then,
> [build from source](#build-from-source) — it's two commands.

**macOS / Linux**

```sh
curl -fsSL https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.sh | sh
```

**Windows** (PowerShell)

```powershell
irm https://raw.githubusercontent.com/mrjeeves/Myo/main/scripts/install.ps1 | iex
```

The installer drops a single `myo` binary on your `PATH`. After that, Myo keeps
itself current — you never run the installer again.

## Usage

```sh
myo                 # open the desktop window
myo update          # update now: check → download → verify → apply
myo update status   # version, channel, install kind, pending update
myo --version
```

Every launch first applies any update a previous run staged, so upgrades are
hands-free. Pause it anytime with `myo update disable` (or `MYO_AUTOUPDATE=0`);
installed via Homebrew / apt / MSI, Myo steps aside for your package manager.

## How it works

A Cargo workspace plus a Svelte 5 desktop shell (Tauri 2).

- [**`crates/myo-core`**](crates/myo-core) is the agent: it streams MyOwnLLM's
  chat completions into a normalized `myo://` event stream the UI renders, runs
  the memory-aware converse loop and native tool loop, and owns the two-layer
  memory plus Dream-mode consolidation. Tauri-agnostic and unit-tested.
- The [**`myo` binary**](src-tauri) is both the CLI and the desktop shell: it
  supervises the engine, bridges `myo://` events to the WebView, and exposes the
  Core API the frontend calls.
- It bundles a pinned **MyOwnLLM** engine as a sidecar and talks to it over
  loopback (`/v1/chat/completions`, `/v1/embeddings`, `/v1/audio/*`).

Go deeper: [`native-agent.md`](docs/native-agent.md) (the blueprint),
[`shell.md`](docs/shell.md) (what's wired today), and
[`myownllm-integration.md`](docs/myownllm-integration.md) (the engine seam).

## Build from source

**Prerequisites:** [Rust](https://rustup.rs) (stable) and
[Node 20+](https://nodejs.org) with [pnpm](https://pnpm.io). On **Linux**, the
WebKitGTK stack:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev
```

```sh
pnpm install && pnpm build   # frontend → dist/ (embedded into the binary)
cargo run -p myo             # launch the desktop window
cargo test --workspace       # run the test suite
```

Myo bundles a pinned MyOwnLLM engine (`.myownllm-rev` +
[`src-tauri/build.rs`](src-tauri/build.rs)); in dev it picks up a sibling
`../MyOwnLLM` checkout or downloads the pinned release.

## Project layout

| Path | What it is |
|---|---|
| [`crates/myo-core`](crates/myo-core) | The agent core — native brain, memory, the converse + tool loops, Dream-mode consolidation, and engine-supervision specs. Tauri-agnostic, unit-tested. |
| [`crates/myo-self-update`](crates/myo-self-update) | The self-updater — release feed, SHA-256 verify, atomic swap, background watcher. Tauri-agnostic, unit-tested. |
| [`src-tauri`](src-tauri) | The `myo` binary — CLI and desktop shell (Tauri 2): Core API commands, engine supervisor, and the `myo://` event bridge. |
| [`src`](src) | Svelte 5 frontend — the dissolved-UI surfaces wired to the Core API in [`src/lib`](src/lib). |
| [`docs`](docs) | The direction, the decisions, what's wired today, and the engine-integration references. |

## Roadmap

The foundations are down; the companion is real and growing.

- [x] Native agent brain — streams MyOwnLLM chat into the `myo://` protocol, persona + memory-backed context
- [x] Dissolved-UI renderer — streamed answers, live tool feed, editable document artifacts
- [x] Conversation spine — open-mic streaming dictation, native TTS (WebSpeech fallback), barge-in
- [x] Native memory — working + long-term recall, review / forget / incognito, Dream-mode consolidation
- [x] Native tool loop — `shell`, file read/write, keyless web search, plus `remember` / `recall`
- [x] Self-update engine + `myo update` CLI + background watcher
- [ ] Reach-out + deep research — email/calendar actions and a multi-step research tool
- [ ] Fine-grained, per-action approval
- [ ] Hive — multiple installs over a local mesh
- [ ] iOS & Android

See [`native-agent.md`](docs/native-agent.md) for the blueprint and
[`decisions-and-rationale.md`](docs/decisions-and-rationale.md) for the *why*.

## Contributing

PRs and ideas are welcome — start with [CONTRIBUTING.md](CONTRIBUTING.md). We
follow a [Code of Conduct](CODE_OF_CONDUCT.md), and security reports go through
[SECURITY.md](SECURITY.md).

## License

[MIT](LICENSE) © Myo contributors.
