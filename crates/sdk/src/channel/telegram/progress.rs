use super::*;
use crate::runtime::agent::AgentProgressEvent;
use crate::state::ActiveTurnState;
use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use std::path::Path;

const INITIAL_PROGRESS_DELAY_MS: u64 = 700;
const INITIAL_PROGRESS_VARIANTS: &[&str] = &[
    "Looking around.",
    "Thinking this through.",
    "Pulling on the right thread.",
    "Poking at the likely bits.",
    "Tracing this through.",
    "Thinking really hard about this.",
    "Following the clues.",
    "Checking where this goes.",
];
const IDLE_PROGRESS_VARIANTS: &[&str] = &[
    "Still on it.",
    "Still pulling on the thread.",
    "Still tracing this through.",
    "Still looking around.",
    "Still untangling it.",
    "Still checking one more thing.",
    "Still thinking this through.",
    "Still trying not to embarrass myself.",
];
const PLAN_PROGRESS_VARIANTS: &[&str] = &[
    "Updating the route.",
    "Narrowing the next step.",
    "Working through the plan.",
];
const REASONING_PROGRESS_PREFIXES: &[&str] = &[
    "Thinking:",
    "Reasoning:",
    "Working theory:",
    "Current read:",
];
const CHECKS_PROGRESS_VARIANTS: &[&str] = &[
    "Running checks.",
    "Kicking the tires.",
    "Putting it through checks.",
    "Running the boring but useful bits.",
];
const INSPECTING_PATTERNS: &[&str] = &[
    "Inspecting {subject}.",
    "Looking through {subject}.",
    "Reading {subject}.",
    "Poking at {subject}.",
    "Tracing {subject}.",
];
const DONE_PROGRESS_TEXT: &str = "Done.";
const CANCELED_PROGRESS_TEXT: &str = "Canceled.";
const FAILED_PROGRESS_TEXT: &str = "Failed.";
const RESTARTING_PROGRESS_TEXT: &str = "Interrupted. Restarting.";
const MAX_PROGRESS_UPDATES_PER_TURN: u32 = 20;
const MAX_PROGRESS_TEXT_CHARS: usize = 220;
const PROGRESS_UPDATE_PREFIXES: &[&str] = &[
    "check",
    "checking",
    "compare",
    "comparing",
    "dig",
    "digging",
    "follow",
    "following",
    "gather",
    "gathering",
    "inspect",
    "inspecting",
    "look at",
    "looking at",
    "narrow",
    "narrowing",
    "read",
    "read complete",
    "reading",
    "review",
    "reviewing",
    "run",
    "running",
    "search",
    "searching",
    "think",
    "thinking",
    "figure out",
    "figuring out",
    "test",
    "testing",
    "trace",
    "tracing",
    "look around",
    "looking around",
    "poke at",
    "poking at",
    "pull on",
    "pulling on",
    "untangle",
    "untangling",
    "validate",
    "validating",
    "work through",
    "working through",
    "write",
    "writing",
];
const PROGRESS_UPDATE_LEAD_INS: &[&str] =
    &["i'm ", "i am ", "i need to ", "need to ", "let me ", "now "];

pub(super) fn progress_enabled(config: &LoadedConfig) -> bool {
    config.values.channel.telegram.progress.enabled
}

pub(super) fn initial_progress_delay() -> Duration {
    Duration::from_millis(INITIAL_PROGRESS_DELAY_MS)
}

pub(super) fn progress_variant_seed(turn_id: &str) -> u64 {
    let hash = blake3::hash(turn_id.as_bytes());
    let bytes = hash.as_bytes();
    u64::from_le_bytes([
        bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7],
    ])
}

pub(super) async fn maybe_send_initial_progress(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    turn: &mut ActiveOwnerTurn,
    now: Instant,
) -> KaiResult<()> {
    if !progress_enabled(config) {
        return Ok(());
    }

    if turn.progress.initial_progress_sent
        || turn.status_message_id.is_some()
        || turn.progress.semantic_update_count > 0
        || now < turn.progress.initial_progress_due_at
    {
        return Ok(());
    }

    apply_progress_text(
        client,
        token,
        state,
        turn,
        select_variant(
            INITIAL_PROGRESS_VARIANTS,
            turn.progress.variant_seed,
            "initial",
            0,
        ),
        false,
        false,
    )
    .await?;

    if turn.status_message_id.is_some() {
        turn.progress.initial_progress_sent = true;
    }

    Ok(())
}

