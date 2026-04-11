use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Parser, Subcommand};
use kai_sdk::{
    ConfigGetOutput, ConfigShowOutput, KaiError, KaiResult, SessionView, SetupCodexOutput,
    SetupOutput, StateStore, acquire_run_guard, config_value_at_key, context_report,
    ensure_config_file, ensure_private_dir, error_envelope, health_report, load_config,
    mobile_help_text, ok_envelope, resolve_telegram_token, run_telegram_loop, service_logs,
    service_restart, service_start, service_status, service_stop, service_uninstall,
    set_config_value, tool_catalog, tool_spec, unset_config_value, write_private_file,
};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

const PAIRING_TTL_MINUTES: i64 = 10;
const PAIRING_MAX_ATTEMPTS: u8 = 5;

#[derive(Debug, Parser)]
#[command(name = "kai")]
#[command(about = "JSON-first local Telegram-to-Codex portal")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Tools {
        name: Option<String>,
    },
    Health,
    Config {
        #[command(subcommand)]
        command: ConfigCommand,
    },
    Setup {
        #[command(subcommand)]
        command: Option<SetupCommand>,
    },
    Context {
        #[command(subcommand)]
        command: ContextCommand,
    },
    Session {
        #[command(subcommand)]
        command: SessionCommand,
    },
    Service {
        #[command(subcommand)]
        command: ServiceCommand,
    },
    Run,
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Show,
    Get { key: String },
    Set { key: String, value: String },
    Unset { key: String },
}

#[derive(Debug, Subcommand)]
enum SetupCommand {
    Telegram {
        #[arg(long)]
        recovery: bool,
    },
    Codex,
}

#[derive(Debug, Subcommand)]
enum ContextCommand {
    Show,
    Check,
}

#[derive(Debug, Subcommand)]
enum SessionCommand {
    Show,
    Set { session_id: String },
    New,
    Reset,
}

#[derive(Debug, Subcommand)]
enum ServiceCommand {
    Status,
    Logs {
        #[arg(long, default_value_t = 50)]
        tail: usize,
    },
    Start,
    Stop,
    Restart,
    Uninstall,
}

#[tokio::main]
async fn main() -> ExitCode {
    let cli = Cli::parse();

    match dispatch(cli).await {
        Ok(Flow::Immediate { tool, payload }) => {
            if let Err(error) = write_json(&ok_envelope(tool, payload)) {
                eprintln!("{error}");
                return ExitCode::from(1);
            }
            ExitCode::SUCCESS
        }
        Ok(Flow::Streaming {
            tool,
            payload,
            future,
        }) => {
            if let Err(error) = write_json(&ok_envelope(tool, payload)) {
                eprintln!("{error}");
                return ExitCode::from(1);
            }

            match future.await {
                Ok(()) => ExitCode::SUCCESS,
                Err(error) => {
                    eprintln!("{error:?}");
                    if error.blocked {
                        ExitCode::from(2)
                    } else {
                        ExitCode::from(1)
                    }
                }
            }
        }
        Err(error) => {
            if let Err(write_error) = write_json(&error_envelope("kai", error.clone())) {
                eprintln!("{write_error}");
            }
            if error.blocked {
                ExitCode::from(2)
            } else {
                ExitCode::from(1)
            }
        }
    }
}

enum Flow {
    Immediate {
        tool: String,
        payload: JsonValue,
    },
    Streaming {
        tool: String,
        payload: JsonValue,
        future: std::pin::Pin<Box<dyn std::future::Future<Output = KaiResult<()>>>>,
    },
}

async fn dispatch(cli: Cli) -> KaiResult<Flow> {
    match cli.command {
        Command::Tools { name } => {
            let payload = match name {
                Some(name) => serde_json::to_value(
                    tool_spec(&name).ok_or_else(|| KaiError::tool_not_found(&name))?,
                )
                .map_err(serialize_error("serialize tool detail"))?,
                None => serde_json::to_value(tool_catalog())
                    .map_err(serialize_error("serialize tool catalog"))?,
            };

            Ok(Flow::Immediate {
                tool: "kai.tools".to_string(),
                payload,
            })
        }
        Command::Health => {
            let config = load_config()?;
            let payload = serde_json::to_value(health_report(&config)?)
                .map_err(serialize_error("serialize health report"))?;

            Ok(Flow::Immediate {
                tool: "kai.health".to_string(),
                payload,
            })
        }
        Command::Config { command } => handle_config_command(command),
        Command::Setup { command } => handle_setup_command(command).await,
        Command::Context { command } => {
            let config = load_config()?;
            let payload = serde_json::to_value(context_report(&config))
                .map_err(serialize_error("serialize context report"))?;

            let tool = match command {
                ContextCommand::Show => "kai.context.show",
                ContextCommand::Check => "kai.context.check",
            };

            Ok(Flow::Immediate {
                tool: tool.to_string(),
                payload,
            })
        }
        Command::Session { command } => handle_session_command(command),
        Command::Service { command } => handle_service_command(command),
        Command::Run => {
            let config = load_config()?;
            if !config.values.channel.telegram.enabled {
                return Err(KaiError::blocked_prerequisite(
                    "telegram is disabled in config",
                ));
            }
            let _ = resolve_telegram_token(&config)?;
            let run_guard = acquire_run_guard(&config)?;
            let state = StateStore::open(&config)?;
            let payload = json!({
                "status": "starting",
                "help": mobile_help_text(),
                "rootApp": config.values.paths.root_app,
                "mode": "foreground",
            });

            Ok(Flow::Streaming {
                tool: "kai.run".to_string(),
                payload,
                future: Box::pin(async move {
                    let _guard = run_guard;
                    run_telegram_loop(&config, &state).await
                }),
            })
        }
    }
}

