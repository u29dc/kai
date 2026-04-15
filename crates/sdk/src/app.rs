use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::config::LoadedConfig;
use crate::context::context_report;
use crate::error::KaiResult;
use crate::runtime::agent::{create_replay_package, run_agent_turn};
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

    let result = run_agent_turn(config, state, channel, sender_id, text, attachments)?;

    state.record_turn(NewTurn {
        role: "assistant",
        channel,
        sender_id: None,
        text: &result.response_text,
        codex_session_id: Some(&result.session_id),
        outcome_status: Some(if result.resumed { "resumed" } else { "fresh" }),
        attachments: &[],
    })?;

    let replay_package = create_replay_package(&result.context_snapshots, &state.recent_turns(24)?);
    state.set_replay_package(&replay_package)?;

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
        "/cancel - stop the current running Codex turn",
        "/send <path> - send a local file from root_work or root_app",
        "/pair <code> - recovery-only owner pairing when locally enabled",
    ]
    .join("\n")
}

pub fn mobile_status_text(config: &LoadedConfig, state: &StateStore) -> KaiResult<String> {
    let mut session = state.session_view()?;
    if session.owner_user_id.is_none() {
        session.owner_user_id = config.values.channel.telegram.owner_user_id;
    }
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
        format!(
            "owner_chat: {}",
            session
                .owner_chat_id
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unpaired".to_string())
        ),
        format!("update_offset: {}", session.update_offset),
        format!("queued_turns: {}", session.queued_turns),
        format!("queue_limit: {}", session.queue_limit),
        format!(
            "pending_reply_deliveries: {}",
            session.pending_reply_deliveries
        ),
    ];

    if let Some(active_turn) = session.active_turn {
        lines.push(format!("active_turn: {}", active_turn.id));
    }

    if let Some(pairing) = session.pending_pairing {
        lines.push(format!(
            "recovery_pairing: open until {} ({} attempt(s) left)",
            pairing.expires_at, pairing.remaining_attempts
        ));
    } else {
        lines.push("recovery_pairing: closed".to_string());
    }

    for entry in context.entries {
        let status = if entry.exists { "ok" } else { "missing" };
        lines.push(format!("context {}: {}", entry.role, status));
    }

    Ok(lines.join("\n"))
}