pub(super) async fn handle_progress_event(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    turn: &mut ActiveOwnerTurn,
    event: &AgentProgressEvent,
    now: Instant,
) -> KaiResult<()> {
    turn.progress.last_event_at = now;
    if !progress_enabled(config) || turn.progress.update_count >= MAX_PROGRESS_UPDATES_PER_TURN {
        return Ok(());
    }

    let Some(text) = progress_text_for_event(event) else {
        return Ok(());
    };

    turn.progress.semantic_update_count = turn.progress.semantic_update_count.saturating_add(1);
    apply_progress_text(client, token, state, turn, &text, false, false).await
}

pub(super) async fn maybe_send_idle_progress(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    turn: &mut ActiveOwnerTurn,
    now: Instant,
) -> KaiResult<()> {
    if !progress_enabled(config) || turn.progress.update_count >= MAX_PROGRESS_UPDATES_PER_TURN {
        return Ok(());
    }

    let idle_window = Duration::from_secs(config.values.channel.telegram.progress.idle_update_secs);
    if now.duration_since(turn.progress.last_event_at) < idle_window {
        return Ok(());
    }

    let text = select_variant(
        IDLE_PROGRESS_VARIANTS,
        turn.progress.variant_seed,
        "idle",
        turn.progress.idle_update_count,
    )
    .to_string();

    apply_progress_text(client, token, state, turn, &text, false, false).await?;
    if turn.status_message_id.is_some() {
        turn.progress.idle_update_count = turn.progress.idle_update_count.saturating_add(1);
    }
    Ok(())
}

pub(super) async fn mark_progress_done(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    chat_id: i64,
    status_message_id: Option<i64>,
) -> KaiResult<()> {
    mark_progress_terminal(
        client,
        token,
        config,
        state,
        chat_id,
        status_message_id,
        DONE_PROGRESS_TEXT,
    )
    .await
}

pub(super) async fn mark_turn_canceled(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    turn: &ActiveOwnerTurn,
) -> KaiResult<()> {
    mark_progress_terminal(
        client,
        token,
        config,
        state,
        turn.pending.chat_id,
        turn.status_message_id,
        CANCELED_PROGRESS_TEXT,
    )
    .await
}

pub(super) async fn mark_turn_failed(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    turn: &ActiveOwnerTurn,
) -> KaiResult<()> {
    mark_progress_terminal(
        client,
        token,
        config,
        state,
        turn.pending.chat_id,
        turn.status_message_id,
        FAILED_PROGRESS_TEXT,
    )
    .await
}

pub(super) async fn mark_recovered_turn_restarting(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &ActiveTurnState,
) -> KaiResult<()> {
    mark_progress_terminal(
        client,
        token,
        config,
        state,
        active_turn.pending.chat_id,
        active_turn.status_message_id,
        RESTARTING_PROGRESS_TEXT,
    )
    .await
}

async fn mark_progress_terminal(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    chat_id: i64,
    status_message_id: Option<i64>,
    text: &str,
) -> KaiResult<()> {
    if !progress_enabled(config) {
        return Ok(());
    }
    let Some(status_message_id) = status_message_id else {
        return Ok(());
    };

    if let Err(error) =
        edit_message_text_with_retry(client, token, chat_id, status_message_id, text).await
    {
        record_progress_error(
            state,
            "telegram.progress_terminal_update_failed",
            chat_id,
            Some(status_message_id),
            text,
            &error,
        )?;
    }

    Ok(())
}

