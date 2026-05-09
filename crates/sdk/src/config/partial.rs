use super::*;

#[derive(Debug, Clone, Deserialize, Default)]
pub(super) struct PartialConfig {
    agent: Option<PartialAgentConfig>,
    channel: Option<PartialChannelConfig>,
    media: Option<PartialMediaConfig>,
    paths: Option<PartialPathsConfig>,
    runner: Option<PartialRunnerConfig>,
    workspaces: Option<PartialWorkspacesConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialAgentConfig {
    timezone: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialChannelConfig {
    telegram: Option<PartialTelegramConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialTelegramConfig {
    enabled: Option<bool>,
    bot_token_env: Option<String>,
    owner_user_id: Option<i64>,
    progress: Option<PartialTelegramProgressConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialTelegramProgressConfig {
    enabled: Option<bool>,
    edit_interval_ms: Option<u64>,
    idle_update_secs: Option<u64>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialPathsConfig {
    root_app: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialMediaConfig {
    transcription: Option<PartialTranscriptionConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialTranscriptionConfig {
    provider: Option<String>,
    groq_api_key_env: Option<String>,
    groq_model: Option<String>,
    command: Option<PartialTranscriptionCommandValue>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum PartialTranscriptionCommandValue {
    Legacy(String),
    Structured(PartialTranscriptionCommandConfig),
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialTranscriptionCommandConfig {
    executable: Option<String>,
    args: Option<String>,
    timeout_secs: Option<u64>,
    max_output_bytes: Option<usize>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialRunnerConfig {
    codex: Option<PartialCodexConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCodexConfig {
    binary: Option<String>,
    service_name: Option<String>,
    #[serde(rename = "override")]
    override_config: Option<PartialCodexOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCodexOverride {
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialWorkspacesConfig {
    #[serde(rename = "default")]
    default_workspace: Option<String>,
    #[serde(flatten)]
    entries: BTreeMap<String, PartialWorkspaceConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialWorkspaceConfig {
    label: Option<String>,
    path: Option<String>,
}

pub(super) fn apply_partial_config(config: &mut Config, partial: PartialConfig) {
    if let Some(agent) = partial.agent
        && let Some(timezone) = agent.timezone
    {
        config.agent.timezone = timezone;
    }

    if let Some(channel) = partial.channel
        && let Some(telegram) = channel.telegram
    {
        if let Some(enabled) = telegram.enabled {
            config.channel.telegram.enabled = enabled;
        }
        if let Some(bot_token_env) = telegram.bot_token_env {
            config.channel.telegram.bot_token_env = bot_token_env;
        }
        if telegram.owner_user_id.is_some() {
            config.channel.telegram.owner_user_id = telegram.owner_user_id;
        }
        if let Some(progress) = telegram.progress {
            if let Some(enabled) = progress.enabled {
                config.channel.telegram.progress.enabled = enabled;
            }
            if let Some(edit_interval_ms) = progress.edit_interval_ms {
                config.channel.telegram.progress.edit_interval_ms = edit_interval_ms;
            }
            if let Some(idle_update_secs) = progress.idle_update_secs {
                config.channel.telegram.progress.idle_update_secs = idle_update_secs;
            }
        }
    }

    if let Some(paths) = partial.paths
        && let Some(root_app) = paths.root_app
    {
        config.paths.root_app = root_app;
    }

    if let Some(media) = partial.media
        && let Some(transcription) = media.transcription
    {
        if let Some(provider) = transcription.provider {
            config.media.transcription.provider = provider;
        }
        if let Some(groq_api_key_env) = transcription.groq_api_key_env {
            config.media.transcription.groq_api_key_env = groq_api_key_env;
        }
        if let Some(groq_model) = transcription.groq_model {
            config.media.transcription.groq_model = groq_model;
        }
        if let Some(command) = transcription.command {
            config.media.transcription.command =
                merge_transcription_command(config.media.transcription.command.take(), command);
        }
    }

    if let Some(runner) = partial.runner
        && let Some(codex) = runner.codex
    {
        if let Some(binary) = codex.binary {
            config.runner.codex.binary = binary;
        }
        if codex.service_name.is_some() {
            config.runner.codex.service_name = codex.service_name;
        }
        if let Some(override_config) = codex.override_config {
            config.runner.codex.override_config = Some(CodexOverride {
                approval_policy: override_config.approval_policy,
                sandbox_mode: override_config.sandbox_mode,
            });
        }
    }

    if let Some(workspaces) = partial.workspaces {
        if let Some(default_workspace) = workspaces.default_workspace {
            config.workspaces.default_workspace = default_workspace;
        }
        if !workspaces.entries.is_empty() {
            config.workspaces.entries.clear();
        }
        for (id, workspace) in workspaces.entries {
            config.workspaces.entries.insert(
                id,
                WorkspaceConfig {
                    label: workspace.label,
                    path: workspace.path.unwrap_or_default(),
                },
            );
        }
    }
}

fn merge_transcription_command(
    current: Option<TranscriptionCommandConfig>,
    partial: PartialTranscriptionCommandValue,
) -> Option<TranscriptionCommandConfig> {
    match partial {
        PartialTranscriptionCommandValue::Legacy(value) => {
            legacy_transcription_command(&value).ok()
        }
        PartialTranscriptionCommandValue::Structured(partial) => {
            let mut command = current.unwrap_or_else(|| TranscriptionCommandConfig {
                executable: String::new(),
                args: String::new(),
                timeout_secs: default_transcription_command_timeout_secs(),
                max_output_bytes: default_transcription_command_max_output_bytes(),
            });
            if let Some(executable) = partial.executable {
                command.executable = executable;
            }
            if let Some(args) = partial.args {
                command.args = args;
            }
            if let Some(timeout_secs) = partial.timeout_secs {
                command.timeout_secs = timeout_secs;
            }
            if let Some(max_output_bytes) = partial.max_output_bytes {
                command.max_output_bytes = max_output_bytes;
            }
            Some(command)
        }
    }
}

pub(super) fn legacy_transcription_command(value: &str) -> KaiResult<TranscriptionCommandConfig> {
    let mut parts = split_argv_like(value)?;
    if parts.is_empty() {
        return Err(KaiError::invalid_argument(
            "transcription command cannot be empty",
        ));
    }
    let executable = parts.remove(0);
    Ok(TranscriptionCommandConfig {
        executable,
        args: parts.join(" "),
        timeout_secs: default_transcription_command_timeout_secs(),
        max_output_bytes: default_transcription_command_max_output_bytes(),
    })
}

fn split_argv_like(input: &str) -> KaiResult<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(char) = chars.next() {
        match (quote, char) {
            (Some(active), value) if value == active => quote = None,
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), value) => current.push(value),
            (None, '\'' | '"') => quote = Some(char),
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            (None, value) => current.push(value),
        }
    }

    if quote.is_some() {
        return Err(KaiError::invalid_argument(
            "transcription command has an unterminated quote",
        ));
    }
    if !current.is_empty() {
        args.push(current);
    }

    Ok(args)
}
