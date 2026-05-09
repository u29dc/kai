use super::*;

pub fn build_default_config_file() -> String {
    [
        "[agent]",
        "timezone = \"Europe/London\"",
        "",
        "[channel.telegram]",
        "enabled = true",
        "bot_token_env = \"KAI_TELEGRAM_BOT_TOKEN\"",
        "",
        "[channel.telegram.progress]",
        "enabled = true",
        "edit_interval_ms = 2500",
        "idle_update_secs = 8",
        "",
        "[media.transcription]",
        "provider = \"groq\"",
        "groq_api_key_env = \"GROQ_API_KEY\"",
        "groq_model = \"whisper-large-v3-turbo\"",
        "",
        "[paths]",
        "root_app = \"~/.tools/kai\"",
        "",
        "[runner.codex]",
        "binary = \"codex\"",
        "",
        "[workspaces]",
        "default = \"main\"",
        "",
        "[workspaces.main]",
        "label = \"Main\"",
        "path = \"~/.tools/kai/work\"",
        "",
    ]
    .join("\n")
}

pub(super) fn default_config(root_app: PathBuf) -> Config {
    Config {
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
            codex: CodexConfig {
                binary: "codex".to_string(),
                service_name: Some("kai".to_string()),
                override_config: None,
            },
        },
        workspaces: WorkspacesConfig {
            default_workspace: "main".to_string(),
            entries: BTreeMap::from([(
                "main".to_string(),
                WorkspaceConfig {
                    label: Some("Main".to_string()),
                    path: root_app.join("work").display().to_string(),
                },
            )]),
        },
    }
}

pub(super) fn default_transcription_command_timeout_secs() -> u64 {
    120
}

pub(super) fn default_transcription_command_max_output_bytes() -> usize {
    1_048_576
}
