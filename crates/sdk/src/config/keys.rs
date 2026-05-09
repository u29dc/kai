use super::*;

pub(super) const EDITABLE_CONFIG_KEYS: &[&str] = &[
    "agent.timezone",
    "channel.telegram.enabled",
    "channel.telegram.bot_token_env",
    "channel.telegram.owner_user_id",
    "channel.telegram.progress.enabled",
    "channel.telegram.progress.edit_interval_ms",
    "channel.telegram.progress.idle_update_secs",
    "media.transcription.provider",
    "media.transcription.groq_api_key_env",
    "media.transcription.groq_model",
    "media.transcription.command.executable",
    "media.transcription.command.args",
    "media.transcription.command.timeout_secs",
    "media.transcription.command.max_output_bytes",
    "paths.root_app",
    "runner.codex.binary",
    "runner.codex.service_name",
    "runner.codex.override.approval_policy",
    "runner.codex.override.sandbox_mode",
    "workspaces.default",
];

pub(super) fn ensure_config_key_allowed(key: &str) -> KaiResult<()> {
    if key.trim().is_empty() || key.split('.').any(|segment| segment.trim().is_empty()) {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    if is_static_config_key(key) || is_workspace_config_key(key) {
        return Ok(());
    }

    Err(
        KaiError::invalid_argument(format!("unknown or unsupported config key: {key}"))
            .with_hint("use `kai config show` to inspect available keys"),
    )
}

fn is_static_config_key(key: &str) -> bool {
    EDITABLE_CONFIG_KEYS.contains(&key)
}

fn is_workspace_config_key(key: &str) -> bool {
    let segments = key.split('.').collect::<Vec<_>>();
    if segments.len() != 3 || segments[0] != "workspaces" {
        return false;
    }
    let workspace_id = segments[1];
    !matches!(workspace_id, "" | "default") && matches!(segments[2], "label" | "path")
}
