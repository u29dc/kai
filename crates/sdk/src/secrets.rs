use std::env;
use std::io::Write;
use std::process::{Command, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

const TELEGRAM_TOKEN_KEYCHAIN_SERVICE: &str = "ai.kai.telegram.bot-token";
const GROQ_API_KEY_KEYCHAIN_SERVICE: &str = "ai.kai.groq.api-key";
#[cfg(target_os = "macos")]
const KEYCHAIN_COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TelegramTokenStatus {
    pub env_key: String,
    pub env_available: bool,
    pub keychain_available: bool,
    pub keychain_service: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GroqApiKeyStatus {
    pub env_key: String,
    pub env_available: bool,
    pub keychain_available: bool,
    pub keychain_service: Option<String>,
}

pub fn resolve_telegram_token(config: &LoadedConfig) -> KaiResult<String> {
    resolve_required_secret(
        &config.values.channel.telegram.bot_token_env,
        TELEGRAM_TOKEN_KEYCHAIN_SERVICE,
        "telegram bot token",
        "export the bot token env var for foreground use, or run `kai service restart` to sync it into the background service store",
    )
}

pub fn telegram_token_status(config: &LoadedConfig) -> KaiResult<TelegramTokenStatus> {
    build_status(
        &config.values.channel.telegram.bot_token_env,
        TELEGRAM_TOKEN_KEYCHAIN_SERVICE,
    )
    .map(|status| TelegramTokenStatus {
        env_key: status.env_key,
        env_available: status.env_available,
        keychain_available: status.keychain_available,
        keychain_service: status.keychain_service,
    })
}

pub fn resolve_groq_api_key(config: &LoadedConfig) -> KaiResult<Option<String>> {
    resolve_optional_secret(
        &config.values.media.transcription.groq_api_key_env,
        GROQ_API_KEY_KEYCHAIN_SERVICE,
    )
}

pub fn groq_api_key_status(config: &LoadedConfig) -> KaiResult<GroqApiKeyStatus> {
    build_status(
        &config.values.media.transcription.groq_api_key_env,
        GROQ_API_KEY_KEYCHAIN_SERVICE,
    )
    .map(|status| GroqApiKeyStatus {
        env_key: status.env_key,
        env_available: status.env_available,
        keychain_available: status.keychain_available,
        keychain_service: status.keychain_service,
    })
}

#[cfg(target_os = "macos")]
pub fn sync_telegram_token_to_keychain(config: &LoadedConfig) -> KaiResult<String> {
    sync_secret_to_keychain(
        &config.values.channel.telegram.bot_token_env,
        TELEGRAM_TOKEN_KEYCHAIN_SERVICE,
        "Telegram token",
    )
}

#[cfg(target_os = "macos")]
pub fn sync_groq_api_key_to_keychain(config: &LoadedConfig) -> KaiResult<String> {
    sync_secret_to_keychain(
        &config.values.media.transcription.groq_api_key_env,
        GROQ_API_KEY_KEYCHAIN_SERVICE,
        "Groq API key",
    )
}

#[cfg(target_os = "macos")]
pub fn telegram_token_keychain_service_name() -> &'static str {
    TELEGRAM_TOKEN_KEYCHAIN_SERVICE
}

#[cfg(target_os = "macos")]
pub fn groq_api_key_keychain_service_name() -> &'static str {
    GROQ_API_KEY_KEYCHAIN_SERVICE
}

fn resolve_required_secret(
    env_key: &str,
    keychain_service: &str,
    label: &str,
    hint: &str,
) -> KaiResult<String> {
    if let Some(value) = resolve_optional_secret(env_key, keychain_service)? {
        return Ok(value);
    }

    Err(missing_required_secret_error(label, env_key).with_hint(hint))
}

fn resolve_optional_secret(env_key: &str, keychain_service: &str) -> KaiResult<Option<String>> {
    if let Ok(value) = env::var(env_key)
        && !value.trim().is_empty()
    {
        return Ok(Some(value));
    }

    #[cfg(target_os = "macos")]
    {
        read_keychain_secret(keychain_service)
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = keychain_service;
        Ok(None)
    }
}

