use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toml_edit::DocumentMut;

use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime_fs::{harden_private_file, write_private_file};

mod document;
#[cfg(test)]
mod tests;

use self::document::{
    load_or_create_document, remove_document_value, set_document_value, write_document,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoadedConfig {
    pub config_path: PathBuf,
    pub values: Config,
    pub config_exists: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub agent: AgentConfig,
    pub channel: ChannelConfig,
    pub media: MediaConfig,
    pub paths: PathsConfig,
    pub runner: RunnerConfig,
    pub context_files: ContextFilesConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub timezone: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelConfig {
    pub telegram: TelegramConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub enabled: bool,
    pub bot_token_env: String,
    pub owner_user_id: Option<i64>,
    pub progress: TelegramProgressConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramProgressConfig {
    pub enabled: bool,
    pub edit_interval_ms: u64,
    pub idle_update_secs: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub root_app: String,
    pub root_work: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MediaConfig {
    pub transcription: TranscriptionConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionConfig {
    pub provider: String,
    pub groq_api_key_env: String,
    pub groq_model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexConfig {
    pub binary: String,
    #[serde(rename = "override", skip_serializing_if = "Option::is_none")]
    pub override_config: Option<CodexOverride>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexOverride {
    pub approval_policy: Option<String>,
    pub sandbox_mode: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ContextFilesConfig {
    pub soul: String,
    pub memory: String,
    pub todo: String,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialConfig {
    agent: Option<PartialAgentConfig>,
    channel: Option<PartialChannelConfig>,
    media: Option<PartialMediaConfig>,
    paths: Option<PartialPathsConfig>,
    runner: Option<PartialRunnerConfig>,
    context_files: Option<PartialContextFilesConfig>,
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
    root_work: Option<String>,
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
    command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialRunnerConfig {
    codex: Option<PartialCodexConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCodexConfig {
    binary: Option<String>,
    #[serde(rename = "override")]
    override_config: Option<PartialCodexOverride>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCodexOverride {
    approval_policy: Option<String>,
    sandbox_mode: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialContextFilesConfig {
    soul: Option<String>,
    memory: Option<String>,
    todo: Option<String>,
}

pub fn load_config() -> KaiResult<LoadedConfig> {
    let config_path = discover_config_path();
    let config_exists = config_path.is_file();
    let root_app = default_root_app();
    let mut config = default_config(root_app);

    if config_exists {
        harden_private_file(&config_path)?;
        let raw = fs::read_to_string(&config_path).map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to read config file: {error}"),
            )
        })?;
        let partial = toml::from_str::<PartialConfig>(&raw).map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to parse config file: {error}"),
            )
        })?;
        apply_partial_config(&mut config, partial);
    }

    apply_env_overrides(&mut config);
    expand_config_paths(&mut config);

    Ok(LoadedConfig {
        config_path,
        values: config,
        config_exists,
    })
}

pub fn default_root_app() -> PathBuf {
    if let Some(path) = env::var_os("KAI_HOME") {
        return PathBuf::from(path);
    }

    if let Some(path) = env::var_os("TOOLS_HOME") {
        return PathBuf::from(path).join("kai");
    }

    if let Some(path) = env::var_os("HOME") {
        return PathBuf::from(path).join(".tools").join("kai");
    }

    PathBuf::from(".tools").join("kai")
}

pub fn default_root_work(root_app: &Path) -> PathBuf {
    root_app.join("work")
}

pub fn expand_home(input: &str) -> PathBuf {
    if input == "~" {
        return env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(input));
    }

    if let Some(rest) = input.strip_prefix("~/")
        && let Some(home) = env::var_os("HOME")
    {
        return PathBuf::from(home).join(rest);
    }

    PathBuf::from(input)
}

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
        "root_work = \"~/.tools/kai/work\"",
        "",
        "[runner.codex]",
        "binary = \"codex\"",
        "",
        "[context_files]",
        "soul = \"~/.tools/kai/SOUL.md\"",
        "memory = \"~/.tools/kai/MEMORY.md\"",
        "todo = \"~/.tools/kai/TODO.md\"",
        "",
    ]
    .join("\n")
}

pub fn ensure_config_file(path: &Path) -> KaiResult<()> {
    if path.is_file() {
        return Ok(());
    }

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to create config directory: {error}"),
            )
        })?;
    }

    write_private_file(path, build_default_config_file().as_bytes())?;
    Ok(())
}

