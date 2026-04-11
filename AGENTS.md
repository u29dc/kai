> `kai` is a Rust-first local Telegram-to-Codex portal. `sdk` owns config defaults and JSON contracts; `cli` stays thin over it.

## 1. Documentation

- Primary references: [`SPEC.md`](SPEC.md), [`Cargo.toml`](Cargo.toml), [`package.json`](package.json)
- Code entrypoints: [`crates/sdk/src/lib.rs`](crates/sdk/src/lib.rs), [`crates/cli/src/main.rs`](crates/cli/src/main.rs)
- Use the `align` skill for repo baseline work and the `compose` skill for CLI contract changes

## 2. Repository Structure

```text
.
├── crates/
│   ├── sdk/                core types, defaults, envelopes, tool metadata
│   └── cli/                clap entrypoint over the sdk
├── .husky/                 local git hooks
├── SPEC.md                 product and architecture source of truth
└── AGENTS.md               canonical repo-level agent instructions
```

- Start behavior changes in [`crates/sdk/`](crates/sdk/)
- Keep [`crates/cli/`](crates/cli/) as an adapter layer only

## 3. Stack

| Layer | Choice | Notes |
| --- | --- | --- |
| Runtime | Rust 2024 workspace | `unsafe_code = "forbid"` at workspace level |
| CLI | `clap` + `serde_json` | JSON-first output by default |
| Tooling | Bun + Husky + Biome | repo tooling only; product runtime stays Rust |

## 4. Commands

- `bun install` installs repo tooling and activates Husky hooks
- `cargo run -p kai-cli -- tools` prints the current placeholder tool catalog
- `cargo run -p kai-cli -- health` prints the current placeholder health report
- `cargo run -p kai-cli -- config show` prints detected default roots
- `bun run util:check` is the completion gate for the current bootstrap

## 5. Architecture

- [`crates/sdk/src/lib.rs`](crates/sdk/src/lib.rs) owns path defaults, JSON envelope types, health output, and tool metadata
- [`crates/cli/src/main.rs`](crates/cli/src/main.rs) parses subcommands and emits one JSON line to stdout
- Keep default output JSON-first; if text mode is added later, make it explicit and keep the default machine-readable contract stable
- Update [`SPEC.md`](SPEC.md) when command surface or architecture intent changes

## 6. Runtime and State

- Planned operator home is `~/.tools/kai`
- Current bootstrap computes default roots but does not yet implement Telegram, Codex session management, or durable state
- Treat future local runtime files under `~/.tools/kai` as operator state; never commit them

## 7. Constraints

- Keep the workspace split to `sdk` and `cli` until a real boundary justifies more crates
- Keep CLI contracts aligned with [`SPEC.md`](SPEC.md)
- Treat [`.tmp/`](.tmp/), `target/`, and `node_modules/` as generated

## 8. Validation

- Required gate: `bun run util:check`
- Rust smoke check: `cargo run -p kai-cli -- tools`, `cargo run -p kai-cli -- health`, `cargo run -p kai-cli -- config show`
