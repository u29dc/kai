use super::*;
use crate::redaction::redact_text;
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
const DONE_PROGRESS_TEXT: &str = "Done.";
const CANCELED_PROGRESS_TEXT: &str = "Canceled.";
const FAILED_PROGRESS_TEXT: &str = "Failed.";
const RESTARTING_PROGRESS_TEXT: &str = "Interrupted. Restarting.";
const MAX_PROGRESS_UPDATES_PER_TURN: u32 = 32;
const MAX_PROGRESS_TEXT_CHARS: usize = 220;
const PROGRESS_UPDATE_PREFIXES: &[&str] = &[
    "check",
    "checking",
    "compare",
    "comparing",
    "confirm",
    "confirming",
    "dig",
    "digging",
    "do",
    "doing",
    "double-check",
    "double-checking",
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
    "summarize",
    "summarizing",
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
    "wrap up",
    "wrapping up",
    "write",
    "writing",
];
const PROGRESS_UPDATE_LEAD_INS: &[&str] = &[
    "i'm ",
    "i am ",
    "i've ",
    "i have ",
    "i need to ",
    "need to ",
    "let me ",
    "now ",
    "so ",
    "then ",
    "but ",
];

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
        AgentProgressEvent::StructuredActivity { text } => {
            progress_text_from_structured_activity(text)
        }
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

fn progress_text_from_structured_activity(text: &str) -> Option<String> {
    let normalized = collapse_whitespace(text);
    if normalized.is_empty() {
        return None;
    }

    let redacted = redact_text(&normalized);
    let trimmed = redacted.trim();
    if trimmed.is_empty() {
        return None;
    }

    Some(truncate_progress_text(trimmed))
}

fn progress_text_from_agent_message(text: &str) -> Option<String> {
    let plain = markdown_to_plain_text(text);
    let normalized = collapse_whitespace(&plain);
    if normalized.is_empty() {
        return None;
    }

    let candidate = extract_latest_progress_clause(&normalized)?;
    let candidate = truncate_progress_text(&candidate);
    if candidate.is_empty() {
        return None;
    }

    Some(candidate)
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
        || strip_progress_lead_ins(&lower)
            .map(has_progress_prefix)
            .unwrap_or(false)
}

fn has_progress_prefix(text: &str) -> bool {
    PROGRESS_UPDATE_PREFIXES
        .iter()
        .any(|prefix| text.starts_with(prefix))
}

fn strip_progress_lead_ins(text: &str) -> Option<&str> {
    let mut current = text.trim_start();
    let mut stripped = false;

    loop {
        let Some(next) = PROGRESS_UPDATE_LEAD_INS
            .iter()
            .find_map(|prefix| current.strip_prefix(prefix))
        else {
            break;
        };
        current = next.trim_start();
        stripped = true;
    }

    stripped.then_some(current)
}

fn extract_latest_progress_clause(text: &str) -> Option<String> {
    let normalized = collapse_whitespace(text);
    if normalized.is_empty() {
        return None;
    }

    let candidates = progress_candidates(&normalized);
    for candidate in candidates.iter().rev() {
        let trimmed = candidate.trim();
        if trimmed.is_empty() {
            continue;
        }
        if looks_like_progress_update(trimmed) {
            return Some(display_progress_clause(trimmed));
        }
    }

    if looks_like_progress_update(&normalized) {
        return Some(display_progress_clause(&normalized));
    }

    None
}

fn progress_candidates(text: &str) -> Vec<String> {
    let mut candidates = Vec::new();
    let sentence_splitters = ['.', '!', '?', ';'];
    for part in text.split(sentence_splitters) {
        let part = collapse_whitespace(part);
        if !part.is_empty() {
            candidates.push(part);
        }
    }

    let clause_markers = [
        ", but ",
        ", now ",
        ", so ",
        ", then ",
        " and now ",
        " and then ",
    ];
    let existing = candidates.clone();
    for candidate in existing {
        for marker in clause_markers {
            for part in candidate.split(marker) {
                let part = collapse_whitespace(part);
                if !part.is_empty() {
                    candidates.push(part);
                }
            }
        }
    }

    candidates
}

fn ensure_terminal_period(text: &str) -> String {
    let trimmed = text.trim().trim_matches(|ch: char| ch == '"' || ch == '\'');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.ends_with(['.', '!', '?']) {
        trimmed.to_string()
    } else {
        format!("{trimmed}.")
    }
}

fn display_progress_clause(text: &str) -> String {
    let normalized = ensure_terminal_period(text);
    capitalize_first_char(&normalized)
}

fn capitalize_first_char(text: &str) -> String {
    let mut chars = text.chars();
    let Some(first) = chars.next() else {
        return String::new();
    };
    let mut output = first.to_uppercase().collect::<String>();
    output.push_str(chars.as_str());
    output
}

