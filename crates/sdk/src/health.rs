use std::env;
use std::path::Path;

use crate::config::LoadedConfig;
use crate::context::context_report;
use crate::contract::{HealthCheck, HealthReport};
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::media::transcription_provider_status;
use crate::runtime_fs::{octal_mode, read_unix_mode};
use crate::secrets::{groq_api_key_status, telegram_token_status};
use crate::state::{StateStore, state_paths};

pub fn health_report(config: &LoadedConfig) -> KaiResult<HealthReport> {
    let mut checks = Vec::new();

    let codex_binary = &config.values.runner.codex.binary;
    let codex_ok = find_binary(codex_binary).is_some();
    checks.push(HealthCheck {
        name: "codex.binary".to_string(),
        ok: codex_ok,
        detail: if codex_ok {
            format!("found `{codex_binary}`")
        } else {
            format!("could not find `{codex_binary}`")
        },
        fix: if codex_ok {
            None
        } else {
            Some("install Codex CLI or point `runner.codex.binary` at an executable".to_string())
        },
    });

    let token_status = telegram_token_status(config)?;
    let token_ok = token_status.env_available || token_status.keychain_available;
    let token_sources = match (token_status.env_available, token_status.keychain_available) {
        (true, true) => format!(
            "env `{}` is set and macOS Keychain service `{}` is available",
            token_status.env_key,
            token_status
                .keychain_service
                .as_deref()
                .unwrap_or("unknown")
        ),
        (true, false) => format!("env `{}` is set", token_status.env_key),
        (false, true) => format!(
            "macOS Keychain service `{}` is available",
            token_status
                .keychain_service
                .as_deref()
                .unwrap_or("unknown")
        ),
        (false, false) => format!("env `{}` is missing", token_status.env_key),
    };
    checks.push(HealthCheck {
        name: "telegram.token".to_string(),
        ok: token_ok,
        detail: token_sources,
        fix: if token_ok {
            None
        } else {
            Some(
                "export the Telegram bot token env var, then run `kai service restart` to sync the background secret store".to_string(),
            )
        },
    });

    let transcription_status = transcription_provider_status(config)?;
    checks.push(HealthCheck {
        name: "media.transcription.provider".to_string(),
        ok: !matches!(
            transcription_status,
            crate::media::TranscriptionProviderStatus::Misconfigured { .. }
        ),
        detail: match &transcription_status {
            crate::media::TranscriptionProviderStatus::Disabled => {
                "transcription is disabled".to_string()
            }
            crate::media::TranscriptionProviderStatus::Ready { provider } => {
                format!("transcription provider `{provider}` is ready")
            }
            crate::media::TranscriptionProviderStatus::Misconfigured { provider, detail } => {
                format!("transcription provider `{provider}` is misconfigured: {detail}")
            }
        },
        fix: match &transcription_status {
            crate::media::TranscriptionProviderStatus::Disabled
            | crate::media::TranscriptionProviderStatus::Ready { .. } => None,
            crate::media::TranscriptionProviderStatus::Misconfigured { provider, .. }
                if provider == "groq" =>
            {
                Some(
                    "export the Groq API key once, then run `kai service restart` to sync the background secret store"
                        .to_string(),
                )
            }
            crate::media::TranscriptionProviderStatus::Misconfigured { provider, .. }
                if provider == "command" =>
            {
                Some(
                    "set `media.transcription.command` or switch providers in config".to_string(),
                )
            }
            crate::media::TranscriptionProviderStatus::Misconfigured { .. } => {
                Some("set a supported `media.transcription.provider` value".to_string())
            }
        },
    });

    if config
        .values
        .media
        .transcription
        .provider
        .eq_ignore_ascii_case("groq")
    {
        let groq_status = groq_api_key_status(config)?;
        let groq_ok = groq_status.env_available || groq_status.keychain_available;
        let detail = match (groq_status.env_available, groq_status.keychain_available) {
            (true, true) => format!(
                "env `{}` is set and macOS Keychain service `{}` is available",
                groq_status.env_key,
                groq_status.keychain_service.as_deref().unwrap_or("unknown")
            ),
            (true, false) => format!("env `{}` is set", groq_status.env_key),
            (false, true) => format!(
                "macOS Keychain service `{}` is available",
                groq_status.keychain_service.as_deref().unwrap_or("unknown")
            ),
            (false, false) => format!("env `{}` is missing", groq_status.env_key),
        };
        checks.push(HealthCheck {
            name: "media.transcription.secret".to_string(),
            ok: groq_ok,
            detail,
            fix: if groq_ok {
                None
            } else {
                Some(
                    "export the Groq API key once, then run `kai service restart` to seed the secure background secret store"
                        .to_string(),
                )
            },
        });
    }

    let ffmpeg_ok = find_binary("ffmpeg").is_some();
    checks.push(HealthCheck {
        name: "media.ffmpeg".to_string(),
        ok: ffmpeg_ok,
        detail: if ffmpeg_ok {
            "found `ffmpeg` for video and animation preprocessing".to_string()
        } else {
            "`ffmpeg` is missing; video/animation understanding will be reduced".to_string()
        },
        fix: if ffmpeg_ok {
            None
        } else {
            Some(
                "install `ffmpeg` if you want preview frames and broader media preprocessing"
                    .to_string(),
            )
        },
    });

    let state_store = StateStore::open(config)?;
    checks.push(HealthCheck {
        name: "state.store".to_string(),
        ok: true,
        detail: format!("sqlite ready at {}", state_store.paths().db_path.display()),
        fix: None,
    });

    let owner_paired = config.values.channel.telegram.owner_user_id.is_some()
        || state_store.get_owner_user_id()?.is_some();
    checks.push(HealthCheck {
        name: "telegram.owner".to_string(),
        ok: owner_paired,
        detail: if owner_paired {
            if config.values.channel.telegram.owner_user_id.is_some() {
                "owner identity is pinned in config".to_string()
            } else {
                "owner identity is paired in runtime state".to_string()
            }
        } else {
            "owner identity is not paired yet".to_string()
        },
        fix: if owner_paired {
            None
        } else {
            Some(
                "set `channel.telegram.owner_user_id`, or open a recovery pairing window locally"
                    .to_string(),
            )
        },
    });

    let recovery = state_store.get_pending_pairing()?;
    checks.push(HealthCheck {
        name: "telegram.recovery".to_string(),
        ok: recovery.is_none(),
        detail: match recovery {
            Some(ref pairing) => format!(
                "recovery pairing is open until {} with {} attempt(s) remaining",
                pairing.expires_at, pairing.remaining_attempts
            ),
            None => "recovery pairing is closed".to_string(),
        },
        fix: if recovery.is_some() {
            Some(
                "let the recovery window expire, complete pairing, or rerun `kai setup telegram` without `--recovery` to close it"
                    .to_string(),
            )
        } else {
            None
        },
    });

    let states = state_paths(config);
    let mut dir_issues = Vec::new();
    for path in [
        Path::new(&config.values.paths.root_app).to_path_buf(),
        states.state_dir.clone(),
        states.logs_dir.clone(),
        states.attachments_dir.clone(),
    ] {
        push_permission_issue(&path, 0o700, &mut dir_issues)?;
    }
    checks.push(HealthCheck {
        name: "security.dir_permissions".to_string(),
        ok: dir_issues.is_empty(),
        detail: if dir_issues.is_empty() {
            "runtime directories are private (0700)".to_string()
        } else {
            dir_issues.join("; ")
        },
        fix: if dir_issues.is_empty() {
            None
        } else {
            Some(
                "restart `kai` after this hardening pass, or manually chmod runtime directories to 700"
                    .to_string(),
            )
        },
    });

    let mut file_issues = Vec::new();
    for path in [
        config.config_path.clone(),
        states.db_path.clone(),
        states.audit_path.clone(),
        Path::new(&config.values.paths.root_app)
            .join("bin")
            .join("service-run.sh"),
    ] {
        let expected = if path.ends_with("service-run.sh") {
            0o700
        } else {
            0o600
        };
        push_permission_issue(&path, expected, &mut file_issues)?;
    }

    let plist_path = Path::new(&env::var("HOME").unwrap_or_default())
        .join("Library")
        .join("LaunchAgents")
        .join("ai.kai.plist");
    push_permission_issue(&plist_path, 0o600, &mut file_issues)?;
    checks.push(HealthCheck {
        name: "security.file_permissions".to_string(),
        ok: file_issues.is_empty(),
        detail: if file_issues.is_empty() {
            "runtime files are private (0600/0700)".to_string()
        } else {
            file_issues.join("; ")
        },
        fix: if file_issues.is_empty() {
            None
        } else {
            Some(
                "restart `kai service` after this hardening pass, or manually chmod runtime files to 600"
                    .to_string(),
            )
        },
    });

    let plist_secret_issue = detect_insecure_plist_secret(config)?;
    let plist_secret_open = plist_secret_issue.is_some();
    checks.push(HealthCheck {
        name: "security.plist_secret".to_string(),
        ok: !plist_secret_open,
        detail: plist_secret_issue.unwrap_or_else(|| {
            "LaunchAgent does not embed the Telegram bot token directly".to_string()
        }),
        fix: if plist_secret_open {
            Some(
                "run `kai service restart` to rewrite the LaunchAgent without embedding the token"
                    .to_string(),
            )
        } else {
            None
        },
    });

    for entry in context_report(config).entries {
        checks.push(HealthCheck {
            name: format!("context.{}", entry.role),
            ok: entry.exists && entry.readable,
            detail: if entry.exists && entry.readable {
                format!("loaded {}", entry.path)
            } else {
                format!("optional file missing at {}", entry.path)
            },
            fix: if entry.exists && entry.readable {
                None
            } else {
                Some(
                    "run `kai setup` to create placeholder context files or point them elsewhere"
                        .to_string(),
                )
            },
        });
    }

    let status = if checks
        .iter()
        .any(|check| !check.ok && matches!(check.name.as_str(), "codex.binary" | "telegram.token"))
    {
        "blocked"
    } else if checks.iter().any(|check| !check.ok) {
        "degraded"
    } else {
        "ready"
    };

    Ok(HealthReport {
        status: status.to_string(),
        checks,
    })
}

