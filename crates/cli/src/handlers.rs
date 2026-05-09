use super::*;

pub(super) async fn dispatch(cli: Cli) -> KaiResult<Flow> {
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
        Command::Workspace { command } => handle_workspace_command(command),
        Command::Setup { command } => handle_setup_command(command).await,
        Command::Context { command } => {
            let config = load_config()?;
            let payload = serde_json::to_value(context_report(&config))
                .map_err(serialize_error("serialize context report"))?;

            let tool = match command {
                ContextCommand::Show => "kai.context.show",
                ContextCommand::Check => "kai.context.show",
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
            validate_run_prerequisites(&config)?;
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

fn validate_run_prerequisites(config: &kai_sdk::LoadedConfig) -> KaiResult<()> {
    if !config.values.channel.telegram.enabled {
        return Err(KaiError::blocked_prerequisite(
            "telegram is disabled in config",
        ));
    }

    let _ = selected_provider(config)?;
    let _ = resolve_telegram_token(config)?;
    Ok(())
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
        ConfigCommand::Migrate => {
            let result = kai_sdk::migrate_config_to_workspaces()?;
            let payload = serde_json::to_value(ConfigMigrationOutput {
                config_path: result.config_path.display().to_string(),
                backup_path: result
                    .backup_path
                    .as_ref()
                    .map(|path| path.display().to_string()),
                migrated: result.migrated,
                default_workspace_id: result.default_workspace_id,
                removed_legacy_keys: result.removed_legacy_keys,
            })
            .map_err(serialize_error("serialize config migration output"))?;

            Ok(Flow::Immediate {
                tool: "kai.config.migrate".to_string(),
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
                default_workspace_id: config.values.workspaces.default_workspace.clone(),
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
            let app_server_available = ProcessCommand::new(&binary)
                .arg("app-server")
                .arg("--help")
                .output()
                .map(|output| output.status.success())
                .unwrap_or(false);

            let payload = serde_json::to_value(SetupCodexOutput {
                binary,
                exec_available,
                resume_available,
                app_server_available,
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
    let target = kai_sdk::execution_target(&config, &state)?;
    let tool = match &command {
        SessionCommand::Show => "kai.session.show",
        SessionCommand::Set { .. } => "kai.session.set",
        SessionCommand::New => "kai.session.new",
        SessionCommand::Reset => "kai.session.new",
    };

    match command {
        SessionCommand::Show => {}
        SessionCommand::Set { session_id } => {
            if session_id.trim().is_empty() {
                return Err(KaiError::invalid_argument("session id cannot be empty"));
            }
            state.set_session_binding(&target, session_id.trim())?;
            state.clear_target_replay_package(&target)?;
        }
        SessionCommand::New | SessionCommand::Reset => {
            state.clear_session_binding(&target)?;
            state.clear_target_replay_package(&target)?;
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
    let mut session = state.session_view(config)?;
    if session.owner_user_id.is_none() {
        session.owner_user_id = config.values.channel.telegram.owner_user_id;
    }
    Ok(session)
}

fn handle_workspace_command(command: WorkspaceCommand) -> KaiResult<Flow> {
    let config = load_config()?;
    let state = StateStore::open(&config)?;

    if let WorkspaceCommand::Select { workspace_id } = &command {
        let workspace = kai_sdk::workspace_by_id(&config, workspace_id)?;
        state.set_selected_workspace_id(&workspace.id)?;
    }

    let payload = serde_json::to_value(state.workspace_status_output(&config)?)
        .map_err(serialize_error("serialize workspace status output"))?;
    let tool = match command {
        WorkspaceCommand::List => "kai.workspace.show",
        WorkspaceCommand::Show => "kai.workspace.show",
        WorkspaceCommand::Select { .. } => "kai.workspace.select",
    };

    Ok(Flow::Immediate {
        tool: tool.to_string(),
        payload,
    })
}

fn create_runtime_dirs(
    config: &kai_sdk::LoadedConfig,
    state: &StateStore,
) -> KaiResult<Vec<String>> {
    let mut created = Vec::new();

    for path in [
        Path::new(&config.values.paths.root_app).to_path_buf(),
        state.paths().attachments_dir.clone(),
        state.paths().logs_dir.clone(),
        state.paths().state_dir.clone(),
    ] {
        ensure_private_dir(&path)?;
        created.push(path.display().to_string());
    }

    for workspace in config.values.workspaces.entries.values() {
        let workspace_path = Path::new(&workspace.path);
        if workspace_path.starts_with(Path::new(&config.values.paths.root_app)) {
            fs::create_dir_all(workspace_path).map_err(io_error("create workspace directory"))?;
            created.push(workspace_path.display().to_string());
        }
    }

    created.sort();
    created.dedup();
    Ok(created)
}

fn create_context_placeholders(config: &kai_sdk::LoadedConfig) -> KaiResult<()> {
    for (path, title) in [
        (config.values.context_files.soul.as_str(), "SOUL"),
        (config.values.context_files.memory.as_str(), "MEMORY"),
    ] {
        let file_path = Path::new(path);
        if file_path.is_file() {
            continue;
        }

        if let Some(parent) = file_path.parent() {
            fs::create_dir_all(parent).map_err(io_error("create context directory"))?;
        }

        if file_path.starts_with(Path::new(&config.values.paths.root_app)) {
            write_private_file(file_path, format!("# {title}\n\n").as_bytes())?;
        } else {
            fs::write(file_path, format!("# {title}\n\n"))
                .map_err(io_error("write context placeholder"))?;
        }
    }

    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use kai_sdk::{
        ErrorCode, LoadedConfig,
        config::{
            AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, MediaConfig,
            PathsConfig, RunnerConfig, RunnerProvider, TelegramConfig, TelegramProgressConfig,
            TranscriptionConfig, WorkspaceConfig, WorkspacesConfig,
        },
    };
    use std::path::PathBuf;

    fn test_config(provider: RunnerProvider) -> LoadedConfig {
        LoadedConfig {
            config_path: PathBuf::from("/tmp/kai.toml"),
            config_exists: false,
            values: Config {
                agent: AgentConfig {
                    timezone: "Europe/London".to_string(),
                },
                channel: ChannelConfig {
                    telegram: TelegramConfig {
                        enabled: true,
                        bot_token_env: "KAI_TELEGRAM_BOT_TOKEN".to_string(),
                        owner_user_id: None,
                        progress: TelegramProgressConfig {
                            enabled: true,
                            edit_interval_ms: 2500,
                            idle_update_secs: 8,
                        },
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
                    root_app: "/tmp/kai".to_string(),
                },
                runner: RunnerConfig {
                    provider,
                    codex: CodexConfig {
                        binary: "codex".to_string(),
                        transport: kai_sdk::CodexTransport::AppServer,
                        service_name: Some("kai".to_string()),
                        override_config: None,
                    },
                },
                context_files: ContextFilesConfig {
                    soul: "/tmp/kai/SOUL.md".to_string(),
                    memory: "/tmp/kai/MEMORY.md".to_string(),
                },
                workspaces: WorkspacesConfig {
                    default_workspace: "main".to_string(),
                    entries: std::collections::BTreeMap::from([(
                        "main".to_string(),
                        WorkspaceConfig {
                            label: Some("Main".to_string()),
                            path: "/tmp/work".to_string(),
                        },
                    )]),
                },
            },
        }
    }

    #[test]
    fn validate_run_prerequisites_blocks_unsupported_provider_before_token_lookup() {
        let error = validate_run_prerequisites(&test_config(RunnerProvider::Claude))
            .expect_err("unsupported provider should be blocked");

        assert!(matches!(error.code, ErrorCode::BlockedPrerequisite));
        assert!(error.message.contains("claude"));
    }
}
