# kai

Status: draft  
Date: 2026-04-11  
Scope: spec only, no implementation commitments beyond repo bootstrap

## 1. Purpose

`kai` is a private, owner-only, local Telegram portal into an operator's existing Codex CLI workflow on their machine.

It exists to let the owner message their machine and reach the same Codex operating mode they already use locally, without rebuilding a full agent platform, multi-user gateway, plugin ecosystem, or custom model backend. `kai` is not conceptually limited to one notes directory, project repo, or knowledge base, even if one of those becomes a common override for `root_work`.

`kai` is not "my version of OpenClaw" in the product sense. It is a much narrower tool:

- one owner
- one primary conversation
- one local machine
- one backend family in v1: Codex CLI
- one primary channel in v1: Telegram
- one runtime stance in v1: reactive portal, not proactive agent

The project goal is not maximum capability and not maximum restriction. The goal is a small system that mirrors real Codex usage, stays easy to keep running, and remains aligned with a Rust-first tooling style.

The practical baseline alternative is "SSH or Tailscale into the Mac from the phone and use Codex directly in a terminal." `kai` only makes sense if it beats that baseline on phone ergonomics.

## 2. Inputs and Research Basis

This spec is based on:

- the project chat transcript and handover material
- the rollout log
- the `align` skill
- the `compose` skill
- local Rust agent/tooling repos used as style references, especially `tao`, `cho`, `fin`, and `let`
- deep inspection of `microclaw`
- deep inspection of `zeroclaw`
- deep inspection of `openclaw`
- deep inspection of `opencrust`
- deep inspection of `ironclaw`
- the OpenClaw DeepWiki pages
- the Every transcript about internal AI-agent deployment
- direct local verification of `codex exec`, `codex exec resume`, and Codex sandbox behavior
- inspection of the current Codex config in `dot/agents/codex.toml`

The important local verification results are:

- `codex exec resume <session-id>` works in headless mode and can continue a prior session
- Codex supports `read-only`, `workspace-write`, and `danger-full-access` sandbox modes
- `--add-dir` grants extra writable directories alongside the main workspace
- binaries under `~/.tools/...` may not be on PATH inside Codex, but explicit absolute paths are executable
- the current Codex setup defaults to `approval_policy = "never"` and `sandbox_mode = "danger-full-access"`

That last point matters for custom local binaries, but it should not force a heavy config model. Optional explicit paths are useful for non-PATH tools; they should not become a mandatory allowlist concept.

## 3. Product Thesis

The right v1 is a local daemon that:

- receives a message from the owner on Telegram
- normalizes it into a structured turn
- stages any attachments locally
- reads any configured external context files such as `SOUL`, `MEMORY`, and `TODO`
- appends the turn to a persistent local session journal
- resumes the existing Codex session when possible
- falls back to a fresh Codex session with replay when necessary
- returns a concise reply back to Telegram
- does not run unless the owner messages it

Everything else is secondary.

The product comparison is not against full claw ecosystems first. It is against mobile terminal access. `kai` should win by being:

- conversational instead of terminal-driven
- async and message-native
- attachment-friendly
- owner-only
- stateful across turns without requiring terminal session management on a phone

The broad claw ecosystems succeed by handling many channels, many users, many tools, many policies, and many runtime modes. `kai` should learn from those patterns while refusing their scope.

## 4. Non-Goals

These are explicitly out of scope for v1:

- multi-user support
- public bots or group-chat support
- webhook-first deployment
- hosted gateway or control plane
- backend/provider abstraction beyond Codex CLI
- plugin marketplace or third-party skill ecosystem
- proactive routines, schedulers, heartbeats, or cron agents
- autonomous background work that runs without a user message
- general shell execution outside Codex's own runtime
- a second approval or policy engine layered on top of Codex
- voice, video, and rich outbound media workflows
- WhatsApp support in the first shipping cut

If a feature pushes `kai` toward "agent platform" instead of "personal bridge", it should be rejected by default.

## 5. Research Synthesis

### `microclaw`

Borrow:

- pragmatic setup-wizard thinking
- Codex auth reuse pattern
- persistent session handling
- Telegram allowlist pattern

Reject or defer:

- broad multi-channel scope
- scheduler, memory, and reflector surface
- larger runtime feature set than `kai` needs

