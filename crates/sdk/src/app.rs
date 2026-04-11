use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::config::LoadedConfig;
use crate::context::context_report;
use crate::error::KaiResult;
use crate::runtime::codex::run_codex_turn;
use crate::state::{AttachmentInfo, NewTurn, StateStore};

pub fn handle_owner_prompt(
    config: &LoadedConfig,
    state: &StateStore,
    channel: &str,
    sender_id: i64,
    text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<String> {
    state.record_turn(NewTurn {
        role: "user",
        channel,
        sender_id: Some(sender_id),
        text,
        codex_session_id: state.get_active_session_id()?.as_deref(),
        outcome_status: Some("received"),
        attachments,
    })?;

    let result = run_codex_turn(config, state, channel, sender_id, text, attachments)?;

    state.record_turn(NewTurn {
        role: "assistant",
        channel,
        sender_id: None,
        text: &result.response_text,
        codex_session_id: Some(&result.session_id),
        outcome_status: Some(if result.resumed { "resumed" } else { "fresh" }),
        attachments: &[],
    })?;

    state.append_audit_json(&json!({
        "timestamp": Utc::now().to_rfc3339(),
        "event": "turn.completed",
        "turnId": Uuid::new_v4().to_string(),
        "channel": channel,
        "senderId": sender_id,
        "codexSessionId": result.session_id,
        "resumed": result.resumed,
        "attachmentCount": attachments.len(),
    }))?;

    Ok(result.response_text)
}

pub fn mobile_help_text() -> String {
    [
        "kai commands:",
        "/help - show this help",
        "/status - show current pairing and session status",
        "/new - clear the current Codex session so the next message starts fresh",
        "/reset - same as /new",
        "/pair <code> - claim owner access after local setup",
    ]
    .join("\n")
}

pub fn mobile_status_text(config: &LoadedConfig, state: &StateStore) -> KaiResult<String> {
    let session = state.session_view()?;
    let context = context_report(config);

    let mut lines = vec![
        format!(
            "owner: {}",
            session
                .owner_user_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unpaired".to_string())
        ),
        format!(
            "session: {}",
            session
                .active_session_id
                .unwrap_or_else(|| "none".to_string())
        ),
        format!("update_offset: {}", session.update_offset),
    ];

    for entry in context.entries {
        let status = if entry.exists { "ok" } else { "missing" };
        lines.push(format!("context {}: {}", entry.role, status));
    }

    Ok(lines.join("\n"))
}
