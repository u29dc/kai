use super::*;

pub(super) fn seed_secrets(config: &LoadedConfig) -> KaiResult<()> {
    let token_status = telegram_token_status(config)?;
    if token_status.env_available {
        sync_telegram_token_to_keychain(config)?;
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
            sync_groq_api_key_to_keychain(config)?;
        }
    }

    Ok(())
}

#[derive(Debug, Clone, Copy)]
pub(super) struct LaunchdStatus {
    pub loaded: bool,
    pub pid: Option<u32>,
}

pub(super) fn current_uid() -> KaiResult<String> {
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

pub(super) fn launch_agent_plist_path() -> KaiResult<PathBuf> {
    let home = env::var("HOME").map_err(|_| {
        KaiError::blocked_prerequisite("HOME is not set")
            .with_hint("run the command from a normal logged-in shell")
    })?;
    Ok(PathBuf::from(home)
        .join("Library")
        .join("LaunchAgents")
        .join(format!("{MAC_LABEL}.plist")))
}

pub(super) fn launch_target_label() -> KaiResult<String> {
    Ok(format!("gui/{}/{}", current_uid()?, MAC_LABEL))
}

pub(super) fn render_macos_plist(config: &LoadedConfig, runner_path: &Path) -> String {
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

pub(super) fn render_service_runner(config: &LoadedConfig, binary_path: &Path) -> String {
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

fn xml_escape(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\\''"))
}

pub(super) fn launchd_status() -> KaiResult<LaunchdStatus> {
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

pub(super) fn run_launchctl<const N: usize>(args: [&str; N]) -> KaiResult<std::process::Output> {
    Command::new("launchctl")
        .args(args)
        .output()
        .map_err(io_error("run launchctl"))
}