fn command_progress_text(command: &str) -> Option<String> {
    let preview = normalized_command_preview(command);
    if preview.is_empty() {
        return None;
    }

    let lower = preview.to_ascii_lowercase();
    if lower.starts_with("git diff") {
        return Some("Running git diff.".to_string());
    }
    if lower.starts_with("git show") {
        return Some("Running git show.".to_string());
    }
    if lower.starts_with("cargo ")
        || lower.starts_with("bun run ")
        || lower.starts_with("bunx ")
        || lower.starts_with("git ")
    {
        return Some(format!("Running {}.", truncate_command_preview(&preview)));
    }
    if lower.starts_with("sed ")
        || lower.starts_with("cat ")
        || lower.starts_with("bat ")
        || lower.starts_with("head ")
        || lower.starts_with("tail ")
    {
        let files = extract_command_files(&preview);
        return match files.as_slice() {
            [] => Some("Reading files.".to_string()),
            [file] => Some(format!("Reading {file}.")),
            [first, second, ..] => Some(format!("Reading {first} and {second}.")),
        };
    }
    if lower.starts_with("rg ") {
        return Some("Searching files.".to_string());
    }
    if lower.starts_with("fd ")
        || lower.starts_with("find ")
        || lower.starts_with("ls ")
        || lower.starts_with("eza ")
    {
        return Some("Listing files.".to_string());
    }

    Some(format!("Running {}.", truncate_command_preview(&preview)))
}

fn normalized_command_preview(command: &str) -> String {
    let collapsed = collapse_whitespace(command);
    if collapsed.is_empty() {
        return String::new();
    }

    extract_shell_wrapped_command(&collapsed)
        .map(collapse_whitespace)
        .unwrap_or(collapsed)
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

fn extract_shell_wrapped_command(command: &str) -> Option<&str> {
    const MARKERS: &[&str] = &[" -lc ", " -c "];

    for marker in MARKERS {
        let Some(start) = command.find(marker) else {
            continue;
        };
        let rest = command.get(start + marker.len()..)?.trim_start();
        let quote = rest.chars().next()?;
        if !matches!(quote, '"' | '\'') {
            continue;
        }
        let body = rest.get(1..)?;
        let end = body.rfind(quote)?;
        return body.get(..end);
    }

    None
}

fn truncate_command_preview(command: &str) -> String {
    truncate_with_limit(command, 120)
}

fn truncate_progress_text(text: &str) -> String {
    truncate_with_limit(text, MAX_PROGRESS_TEXT_CHARS)
}

fn truncate_with_limit(text: &str, limit: usize) -> String {
    let mut output = String::new();
    for (index, ch) in text.chars().enumerate() {
        if index >= limit {
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
        assert!(
            progress_text_from_agent_message(
                "I've got the fresh research pass; now I'm checking the git history."
            )
            .is_some()
        );
        assert!(
            progress_text_from_agent_message(
                "I've got enough to summarize, but I'm doing one last clean diff check."
            )
            .is_some()
        );
        assert!(progress_text_from_agent_message("Here is the final answer.").is_none());
        assert!(progress_text_from_agent_message("I think the issue is in recovery.").is_none());
    }

    #[test]
    fn command_progress_extracts_file_names() {
        let file_progress = command_progress_text("/bin/zsh -lc \"sed -n '1,10p' AGENTS.md\"")
            .expect("file progress");
        assert_eq!(file_progress, "Reading AGENTS.md.");
        assert_eq!(
            command_progress_text("/bin/zsh -lc \"cargo test --workspace --release\""),
            Some("Running cargo test --workspace --release.".to_string())
        );
        assert_eq!(
            command_progress_text("/bin/zsh -lc \"git diff --stat\""),
            Some("Running git diff.".to_string())
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

    #[test]
    fn structured_activity_progress_is_passthrough_but_redacted() {
        let text = progress_text_from_structured_activity(
            "Searching the web for \"codex\" with token=abc123",
        )
        .expect("structured activity");
        assert!(text.contains("Searching the web"));
        assert!(text.contains("[REDACTED]"));
    }

    #[test]
    fn agent_message_progress_prefers_latest_progress_clause_from_accumulated_text() {
        let text = progress_text_from_agent_message(
            "I'm checking two things in parallel. I've got the fresh research pass; now I'm checking the git history around the notebook work.",
        )
        .expect("agent progress");
        assert_eq!(
            text,
            "Now I'm checking the git history around the notebook work."
        );

        let text = progress_text_from_agent_message(
            "I'm checking two things in parallel. I've got enough to summarize, but I'm doing one last clean diff check so I can distinguish committed change from local state.",
        )
        .expect("agent progress");
        assert_eq!(
            text,
            "I'm doing one last clean diff check so I can distinguish committed change from local state."
        );
    }
}
