use chrono::Utc;
use serde_json::json;
use uuid::Uuid;

use crate::config::LoadedConfig;
use crate::context::context_report;
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

    let replay_package = create_replay_package(
        &result.context_snapshots,
        &state.recent_turns_for_target(&target, 24)?,
    );
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
        "/reset - same as /new",
        "/cancel - stop the current running Codex turn",
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

    for entry in context.entries {
        let status = if entry.exists { "ok" } else { "missing" };
        lines.push(format!("context {}: {}", entry.role, status));
    }

    Ok(lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, MediaConfig,
        PathsConfig, RunnerConfig, RunnerProvider, TelegramConfig, TelegramProgressConfig,
        TranscriptionConfig, WorkspaceConfig, WorkspacesConfig,
    };
    use crate::error::ErrorCode;
    use std::path::Path;
    use tempfile::tempdir;

    fn test_config(root_app: &Path, root_work: &Path, provider: RunnerProvider) -> LoadedConfig {
        LoadedConfig {
            config_path: root_app.join("config.toml"),
            config_exists: false,
            values: Config {
                agent: AgentConfig {
                    timezone: "Europe/London".to_string(),
                },
                channel: ChannelConfig {
                    telegram: TelegramConfig {
                        enabled: true,
                        bot_token_env: "KAI_TELEGRAM_BOT_TOKEN".to_string(),
                        owner_user_id: None,
                        progress: TelegramProgressConfig {
                            enabled: true,
                            edit_interval_ms: 2500,
                            idle_update_secs: 8,
                        },
                    },
                },
                media: MediaConfig {
                    transcription: TranscriptionConfig {
                        provider: "groq".to_string(),
                        groq_api_key_env: "GROQ_API_KEY".to_string(),
                        groq_model: "whisper-large-v3-turbo".to_string(),
                        command: None,
                    },
                },
                paths: PathsConfig {
                    root_app: root_app.display().to_string(),
                },
                runner: RunnerConfig {
                    provider,
                    codex: CodexConfig {
                        binary: "codex".to_string(),
                        override_config: None,
                    },
                },
                context_files: ContextFilesConfig {
                    soul: root_app.join("SOUL.md").display().to_string(),
                    memory: root_app.join("MEMORY.md").display().to_string(),
                },
                workspaces: WorkspacesConfig {
                    default_workspace: "main".to_string(),
                    entries: std::collections::BTreeMap::from([(
                        "main".to_string(),
                        WorkspaceConfig {
                            label: Some("Main".to_string()),
                            path: root_work.display().to_string(),
                        },
                    )]),
                },
            },
        }
    }

    #[test]
    fn handle_owner_prompt_blocks_unsupported_provider_before_recording_turn() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work, RunnerProvider::Claude);
        let state = StateStore::open(&config).expect("state store");

        let error = handle_owner_prompt(&config, &state, "telegram", 42, "hello", &[])
            .expect_err("unsupported provider should be blocked");

        assert!(matches!(error.code, ErrorCode::BlockedPrerequisite));
        assert_eq!(state.recent_turns(10).expect("recent turns").len(), 0);
    }
}