pub fn config_value_at_key(config: &LoadedConfig, key: &str) -> KaiResult<JsonValue> {
    let root = serde_json::to_value(&config.values).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to serialize config view: {error}"),
        )
    })?;

    let mut current = &root;
    for segment in key.split('.') {
        current = current.get(segment).ok_or_else(|| {
            KaiError::invalid_argument(format!("unknown config key: {key}"))
                .with_hint("use `kai config show` to inspect available keys")
        })?;
    }

    Ok(current.clone())
}

pub fn set_config_value(key: &str, raw_value: &str) -> KaiResult<PathBuf> {
    let config_path = discover_config_path();
    let mut document = load_or_create_document(&config_path)?;
    set_document_value(&mut document, key, raw_value)?;
    write_document(&config_path, &mut document)?;
    Ok(config_path)
}

pub fn unset_config_value(key: &str) -> KaiResult<PathBuf> {
    let config_path = discover_config_path();
    let mut document = load_or_create_document(&config_path)?;
    remove_document_value(&mut document, key)?;
    write_document(&config_path, &mut document)?;
    Ok(config_path)
}

fn default_config(root_app: PathBuf) -> Config {
    let root_work = default_root_work(&root_app);

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
            root_work: root_work.display().to_string(),
        },
        runner: RunnerConfig {
            codex: CodexConfig {
                binary: "codex".to_string(),
                override_config: None,
            },
        },
        context_files: ContextFilesConfig {
            soul: root_app.join("SOUL.md").display().to_string(),
            memory: root_app.join("MEMORY.md").display().to_string(),
            todo: root_app.join("TODO.md").display().to_string(),
        },
    }
}

fn discover_config_path() -> PathBuf {
    if let Some(path) = env::var_os("KAI_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    default_root_app().join("config.toml")
}

fn apply_partial_config(config: &mut Config, partial: PartialConfig) {
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

    if let Some(paths) = partial.paths {
        if let Some(root_app) = paths.root_app {
            config.paths.root_app = root_app;
        }
        if let Some(root_work) = paths.root_work {
            config.paths.root_work = root_work;
        }
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
        if transcription.command.is_some() {
            config.media.transcription.command = transcription.command;
        }
    }

    if let Some(runner) = partial.runner
        && let Some(codex) = runner.codex
    {
        if let Some(binary) = codex.binary {
            config.runner.codex.binary = binary;
        }
        if let Some(override_config) = codex.override_config {
            config.runner.codex.override_config = Some(CodexOverride {
                approval_policy: override_config.approval_policy,
                sandbox_mode: override_config.sandbox_mode,
            });
        }
    }

    if let Some(context_files) = partial.context_files {
        if let Some(soul) = context_files.soul {
            config.context_files.soul = soul;
        }
        if let Some(memory) = context_files.memory {
            config.context_files.memory = memory;
        }
        if let Some(todo) = context_files.todo {
            config.context_files.todo = todo;
        }
    }
}

fn apply_env_overrides(config: &mut Config) {
    if let Ok(value) = env::var("KAI_TIMEZONE") {
        config.agent.timezone = value;
    }
    if let Ok(value) = env::var("KAI_ROOT_APP") {
        config.paths.root_app = value;
    }
    if let Ok(value) = env::var("KAI_ROOT_WORK") {
        config.paths.root_work = value;
    }
    if let Ok(value) = env::var("KAI_CODEX_BINARY") {
        config.runner.codex.binary = value;
    }
    if let Ok(value) = env::var("KAI_TRANSCRIPTION_PROVIDER") {
        config.media.transcription.provider = value;
    }
    if let Ok(value) = env::var("KAI_GROQ_API_KEY_ENV") {
        config.media.transcription.groq_api_key_env = value;
    }
    if let Ok(value) = env::var("KAI_GROQ_MODEL") {
        config.media.transcription.groq_model = value;
    }
    if let Ok(value) = env::var("KAI_TRANSCRIPTION_COMMAND") {
        config.media.transcription.command = Some(value);
    }
    if let Ok(value) = env::var("KAI_TELEGRAM_BOT_TOKEN_ENV") {
        config.channel.telegram.bot_token_env = value;
    }
}

fn expand_config_paths(config: &mut Config) {
    config.paths.root_app = expand_home(&config.paths.root_app).display().to_string();
    config.paths.root_work = expand_home(&config.paths.root_work).display().to_string();
    config.context_files.soul = expand_home(&config.context_files.soul)
        .display()
        .to_string();
    config.context_files.memory = expand_home(&config.context_files.memory)
        .display()
        .to_string();
    config.context_files.todo = expand_home(&config.context_files.todo)
        .display()
        .to_string();
}