fn find_binary(binary: &str) -> Option<String> {
    let path = Path::new(binary);
    if path.is_absolute() && path.is_file() {
        return Some(path.display().to_string());
    }

    let path_value = env::var_os("PATH")?;
    for entry in env::split_paths(&path_value) {
        let candidate = entry.join(binary);
        if candidate.is_file() {
            return Some(candidate.display().to_string());
        }
    }

    None
}

fn push_permission_issue(path: &Path, expected: u32, issues: &mut Vec<String>) -> KaiResult<()> {
    let Some(mode) = read_unix_mode(path)? else {
        return Ok(());
    };

    if mode != expected {
        issues.push(format!(
            "{} is {} (expected {})",
            path.display(),
            octal_mode(mode),
            octal_mode(expected)
        ));
    }

    Ok(())
}

fn detect_insecure_plist_secret(config: &LoadedConfig) -> KaiResult<Option<String>> {
    let plist_path = Path::new(&env::var("HOME").unwrap_or_default())
        .join("Library")
        .join("LaunchAgents")
        .join("ai.kai.plist");
    if !plist_path.is_file() {
        return Ok(None);
    }

    let raw = std::fs::read_to_string(&plist_path).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to read LaunchAgent plist: {error}"),
        )
    })?;
    if raw.contains(&format!(
        "<key>{}</key>",
        config.values.channel.telegram.bot_token_env
    )) {
        return Ok(Some(format!(
            "{} still embeds `{}` in EnvironmentVariables",
            plist_path.display(),
            config.values.channel.telegram.bot_token_env
        )));
    }

    Ok(None)
}
