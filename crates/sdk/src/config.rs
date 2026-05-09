use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{SystemTime, UNIX_EPOCH};

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
    pub workspaces: WorkspacesConfig,
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
    pub provider: RunnerProvider,
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CodexTransport {
    Exec,
    #[default]
    AppServer,
}

impl CodexTransport {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Exec => "exec",
            Self::AppServer => "app_server",
        }
    }
}

impl FromStr for CodexTransport {
    type Err = KaiError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        match input.trim().to_ascii_lowercase().as_str() {
            "exec" => Ok(Self::Exec),
            "app_server" | "app-server" | "appserver" => Ok(Self::AppServer),
            _ => Err(KaiError::invalid_argument(format!(
                "unsupported codex transport: {input}"
            ))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RunnerProvider {
    #[default]
    Codex,
    Claude,
}

impl RunnerProvider {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexConfig {
    pub binary: String,
    #[serde(default)]
    pub transport: CodexTransport,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspacesConfig {
    #[serde(rename = "default")]
    pub default_workspace: String,
    #[serde(flatten)]
    pub entries: BTreeMap<String, WorkspaceConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    pub path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMigrationResult {
    pub config_path: PathBuf,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<PathBuf>,
    pub migrated: bool,
    pub default_workspace_id: String,
    pub removed_legacy_keys: Vec<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialConfig {
    agent: Option<PartialAgentConfig>,
    channel: Option<PartialChannelConfig>,
    media: Option<PartialMediaConfig>,
    paths: Option<PartialPathsConfig>,
    runner: Option<PartialRunnerConfig>,
    context_files: Option<PartialContextFilesConfig>,
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
    command: Option<String>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialRunnerConfig {
    provider: Option<RunnerProvider>,
    codex: Option<PartialCodexConfig>,
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialCodexConfig {
    binary: Option<String>,
    transport: Option<CodexTransport>,
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
struct PartialContextFilesConfig {
    soul: Option<String>,
    memory: Option<String>,
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
        let legacy_keys = legacy_config_keys(&raw)?;
        if !legacy_keys.is_empty() {
            return Err(KaiError::blocked_prerequisite(
                "legacy kai config format is no longer supported",
            )
            .with_hint(format!(
                "run `kai config migrate` to rewrite {}",
                config_path.display()
            )));
        }
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
    validate_config(&config)?;

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
        "",
        "[runner]",
        "provider = \"codex\"",
        "",
        "[runner.codex]",
        "binary = \"codex\"",
        "transport = \"app_server\"",
        "",
        "[context_files]",
        "soul = \"~/.tools/kai/SOUL.md\"",
        "memory = \"~/.tools/kai/MEMORY.md\"",
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
    ensure_config_key_allowed(key)?;
    let mut document = load_or_create_document(&config_path)?;
    set_document_value(&mut document, key, raw_value)?;
    validate_document_config(&document, &config_path)?;
    write_document(&config_path, &mut document)?;
    Ok(config_path)
}

pub fn unset_config_value(key: &str) -> KaiResult<PathBuf> {
    let config_path = discover_config_path();
    ensure_config_key_allowed(key)?;
    let mut document = load_or_create_document(&config_path)?;
    remove_document_value(&mut document, key)?;
    validate_document_config(&document, &config_path)?;
    write_document(&config_path, &mut document)?;
    Ok(config_path)
}

pub fn migrate_config_to_workspaces() -> KaiResult<ConfigMigrationResult> {
    let config_path = discover_config_path();
    migrate_config_to_workspaces_at(&config_path)
}

pub fn migrate_config_to_workspaces_at(config_path: &Path) -> KaiResult<ConfigMigrationResult> {
    if !config_path.is_file() {
        ensure_config_file(config_path)?;
        return Ok(ConfigMigrationResult {
            config_path: config_path.to_path_buf(),
            backup_path: None,
            migrated: false,
            default_workspace_id: "main".to_string(),
            removed_legacy_keys: Vec::new(),
        });
    }

    harden_private_file(config_path)?;
    let raw = fs::read_to_string(config_path).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to read config file: {error}"),
        )
    })?;
    let legacy_keys = legacy_config_keys(&raw)?;
    let mut document = DocumentMut::from_str(&raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to parse config file for migration: {error}"),
        )
    })?;

    let default_workspace_id = ensure_workspace_document_layout(&mut document)?;
    let mut removed_legacy_keys = Vec::new();
    for key in ["paths.root_work", "context_files.todo"] {
        if remove_document_value(&mut document, key).is_ok() {
            removed_legacy_keys.push(key.to_string());
        }
    }

    let backup_path = if legacy_keys.is_empty() && removed_legacy_keys.is_empty() {
        None
    } else {
        let backup_path = backup_config_path(config_path);
        fs::copy(config_path, &backup_path).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to write config backup: {error}"),
            )
        })?;
        harden_private_file(&backup_path)?;
        write_document(config_path, &mut document)?;
        Some(backup_path)
    };

    Ok(ConfigMigrationResult {
        config_path: config_path.to_path_buf(),
        backup_path: backup_path.clone(),
        migrated: backup_path.is_some(),
        default_workspace_id,
        removed_legacy_keys,
    })
}

fn default_config(root_app: PathBuf) -> Config {
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
            provider: RunnerProvider::Codex,
            codex: CodexConfig {
                binary: "codex".to_string(),
                transport: CodexTransport::AppServer,
                service_name: Some("kai".to_string()),
                override_config: None,
            },
        },
        context_files: ContextFilesConfig {
            soul: root_app.join("SOUL.md").display().to_string(),
            memory: root_app.join("MEMORY.md").display().to_string(),
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
        if transcription.command.is_some() {
            config.media.transcription.command = transcription.command;
        }
    }

    if let Some(runner) = partial.runner {
        if let Some(provider) = runner.provider {
            config.runner.provider = provider;
        }

        if let Some(codex) = runner.codex {
            if let Some(binary) = codex.binary {
                config.runner.codex.binary = binary;
            }
            if let Some(transport) = codex.transport {
                config.runner.codex.transport = transport;
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
    }

    if let Some(context_files) = partial.context_files {
        if let Some(soul) = context_files.soul {
            config.context_files.soul = soul;
        }
        if let Some(memory) = context_files.memory {
            config.context_files.memory = memory;
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

fn apply_env_overrides(config: &mut Config) {
    if let Ok(value) = env::var("KAI_TIMEZONE") {
        config.agent.timezone = value;
    }
    if let Ok(value) = env::var("KAI_ROOT_APP") {
        config.paths.root_app = value;
    }
    if let Ok(value) = env::var("KAI_CODEX_BINARY") {
        config.runner.codex.binary = value;
    }
    if let Ok(value) = env::var("KAI_CODEX_TRANSPORT")
        && let Ok(transport) = CodexTransport::from_str(&value)
    {
        config.runner.codex.transport = transport;
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
    config.context_files.soul = expand_home(&config.context_files.soul)
        .display()
        .to_string();
    config.context_files.memory = expand_home(&config.context_files.memory)
        .display()
        .to_string();
    for workspace in config.workspaces.entries.values_mut() {
        workspace.path = expand_home(&workspace.path).display().to_string();
    }
}

fn validate_config(config: &Config) -> KaiResult<()> {
    if config.workspaces.default_workspace.trim().is_empty() {
        return Err(KaiError::new(
            ErrorCode::ConfigError,
            "workspaces.default cannot be empty",
        ));
    }
    if config.workspaces.entries.is_empty() {
        return Err(KaiError::new(
            ErrorCode::ConfigError,
            "at least one workspace must be configured",
        ));
    }
    if !config
        .workspaces
        .entries
        .contains_key(config.workspaces.default_workspace.as_str())
    {
        return Err(KaiError::new(
            ErrorCode::ConfigError,
            format!(
                "workspaces.default `{}` does not exist",
                config.workspaces.default_workspace
            ),
        ));
    }
    for (id, workspace) in &config.workspaces.entries {
        if id.trim().is_empty() || id == "default" {
            return Err(KaiError::new(
                ErrorCode::ConfigError,
                "workspace ids cannot be empty or `default`",
            ));
        }
        if workspace.path.trim().is_empty() {
            return Err(KaiError::new(
                ErrorCode::ConfigError,
                format!("workspace `{id}` must define a path"),
            ));
        }
    }
    Ok(())
}

fn validate_document_config(document: &DocumentMut, config_path: &Path) -> KaiResult<()> {
    let raw = document.to_string();
    let legacy_keys = legacy_config_keys(&raw)?;
    if !legacy_keys.is_empty() {
        return Err(KaiError::blocked_prerequisite(
            "legacy kai config format is no longer supported",
        )
        .with_hint(format!(
            "run `kai config migrate` to rewrite {}",
            config_path.display()
        )));
    }

    let partial = toml::from_str::<PartialConfig>(&raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to parse updated config file: {error}"),
        )
    })?;
    let mut config = default_config(default_root_app());
    apply_partial_config(&mut config, partial);
    expand_config_paths(&mut config);
    validate_config(&config)
}

fn ensure_config_key_allowed(key: &str) -> KaiResult<()> {
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
    matches!(
        key,
        "agent.timezone"
            | "channel.telegram.enabled"
            | "channel.telegram.bot_token_env"
            | "channel.telegram.owner_user_id"
            | "channel.telegram.progress.enabled"
            | "channel.telegram.progress.edit_interval_ms"
            | "channel.telegram.progress.idle_update_secs"
            | "media.transcription.provider"
            | "media.transcription.groq_api_key_env"
            | "media.transcription.groq_model"
            | "media.transcription.command"
            | "paths.root_app"
            | "runner.provider"
            | "runner.codex.binary"
            | "runner.codex.transport"
            | "runner.codex.service_name"
            | "runner.codex.override.approval_policy"
            | "runner.codex.override.sandbox_mode"
            | "context_files.soul"
            | "context_files.memory"
            | "workspaces.default"
    )
}

fn is_workspace_config_key(key: &str) -> bool {
    let segments = key.split('.').collect::<Vec<_>>();
    if segments.len() != 3 || segments[0] != "workspaces" {
        return false;
    }
    let workspace_id = segments[1];
    !matches!(workspace_id, "" | "default") && matches!(segments[2], "label" | "path")
}

fn legacy_config_keys(raw: &str) -> KaiResult<Vec<String>> {
    let document = DocumentMut::from_str(raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to inspect config file: {error}"),
        )
    })?;

    let mut keys = Vec::new();
    if document
        .get("paths")
        .and_then(|item| item.get("root_work"))
        .is_some()
    {
        keys.push("paths.root_work".to_string());
    }
    if document
        .get("context_files")
        .and_then(|item| item.get("todo"))
        .is_some()
    {
        keys.push("context_files.todo".to_string());
    }
    let has_workspaces = document
        .get("workspaces")
        .and_then(|item| item.get("default"))
        .is_some();
    if !has_workspaces {
        keys.push("workspaces.default".to_string());
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

fn ensure_workspace_document_layout(document: &mut DocumentMut) -> KaiResult<String> {
    let existing_default = document
        .get("workspaces")
        .and_then(|item| item.get("default"))
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    if let Some(default_workspace_id) = existing_default {
        return Ok(default_workspace_id);
    }

    let workspace_path = document
        .get("paths")
        .and_then(|item| item.get("root_work"))
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "~/.tools/kai/work".to_string());
    let workspace_id = derive_workspace_id(&workspace_path);
    let workspace_label = derive_workspace_label(&workspace_id);
    set_document_value(document, "workspaces.default", &workspace_id)?;
    set_document_value(
        document,
        &format!("workspaces.{workspace_id}.label"),
        &workspace_label,
    )?;
    set_document_value(
        document,
        &format!("workspaces.{workspace_id}.path"),
        &workspace_path,
    )?;
    Ok(workspace_id)
}

fn derive_workspace_id(path: &str) -> String {
    let file_name = expand_home(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "main".to_string());
    let mut id = file_name
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() {
                char.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    if id.is_empty() || id == "default" {
        "main".to_string()
    } else {
        id
    }
}

fn derive_workspace_label(id: &str) -> String {
    if id == "main" {
        return "Main".to_string();
    }

    id.split('-')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = String::new();
                    label.push(first.to_ascii_uppercase());
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn backup_config_path(config_path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let base_name = config_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config.toml");
    config_path.with_file_name(format!("{base_name}.bak.{timestamp}"))
}
