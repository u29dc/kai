use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::config::LoadedConfig;
use crate::error::KaiResult;
use crate::runtime::agent::{create_replay_package, run_agent_turn, selected_provider};
use crate::state::{AttachmentInfo, NewTurn, StateStore};
use crate::workspace::execution_target;

pub fn handle_owner_prompt(
    config: &LoadedConfig,
    state: &StateStore,
    channel: &str,
    sender_id: i64,
    text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<String> {
    let target = execution_target(config, state)?;
    let _ = selected_provider(config)?;

    state.record_turn(NewTurn {
        provider: target.provider,
        workspace_id: &target.workspace_id,
        working_dir: &target.working_dir,
        role: "user",
        channel,
        sender_id: Some(sender_id),
        text,
        codex_session_id: state
            .get_session_binding(&target)?
            .as_ref()
            .map(|binding| binding.session_id.as_str()),
        outcome_status: Some("received"),
        attachments,
    })?;

    let result = run_agent_turn(
        config,
        state,
        &target,
        channel,
        sender_id,
        text,
        attachments,
    )?;

    state.record_turn(NewTurn {
        provider: target.provider,
        workspace_id: &target.workspace_id,
        working_dir: &target.working_dir,
        role: "assistant",
        channel,
        sender_id: None,
        text: &result.response_text,
        codex_session_id: Some(&result.session_id),
        outcome_status: Some(if result.resumed { "resumed" } else { "fresh" }),
        attachments: &[],
    })?;

    let replay_package = create_replay_package(&state.recent_turns_for_target(&target, 24)?);
    state.set_target_replay_package(&target, &replay_package)?;

    state.append_audit_json(&json!({
        "timestamp": Utc::now().to_rfc3339(),
        "event": "turn.completed",
        "turnId": Uuid::new_v4().to_string(),
        "provider": target.provider.as_key(),
        "workspaceId": target.workspace_id,
        "workingDir": target.working_dir,
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
        "/dir - show the current workspace and available workspaces",
        "/dir <workspace> - switch the next turns to a configured workspace",
        "/new - clear the current workspace session so the next message starts fresh",
        "/cancel - stop the current running Codex turn",
        "/ask <prompt> - run one parallel side question with the normal agent config",
        "/send <path> - send a local file from the current workspace or root_app",
        "/pair <code> - recovery-only owner pairing when locally enabled",
    ]
    .join("\n")
}

pub fn mobile_status_text(config: &LoadedConfig, state: &StateStore) -> KaiResult<String> {
    let mut session = state.session_view(config)?;
    if session.owner_user_id.is_none() {
        session.owner_user_id = config.values.channel.telegram.owner_user_id;
    }
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
        format!("provider: {}", session.provider),
        format!("workspace: {}", session.selected_workspace_id),
        format!("workspace_path: {}", session.selected_workspace_path),
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
    if let Some(side_query) = session.active_side_query {
        lines.push(format!(
            "side_query: {} ({}, {})",
            side_query.id, side_query.status, side_query.workspace_id
        ));
    }

    if let Some(pairing) = session.pending_pairing {
        lines.push(format!(
            "recovery_pairing: open until {} ({} attempt(s) left)",
            pairing.expires_at, pairing.remaining_attempts
        ));
    } else {
        lines.push("recovery_pairing: closed".to_string());
    }

    let workspace_summary = session
        .workspaces
        .iter()
        .map(|workspace| {
            if workspace.selected {
                format!("{}*", workspace.id)
            } else {
                workspace.id.clone()
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    lines.push(format!("workspaces: {}", workspace_summary));

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mobile_help_shows_canonical_commands_only() {
        let help = mobile_help_text();
        assert!(help.contains("/ask <prompt>"));
        assert!(help.contains("/new -"));
        assert!(!help.contains("/reset"));
        assert!(!help.contains("/interrupt"));
        assert!(!help.contains("/switchdir"));
    }
}
