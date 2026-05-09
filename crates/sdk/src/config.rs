use std::collections::BTreeMap;
use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toml_edit::DocumentMut;

use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime_fs::{harden_private_file, write_private_file};

mod defaults;
mod document;
mod keys;
mod migration;
mod partial;
#[cfg(test)]
mod tests;

pub use self::defaults::build_default_config_file;
use self::defaults::{
    default_config, default_transcription_command_max_output_bytes,
    default_transcription_command_timeout_secs,
};
use self::document::{
    load_or_create_document, remove_document_value, set_document_value, write_document,
};
#[cfg(test)]
use self::keys::EDITABLE_CONFIG_KEYS;
use self::keys::ensure_config_key_allowed;
pub use self::migration::{migrate_config_to_workspaces, migrate_config_to_workspaces_at};
use self::partial::{PartialConfig, apply_partial_config, legacy_transcription_command};

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
    pub command: Option<TranscriptionCommandConfig>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TranscriptionCommandConfig {
    pub executable: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub args: String,
    #[serde(default = "default_transcription_command_timeout_secs")]
    pub timeout_secs: u64,
    #[serde(default = "default_transcription_command_max_output_bytes")]
    pub max_output_bytes: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerConfig {
    pub codex: CodexConfig,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunnerProvider {
    #[default]
    Codex,
}

impl RunnerProvider {
    pub fn as_key(self) -> &'static str {
        match self {
            Self::Codex => "codex",
        }
    }
}

impl Serialize for RunnerProvider {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(self.as_key())
    }
}

impl<'de> Deserialize<'de> for RunnerProvider {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let _stored = String::deserialize(deserializer)?;
        Ok(Self::Codex)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexConfig {
    pub binary: String,
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
        let legacy_keys = migration::legacy_config_keys(&raw)?;
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

fn discover_config_path() -> PathBuf {
    if let Some(path) = env::var_os("KAI_CONFIG_PATH") {
        return PathBuf::from(path);
    }

    default_root_app().join("config.toml")
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
        config.media.transcription.command = legacy_transcription_command(&value).ok();
    }
    if let Ok(value) = env::var("KAI_TELEGRAM_BOT_TOKEN_ENV") {
        config.channel.telegram.bot_token_env = value;
    }
}

fn expand_config_paths(config: &mut Config) {
    config.paths.root_app = expand_home(&config.paths.root_app).display().to_string();
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
    if let Some(command) = &config.media.transcription.command {
        if command.executable.trim().is_empty() {
            return Err(KaiError::new(
                ErrorCode::ConfigError,
                "media.transcription.command.executable cannot be empty",
            ));
        }
        if command.timeout_secs == 0 {
            return Err(KaiError::new(
                ErrorCode::ConfigError,
                "media.transcription.command.timeout_secs must be greater than zero",
            ));
        }
        if command.max_output_bytes == 0 {
            return Err(KaiError::new(
                ErrorCode::ConfigError,
                "media.transcription.command.max_output_bytes must be greater than zero",
            ));
        }
    }
    Ok(())
}

fn validate_document_config(document: &DocumentMut, config_path: &Path) -> KaiResult<()> {
    let raw = document.to_string();
    let legacy_keys = migration::legacy_config_keys(&raw)?;
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
