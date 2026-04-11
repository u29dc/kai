use fs2::FileExt;
use serde::Serialize;
use std::env;
use std::fs::{self, File, OpenOptions};
use std::io::{Seek, SeekFrom, Write};
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

pub fn acquire_run_guard(config: &LoadedConfig) -> KaiResult<RunGuard> {
    let lock_path = run_lock_path(config);
    let parent = lock_path.parent().ok_or_else(|| {
        KaiError::new(
            ErrorCode::StateError,
            format!("invalid run lock path: {}", lock_path.display()),
        )
    })?;
    fs::create_dir_all(parent).map_err(io_error("create state directory"))?;

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io_error("open run lock"))?;

    if let Err(error) = file.try_lock_exclusive() {
        if is_lock_contended(&error) {
            let pid = read_lock_pid(&lock_path);
            let detail = pid
                .map(|value| format!("kai is already running (pid {value})"))
                .unwrap_or_else(|| "kai is already running".to_string());
            return Err(KaiError::blocked_prerequisite(detail)
                .with_hint("stop the existing `kai run` process or `kai service stop` first"));
        }

        return Err(KaiError::new(
            ErrorCode::StateError,
            format!("failed to lock run state: {error}"),
        ));
    }

    write_lock_pid(&mut file, std::process::id())?;
    Ok(RunGuard { file })
}

pub fn run_lock_status(config: &LoadedConfig) -> KaiResult<RunLockStatus> {
    let lock_path = run_lock_path(config);
    let recorded_pid = read_lock_pid(&lock_path);

    if !lock_path.exists() {
        return Ok(RunLockStatus {
            lock_path: lock_path.display().to_string(),
            locked: false,
            pid: None,
            stale: false,
        });
    }

    let mut file = OpenOptions::new()
        .create(true)
        .read(true)
        .write(true)
        .truncate(false)
        .open(&lock_path)
        .map_err(io_error("open run lock"))?;

    match file.try_lock_exclusive() {
        Ok(()) => {
            let (pid, stale) = match recorded_pid {
                Some(pid) if process_is_alive(pid) => (Some(pid), true),
                Some(_) => {
                    clear_lock_file(&mut file)?;
                    (None, false)
                }
                None => (None, false),
            };
            FileExt::unlock(&file).map_err(io_error("unlock run lock"))?;
            Ok(RunLockStatus {
                lock_path: lock_path.display().to_string(),
                locked: false,
                pid,
                stale,
            })
        }
        Err(error) if is_lock_contended(&error) => Ok(RunLockStatus {
            lock_path: lock_path.display().to_string(),
            locked: true,
            pid: recorded_pid,
            stale: false,
        }),
        Err(error) => Err(KaiError::new(
            ErrorCode::StateError,
            format!("failed to inspect run lock: {error}"),
        )),
    }
}

pub fn service_status(config: &LoadedConfig) -> KaiResult<ServiceStatus> {
    let lock = run_lock_status(config)?;
    let stdout_path = service_stdout_path(config).display().to_string();
    let stderr_path = service_stderr_path(config).display().to_string();

    #[cfg(target_os = "macos")]
    {
        let plist_path = launch_agent_plist_path()?;
        let installed = plist_path.exists();
        let launchd = launchd_status()?;
        let active_mode = if launchd.loaded && launchd.pid.is_some() {
            "service"
        } else if lock.locked {
            "manual"
        } else {
            "stopped"
        };

        Ok(ServiceStatus {
            platform: env::consts::OS.to_string(),
            label: MAC_LABEL.to_string(),
            installed,
            loaded: launchd.loaded,
            running: launchd.pid.is_some() || lock.locked,
            pid: launchd.pid.or(lock.pid.filter(|_| lock.locked)),
            active_mode: active_mode.to_string(),
            plist_path: Some(plist_path.display().to_string()),
            stdout_path,
            stderr_path,
            lock,
        })
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(ServiceStatus {
            platform: env::consts::OS.to_string(),
            label: MAC_LABEL.to_string(),
            installed: false,
            loaded: false,
            running: lock.locked,
            pid: lock.pid.filter(|_| lock.locked),
            active_mode: if lock.locked {
                "manual".to_string()
            } else {
                "stopped".to_string()
            },
            plist_path: None,
            stdout_path,
            stderr_path,
            lock,
        })
    }
}