fn build_status(env_key: &str, keychain_service: &str) -> KaiResult<SecretStatus> {
    let env_available = env::var(env_key)
        .map(|value| !value.trim().is_empty())
        .unwrap_or(false);

    #[cfg(target_os = "macos")]
    let keychain_available = read_keychain_secret(keychain_service)?.is_some();

    #[cfg(not(target_os = "macos"))]
    let keychain_available = false;

    Ok(SecretStatus {
        env_key: env_key.to_string(),
        env_available,
        keychain_available,
        keychain_service: if cfg!(target_os = "macos") {
            Some(keychain_service.to_string())
        } else {
            None
        },
    })
}

#[cfg(target_os = "macos")]
fn sync_secret_to_keychain(
    env_key: &str,
    keychain_service: &str,
    label: &str,
) -> KaiResult<String> {
    let value = env::var(env_key)
        .map_err(|_| missing_required_secret_error(&format!("{label} env"), env_key))?;
    let account = current_username()?;

    if read_keychain_secret(keychain_service)?.as_deref() == Some(value.as_str()) {
        return Ok(keychain_service.to_string());
    }

    let mut command = Command::new("/usr/bin/security");
    command.args([
        "add-generic-password",
        "-U",
        "-a",
        account.as_str(),
        "-s",
        keychain_service,
        "-T",
        "/usr/bin/security",
        "-w",
    ]);
    let output = command_output_with_timeout(
        command,
        KEYCHAIN_COMMAND_TIMEOUT,
        "sync secret to macOS Keychain",
        Some(&value),
    )?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "failed to sync {label} to macOS Keychain: {}",
                stderr.trim()
            ),
        ));
    }

    Ok(keychain_service.to_string())
}

#[cfg(target_os = "macos")]
fn read_keychain_secret(keychain_service: &str) -> KaiResult<Option<String>> {
    let account = current_username()?;
    let mut command = Command::new("/usr/bin/security");
    command.args([
        "find-generic-password",
        "-w",
        "-a",
        account.as_str(),
        "-s",
        keychain_service,
    ]);
    let output = command_output_with_timeout(
        command,
        KEYCHAIN_COMMAND_TIMEOUT,
        "read secret from macOS Keychain",
        None,
    )?;

    if !output.status.success() {
        return Ok(None);
    }

    let value = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if value.is_empty() {
        return Ok(None);
    }

    Ok(Some(value))
}

#[cfg(target_os = "macos")]
fn current_username() -> KaiResult<String> {
    if let Ok(user) = env::var("USER")
        && !user.trim().is_empty()
    {
        return Ok(user);
    }

    let mut command = Command::new("id");
    command.arg("-un");
    let output =
        command_output_with_timeout(command, Duration::from_secs(5), "run `id -un`", None)?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to determine current username: {}", stderr.trim()),
        ));
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "macos")]
fn command_output_with_timeout(
    mut command: Command,
    timeout_duration: Duration,
    action: &'static str,
    stdin: Option<&str>,
) -> KaiResult<Output> {
    command.stdout(Stdio::piped()).stderr(Stdio::piped());
    if stdin.is_some() {
        command.stdin(Stdio::piped());
    } else {
        command.stdin(Stdio::null());
    }
    let mut child = command.spawn().map_err(io_error(action))?;
    if let Some(input) = stdin
        && let Some(mut child_stdin) = child.stdin.take()
    {
        child_stdin
            .write_all(input.as_bytes())
            .and_then(|_| child_stdin.write_all(b"\n"))
            .map_err(io_error(action))?;
    }
    let started = Instant::now();

    loop {
        if child.try_wait().map_err(io_error(action))?.is_some() {
            return child.wait_with_output().map_err(io_error(action));
        }
        if started.elapsed() >= timeout_duration {
            let _ = child.kill();
            let _ = child.wait();
            return Err(KaiError::new(
                ErrorCode::RuntimeError,
                format!("timed out while trying to {action}"),
            ));
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn missing_required_secret_error(label: &str, env_key: &str) -> KaiError {
    KaiError::blocked_prerequisite(format!("{label} env `{env_key}` is not set"))
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| KaiError::new(ErrorCode::IoError, format!("failed to {action}: {error}"))
}

#[derive(Debug, Clone)]
struct SecretStatus {
    env_key: String,
    env_available: bool,
    keychain_available: bool,
    keychain_service: Option<String>,
}
