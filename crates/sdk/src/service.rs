use fs2::FileExt;
use serde::Serialize;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime_fs::{
    ensure_private_dir, ensure_private_file, harden_private_file, write_private_executable,
};
use crate::state::state_paths;

#[cfg(target_os = "macos")]
use crate::secrets::{
    groq_api_key_keychain_service_name, groq_api_key_status, sync_groq_api_key_to_keychain,
    sync_telegram_token_to_keychain, telegram_token_keychain_service_name, telegram_token_status,
};

mod launchd;
mod lock;
mod logs;
#[cfg(test)]
mod tests;

pub use self::launchd::{
    service_restart, service_start, service_status, service_stop, service_uninstall,
};
pub use self::lock::{acquire_run_guard, run_lock_status};
pub use self::logs::service_logs;

const MAC_LABEL: &str = "ai.kai";
const SERVICE_STDOUT_FILE: &str = "service.stdout.log";
const SERVICE_STDERR_FILE: &str = "service.stderr.log";
const SERVICE_RUNNER_FILE: &str = "service-run.sh";
const SERVICE_SETTLE_TIMEOUT: Duration = Duration::from_secs(5);
const SERVICE_SETTLE_POLL: Duration = Duration::from_millis(100);

#[derive(Debug)]
pub struct RunGuard {
    file: File,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RunLockStatus {
    pub lock_path: String,
    pub locked: bool,
    pub pid: Option<u32>,
    pub stale: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatus {
    pub platform: String,
    pub label: String,
    pub installed: bool,
    pub loaded: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub active_mode: String,
    pub plist_path: Option<String>,
    pub stdout_path: String,
    pub stderr_path: String,
    pub lock: RunLockStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceActionOutput {
    pub action: String,
    pub status: ServiceStatus,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceLogsOutput {
    pub status: ServiceStatus,
    pub stdout_path: String,
    pub stderr_path: String,
    pub stdout_tail: Vec<String>,
    pub stderr_tail: Vec<String>,
}
fn runtime_binary_path(config: &LoadedConfig) -> PathBuf {
    Path::new(&config.values.paths.root_app).join("kai")
}

fn service_stdout_path(config: &LoadedConfig) -> PathBuf {
    Path::new(&config.values.paths.root_app)
        .join("logs")
        .join(SERVICE_STDOUT_FILE)
}

fn service_stderr_path(config: &LoadedConfig) -> PathBuf {
    Path::new(&config.values.paths.root_app)
        .join("logs")
        .join(SERVICE_STDERR_FILE)
}

fn service_runner_path(config: &LoadedConfig) -> PathBuf {
    Path::new(&config.values.paths.root_app)
        .join("bin")
        .join(SERVICE_RUNNER_FILE)
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| KaiError::new(ErrorCode::IoError, format!("failed to {action}: {error}"))
}

fn command_error(action: &str, stderr: &str, hint: Option<&str>) -> KaiError {
    let message = format!("{action} failed: {}", stderr.trim());
    let error = KaiError::new(ErrorCode::RuntimeError, message);
    match hint {
        Some(value) => error.with_hint(value),
        None => error,
    }
}