fn handle_config_command(command: ConfigCommand) -> KaiResult<Flow> {
    match command {
        ConfigCommand::Show => {
            let config = load_config()?;
            let payload = serde_json::to_value(ConfigShowOutput {
                config_path: config.config_path.display().to_string(),
                config_exists: config.config_exists,
                values: serde_json::to_value(&config.values)
                    .map_err(serialize_error("serialize config values"))?,
            })
            .map_err(serialize_error("serialize config show output"))?;

            Ok(Flow::Immediate {
                tool: "kai.config.show".to_string(),
                payload,
            })
        }
        ConfigCommand::Get { key } => {
            let config = load_config()?;
            let payload = serde_json::to_value(ConfigGetOutput {
                key: key.clone(),
                value: config_value_at_key(&config, &key)?,
            })
            .map_err(serialize_error("serialize config get output"))?;

            Ok(Flow::Immediate {
                tool: "kai.config.get".to_string(),
                payload,
            })
        }
        ConfigCommand::Set { key, value } => {
            let config_path = set_config_value(&key, &value)?;
            let config = load_config()?;
            let payload = json!({
                "configPath": config_path.display().to_string(),
                "key": key,
                "value": config_value_at_key(&config, &key)?,
            });

            Ok(Flow::Immediate {
                tool: "kai.config.set".to_string(),
                payload,
            })
        }
        ConfigCommand::Unset { key } => {
            let config_path = unset_config_value(&key)?;
            let payload = json!({
                "configPath": config_path.display().to_string(),
                "key": key,
            });

            Ok(Flow::Immediate {
                tool: "kai.config.unset".to_string(),
                payload,
            })
        }
    }
}

async fn handle_setup_command(command: Option<SetupCommand>) -> KaiResult<Flow> {
    match command {
        None => {
            let config = load_config()?;
            ensure_config_file(&config.config_path)?;
            let config = load_config()?;
            let state = StateStore::open(&config)?;

            let created_paths = create_runtime_dirs(&config, &state)?;
            create_context_placeholders(&config)?;

            let payload = serde_json::to_value(SetupOutput {
                config_path: config.config_path.display().to_string(),
                root_app: config.values.paths.root_app.clone(),
                root_work: config.values.paths.root_work.clone(),
                created_paths,
            })
            .map_err(serialize_error("serialize setup output"))?;

            Ok(Flow::Immediate {
                tool: "kai.setup".to_string(),
                payload,
            })
        }
        Some(SetupCommand::Telegram { recovery }) => {
            let config = load_config()?;
            ensure_config_file(&config.config_path)?;
            let config = load_config()?;
            let state = StateStore::open(&config)?;
            let created_paths = create_runtime_dirs(&config, &state)?;
            create_context_placeholders(&config)?;

            if config.values.channel.telegram.owner_user_id.is_some() && !recovery {
                state.clear_pending_pairing()?;
                return Err(KaiError::blocked_prerequisite(
                    "owner_user_id is already pinned in config; pairing is disabled by default",
                )
                .with_hint(
                    "use `kai setup telegram --recovery` only when you explicitly need to re-pair",
                ));
            }

            let pair_code = Uuid::new_v4().simple().to_string().to_uppercase();
            state.set_pending_pairing(&kai_sdk::state::PendingPairing::issue(
                &pair_code,
                PAIRING_TTL_MINUTES,
                PAIRING_MAX_ATTEMPTS,
            ))?;

            let payload = json!({
                "configPath": config.config_path.display().to_string(),
                "pairCode": pair_code,
                "botTokenEnv": config.values.channel.telegram.bot_token_env,
                "createdPaths": created_paths,
                "recovery": recovery,
                "expiresInMinutes": PAIRING_TTL_MINUTES,
                "remainingAttempts": PAIRING_MAX_ATTEMPTS,
                "instruction": "Send `/pair <code>` to the bot from Telegram after starting `kai run`."
            });

            Ok(Flow::Immediate {
                tool: "kai.setup.telegram".to_string(),
                payload,
            })
        }
        Some(SetupCommand::Codex) => {
            let config = load_config()?;
            let binary = config.values.runner.codex.binary.clone();
            let exec_available = ProcessCommand::new(&binary)
                .arg("exec")
                .arg("--help")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);
            let resume_available = ProcessCommand::new(&binary)
                .arg("exec")
                .arg("resume")
                .arg("--help")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);

            let payload = serde_json::to_value(SetupCodexOutput {
                binary,
                exec_available,
                resume_available,
            })
            .map_err(serialize_error("serialize setup codex output"))?;

            Ok(Flow::Immediate {
                tool: "kai.setup.codex".to_string(),
                payload,
            })
        }
    }
}

