use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Parser, Subcommand};
use kai_sdk::{
    ConfigGetOutput, ConfigShowOutput, KaiError, KaiResult, SessionView, SetupCodexOutput,
    SetupOutput, StateStore, config_value_at_key, context_report, ensure_config_file,
    error_envelope, health_report, load_config, mobile_help_text, ok_envelope, run_telegram_loop,
    set_config_value, tool_catalog, tool_spec, unset_config_value,
};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

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
    Telegram,
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
    New,
    Reset,
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
        Command::Run => {
            let config = load_config()?;
            if !config.values.channel.telegram.enabled {
                return Err(KaiError::blocked_prerequisite(
                    "telegram is disabled in config",
                ));
            }
            if std::env::var(&config.values.channel.telegram.bot_token_env).is_err() {
                return Err(KaiError::blocked_prerequisite(format!(
                    "telegram bot token env `{}` is not set",
                    config.values.channel.telegram.bot_token_env
                ))
                .with_hint("export the bot token env var before running `kai run`"));
            }
            let state = StateStore::open(&config)?;
            let payload = json!({
                "status": "starting",
                "help": mobile_help_text(),
                "rootApp": config.values.paths.root_app,
            });

            Ok(Flow::Streaming {
                tool: "kai.run".to_string(),
                payload,
                future: Box::pin(async move { run_telegram_loop(&config, &state).await }),
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
        Some(SetupCommand::Telegram) => {
            let config = load_config()?;
            ensure_config_file(&config.config_path)?;
            let config = load_config()?;
            let state = StateStore::open(&config)?;
            let created_paths = create_runtime_dirs(&config, &state)?;
            create_context_placeholders(&config)?;

            let pair_code = Uuid::new_v4()
                .simple()
                .to_string()
                .chars()
                .take(8)
                .collect::<String>()
                .to_uppercase();
            state.set_pending_pair_code(&pair_code)?;

            let payload = json!({
                "configPath": config.config_path.display().to_string(),
                "pairCode": pair_code,
                "botTokenEnv": config.values.channel.telegram.bot_token_env,
                "createdPaths": created_paths,
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

    match command {
        SessionCommand::Show => {}
        SessionCommand::New | SessionCommand::Reset => {
            state.clear_active_session_id()?;
        }
    }

    let payload = serde_json::to_value(session_view_with_override(&config, &state)?)
        .map_err(serialize_error("serialize session view"))?;

    let tool = match command {
        SessionCommand::Show => "kai.session.show",
        SessionCommand::New => "kai.session.new",
        SessionCommand::Reset => "kai.session.reset",
    };

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
        fs::create_dir_all(&path).map_err(io_error("create runtime directory"))?;
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

        fs::write(file_path, format!("# {title}\n\n"))
            .map_err(io_error("write context placeholder"))?;
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

fn io_error(action: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| {
        KaiError::new(
            kai_sdk::ErrorCode::IoError,
            format!("failed to {action}: {error}"),
        )
    }
}
