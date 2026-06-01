# Myo
My own agent.

## Planning docs

The design for Myo lives in [`docs/`](docs/) — **start with
[`docs/getting-started.md`](docs/getting-started.md)**, then
[`docs/PLAN.md`](docs/PLAN.md) (the master spec) and
[`docs/decisions-and-rationale.md`](docs/decisions-and-rationale.md).
The `*-integration.md` files are verified HTTP/SSE + code contracts for the
three engines Myo composes (Odysseus, MyOwnLLM, MyOwnMesh).

> These docs were authored as a self-contained handoff package and originally
> parked on a MyOwnLLM branch only because this repo wasn't writable at the
> time. They are imported here verbatim; any "this lives on a MyOwnLLM branch /
> transfer vehicle" framing inside them is now historical.

## Codebase

A Cargo workspace + Svelte 5 frontend (the seed of the `myo` orchestrator from
`docs/PLAN.md`, laid out like MyOwnLLM):

| Path | What it is |
|---|---|
| [`crates/myo-self-update`](crates/myo-self-update) | Set-it-and-forget-it self-updater — GitHub-releases feed, SHA-256-verified download, atomic binary swap, background watcher. Tauri-agnostic, fully unit-tested. |
| [`src-tauri`](src-tauri) | The `myo` binary — CLI **and** desktop shell (Tauri 2). Applies staged updates on launch; `myo update …` / `myo watch`; hosts the update commands. |
| [`src`](src) | Svelte 5 frontend. The **Settings → Updates** panel lives in [`src/surfaces/UpdatesSection.svelte`](src/surfaces/UpdatesSection.svelte). |

```sh
pnpm install && pnpm build   # frontend → dist/ (embedded into the binary)
cargo run -p myo             # desktop window
cargo run -p myo -- update   # or drive the updater headless
cargo test --workspace       # unit tests
```

Linux needs the webkit/gtk/soup dev packages (see `docs/auto-update.md`); CI
installs them and builds the whole engine → Tauri command → Svelte path.

**Auto-update** keeps Myo current with zero interaction — the update half of the
hands-free experience. Full design, config, and GUI wiring:
[`docs/auto-update.md`](docs/auto-update.md).
