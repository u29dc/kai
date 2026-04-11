use std::env;
use std::process::Command;

use serde::Serialize;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

const TELEGRAM_TOKEN_KEYCHAIN_SERVICE: &str = "ai.kai.telegram.bot-token";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramTokenStatus {
    pub env_key: String,
    pub env_available: bool,
    pub keychain_available: bool,
    pub keychain_service: Option<String>,
}

pub fn resolve_telegram_token(config: &LoadedConfig) -> KaiResult<String> {
    let status = telegram_token_status(config)?;
    let key = &config.values.channel.telegram.bot_token_env;

    if status.env_available {
        let value = env::var(key).map_err(missing_token_error(key))?;
        if !value.trim().is_empty() {
            return Ok(value);
        }
    }

    #[cfg(target_os = "macos")]
    {
        if let Some(value) = read_keychain_token()? {
            return Ok(value);
        }
    }

    Err(missing_token_error(key)(env::VarError::NotPresent))
}

pub fn telegram_token_status(config: &LoadedConfig) -> KaiResult<TelegramTokenStatus> {
    let env_key = config.values.channel.telegram.bot_token_env.clone();
    let env_available = env::var(&env_key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    #[cfg(target_os = "macos")]
    let keychain_available = read_keychain_token()?.is_some();

    #[cfg(not(target_os = "macos"))]
    let keychain_available = false;

    Ok(TelegramTokenStatus {
        env_key,
        env_available,
        keychain_available,
        keychain_service: if cfg!(target_os = "macos") {
            Some(TELEGRAM_TOKEN_KEYCHAIN_SERVICE.to_string())
        } else {
            None
        },
    })
}

#[cfg(target_os = "macos")]
pub fn sync_telegram_token_to_keychain(config: &LoadedConfig) -> KaiResult<String> {
    let env_key = &config.values.channel.telegram.bot_token_env;
    let token = env::var(env_key).map_err(missing_token_error(env_key))?;
    let account = current_username()?;

    let output = Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-U",
            "-a",
            account.as_str(),
            "-s",
            TELEGRAM_TOKEN_KEYCHAIN_SERVICE,
            "-T",
            "/usr/bin/security",
            "-w",
            token.as_str(),
        ])
        .output()
        .map_err(io_error("sync Telegram token to macOS Keychain"))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "failed to sync Telegram token to macOS Keychain: {}",
                stderr.trim()
            ),
        ));
    }

    Ok(TELEGRAM_TOKEN_KEYCHAIN_SERVICE.to_string())
}

#[cfg(target_os = "macos")]
pub fn telegram_token_keychain_service_name() -> &'static str {
    TELEGRAM_TOKEN_KEYCHAIN_SERVICE
}

#[cfg(target_os = "macos")]
fn read_keychain_token() -> KaiResult<Option<String>> {
    let account = current_username()?;
    let output = Command::new("/usr/bin/security")
        .args([
            "find-generic-password",
            "-w",
            "-a",
            account.as_str(),
            "-s",
            TELEGRAM_TOKEN_KEYCHAIN_SERVICE,
        ])
        .output()
        .map_err(io_error("read Telegram token from macOS Keychain"))?;

    if !output.status.success() {
        return Ok(None);
    }

    let token = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if token.is_empty() {
        return Ok(None);
    }

    Ok(Some(token))
}

#[cfg(target_os = "macos")]
fn current_username() -> KaiResult<String> {
    if let Ok(user) = env::var("USER")
        && !user.trim().is_empty()
    {
        return Ok(user);
    }

    let output = Command::new("id")
        .arg("-un")
        .output()
        .map_err(io_error("run `id -un`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to determine current username: {}", stderr.trim()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn missing_token_error(key: &str) -> impl Fn(std::env::VarError) -> KaiError {
    let key = key.to_string();
    move |_| {
        KaiError::blocked_prerequisite(format!("telegram bot token env `{key}` is not set"))
            .with_hint(
                "export the bot token env var for foreground use, or run `kai service restart` to sync it into the background service store",
            )
    }
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| KaiError::new(ErrorCode::IoError, format!("failed to {action}: {error}"))
}