pub fn service_logs(config: &LoadedConfig, tail: usize) -> KaiResult<ServiceLogsOutput> {
    let status = service_status(config)?;
    let stdout_path = service_stdout_path(config);
    let stderr_path = service_stderr_path(config);

    Ok(ServiceLogsOutput {
        status,
        stdout_path: stdout_path.display().to_string(),
        stderr_path: stderr_path.display().to_string(),
        stdout_tail: read_tail_lines(&stdout_path, tail)?,
        stderr_tail: read_tail_lines(&stderr_path, tail)?,
    })
}

pub fn service_start(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;
    if status.running {
        let detail = status
            .pid
            .map(|pid| format!("kai is already running (pid {pid})"))
            .unwrap_or_else(|| "kai is already running".to_string());
        return Err(KaiError::blocked_prerequisite(detail)
            .with_hint("run `kai service restart` or `kai service stop` before starting again"));
    }

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let token_status = telegram_token_status(config)?;
        if token_status.env_available {
            let _ = sync_telegram_token_to_keychain(config)?;
        } else if !token_status.keychain_available {
            return Err(KaiError::blocked_prerequisite(format!(
                "telegram bot token env `{}` is not set and no macOS Keychain secret is available",
                config.values.channel.telegram.bot_token_env
            ))
            .with_hint(
                "export the bot token env var once, then run `kai service start` to seed the secure background token store",
            ));
        }

        if config
            .values
            .media
            .transcription
            .provider
            .eq_ignore_ascii_case("groq")
        {
            let groq_status = groq_api_key_status(config)?;
            if groq_status.env_available {
                let _ = sync_groq_api_key_to_keychain(config)?;
            }
        }

        let binary_path = runtime_binary_path(config);
        if !binary_path.is_file() {
            return Err(KaiError::blocked_prerequisite(format!(
                "installed kai binary is missing at {}",
                binary_path.display()
            ))
            .with_hint(
                "run `bun run build` so `~/.tools/kai/kai` exists before starting the service",
            ));
        }

        let plist_path = launch_agent_plist_path()?;
        let launch_agents_dir = plist_path.parent().ok_or_else(|| {
            KaiError::new(
                ErrorCode::StateError,
                format!("invalid plist path: {}", plist_path.display()),
            )
        })?;
        let stdout_path = service_stdout_path(config);
        let stderr_path = service_stderr_path(config);
        let state_dir = state_paths(config).state_dir;
        let runner_path = service_runner_path(config);

        fs::create_dir_all(launch_agents_dir).map_err(io_error("create LaunchAgents directory"))?;
        ensure_private_dir(Path::new(&config.values.paths.root_app))?;
        ensure_private_dir(&state_dir)?;
        ensure_private_dir(
            stdout_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid stdout path"))?,
        )?;
        ensure_private_dir(
            stderr_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid stderr path"))?,
        )?;
        ensure_private_dir(
            runner_path
                .parent()
                .ok_or_else(|| KaiError::new(ErrorCode::StateError, "invalid runner path"))?,
        )?;
        ensure_private_file(&stdout_path)?;
        ensure_private_file(&stderr_path)?;

        let runner = render_service_runner(config, &binary_path);
        write_private_executable(&runner_path, runner.as_bytes())?;

        let plist = render_macos_plist(config, &runner_path);
        fs::write(&plist_path, plist).map_err(io_error("write launch agent plist"))?;
        harden_private_file(&plist_path)?;

        let _ = run_launchctl(["bootout", &launch_target_label()?]);
        let gui_target = format!("gui/{}", current_uid()?);
        let plist_path_string = plist_path.display().to_string();

        let bootstrap =
            run_launchctl(["bootstrap", gui_target.as_str(), plist_path_string.as_str()])?;
        if !bootstrap.status.success() {
            let stderr = String::from_utf8_lossy(&bootstrap.stderr);
            if !(stderr.contains("already loaded") || stderr.contains("already exists")) {
                return Err(command_error(
                    "launchctl bootstrap",
                    &stderr,
                    Some("check the generated LaunchAgent plist and retry"),
                ));
            }
        }

        wait_for_service_running(config)?;
        ensure_private_file(&stdout_path)?;
        ensure_private_file(&stderr_path)?;

        Ok(ServiceActionOutput {
            action: "started".to_string(),
            status: service_status(config)?,
        })
    }
}