async fn apply_progress_text(
    client: &Client,
    token: &str,
    state: &StateStore,
    turn: &mut ActiveOwnerTurn,
    text: &str,
    allow_duplicate: bool,
    ignore_rate_limit: bool,
) -> KaiResult<()> {
    let normalized = normalize_progress_text(text);
    if normalized.is_empty() {
        return Ok(());
    }

    if !allow_duplicate && turn.progress.last_sent_text.as_deref() == Some(normalized.as_str()) {
        return Ok(());
    }

    if !ignore_rate_limit
        && turn.progress.last_visible_update_at.elapsed()
            < Duration::from_millis(turn.progress.edit_interval_ms)
    {
        return Ok(());
    }

    let text = truncate_progress_text(text);
    if text.is_empty() {
        return Ok(());
    }

    if let Some(message_id) = turn.status_message_id {
        match edit_message_text_with_retry(client, token, turn.pending.chat_id, message_id, &text)
            .await
        {
            Ok(()) => {
                note_progress_delivery(turn, normalized);
                sync_active_turn_state(state, turn)?;
                return Ok(());
            }
            Err(error) if is_telegram_edit_target_lost(&error) => {
                turn.status_message_id = None;
                sync_active_turn_state(state, turn)?;
            }
            Err(error) => {
                record_progress_error(
                    state,
                    "telegram.progress_edit_failed",
                    turn.pending.chat_id,
                    Some(message_id),
                    &text,
                    &error,
                )?;
                return Ok(());
            }
        }
    }

    match send_status_message(client, token, turn.pending.chat_id, &text).await {
        Ok(message_id) => {
            turn.status_message_id = Some(message_id);
            note_progress_delivery(turn, normalized);
            sync_active_turn_state(state, turn)?;
        }
        Err(error) => {
            record_progress_error(
                state,
                "telegram.progress_send_failed",
                turn.pending.chat_id,
                None,
                &text,
                &error,
            )?;
        }
    }

    Ok(())
}

fn note_progress_delivery(turn: &mut ActiveOwnerTurn, normalized: String) {
    turn.progress.last_sent_text = Some(normalized);
    turn.progress.last_visible_update_at = Instant::now();
    turn.progress.update_count = turn.progress.update_count.saturating_add(1);
}

fn sync_active_turn_state(state: &StateStore, turn: &ActiveOwnerTurn) -> KaiResult<()> {
    state.set_active_turn_state(&ActiveTurnState {
        pending: turn.pending.clone(),
        status_message_id: turn.status_message_id,
    })
}

fn record_progress_error(
    state: &StateStore,
    event: &str,
    chat_id: i64,
    status_message_id: Option<i64>,
    text: &str,
    error: &KaiError,
) -> KaiResult<()> {
    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "chatId": chat_id,
        "statusMessageId": status_message_id,
        "text": text,
        "message": error.message,
        "hint": error.hint,
    }))
}

fn progress_text_for_event(event: &AgentProgressEvent) -> Option<String> {
    match event {
        AgentProgressEvent::AgentMessage { text } => progress_text_from_agent_message(text),
        AgentProgressEvent::Plan { text } => progress_text_from_plan(text),
        AgentProgressEvent::CommandStarted { command } => command_progress_text(command),
        AgentProgressEvent::ReasoningSummary { text } => progress_text_from_reasoning_summary(text),
    }
}

fn progress_text_from_plan(text: &str) -> Option<String> {
    let normalized = collapse_whitespace(text);
    if normalized.is_empty() {
        return Some(PLAN_PROGRESS_VARIANTS[0].to_string());
    }

    let summary = trim_progress_summary(&normalized)?;
    Some(summary)
}

fn progress_text_from_reasoning_summary(text: &str) -> Option<String> {
    let normalized = collapse_whitespace(text);
    let summary = trim_progress_summary(&normalized)?;
    let prefix = select_text_variant(REASONING_PROGRESS_PREFIXES, &normalized, "reasoning", 0);
    Some(format!("{prefix} {summary}"))
}

fn progress_text_from_agent_message(text: &str) -> Option<String> {
    let plain = markdown_to_plain_text(text);
    let normalized = collapse_whitespace(&plain);
    if normalized.is_empty() || !looks_like_progress_update(&normalized) {
        return None;
    }

    let sentence_count = normalized.matches(['.', '!', '?']).count();
    if normalized.chars().count() > MAX_PROGRESS_TEXT_CHARS || sentence_count > 2 {
        return None;
    }

    Some(normalized)
}

