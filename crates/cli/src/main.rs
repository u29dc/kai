use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Command as ProcessCommand, ExitCode};

use clap::{Parser, Subcommand};
use kai_sdk::{
    ConfigGetOutput, ConfigMigrationOutput, ConfigShowOutput, KaiError, KaiResult, SessionView,
    SetupCodexOutput, SetupOutput, StateStore, acquire_run_guard, config_value_at_key,
    ensure_config_file, ensure_private_dir, error_envelope, health_report, load_config,
    mobile_help_text, ok_envelope, resolve_telegram_token, run_telegram_loop, selected_provider,
    service_logs, service_restart, service_start, service_status, service_stop, service_uninstall,
    set_config_value, tool_catalog, tool_spec, unset_config_value,
};
use serde::Serialize;
use serde_json::{Value as JsonValue, json};
use uuid::Uuid;

mod handlers;

use self::handlers::dispatch;

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
    Workspace {
        #[command(subcommand)]
        command: WorkspaceCommand,
    },
    Setup {
        #[command(subcommand)]
        command: Option<SetupCommand>,
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
    Migrate,
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
enum SessionCommand {
    Show,
    Set { session_id: String },
    New,
}

#[derive(Debug, Subcommand)]
enum WorkspaceCommand {
    Show,
    Select { workspace_id: String },
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

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    fn help_text_for(command_path: &[&str]) -> String {
        let mut command = Cli::command();
        for name in command_path {
            command = command
                .find_subcommand_mut(name)
                .expect("subcommand")
                .clone();
        }
        command.render_help().to_string()
    }

    #[test]
    fn session_help_exposes_canonical_commands_only() {
        let help = help_text_for(&["session"]);
        assert!(help.contains("new"));
        assert!(!help.contains("reset"));
    }

    #[test]
    fn workspace_help_exposes_canonical_commands_only() {
        let help = help_text_for(&["workspace"]);
        assert!(help.contains("show"));
        assert!(help.contains("select"));
        assert!(!help.contains("list"));
    }

    #[test]
    fn removed_aliases_are_rejected() {
        assert!(Cli::try_parse_from(["kai", "context", "check"]).is_err());
        assert!(Cli::try_parse_from(["kai", "session", "reset"]).is_err());
        assert!(Cli::try_parse_from(["kai", "workspace", "list"]).is_err());
    }

    #[test]
    fn tool_catalog_matches_public_cli_surface() {
        let mut names = tool_catalog()
            .tools
            .into_iter()
            .map(|tool| tool.name)
            .collect::<Vec<_>>();
        names.sort();

        let mut expected = vec![
            "config.get",
            "config.migrate",
            "config.set",
            "config.show",
            "config.unset",
            "health",
            "run",
            "service.logs",
            "service.restart",
            "service.start",
            "service.status",
            "service.stop",
            "service.uninstall",
            "session.new",
            "session.set",
            "session.show",
            "setup",
            "setup.codex",
            "setup.telegram",
            "tools",
            "workspace.select",
            "workspace.show",
        ];
        expected.sort();

        assert_eq!(names, expected);
    }
}