pub fn service_stop(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if !status.loaded {
            if status.lock.locked {
                let detail = status
                    .lock
                    .pid
                    .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                    .unwrap_or_else(|| "kai is running in the foreground".to_string());
                return Err(KaiError::blocked_prerequisite(detail)
                    .with_hint("stop the foreground `kai run` process directly"));
            }

            return Ok(ServiceActionOutput {
                action: "already_stopped".to_string(),
                status,
            });
        }

        let target = launch_target_label()?;
        let output = run_launchctl(["bootout", target.as_str()])?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if !stderr.contains("No such process")
                && !stderr.contains("Could not find specified service")
            {
                return Err(command_error(
                    "launchctl bootout",
                    &stderr,
                    Some("inspect the LaunchAgent status and retry"),
                ));
            }
        }

        wait_for_service_stopped(config)?;

        Ok(ServiceActionOutput {
            action: "stopped".to_string(),
            status: service_status(config)?,
        })
    }
}

pub fn service_restart(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        let status = service_status(config)?;
        if status.lock.locked && !status.loaded {
            let detail = status
                .lock
                .pid
                .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                .unwrap_or_else(|| "kai is running in the foreground".to_string());
            return Err(KaiError::blocked_prerequisite(detail).with_hint(
                "stop the foreground `kai run` process before using `kai service restart`",
            ));
        }

        let _ = service_stop(config)?;
        service_start(config).map(|mut output| {
            output.action = "restarted".to_string();
            output
        })
    }
}

pub fn service_uninstall(config: &LoadedConfig) -> KaiResult<ServiceActionOutput> {
    let status = service_status(config)?;

    #[cfg(not(target_os = "macos"))]
    {
        let _ = config;
        return Err(KaiError::blocked_prerequisite(
            "background service management is only implemented for macOS right now",
        ));
    }

    #[cfg(target_os = "macos")]
    {
        if status.lock.locked && !status.loaded {
            let detail = status
                .lock
                .pid
                .map(|pid| format!("kai is running in the foreground (pid {pid})"))
                .unwrap_or_else(|| "kai is running in the foreground".to_string());
            return Err(KaiError::blocked_prerequisite(detail).with_hint(
                "stop the foreground `kai run` process before uninstalling the service",
            ));
        }

        let _ = service_stop(config)?;
        if let Some(plist_path) = service_status(config)?.plist_path {
            let path = PathBuf::from(plist_path);
            if path.exists() {
                fs::remove_file(&path).map_err(io_error("remove launch agent plist"))?;
            }
        }
        let runner_path = service_runner_path(config);
        if runner_path.exists() {
            fs::remove_file(&runner_path).map_err(io_error("remove service runner"))?;
        }

        Ok(ServiceActionOutput {
            action: "uninstalled".to_string(),
            status: service_status(config)?,
        })
    }
}

impl Drop for RunGuard {
    fn drop(&mut self) {
        let _ = self.file.set_len(0);
        let _ = self.file.seek(SeekFrom::Start(0));
        let _ = FileExt::unlock(&self.file);
    }
}

fn runtime_binary_path(config: &LoadedConfig) -> PathBuf {
    Path::new(&config.values.paths.root_app).join("kai")
}

