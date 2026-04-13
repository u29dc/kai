> `kai` is a Rust-first local Telegram-to-Codex portal. `sdk` owns config, transport, runtime, state, and JSON contracts; `cli` stays thin over it.

## 1. Documentation

- Primary references: [`AGENTS.md`](AGENTS.md), [`Cargo.toml`](Cargo.toml), [`package.json`](package.json)
- Code entrypoints: [`crates/sdk/src/lib.rs`](crates/sdk/src/lib.rs), [`crates/cli/src/main.rs`](crates/cli/src/main.rs), [`crates/cli/src/handlers.rs`](crates/cli/src/handlers.rs)
- Use the `align` skill for repo baseline work and the `compose` skill for CLI contract changes

## 2. Repository Structure

```text
.
├── crates/
│   ├── sdk/                core types, defaults, envelopes, tool metadata
│   └── cli/                clap entrypoint over the sdk
├── .husky/                 local git hooks
└── AGENTS.md               canonical product, architecture, and repo-level agent instructions
```

- Start behavior changes in [`crates/sdk/`](crates/sdk/)
- Keep [`crates/cli/`](crates/cli/) as an adapter layer only
- Prefer nested module directories over giant files. Keep Rust source files under roughly 500 lines unless a stronger boundary argues otherwise.

## 3. Stack

| Layer | Choice | Notes |
| --- | --- | --- |
| Runtime | Rust 2024 workspace | `unsafe_code = "forbid"` at workspace level |
| CLI | `clap` + `serde_json` | JSON-first output by default |
| Tooling | Bun + Husky + Biome | repo tooling only; product runtime stays Rust |

## 4. Commands

- `bun install` installs repo tooling and activates Husky hooks
- `bun run util:check` is the repo completion gate
- `cargo run -p kai-cli -- tools` prints the current tool catalog during local development
- `cargo run -p kai-cli -- health` prints runtime readiness and remediation hints during local development
- `cargo run -p kai-cli -- config show` prints the effective config view during local development
- `~/.tools/kai/kai tools` is the installed release-binary tool catalog
- `~/.tools/kai/kai health` is the installed release-binary readiness surface
- `~/.tools/kai/kai service status` shows background service state
- `~/.tools/kai/kai service logs --tail 100` shows recent background logs
- `~/.tools/kai/kai service restart` reloads the background service after a rebuild or env change

## 5. Architecture

- [`crates/sdk/src/lib.rs`](crates/sdk/src/lib.rs) owns path defaults, JSON envelope types, health output, and tool metadata
- [`crates/cli/src/main.rs`](crates/cli/src/main.rs) parses subcommands and emits one JSON line to stdout
- [`crates/sdk/src/channel/telegram/`](crates/sdk/src/channel/telegram/) owns Telegram long-polling, owner filtering, typing status, native command-menu sync, queued follow-ups, fragment/media buffering, media intake, mobile commands, and outbound delivery
- [`crates/sdk/src/runtime/codex/`](crates/sdk/src/runtime/codex/) owns `codex exec` / `exec resume` orchestration, replay fallback, prompt shaping, and JSON-event parsing
- [`crates/sdk/src/state/`](crates/sdk/src/state/) owns SQLite state, queue persistence, in-flight recovery, replay package storage, processed-update caching, cleanup, and audit logging
- [`crates/sdk/src/service/`](crates/sdk/src/service/) owns macOS LaunchAgent lifecycle, Keychain-backed secret seeding, and runtime log inspection
- [`crates/sdk/src/media/`](crates/sdk/src/media/) owns attachment policy, transcription, and derived-media enrichment
- Default prompt mode is currently the lighter passthrough-style turn envelope; the heavier system-instruction wrapper remains code-gated for experimentation
- Outbound local file sending is explicit via `/send`; do not reintroduce assistant-text path scraping as an implicit delivery trigger
- Keep default output JSON-first; if text mode is added later, make it explicit and keep the default machine-readable contract stable
- Update [`AGENTS.md`](AGENTS.md) when command surface or architecture intent changes

## 6. Runtime and State

- Planned operator home is `~/.tools/kai`
- Runtime state lives under `~/.tools/kai/{state,logs,attachments}` by default unless `root_app` is overridden
- Telegram owner filtering, recovery pairing, Codex session continuity, durable queueing, media staging, and background service management are implemented in the current codebase
- Treat future local runtime files under `~/.tools/kai` as operator state; never commit them

## 7. Constraints

- Keep the workspace split to `sdk` and `cli` until a real boundary justifies more crates
- Keep CLI contracts aligned with [`AGENTS.md`](AGENTS.md)
- Treat [`.tmp/`](.tmp/), `target/`, and `node_modules/` as generated

## 8. Validation

- Required gate: `bun run util:check`
- Rust regression gate: `cargo test --workspace --release`
- Rust smoke check: `cargo run -p kai-cli -- tools`, `cargo run -p kai-cli -- health`, `cargo run -p kai-cli -- config show`
- Installed-binary smoke check: `~/.tools/kai/kai health`, `~/.tools/kai/kai service status`