fn markdown_to_plain_text(input: &str) -> String {
    let mut output = String::new();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_TASKLISTS);

    for event in Parser::new_ext(input, options) {
        match event {
            Event::Text(text) | Event::Code(text) => output.push_str(text.as_ref()),
            Event::SoftBreak | Event::HardBreak => output.push(' '),
            Event::Start(Tag::Paragraph | Tag::Heading { .. } | Tag::BlockQuote(_)) => {}
            Event::Start(Tag::CodeBlock(kind)) => {
                if let CodeBlockKind::Fenced(language) = kind
                    && !language.trim().is_empty()
                {
                    output.push_str(language.as_ref());
                    output.push(' ');
                }
            }
            Event::End(
                TagEnd::Paragraph
                | TagEnd::Heading(_)
                | TagEnd::BlockQuote(_)
                | TagEnd::CodeBlock
                | TagEnd::Item,
            ) => output.push(' '),
            _ => {}
        }
    }

    output
}

fn looks_like_progress_update(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    has_progress_prefix(&lower)
        || PROGRESS_UPDATE_LEAD_INS
            .iter()
            .filter_map(|prefix| lower.strip_prefix(prefix))
            .any(has_progress_prefix)
}

fn has_progress_prefix(text: &str) -> bool {
    PROGRESS_UPDATE_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn command_progress_text(command: &str) -> Option<String> {
    let lower = command.to_ascii_lowercase();
    if lower.contains("cargo test")
        || lower.contains("cargo check")
        || lower.contains("cargo clippy")
        || lower.contains("cargo build")
        || lower.contains("bun run util:")
        || lower.contains("bun run build")
    {
        return Some(
            select_text_variant(CHECKS_PROGRESS_VARIANTS, command, "checks", 0).to_string(),
        );
    }

    let is_file_inspection = [
        "rg ", "sed ", "cat ", "bat ", "fd ", "find ", "ls ", "git show", "git diff",
    ]
    .iter()
    .any(|needle| lower.contains(needle));
    if !is_file_inspection {
        return None;
    }

    let files = extract_command_files(command);
    match files.as_slice() {
        [] => Some(apply_subject_pattern(
            select_text_variant(INSPECTING_PATTERNS, command, "inspect", 0),
            "files",
        )),
        [file] => Some(apply_subject_pattern(
            select_text_variant(INSPECTING_PATTERNS, file, "inspect", 0),
            file,
        )),
        [first, second, ..] => Some(apply_subject_pattern(
            select_text_variant(INSPECTING_PATTERNS, command, "inspect", 1),
            &format!("{first} and {second}"),
        )),
    }
}

fn extract_command_files(command: &str) -> Vec<String> {
    let mut files = Vec::new();
    for token in command.split(|ch: char| {
        ch.is_whitespace() || matches!(ch, '"' | '\'' | ',' | '(' | ')' | '[' | ']')
    }) {
        let trimmed = token.trim_matches(|ch: char| ch == ';' || ch == ':');
        if trimmed.is_empty() || !trimmed.contains('.') {
            continue;
        }

        let Some(file_name) = Path::new(trimmed)
            .file_name()
            .and_then(|value| value.to_str())
        else {
            continue;
        };

        let lower = file_name.to_ascii_lowercase();
        if !matches!(
            lower.rsplit('.').next(),
            Some("md" | "rs" | "toml" | "json" | "yaml" | "yml" | "txt")
        ) {
            continue;
        }

        if !files.iter().any(|existing| existing == file_name) {
            files.push(file_name.to_string());
        }
    }

    files
}

fn truncate_progress_text(text: &str) -> String {
    let mut output = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= MAX_PROGRESS_TEXT_CHARS {
            break;
        }
        output.push(ch);
    }
    output.trim().to_string()
}