fn run_lock_path(config: &LoadedConfig) -> PathBuf {
    state_paths(config).state_dir.join("run.lock")
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

fn is_lock_contended(error: &std::io::Error) -> bool {
    error.kind() == std::io::ErrorKind::WouldBlock
}

fn read_lock_pid(path: &Path) -> Option<u32> {
    let raw = fs::read_to_string(path).ok()?;
    raw.trim().parse::<u32>().ok()
}

fn write_lock_pid(file: &mut File, pid: u32) -> KaiResult<()> {
    clear_lock_file(file)?;
    file.write_all(pid.to_string().as_bytes())
        .map_err(io_error("write run lock"))?;
    file.write_all(b"\n").map_err(io_error("write run lock"))?;
    file.sync_data().map_err(io_error("sync run lock"))?;
    Ok(())
}

fn clear_lock_file(file: &mut File) -> KaiResult<()> {
    file.set_len(0).map_err(io_error("clear run lock"))?;
    file.seek(SeekFrom::Start(0))
        .map_err(io_error("seek run lock"))?;
    file.sync_data().map_err(io_error("sync run lock"))?;
    Ok(())
}

fn process_is_alive(pid: u32) -> bool {
    Command::new("kill")
        .arg("-0")
        .arg(pid.to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn read_tail_lines(path: &Path, tail: usize) -> KaiResult<Vec<String>> {
    if tail == 0 || !path.exists() {
        return Ok(Vec::new());
    }

    let raw = fs::read_to_string(path).map_err(io_error("read service log"))?;
    let mut lines = raw.lines().map(ToOwned::to_owned).collect::<Vec<_>>();
    if lines.len() > tail {
        lines.drain(0..(lines.len() - tail));
    }
    Ok(lines)
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

fn wait_for_service_running(config: &LoadedConfig) -> KaiResult<()> {
    wait_for_service_condition(config, |status| status.loaded && status.running).map(|_| ())
}

fn wait_for_service_stopped(config: &LoadedConfig) -> KaiResult<()> {
    wait_for_service_condition(config, |status| !status.loaded && !status.running).map(|_| ())
}

fn wait_for_service_condition(
    config: &LoadedConfig,
    predicate: impl Fn(&ServiceStatus) -> bool,
) -> KaiResult<ServiceStatus> {
    let started_at = Instant::now();
    loop {
        let status = service_status(config)?;
        if predicate(&status) {
            return Ok(status);
        }

        if started_at.elapsed() >= SERVICE_SETTLE_TIMEOUT {
            return Err(KaiError::new(
                ErrorCode::RuntimeError,
                "service state did not settle before timeout",
            )
            .with_hint("run `kai service status` and inspect the service logs"));
        }

        thread::sleep(SERVICE_SETTLE_POLL);
    }
}

#[cfg(target_os = "macos")]
#[derive(Debug, Clone, Copy)]
struct LaunchdStatus {
    loaded: bool,
    pid: Option<u32>,
}

#[cfg(target_os = "macos")]
fn current_uid() -> KaiResult<String> {
    if let Ok(uid) = env::var("UID")
        && !uid.trim().is_empty()
    {
        return Ok(uid);
    }

    let output = Command::new("id")
        .arg("-u")
        .output()
        .map_err(io_error("run `id -u`"))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(command_error("id -u", &stderr, None));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(target_os = "macos")]
fn launch_agent_plist_path() -> KaiResult<PathBuf> {
    let home = env::var("HOME").map_err(|_| {
        KaiError::blocked_prerequisite("HOME is not set")
            .with_hint("run the command from a normal logged-in shell")
    })?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MAC_LABEL}.plist")))
}

#[cfg(target_os = "macos")]
fn launch_target_label() -> KaiResult<String> {
    Ok(format!("gui/{}/{}", current_uid()?, MAC_LABEL))
}

#[cfg(target_os = "macos")]
fn render_macos_plist(config: &LoadedConfig, runner_path: &Path) -> String {
    let mut lines = vec![
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>".to_string(),
        "<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">".to_string(),
        "<plist version=\"1.0\">".to_string(),
        "<dict>".to_string(),
        "  <key>Label</key>".to_string(),
        format!("  <string>{MAC_LABEL}</string>"),
        "  <key>ProgramArguments</key>".to_string(),
        "  <array>".to_string(),
        format!(
            "    <string>{}</string>",
            xml_escape(&runner_path.display().to_string())
        ),
        "  </array>".to_string(),
        "  <key>WorkingDirectory</key>".to_string(),
        format!(
            "  <string>{}</string>",
            xml_escape(&config.values.paths.root_app)
        ),
        "  <key>RunAtLoad</key>".to_string(),
        "  <true/>".to_string(),
        "  <key>KeepAlive</key>".to_string(),
        "  <true/>".to_string(),
        "  <key>StandardOutPath</key>".to_string(),
        format!(
            "  <string>{}</string>",
            xml_escape(&service_stdout_path(config).display().to_string())
        ),
        "  <key>StandardErrorPath</key>".to_string(),
        format!(
            "  <string>{}</string>",
            xml_escape(&service_stderr_path(config).display().to_string())
        ),
    ];

    lines.extend(["</dict>".to_string(), "</plist>".to_string()]);

    lines.join("\n")
}

#[cfg(target_os = "macos")]
fn render_service_runner(config: &LoadedConfig, binary_path: &Path) -> String {
    let path = env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".to_string());
    let telegram_env_key = &config.values.channel.telegram.bot_token_env;
    let telegram_service_name = telegram_token_keychain_service_name();
    let mut lines = vec![
        "#!/bin/zsh".to_string(),
        "set -euo pipefail".to_string(),
        "umask 077".to_string(),
        format!(
            "export HOME={}",
            shell_quote(&env::var("HOME").unwrap_or_default())
        ),
        format!(
            "export KAI_HOME={}",
            shell_quote(&config.values.paths.root_app)
        ),
        format!("export PATH={}", shell_quote(&path)),
        format!(
            "export {}=\"$('/usr/bin/security' find-generic-password -w -a \"$('/usr/bin/id' -un)\" -s {})\"",
            telegram_env_key,
            shell_quote(telegram_service_name)
        ),
    ];

    if config
        .values
        .media
        .transcription
        .provider
        .eq_ignore_ascii_case("groq")
    {
        let groq_env_key = &config.values.media.transcription.groq_api_key_env;
        let groq_service_name = groq_api_key_keychain_service_name();
        lines.push(format!(
            "if KAI_GROQ_KEY=\"$('/usr/bin/security' find-generic-password -w -a \"$('/usr/bin/id' -un)\" -s {} 2>/dev/null)\"; then export {}=\"$KAI_GROQ_KEY\"; fi",
            shell_quote(groq_service_name),
            groq_env_key
        ));
    }

    lines.extend([
        format!(
            "exec {} run",
            shell_quote(&binary_path.display().to_string())
        ),
        "".to_string(),
    ]);

    lines.join("\n")
}

#[cfg(target_os = "macos")]
fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "macos")]
fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\\''"))
}