Interpretation: `microclaw` is the nearest implementation cousin, but still too broad. It shows that a Rust-first local agent can be practical. It does not justify inheriting the rest of its feature surface.

### `zeroclaw`

Borrow:

- explicit policy vocabulary
- pairing and allowlist concepts
- autonomy-level framing
- clear channel abstraction boundaries

Reject or defer:

- gateway and control-plane shape
- broad productization
- platform-level policy complexity

Interpretation: useful as a safety and config-language reference, not as a repo template.

### `openclaw`

Borrow:

- strong channel policy patterns
- pairing flow concepts
- Telegram long-polling defaults
- media staging and path-safety ideas
- persisted resume identity handling

Reject or defer:

- plugin and ACP control-plane complexity
- multi-session routing complexity
- broad runtime and product surface

Interpretation: `openclaw` is the best source for channel and safety patterns, but the wrong operational scale for `kai`.

### `opencrust`

Borrow:

- pragmatic Rust implementation patterns
- clear WhatsApp split between Cloud API and WhatsApp Web
- session-summary persistence ideas
- setup-wizard focus on practical onboarding

Reject or defer:

- scheduled jobs
- hot-reload config machinery
- broad channel matrix
- personality/runtime features unrelated to `kai`

Interpretation: `opencrust` is useful when a smaller, Rust-native example is needed, especially around channel implementation tradeoffs.

### `ironclaw`

Borrow:

- seriousness about security documentation
- explicit attachment caps and validation
- conservative defaults around approval context

Reject or defer:

- webhook and heavy infra assumptions
- WASM/plugin runtime
- routine and memory complexity
- database-heavy shape

Interpretation: `ironclaw` is philosophically aligned on safety, but architecturally too heavy for this project.

## 6. Channel Decision

V1 channel: Telegram only, using long-polling.

Reasons:

- no inbound ports
- no public webhook exposure
- official bot API
- mature Rust ecosystem via `teloxide`
- owner-only filtering is straightforward
- setup is materially simpler than WhatsApp

WhatsApp stays out of v1.

The reference ecosystem confirms two viable WhatsApp paths:

- WhatsApp Cloud API: official, but requires webhook infrastructure and heavier onboarding
- WhatsApp Web: much simpler QR onboarding, but operationally fragile and carries account-risk concerns

That means WhatsApp is real, but not free. `kai` should keep a clean channel adapter boundary so WhatsApp can be added later without contaminating the v1 architecture.

## 7. Backend Decision

V1 backend: Codex CLI only.

Reasons:

- the operator already uses it
- auth and billing already exist
- `codex exec` and `codex exec resume` are directly usable
- local behavior has already been verified
- avoiding provider abstraction reduces surface area and risk

`kai` is a harness around an existing agent runtime, not a new orchestration engine. V1 should not try to support Claude Code, raw APIs, or pluggable model providers.

## 8. Core Product Decisions

### 8.1 Owner-only

`kai` is single-owner by design.

- one owner record
- one primary DM channel
- no groups
- no shared access
- unknown senders are rejected before any backend call

Reference projects support pairing, allowlists, and open/public modes. `kai` only needs owner-only.

### 8.2 One primary session

`kai` should maintain one primary persistent conversational session for the owner.

- every inbound message joins a single serialized queue
- only one Codex turn runs at a time
- no concurrent turns into the same session
- new messages received during an active turn are queued

This avoids race conditions, session corruption, and control-plane complexity.

### 8.3 Reactive-only by default

The default operating mode is reactive, not proactive.

- `kai` responds only to direct inbound messages from the owner
- no cron
- no scheduled routines
- no unattended background loops
- no autonomous task starting outside a user-triggered turn

This is the main safety boundary in v1.

### 8.4 Codex operating mode mirrors real usage

V1 should behave like a remote portal into the operator's existing Codex setup, not a separate restricted runtime.

- default Codex posture is full access
- approval prompts are not part of the v1 UX
- writes are allowed because the underlying Codex session allows them
- `kai` adds transport, continuity, logging, setup, and attachment handling, not a second mutation gate

This makes the product simpler and truer to the user's stated intent, but it also means the safety model must be described honestly.

## 9. Safety Model

`kai` should not rebuild OpenClaw-style tool policy machinery or pretend to offer strong isolation while running Codex in full-access mode.

