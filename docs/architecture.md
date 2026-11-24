# Architecture

argus is a std-only Rust workspace plus a TypeScript UI. See
[design/RATIONALE.md](design/RATIONALE.md) for why it is built this way.

## Crates

| Crate | Holds | May depend on |
|---|---|---|
| `argus-log` | Per-actor append logs: framing, locking, torn-tail repair, stamps | nothing |
| `argus-json` | The JSON reader and writer | nothing |
| `argus-core` | The domain: events, sources, adapters, views, policy, plans, the ledger | `argus-log`, `argus-json` |
| `argus-api` | The op list, view models, the wire format, and the generated TS | `argus-json` |
| `argus-host` | The one process that owns a store: runs, git, PTYs, tailers, subscriptions | all of the above |
| `argus-cli` | The `argus` command, a client of the API like the apps | `argus-json`, `argus-api` |

`argus-app` (Tauri) is outside the workspace because it needs crates.io. The
architecture test reads `Cargo.lock` and fails on any other edge or on a
third-party crate in the workspace.

## Layers of argus-core

A top-level module of `argus-core` may use only modules in earlier layers.
`crates/argus-core/tests/architecture.rs` checks this on every test run, so
the module graph has no cycles. `lib.rs` is the facade above every layer;
test code is not checked.

| Layer | Modules | What they hold |
|---|---|---|
| L0 | `ids`, then `period`, `models` | Ids, paths, and time values; periods and time windows; agent CLIs and model presets |
| L1 | `source`, `diff`, `event`, then `fact`, `command` | What is recorded: decoded observations, diff hunks, authored events, facts; the plain command types the host executes |
| L2 | `adapters` | One module per external tool: how to invoke it and how to read what it wrote |
| L3 | `views` | Projections: pure folds over facts |
| L4 | `policy`, `plan` | What to do next from the views; a run's purpose turned into a command |
| L5 | `codec`, then `ledger`, then `store` | Encoding events and headers; the per-job ledger; the store of ledgers |

`views`, `policy`, `plan`, and `diff` are pure: the test also fails when they
use files, the environment, threads, processes, the network, or the clock.
They take `now` as an argument.

When a lower module needs something from a higher one, change the design
instead of the check: move what both need down, or let the caller pass it in.
For example, `Window` lives in `period` so that events can name it, and
`summary_prompt` lives in `policy`, which reads the timeline, instead of in
`adapters::llm`.

## Wrappers in argus-host

The host reads the clock only in `clock.rs` and builds child processes only
in `proc.rs`. `clippy.toml` bans `SystemTime::now`, `Instant::now`,
`Command::new`, `env::var`, and `process::exit` everywhere else; the wrappers
carry `#[expect(clippy::disallowed_methods, reason = ...)]`. Configuration
such as the home directory comes in through `HostConfig`.
