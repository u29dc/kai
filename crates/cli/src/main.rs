use std::io::{self, Write};

use anyhow::Result;
use clap::{Parser, Subcommand};
use kai_sdk::{OkEnvelope, default_config, health_report, tool_catalog, tool_spec};

#[derive(Debug, Parser)]
#[command(name = "kai")]
#[command(about = "JSON-first local Codex portal bootstrap")]
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
}

#[derive(Debug, Subcommand)]
enum ConfigCommand {
    Show,
}

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Tools { name } => match name {
            Some(name) => write_json(&OkEnvelope::new(
                "kai.tools.detail",
                tool_spec(&name).unwrap_or_else(|| missing_tool(&name)),
            )),
            None => write_json(&OkEnvelope::new("kai.tools", tool_catalog())),
        },
        Command::Health => write_json(&OkEnvelope::new("kai.health", health_report())),
        Command::Config { command } => match command {
            ConfigCommand::Show => {
                write_json(&OkEnvelope::new("kai.config.show", default_config()))
            }
        },
    }
}

fn write_json<T>(value: &T) -> Result<()>
where
    T: serde::Serialize,
{
    let stdout = io::stdout();
    let mut handle = stdout.lock();

    serde_json::to_writer(&mut handle, value)?;
    handle.write_all(b"\n")?;
    Ok(())
}

fn missing_tool(name: &str) -> kai_sdk::ToolSpec {
    kai_sdk::ToolSpec {
        name: "missing",
        command: "kai tools <name>",
        category: "infra",
        description: Box::leak(format!("unknown tool: {name}").into_boxed_str()),
        parameters: vec![],
        output_fields: vec![],
        output_schema: "missing-tool",
        input_schema: None,
        idempotent: true,
        rate_limit: None,
        example: "kai tools health",
    }
}
