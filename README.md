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

A Cargo workspace (the seed of the `myo` orchestrator from `docs/PLAN.md`):

| Crate | What it is |
|---|---|
| [`crates/myo-self-update`](crates/myo-self-update) | Set-it-and-forget-it self-updater — GitHub-releases feed, SHA-256-verified download, atomic binary swap, background watcher. Fully unit-tested. |
| [`crates/myo`](crates/myo) | The `myo` binary: applies staged updates on launch, plus `myo update …` / `myo watch`. |

```sh
cargo test --workspace      # unit tests
cargo run -p myo -- --help  # CLI
```

**Auto-update** keeps Myo current with zero interaction — the update half of
the hands-free experience. Design, config, and the (shell-pending) GUI wiring:
[`docs/auto-update.md`](docs/auto-update.md).