Instead, v1 safety comes from four layers.

### 9.1 Channel boundary

- only paired or configured owner traffic is accepted
- non-owner traffic is ignored
- no public interaction surface beyond the bot receiving messages

### 9.2 Reactive execution

- no background jobs
- no periodic agents
- no automatic follow-ups
- one inbound message produces one queued Codex turn

This reduces risk more than a cosmetic wrapper policy would.

### 9.3 Honest runtime boundary

V1 assumes Codex is running in the same full-access posture the operator already uses locally.

- `approval_policy = "never"`
- `sandbox_mode = "danger-full-access"`
- no mobile approval relay
- no claimed filesystem blacklist enforcement

If `kai` launches Codex this way, then `kai` cannot honestly promise hard path allowlists or blacklists. Any such restriction would be advisory unless Codex itself is moved into a restricted sandbox or an external OS/container boundary is introduced.

The practical implication is simple:

- if we want a full-access portal, we accept that it is a full-access portal
- if we later want hard boundaries, we must change the runtime model rather than describing soft rules as hard guarantees

### 9.4 Soft guardrails

V1 may still add a small number of convenience safeguards, but they must be described as soft guardrails rather than enforcement.

Candidate soft guardrails:

- reject explicit user requests that ask for `sudo`
- reject requests that clearly ask `kai` to daemonize or schedule itself
- keep audit logs outside the conversational transcript
- avoid leaking secrets into user-visible replies

These are useful, but they are not a sandbox.

### 9.5 Optional custom tool paths

Some custom local binaries may need explicit absolute paths because they are not reliably on PATH inside Codex.

This should be treated as an optional convenience mechanism, not an allowlist.

Examples:

- `~/.tools/tao/tao`
- `~/.tools/cho/cho`
- `~/.tools/fin/fin`
- `~/.tools/let/let`

If a tool already works through normal PATH inheritance, `kai` should not require extra config for it.

## 10. Session Continuity

Session continuity has two layers.

### 10.1 Primary continuity

Primary continuity uses the real Codex session:

- first turn creates a Codex session
- `kai` stores the session id
- later turns use `codex exec resume <session-id>`

### 10.2 Durable continuity

Durable continuity belongs to `kai`, not Codex.

`kai` must persist enough local state to survive:

- process restarts
- machine reboots
- Codex resume failures
- auth instability
- stale or lost backend session ids

Required persisted state:

- owner identity
- active Codex session id
- turn log
- attachment metadata
- configured context file metadata
- latest usable session summary
- replay package inputs

If resume fails, `kai` starts a fresh Codex session and replays:

- the core system prompt
- configured stable context files
- the latest session summary
- a bounded slice of recent turns
- relevant attachment references

Design principle: the backend session is preferred, but the local store owns continuity.

The active Codex session id belongs in runtime state, not user config. It should be persisted under `root_app/state` alongside other operational state so `kai` can resume cleanly without rewriting `config.toml`.

## 11. Optional Context Files

`kai` should support a thin, explicit version of `SOUL` and `MEMORY` without turning them into a full autonomous memory subsystem.

These should be treated as optional external context files with named roles:

- `SOUL`: stable steering, purpose, preferences, and personal operator context
- `MEMORY`: durable facts or high-signal state worth carrying across sessions
- `TODO`: optional actionable list or current work queue

These are file roles, not mandatory filenames. By default they should live under `~/.tools/kai`, and they may be overridden to other user-managed locations.

Instead:

- the paths are configured explicitly
- the default paths live under `~/.tools/kai`
- the files may be overridden to live in another notes directory, synced folder, or project workspace
- `kai` reads them in place
- `kai` does not duplicate them into a private shadow copy by default

Recommended v1 behavior:

- if `SOUL` is configured, load it at session bootstrap and replay
- if `MEMORY` is configured, load it as durable context at bootstrap and replay
- if `TODO` is configured, make it available as optional working context and an explicit edit target when requested

Recommended mutation stance:

- `SOUL` is read-only in normal use and should be edited intentionally by the user
- `MEMORY` should not be auto-rewritten by a background process
- `TODO` and `MEMORY` may be edited when the user explicitly asks, because `kai` is a full-access portal