fn handle_session_command(command: SessionCommand) -> KaiResult<Flow> {
    let config = load_config()?;
    let state = StateStore::open(&config)?;
    let tool = match &command {
        SessionCommand::Show => "kai.session.show",
        SessionCommand::Set { .. } => "kai.session.set",
        SessionCommand::New => "kai.session.new",
        SessionCommand::Reset => "kai.session.reset",
    };

    match command {
        SessionCommand::Show => {}
        SessionCommand::Set { session_id } => {
            if session_id.trim().is_empty() {
                return Err(KaiError::invalid_argument("session id cannot be empty"));
            }
            state.set_active_session_id(session_id.trim())?;
            state.clear_replay_package()?;
        }
        SessionCommand::New | SessionCommand::Reset => {
            state.clear_active_session_id()?;
            state.clear_replay_package()?;
        }
    }

    let payload = serde_json::to_value(session_view_with_override(&config, &state)?)
        .map_err(serialize_error("serialize session view"))?;

    Ok(Flow::Immediate {
        tool: tool.to_string(),
        payload,
    })
}

fn session_view_with_override(
    config: &kai_sdk::LoadedConfig,
    state: &StateStore,
) -> KaiResult<SessionView> {
    let mut session = state.session_view()?;
    if session.owner_user_id.is_none() {
        session.owner_user_id = config.values.channel.telegram.owner_user_id;
    }
    Ok(session)
}

fn create_runtime_dirs(
    config: &kai_sdk::LoadedConfig,
    state: &StateStore,
) -> KaiResult<Vec<String>> {
    let mut created = Vec::new();

    for path in [
        Path::new(&config.values.paths.root_app).to_path_buf(),
        Path::new(&config.values.paths.root_work).to_path_buf(),
        state.paths().attachments_dir.clone(),
        state.paths().logs_dir.clone(),
        state.paths().state_dir.clone(),
    ] {
        ensure_private_dir(&path)?;
        created.push(path.display().to_string());
    }

    created.sort();
    created.dedup();
    Ok(created)
}

fn create_context_placeholders(config: &kai_sdk::LoadedConfig) -> KaiResult<()> {
    for (path, title) in [
        (config.values.context_files.soul.as_str(), "SOUL"),
        (config.values.context_files.memory.as_str(), "MEMORY"),
        (config.values.context_files.todo.as_str(), "TODO"),
    ] {
        let file_path = Path::new(path);
        if file_path.is_file() {
            continue;
        }

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(io_error("create context directory"))?;
        }

        write_private_file(file_path, format!("# {title}\n\n").as_bytes())?;
    }

    Ok(())
}

fn write_json<T>(value: &T) -> io::Result<()>
where
    T: Serialize,
{
    let stdout = io::stdout();
    let mut handle = stdout.lock();
    serde_json::to_writer(&mut handle, value)?;
    handle.write_all(b"\n")?;
    handle.flush()?;
    Ok(())
}

fn serialize_error(action: &'static str) -> impl Fn(serde_json::Error) -> KaiError {
    move |error| {
        KaiError::new(
            kai_sdk::ErrorCode::RuntimeError,
            format!("failed to {action}: {error}"),
        )
    }
}

fn handle_service_command(command: ServiceCommand) -> KaiResult<Flow> {
    let config = load_config()?;
    let (tool, payload) = match command {
        ServiceCommand::Status => (
            "kai.service.status",
            serde_json::to_value(service_status(&config)?)
                .map_err(serialize_error("serialize service status"))?,
        ),
        ServiceCommand::Logs { tail } => (
            "kai.service.logs",
            serde_json::to_value(service_logs(&config, tail)?)
                .map_err(serialize_error("serialize service logs"))?,
        ),
        ServiceCommand::Start => (
            "kai.service.start",
            serde_json::to_value(service_start(&config)?)
                .map_err(serialize_error("serialize service start"))?,
        ),
        ServiceCommand::Stop => (
            "kai.service.stop",
            serde_json::to_value(service_stop(&config)?)
                .map_err(serialize_error("serialize service stop"))?,
        ),
        ServiceCommand::Restart => (
            "kai.service.restart",
            serde_json::to_value(service_restart(&config)?)
                .map_err(serialize_error("serialize service restart"))?,
        ),
        ServiceCommand::Uninstall => (
            "kai.service.uninstall",
            serde_json::to_value(service_uninstall(&config)?)
                .map_err(serialize_error("serialize service uninstall"))?,
        ),
    };

    Ok(Flow::Immediate {
        tool: tool.to_string(),
        payload,
    })
}

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| {
        KaiError::new(
            kai_sdk::ErrorCode::IoError,
            format!("failed to {action}: {error}"),
        )
    }
}