#[cfg(target_os = "macos")]
fn launchd_status() -> KaiResult<LaunchdStatus> {
    let target = launch_target_label()?;
    let output = run_launchctl(["print", target.as_str()])?;
    if !output.status.success() {
        return Ok(LaunchdStatus {
            loaded: false,
            pid: None,
        });
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    Ok(LaunchdStatus {
        loaded: true,
        pid: parse_launchd_pid(&stdout),
    })
}

#[cfg(target_os = "macos")]
fn parse_launchd_pid(stdout: &str) -> Option<u32> {
    stdout.lines().find_map(|line| {
        let trimmed = line.trim();
        let value = trimmed
            .strip_prefix("pid = ")
            .or_else(|| trimmed.strip_prefix("\"pid\" = "))?;
        value
            .split_whitespace()
            .next()
            .and_then(|pid| pid.trim_matches('"').parse::<u32>().ok())
    })
}

#[cfg(target_os = "macos")]
fn run_launchctl<const N: usize>(args: [&str; N]) -> KaiResult<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(io_error("run launchctl"))
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, LoadedConfig,
        MediaConfig, PathsConfig, RunnerConfig, TelegramConfig, TranscriptionConfig,
    };

    fn test_config(root_app: &Path, root_work: &Path) -> LoadedConfig {
        LoadedConfig {
            config_path: root_app.join("config.toml"),
            config_exists: true,
            values: Config {
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
            },
        }
    }

    #[test]
    fn run_lock_status_reports_stale_after_release() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);

        {
            let _guard = acquire_run_guard(&config).expect("acquire run guard");
            let status = run_lock_status(&config).expect("run lock status while running");
            assert!(status.locked);
            assert!(status.pid.is_some());
            assert!(!status.stale);
        }

        let status = run_lock_status(&config).expect("run lock status after release");
        assert!(!status.locked);
        assert!(status.pid.is_none());
        assert!(!status.stale);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn render_macos_plist_contains_required_fields() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);
        let runner = root_app.join("bin").join("service-run.sh");

        let plist = render_macos_plist(&config, &runner);

        assert!(plist.contains("<key>Label</key>"));
        assert!(plist.contains(MAC_LABEL));
        assert!(plist.contains("service-run.sh"));
        assert!(plist.contains("service.stdout.log"));
        assert!(plist.contains("service.stderr.log"));
        assert!(!plist.contains("KAI_TELEGRAM_BOT_TOKEN"));
        assert!(!plist.contains("secret-token"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn render_service_runner_uses_keychain_lookup() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);
        let binary = root_app.join("kai");

        let runner = render_service_runner(&config, &binary);

        assert!(runner.contains("find-generic-password"));
        assert!(runner.contains("KAI_TELEGRAM_BOT_TOKEN"));
        assert!(runner.contains("ai.kai.telegram.bot-token"));
        assert!(runner.contains("exec"));
    }
}