This keeps the system close to raw Codex while still preserving the useful flavor of OpenClaw-style personal context files.

## 12. Message Passing Model

`kai` should not forward raw channel text into Codex with no wrapper.

Each inbound turn should be normalized into a stable envelope before execution.

The envelope should include:

- channel name
- sender identity
- local timestamp and timezone
- cleaned user text
- attachment descriptors
- explicit operating mode
- configured context file references when present
- concise response formatting instructions

This follows the broader ecosystem pattern. The channel adapter handles transport concerns. The runner receives a normalized, channel-agnostic turn object.

## 13. Attachment Strategy

All inbound attachments should be staged locally by `kai` before Codex sees them.

Rules:

- download to a dedicated attachment directory
- enforce size limits
- validate mime type and extension
- sanitize filenames
- compute checksum
- store metadata in state
- pass local absolute paths to Codex
- never rely on ephemeral remote URLs in the prompt

V1 supported attachment classes should be narrow:

- images
- PDFs
- plain text or markdown files

V1 should reject or defer:

- audio transcription
- video
- large archives
- executable files

The attachment directory is an intake surface, not the main vault. In v1 it should be derived from the main app root, not exposed as a separate required config root.

## 14. Setup and Onboarding

The setup experience should be much smaller than the large claw projects.

Goal: one short local setup flow plus one owner pairing step.

Recommended setup flow:

1. `kai health` verifies local prerequisites and remediation hints.
2. `kai setup` writes config and app-root scaffolding.
3. `kai setup telegram` stores the bot token source and starts owner pairing.
4. `kai setup codex` verifies `codex exec`, `codex exec resume`, and auth presence.
5. `kai health` confirms the full stack is healthy.

The primary UX should be CLI subcommands, not manual TOML editing.

Owner pairing should be simple but not open-ended.

Recommended v1 pairing model:

- local setup generates a short pairing secret or one-time code
- the owner sends the bot a pairing command containing that code
- `kai` stores the sender as the owner
- later messages from any other sender are ignored

This borrows the pairing idea from the claw projects while keeping the model owner-only.

## 15. Configuration Model

Configuration should follow the same general conventions used in the related local tools:

- deterministic
- explicit
- machine-readable
- environment override friendly
- stable precedence rules

Recommended precedence:

- CLI flags
- environment variables
- user config file
- defaults

Recommended config file: `~/.tools/kai/config.toml`

Recommended app root: `~/.tools/kai`

Naming should favor grouped prefixes for sibling settings so related keys sort together cleanly.

Path model should stay simple:

- one `root_work` for the Codex working directory
- one `root_app` for `kai`'s own config, logs, state, and staged attachments
- no separate user-managed secondary roots in v1

`root_work` may point at `~/.tools/kai/work`, a notes directory, a project repo, or any other directory the operator wants as the default Codex starting point. `kai` should not force a second app-specific root abstraction on top of Codex.

Defaults should stay sparse:

- if a setting can inherit from normal Codex behavior, omit it
- only add config when `kai` actually needs a stable value of its own
- prefer optional overrides over duplicated restatement of global defaults

Config should contain operator intent and stable defaults. Runtime artifacts such as the active Codex session id should live in state under `root_app`, not be written back into config.

Illustrative config shape:

```toml
[agent]
timezone = "Europe/London"

[channel.telegram]
enabled = true
bot_token_env = "KAI_TELEGRAM_BOT_TOKEN"

[paths]
root_app = "~/.tools/kai"
root_work = "~/.tools/kai/work"

[runner.codex]
binary = "codex"

[context_files]
soul = "~/.tools/kai/SOUL.md"
memory = "~/.tools/kai/MEMORY.md"
todo = "~/.tools/kai/TODO.md"
```

Internal directories such as attachments and state should derive from `root_app`, for example:

- `<root_app>/attachments`
- `<root_app>/state`
- `<root_app>/logs`

Reactive-only behavior and any tiny wrapper-level guardrails should be implementation defaults in v1, not user-managed config keys.

Optional Codex overrides should exist only when needed. For example:

```toml
[runner.codex.override]
approval_policy = "never"
sandbox_mode = "danger-full-access"
```

If the override block is omitted, `kai` should simply inherit normal global Codex behavior.

Manual owner-id override may exist as an optional recovery or bootstrap shortcut, but pairing should remain the primary path. For example:

```toml
[channel.telegram]
owner_user_id = 123456789
```

Other user-managed paths remain valid override patterns for `root_work` and any context file, but they should not be the default example.

The exact file layout can change. The important contract is minimal root concepts, sparse config, optional override support, external context-file path support, and config subcommands as the normal operator interface.

## 16. Repository and Code Structure

The repo should follow the existing Rust workspace pattern used in the related local tools.

Tooling baseline should also follow the same alignment standard used in the related local tools:

- Cargo workspace for product code
- Bun for repo utilities and quality-gate scripts
- Biome for formatting and linting where applicable
- commitlint, lint-staged, and Husky at the repo root
- root `AGENTS.md` as the canonical operating contract

Recommended top-level layout:

```text
kai/
  AGENTS.md
  SPEC.md
  Cargo.toml
  Cargo.lock
  package.json
  biome.json
  crates/
    sdk/
    cli/
```

Recommended v1 workspace shape:

- `sdk` is the main engine crate
- `cli` is a thin operator surface over `sdk`

This maps naturally to the existing `sdk` plus thin `cli` pattern already used in related tools, without splitting Telegram, Codex, and state into separate crates too early.

At the top crate layer, terse names like `sdk` and `cli` are a good fit. Inside `sdk`, prefer clear module names over forced abbreviations. A good default is:

```text
crates/sdk/src/
  config/
  context/
  channel/
    telegram.rs
  runtime/
    codex.rs
  state/
  lib.rs
```

Cargo package naming should stay conventional and sortable:

- `kai-sdk`
- `kai-cli`

Module responsibilities:

- `sdk`: domain types, policies, config, envelopes, continuity, state abstractions, Telegram adapter, and Codex runtime integration
- `cli`: thin command surface over `sdk`, with registry-driven `tools`, `health`, and `config`

Possible later extractions:

- a dedicated WhatsApp crate if that adapter becomes real enough to justify it
- a dedicated Codex runtime crate if that layer becomes independently complex
- a dedicated state or storage crate if persistence becomes independently complex

Do not split crates further until the boundaries are real. In v1, prefer one strong `sdk` over multiple premature adapter crates.

## 17. CLI Surface

Following `compose` in contract shape, the CLI should expose small, composable, machine-readable commands instead of one monolithic interface. The deliberate difference is output mode: `kai` should be JSON-first by default rather than requiring a `--json` opt-in flag.

Orientation surface should be the highest priority, matching the house style used in related tools:

- `kai tools`
- `kai tools <name>`
- `kai health`
- `kai config show`
- `kai config get <key>`
- `kai config set <key> <value>`
- `kai config unset <key>`

These commands should work as the primary machine-readable discovery layer:

- `tools` is the authoritative catalog
- `tools <name>` returns one command contract
- `health` reports readiness, degraded or blocked state, and remediation hints
- `config show` reports effective configuration

Operator commands should stay secondary and thin:

- `kai setup`
- `kai run`
- `kai session show`
- `kai session new`
- `kai session reset`
- `kai context show`
- `kai context check`

`tools` metadata should come from one registry source in `sdk`, not duplicated across help text, docs, and tests.

The default machine-readable contract applies directly to:

- `kai tools`
- `kai tools <name>`
- `kai health`
- `kai config show`

Required `tools` catalog metadata:

- `name`
- `command`
- `category`
- `description`
- `parameters`
- `outputFields`
- `outputSchema`
- `inputSchema`
- `idempotent`
- `rateLimit`
- `example`
- `globalFlags` in the catalog response

Default output behavior:

- exactly one JSON line on stdout
- no logs on stdout
- logs and diagnostics go to stderr
- stable envelope keys: `ok`, `data`, `error`, `meta`
- optional future text or human-oriented modes must be explicit opt-ins and must not weaken the default JSON contract

Exit codes should stay stable:

- `0` success
- `1` runtime, validation, or business failure
- `2` blocked prerequisites or missing runtime requirements

`health` should return lifecycle status such as `ready`, `degraded`, or `blocked`, plus actionable fix guidance.

Default envelope shape should follow the existing local-tool envelope pattern:

```json
{
  "ok": true,
  "data": {},
  "meta": {}
}
```

