use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use toml_edit::{DocumentMut, Item, Table, value};

use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime_fs::{harden_private_file, write_private_file};

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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PathsConfig {
    pub root_app: String,
    pub root_work: String,
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
}

#[derive(Debug, Clone, Deserialize, Default)]
struct PartialPathsConfig {
    root_app: Option<String>,
    root_work: Option<String>,
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
    }

    if let Some(paths) = partial.paths {
        if let Some(root_app) = paths.root_app {
            config.paths.root_app = root_app;
        }
        if let Some(root_work) = paths.root_work {
            config.paths.root_work = root_work;
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

fn load_or_create_document(path: &Path) -> KaiResult<DocumentMut> {
    if path.is_file() {
        let raw = fs::read_to_string(path).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to read config file: {error}"),
            )
        })?;
        return DocumentMut::from_str(&raw).map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to parse config document: {error}"),
            )
        });
    }

    DocumentMut::from_str("").map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to initialize config document: {error}"),
        )
    })
}

fn write_document(path: &Path, document: &mut DocumentMut) -> KaiResult<()> {
    prune_empty_tables(document.as_item_mut());

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to create config directory: {error}"),
            )
        })?;
    }

    fs::write(path, document.to_string()).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to write config file: {error}"),
        )
    })?;

    Ok(())
}

fn prune_empty_tables(item: &mut Item) -> bool {
    let Some(table) = item.as_table_like_mut() else {
        return false;
    };

    let keys = table
        .iter()
        .map(|(key, _)| key.to_string())
        .collect::<Vec<_>>();
    let mut empty_keys = Vec::new();

    for key in keys {
        let Some(child) = table.get_mut(&key) else {
            continue;
        };

        if prune_empty_tables(child) {
            empty_keys.push(key);
        }
    }

    for key in empty_keys {
        table.remove(&key);
    }

    table.is_empty()
}

fn set_document_value(document: &mut DocumentMut, key: &str, raw_value: &str) -> KaiResult<()> {
    let mut segments = key.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    let leaf = segments.pop().unwrap_or_default();
    let mut current = document.as_item_mut();
    for segment in segments {
        if !current.is_table() {
            *current = Item::Table(Table::new());
        }
        current = &mut current[segment];
    }

    *current = ensure_table_item(current);
    current[leaf] = parse_config_item(raw_value)?;
    Ok(())
}

fn remove_document_value(document: &mut DocumentMut, key: &str) -> KaiResult<()> {
    let segments = key.split('.').collect::<Vec<_>>();
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    remove_item_value(document.as_item_mut(), &segments, key)?;
    Ok(())
}

fn remove_item_value(item: &mut Item, segments: &[&str], full_key: &str) -> KaiResult<bool> {
    if segments.is_empty() {
        return Err(KaiError::invalid_argument("config key cannot be empty"));
    }

    let segment = segments[0];
    let Some(table) = item.as_table_like_mut() else {
        return Err(KaiError::invalid_argument(format!(
            "unknown config key: {full_key}"
        )));
    };

    if segments.len() == 1 {
        if table.remove(segment).is_none() {
            return Err(
                KaiError::invalid_argument(format!("unknown config key: {full_key}"))
                    .with_hint("use `kai config show` to inspect available keys"),
            );
        }
        return Ok(table.is_empty());
    }

    let child = table.get_mut(segment).ok_or_else(|| {
        KaiError::invalid_argument(format!("unknown config key: {full_key}"))
            .with_hint("use `kai config show` to inspect available keys")
    })?;
    let child_empty = remove_item_value(child, &segments[1..], full_key)?;
    if child_empty {
        table.remove(segment);
    }
    Ok(table.is_empty())
}

fn ensure_table_item(item: &mut Item) -> Item {
    if item.is_table() {
        return item.clone();
    }

    Item::Table(Table::new())
}

fn parse_config_item(raw_value: &str) -> KaiResult<Item> {
    if let Ok(boolean) = raw_value.parse::<bool>() {
        return Ok(value(boolean));
    }
    if let Ok(integer) = raw_value.parse::<i64>() {
        return Ok(value(integer));
    }
    if let Ok(float) = raw_value.parse::<f64>() {
        return Ok(value(float));
    }

    Ok(value(raw_value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_default_config_contains_expected_sections() {
        let config = build_default_config_file();
        assert!(config.contains("[agent]"));
        assert!(config.contains("[channel.telegram]"));
        assert!(config.contains("[paths]"));
    }

    #[test]
    fn expand_home_resolves_tilde_prefix() {
        let home = std::env::var("HOME").expect("HOME must be available in tests");
        let path = expand_home("~/tmp/kai-test");
        assert_eq!(path, PathBuf::from(home).join("tmp").join("kai-test"));
    }

    #[test]
    fn remove_document_value_prunes_empty_parent_tables() {
        let mut document = DocumentMut::from_str(
            r#"
[channel.telegram]
owner_user_id = 123
"#,
        )
        .expect("document");

        remove_document_value(&mut document, "channel.telegram.owner_user_id")
            .expect("remove value");

        let rendered = document.to_string();
        assert!(!rendered.contains("[channel]"));
        assert!(!rendered.contains("[channel.telegram]"));
    }
}
