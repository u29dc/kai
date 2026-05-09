> `kai` is a Rust-first local Telegram-to-agent portal with named workspaces and target-scoped session continuity. `sdk` owns config, transport, runtime, state, and JSON contracts; `cli` stays thin over it. `codex` is the only active runner today.

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
- `cargo run -p kai-cli -- config show` prints the effective config view during local development
- `cargo run -p kai-cli -- config migrate` rewrites legacy config into the workspace-based layout
- `cargo run -p kai-cli -- health` prints runtime readiness and remediation hints during local development
- `cargo run -p kai-cli -- workspace show` prints the current workspace selection and configured workspaces
- `cargo run -p kai-cli -- session show` prints session and runtime state for the current workspace target
- `cargo run -p kai-cli -- tools` prints the current tool catalog during local development
- `~/.tools/kai/kai health` is the installed release-binary readiness surface
- `~/.tools/kai/kai workspace show` prints the installed binary's active workspace view
- `~/.tools/kai/kai service status` shows background service state
- `~/.tools/kai/kai service restart` reloads the background service after a rebuild or env change
- `~/.tools/kai/kai service logs --tail 100` shows recent background logs

## 5. Architecture

- [`crates/sdk/src/lib.rs`](crates/sdk/src/lib.rs) owns path defaults, JSON envelope types, health output, and tool metadata
- [`crates/cli/src/main.rs`](crates/cli/src/main.rs) parses subcommands and emits one JSON object to stdout
- [`crates/sdk/src/workspace.rs`](crates/sdk/src/workspace.rs) resolves configured workspaces, current selection, and the execution target `{ workspace_id, working_dir, provider }`
- [`crates/sdk/src/channel/telegram/`](crates/sdk/src/channel/telegram/) owns Telegram long-polling, owner filtering, typing status, native command-menu sync, queued follow-ups, one active side query via `/ask`, fragment/media buffering, media intake, `/dir`, `/new`, `/send`, and outbound delivery
- [`crates/sdk/src/runtime/agent.rs`](crates/sdk/src/runtime/agent.rs) is the provider seam; it currently accepts `codex` and blocks `claude` until the adapter lands
- [`crates/sdk/src/runtime/codex/`](crates/sdk/src/runtime/codex/) owns the Codex transport layer: App Server JSON-RPC over stdio by default, `exec` / `exec resume` fallback support, replay fallback, prompt shaping, and event parsing for the selected workspace target
- [`crates/sdk/src/state/`](crates/sdk/src/state/) owns SQLite state, target-scoped queue persistence, in-flight recovery, per-workspace session and replay bindings, processed-update caching, cleanup, and audit logging
- [`crates/sdk/src/service/`](crates/sdk/src/service/) owns macOS LaunchAgent lifecycle, Keychain-backed secret seeding, and runtime log inspection
- [`crates/sdk/src/media/`](crates/sdk/src/media/) owns attachment policy, transcription, and derived-media enrichment
- Queued turns snapshot their execution target when enqueued; do not reintroduce late-bound global workspace lookup
- `/ask <prompt>` is a full-capability trusted-owner side query. It inherits the normal configured Codex policy, does not enter the main durable queue, does not overwrite the main session binding, and only one side query runs at a time.
- Outbound local file sending is explicit via `/send`; do not reintroduce assistant-text path scraping as an implicit delivery trigger
- Keep default output JSON-first; if text mode is added later, make it explicit and keep the default machine-readable contract stable
- Update [`AGENTS.md`](AGENTS.md) when command surface, workspace model, or architecture intent changes

## 6. Runtime and State

- Operator home defaults to `~/.tools/kai`
- Config lives at `~/.tools/kai/config.toml` unless `KAI_CONFIG_PATH` is set; `paths.root_app` controls runtime state root
- Runtime state lives under `~/.tools/kai/{state,logs,attachments}` by default unless `root_app` is overridden
- Named workspaces are mandatory config. Legacy configs with `paths.root_work` or `context_files.todo` must be rewritten with `kai config migrate`
- Global core context files are `context_files.soul` and `context_files.memory`; `TODO.md` is no longer a configured special file
- Selected workspace is runtime state. Session continuity and replay bindings are scoped per `{ provider, workspace_id }`, not one global session id
- In App Server mode, persisted `session_id` values are Codex thread ids bound per `{ provider, workspace_id }`
- Relative `/send` paths resolve against the selected workspace root or `root_app`
- Telegram owner filtering, recovery pairing, Codex session continuity, durable queueing, media staging, and background service management are implemented in the current codebase
- `runner.provider = "claude"` is config-valid but reserved and intentionally blocked in `run` and `health`; Codex is the only active runner
- `approval_policy = "never"` and network-capable Codex settings are intentional high-capability owner-portal defaults when configured through Codex override settings
- Treat future local runtime files under `~/.tools/kai` as operator state; never commit them

## 7. Constraints

- Keep the workspace split to `sdk` and `cli` until a real boundary justifies more crates
- Keep CLI contracts aligned with [`AGENTS.md`](AGENTS.md)
- Keep workspace-aware behavior centered on [`crates/sdk/src/workspace.rs`](crates/sdk/src/workspace.rs) and target-scoped state; do not reintroduce global `root_work` or global session singletons
- Keep global configured context limited to `SOUL` and `MEMORY` unless a new context model is intentionally designed
- Treat [`.tmp/`](.tmp/), `target/`, and `node_modules/` as generated

## 8. Validation

- Required gate: `bun run util:check`
- Rust regression gate: `cargo test --workspace --release`
- Rust smoke check: `cargo run -p kai-cli -- config show`, `cargo run -p kai-cli -- config migrate`, `cargo run -p kai-cli -- health`, `cargo run -p kai-cli -- workspace show`, `cargo run -p kai-cli -- session show`, `cargo run -p kai-cli -- tools`
- Installed-binary smoke check: `~/.tools/kai/kai health`, `~/.tools/kai/kai workspace show`, `~/.tools/kai/kai service status`
- Telegram smoke check after runtime changes: `/help`, `/status`, a normal queued message during an active turn, `/ask <prompt>` during an active turn, `/cancel`, optional `/cancel ask`, and `/send <path>`