Errors should be stable and contextual, with explicit codes.

## 18. Persistence and Audit

`kai` should keep both structured state and an append-only audit trail.

Recommended persistence split:

- SQLite for operational state
- JSONL for append-only turn/audit logging

Minimum audit fields:

- timestamp
- channel
- sender id
- turn id
- Codex session id
- Codex runtime mode
- context files consulted
- attachment list
- outcome status
- error summary when present

This is part of the trust model. If `kai` behaves unexpectedly, the system should leave evidence.

## 19. Operational Behavior

The runtime should stay boring.

- one local daemon process
- one serialized inbound queue
- long-polling for Telegram
- no web server in v1
- no background autonomous work
- no periodic jobs beyond optional internal housekeeping

If the daemon is offline, messages wait at the channel and are processed when it reconnects.

## 20. Validation Standard

The repo should inherit the same quality bar used in the related local tools.

Required before calling v1 usable:

- zero compiler errors
- zero clippy warnings in enforced targets
- passing tests
- successful release build
- deterministic config and path handling
- verified session resume
- verified restart recovery
- verified attachment rejection for invalid or oversized files
- verified non-owner rejection

High-value integration tests:

- owner pairing flow
- resume success path
- resume failure with replay fallback
- queue serialization
- attachment staging and cleanup
- configured context-file loading
- optional custom tool-path execution
- full-access portal turn

## 21. Proposed Phases

### Phase 0

- finalize this spec
- confirm the small set of wrapper-level soft guardrails
- confirm Telegram-first scope

### Phase 1

- bootstrap Rust workspace and repo tooling
- implement config, tools, and health surfaces
- implement configurable context-file plumbing
- implement SQLite plus JSONL store

### Phase 2

- implement Telegram long-polling adapter
- implement pairing and owner filtering
- implement serialized turn queue

### Phase 3

- implement Codex runner
- implement session creation, resume, and replay fallback
- implement normalized prompt envelope

### Phase 4

- implement attachment intake
- enforce attachment limits and metadata capture
- add integration tests

### Phase 5

- optionally add explicit override support for a dedicated `kai` Codex runtime profile
- only after the portal core proves stable

### Phase 6

- evaluate WhatsApp as a separate adapter
- only if Telegram proves insufficient

## 22. Decisions Locked by This Spec

These points should be treated as decided unless new evidence forces a change:

- `kai` is owner-only
- `kai` is local-first
- Telegram is the v1 channel
- Codex CLI is the v1 backend
- long-polling beats webhooks in v1
- continuity uses `codex exec resume` plus local replay fallback
- attachment staging is local and explicit
- optional `SOUL` / `MEMORY` / `TODO` context files are supported via configured paths
- `kai` does not assume or create duplicate private copies of those context files
- custom non-PATH tools may use optional explicit path hints
- the default operating mode is reactive-only
- v1 is a full-access Codex portal, not a second safety harness
- v1 inherits the operator's global Codex config
- no hard folder blacklist is promised in v1
- primary owner onboarding uses a one-time pairing code
- `kai` should not become a hosted gateway or general agent platform

## 23. Open Questions

These are the remaining questions that materially affect the implementation plan.

### 23.1 Soft guardrails

Do we want wrapper-level rejection for a tiny set of obvious risk escalators such as explicit `sudo` requests, knowing that these are convenience checks rather than hard isolation?

Recommended default: yes for `sudo` and background-self-scheduling requests, no for a larger faux-blacklist.

### 23.2 Context-file write policy

Should `MEMORY` and `TODO` be treated as explicit edit targets only when you ask, or should `kai` also offer lightweight command helpers for appending/updating them?

Recommended default: explicit edits when asked, then add helpers later if they are clearly useful.

### 23.3 WhatsApp timing

Is WhatsApp a near-term v2 requirement, or should it stay completely out of the first milestone?

Recommended default: keep it out until Telegram proves the product shape.

## 24. Bottom Line

`kai` should start as a small, owner-only, Telegram-to-Codex portal with strong local state, reactive behavior, and no platform ambitions.

The large reference projects are useful because they show what must be handled once the surface area grows. The lesson for `kai` is not to replicate that growth. The lesson is to borrow only the parts that remain necessary after the scope is cut down to one person, one machine, one backend, and one safe path to usefulness.
