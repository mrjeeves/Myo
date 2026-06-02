# Contributing to Myo

Thanks for being here! Myo is early — the foundations (self-update + desktop
shell) are in, and the companion is being built on top per
[`docs/native-agent.md`](docs/native-agent.md). Issues, ideas, and PRs are all welcome.

## Dev setup

**Prerequisites:** [Rust](https://rustup.rs) (stable), [Node 20+](https://nodejs.org)
with [pnpm](https://pnpm.io). On **Linux**, install the WebKitGTK stack:

```sh
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev libsoup-3.0-dev \
  libjavascriptcoregtk-4.1-dev librsvg2-dev libayatana-appindicator3-dev
```

```sh
pnpm install && pnpm build   # frontend → dist/ (the Tauri crate embeds it at compile time)
cargo run -p myo             # launch the desktop window
cargo run -p myo -- update   # or drive the updater headless
```

> The frontend must be built (`pnpm build` produces `dist/`) **before** the
> Tauri crate will compile, since `dist/` is embedded into the binary.

## Before you push — the gates CI enforces

CI ([`.github/workflows/ci.yml`](.github/workflows/ci.yml)) runs all of these;
run them locally so your PR goes green on the first try:

```sh
cargo fmt --all                                      # format
cargo clippy --workspace --all-targets -- -D warnings # lint (warnings are errors)
cargo test --workspace                               # tests
pnpm check                                           # Svelte/TS type-check
```

## Project layout

| Path | What it is |
|---|---|
| `crates/myo-self-update` | The self-updater (Tauri-agnostic, unit-tested). |
| `src-tauri` | The `myo` binary — CLI + desktop shell. |
| `src` | Svelte 5 frontend. |
| `docs` | Direction, decisions, and engine-integration references. |

## Conventions

- **Keep PRs focused.** One change per PR; a clear title and a short "what / why."
- **Update docs + `CHANGELOG.md`** when behavior changes.
- **Match the surrounding style.** Rust is `rustfmt` + `clippy`-clean; tests live
  next to the code they cover.
- **Read the *why* before changing direction.** The product decisions are
  deliberate — see [`docs/decisions-and-rationale.md`](docs/decisions-and-rationale.md)
  and [`docs/native-agent.md`](docs/native-agent.md).
  In particular, Myo *is* the agent: the brain, memory, tools, and TTS are
  native (Rust), talking to MyOwnLLM for inference, ASR, and voice. Don't reach
  back for a Python brain sidecar — Odysseus is a code *reference*, not a
  dependency; harvest features by reimplementing them natively.

## Be kind

We follow the [Code of Conduct](CODE_OF_CONDUCT.md). Found a security issue?
Please **don't** open a public issue — see [SECURITY.md](SECURITY.md).