fn normalize_progress_text(text: &str) -> String {
    collapse_whitespace(text).to_ascii_lowercase()
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn trim_progress_summary(input: &str) -> Option<String> {
    let trimmed = input
        .trim()
        .trim_matches(|ch: char| matches!(ch, '.' | ':' | ';'));
    if trimmed.is_empty() {
        return None;
    }

    Some(truncate_progress_text(trimmed))
}

fn select_variant<'a>(variants: &'a [&'a str], seed: u64, lane: &str, cycle: u32) -> &'a str {
    if variants.is_empty() {
        return "";
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(&seed.to_le_bytes());
    hasher.update(lane.as_bytes());
    hasher.update(&cycle.to_le_bytes());
    let bytes = hasher.finalize();
    let index = u16::from_le_bytes([bytes.as_bytes()[0], bytes.as_bytes()[1]]) as usize;
    variants[index % variants.len()]
}

fn select_text_variant<'a>(variants: &'a [&'a str], text: &str, lane: &str, cycle: u32) -> &'a str {
    if variants.is_empty() {
        return "";
    }

    let mut hasher = blake3::Hasher::new();
    hasher.update(text.as_bytes());
    hasher.update(lane.as_bytes());
    hasher.update(&cycle.to_le_bytes());
    let bytes = hasher.finalize();
    let index = u16::from_le_bytes([bytes.as_bytes()[0], bytes.as_bytes()[1]]) as usize;
    variants[index % variants.len()]
}

fn apply_subject_pattern(pattern: &str, subject: &str) -> String {
    pattern.replace("{subject}", subject)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_message_progress_requires_progress_like_prefix() {
        assert!(
            progress_text_from_agent_message("Inspecting [AGENTS.md](file:///tmp) now.").is_some()
        );
        assert!(progress_text_from_agent_message("I need to read AGENTS.md first.").is_some());
        assert!(progress_text_from_agent_message("Let me check the runtime path.").is_some());
        assert!(progress_text_from_agent_message("Here is the final answer.").is_none());
        assert!(progress_text_from_agent_message("I think the issue is in recovery.").is_none());
    }

    #[test]
    fn command_progress_extracts_file_names() {
        let file_progress = command_progress_text("/bin/zsh -lc \"sed -n '1,10p' AGENTS.md\"")
            .expect("file progress");
        assert!(file_progress.contains("AGENTS.md"));
        assert!(
            command_progress_text("/bin/zsh -lc \"cargo test --workspace --release\"").is_some()
        );
    }

    #[test]
    fn progress_text_truncates_long_updates() {
        let input = format!("Inspecting {}", "a".repeat(400));
        let output = truncate_progress_text(&input);
        assert!(output.chars().count() <= MAX_PROGRESS_TEXT_CHARS);
    }

    #[test]
    fn progress_variant_seed_is_deterministic() {
        assert_eq!(
            progress_variant_seed("turn-123"),
            progress_variant_seed("turn-123")
        );
        assert_ne!(
            progress_variant_seed("turn-123"),
            progress_variant_seed("turn-456")
        );
    }

    #[test]
    fn reasoning_summary_progress_is_surfaced_as_text() {
        let text =
            progress_text_from_reasoning_summary("Need to confirm which workspace owns this");
        assert!(text.is_some());
        let text = text.expect("reasoning text");
        assert!(
            text.starts_with("Thinking:")
                || text.starts_with("Reasoning:")
                || text.starts_with("Working theory:")
                || text.starts_with("Current read:")
        );
        assert!(text.contains("workspace"));
    }

    #[test]
    fn plan_progress_prefers_actual_text() {
        assert_eq!(
            progress_text_from_plan("Inspect queue state and retry path."),
            Some("Inspect queue state and retry path".to_string())
        );
    }

    #[test]
    fn select_variant_is_stable_for_same_seed_and_lane() {
        let first = select_variant(INITIAL_PROGRESS_VARIANTS, 123, "initial", 0);
        let second = select_variant(INITIAL_PROGRESS_VARIANTS, 123, "initial", 0);
        let rotated = select_variant(INITIAL_PROGRESS_VARIANTS, 123, "initial", 1);
        assert_eq!(first, second);
        assert_ne!(first, rotated);
    }
}
