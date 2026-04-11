use std::env;
use std::path::Path;

use crate::config::LoadedConfig;
use crate::context::context_report;
use crate::contract::{HealthCheck, HealthReport};
use crate::error::KaiResult;
use crate::state::StateStore;

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

    let token_env = &config.values.channel.telegram.bot_token_env;
    let token_ok = env::var(token_env).is_ok();
    checks.push(HealthCheck {
        name: "telegram.token".to_string(),
        ok: token_ok,
        detail: if token_ok {
            format!("env `{token_env}` is set")
        } else {
            format!("env `{token_env}` is missing")
        },
        fix: if token_ok {
            None
        } else {
            Some("export the Telegram bot token env var before running `kai run`".to_string())
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
            "owner identity is configured".to_string()
        } else {
            "owner identity is not paired yet".to_string()
        },
        fix: if owner_paired {
            None
        } else {
            Some("run `kai setup telegram` locally, then pair the bot from Telegram".to_string())
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
