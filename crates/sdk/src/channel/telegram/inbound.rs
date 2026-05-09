use super::*;
use crate::workspace::workspace_by_id;

pub(super) async fn handle_message(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active: &mut ActiveTelegramTurns,
    update_id: i64,
    message: TelegramMessage,
) -> KaiResult<()> {
    if let Some(processed) = state.get_processed_update(update_id)? {
        return replay_processed_update(client, token, message.chat.id, &processed).await;
    }

    let Some(validated) = validate_inbound_message(client, token, config, state, &message).await?
    else {
        return Ok(());
    };

    if let Some(command) = parse_mobile_command(&validated.text) {
        let outcome =
            handle_mobile_command(client, token, config, state, active, &validated, command)
                .await?;
        state.set_processed_update(update_id, &outcome, None)?;
        return Ok(());
    }

    let attachments = download_message_attachments(client, token, config, state, &message).await?;
    enqueue_owner_turn(
        client,
        token,
        state,
        &mut active.main,
        PendingTurn {
            id: stable_pending_turn_id(
                "telegram",
                validated.chat_id,
                validated.sender_id,
                &[update_id],
            ),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
            target: crate::workspace::execution_target(config, state)?,
            channel: "telegram".to_string(),
            update_ids: vec![update_id],
            chat_id: validated.chat_id,
            sender_id: validated.sender_id,
            text: validated.text,
            attachments,
        },
    )
    .await
}

pub(super) async fn validate_inbound_message(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    message: &TelegramMessage,
) -> KaiResult<Option<ValidatedInbound>> {
    if message.chat.kind != "private" {
        return Ok(None);
    }

    let sender_id = match message.from.as_ref() {
        Some(user) => user.id,
        None => return Ok(None),
    };
    let chat_id = message.chat.id;

    let owner_id = config
        .values
        .channel
        .telegram
        .owner_user_id
        .or(state.get_owner_user_id()?);
    let owner_chat_id = state.get_owner_chat_id()?;

    let text = message
        .text
        .clone()
        .or(message.caption.clone())
        .unwrap_or_default()
        .trim()
        .to_string();

    if owner_id.is_none() && try_pair(client, token, state, chat_id, sender_id, &text).await? {
        return Ok(None);
    }

    let Some(owner_id) = owner_id.or(state.get_owner_user_id()?) else {
        return Ok(None);
    };

    if sender_id != owner_id {
        return Ok(None);
    }

    if let Some(expected_chat_id) = owner_chat_id {
        if chat_id != expected_chat_id {
            return Ok(None);
        }
    } else {
        state.set_owner_chat_id(chat_id)?;
    }

    Ok(Some(ValidatedInbound {
        chat_id,
        sender_id,
        text,
    }))
}

pub(super) fn should_ignore_message_before_buffering(
    config: &LoadedConfig,
    state: &StateStore,
    message: &TelegramMessage,
) -> KaiResult<bool> {
    if message.chat.kind != "private" {
        return Ok(true);
    }

    let Some(sender_id) = message.from.as_ref().map(|user| user.id) else {
        return Ok(true);
    };

    let owner_id = config
        .values
        .channel
        .telegram
        .owner_user_id
        .or(state.get_owner_user_id()?);
    let owner_chat_id = state.get_owner_chat_id()?;

    if let Some(owner_id) = owner_id {
        if sender_id != owner_id {
            return Ok(true);
        }
        if let Some(expected_chat_id) = owner_chat_id
            && message.chat.id != expected_chat_id
        {
            return Ok(true);
        }
    }

    Ok(false)
}

pub(super) fn parse_mobile_command(text: &str) -> Option<MobileCommand> {
    let trimmed = text.trim();
    match trimmed {
        "/help" | "help" => Some(MobileCommand::Help),
        "/status" => Some(MobileCommand::Status),
        "/dir" | "/switchdir" | "dir" => Some(MobileCommand::Dir { workspace_id: None }),
        "/new" | "/reset" => Some(MobileCommand::Reset),
        "/cancel" | "/interrupt" => Some(MobileCommand::Cancel { side_query: false }),
        "/cancel ask" => Some(MobileCommand::Cancel { side_query: true }),
        _ => trimmed
            .strip_prefix("/dir ")
            .or_else(|| trimmed.strip_prefix("/switchdir "))
            .or_else(|| trimmed.strip_prefix("dir "))
            .map(str::trim)
            .filter(|workspace_id| !workspace_id.is_empty())
            .map(|workspace_id| MobileCommand::Dir {
                workspace_id: Some(workspace_id.to_string()),
            })
            .or_else(|| {
                trimmed
                    .strip_prefix("/ask ")
                    .map(str::trim)
                    .filter(|prompt| !prompt.is_empty())
                    .map(|prompt| MobileCommand::Ask {
                        prompt: prompt.to_string(),
                    })
            })
            .or_else(|| {
                trimmed
                    .strip_prefix("/send ")
                    .map(str::trim)
                    .filter(|path| !path.is_empty())
                    .map(|path| MobileCommand::Send {
                        path: path.to_string(),
                    })
            }),
    }
}

pub(super) async fn handle_mobile_command(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active: &mut ActiveTelegramTurns,
    validated: &ValidatedInbound,
    command: MobileCommand,
) -> KaiResult<ProcessedUpdateOutcome> {
    match command {
        MobileCommand::Help => {
            let text = mobile_help_text();
            send_message(client, token, validated.chat_id, &text).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text })
        }
        MobileCommand::Status => {
            let mut status = mobile_status_text(config, state)?;
            status.push_str(&format!(
                "\nmain: {}\nqueue: {} pending\nside: {}",
                if active.main.is_some() { "yes" } else { "no" },
                state.pending_turn_queue_len()?,
                active
                    .side_query
                    .as_ref()
                    .map(|query| format!(
                        "working {} ({})",
                        query.state.id, query.state.target.workspace_id
                    ))
                    .unwrap_or_else(|| "idle".to_string())
            ));
            send_message(client, token, validated.chat_id, &status).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text: status })
        }
        MobileCommand::Dir { workspace_id } => {
            if let Some(workspace_id) = workspace_id {
                let workspace = workspace_by_id(config, &workspace_id)?;
                state.set_selected_workspace_id(&workspace.id)?;
                let session_status = state
                    .get_session_binding(&crate::workspace::execution_target(config, state)?)?
                    .map(|binding| format!("resuming session {}", binding.session_id))
                    .unwrap_or_else(|| "next turn will start fresh".to_string());
                let text = format!(
                    "Workspace set to {} ({})\n{}",
                    workspace.id, workspace.path, session_status
                );
                send_message(client, token, validated.chat_id, &text).await?;
                return Ok(ProcessedUpdateOutcome::TextReply { text });
            }

            let workspace_status = state.workspace_status_output(config)?;
            let lines = workspace_status
                .workspaces
                .into_iter()
                .map(|workspace| {
                    let marker = if workspace.selected { "*" } else { " " };
                    format!(
                        "{} {} ({}){}",
                        marker,
                        workspace.id,
                        workspace.path,
                        workspace
                            .active_session_id
                            .as_ref()
                            .map(|session_id| format!(" session={session_id}"))
                            .unwrap_or_default()
                    )
                })
                .collect::<Vec<_>>();
            let text = format!(
                "Current workspace: {}\nAvailable:\n{}",
                workspace_status.selected_workspace_id,
                lines.join("\n")
            );
            send_message(client, token, validated.chat_id, &text).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text })
        }
        MobileCommand::Reset => {
            if let Some(turn) = active.main.as_mut() {
                turn.cancel_requested = true;
                let _ = cancel_agent_turn(&turn.running);
            }
            let target = crate::workspace::execution_target(config, state)?;
            state.clear_session_binding(&target)?;
            state.clear_target_replay_package(&target)?;
            let text =
                "Cleared the current workspace session. The next queued or new message will start fresh."
                    .to_string();
            send_message(client, token, validated.chat_id, &text).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text })
        }
        MobileCommand::Cancel { side_query: true } => {
            let Some(query) = active.side_query.as_mut() else {
                let text = "No active side query to cancel.".to_string();
                send_message(client, token, validated.chat_id, &text).await?;
                return Ok(ProcessedUpdateOutcome::TextReply { text });
            };
            query.cancel_requested = true;
            cancel_agent_turn(&query.running)?;
            let text = "Cancel requested for the active side query.".to_string();
            send_message(client, token, validated.chat_id, &text).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text })
        }
        MobileCommand::Cancel { side_query: false } => {
            let Some(turn) = active.main.as_mut() else {
                let text = "No active run to cancel.".to_string();
                send_message(client, token, validated.chat_id, &text).await?;
                return Ok(ProcessedUpdateOutcome::TextReply { text });
            };
            turn.cancel_requested = true;
            cancel_agent_turn(&turn.running)?;
            let text = "Cancel requested. I will stop the current run.".to_string();
            send_message(client, token, validated.chat_id, &text).await?;
            Ok(ProcessedUpdateOutcome::TextReply { text })
        }
        MobileCommand::Ask { prompt } => {
            start_side_query(
                client,
                token,
                config,
                state,
                &mut active.side_query,
                validated,
                prompt,
            )
            .await
        }
        MobileCommand::Send { path } => {
            let resolved = resolve_requested_path(config, state, &path)?;
            let sent = send_local_paths(
                client,
                token,
                validated.chat_id,
                std::slice::from_ref(&resolved),
            )
            .await?;
            let response_text = format!("Sent {} file(s).", sent);
            send_message(client, token, validated.chat_id, &response_text).await?;
            Ok(ProcessedUpdateOutcome::SendLocalPaths {
                paths: vec![resolved.display().to_string()],
                response_text,
            })
        }
    }
}

async fn replay_processed_update(
    client: &Client,
    token: &str,
    chat_id: i64,
    processed: &ProcessedUpdate,
) -> KaiResult<()> {
    match &processed.outcome {
        ProcessedUpdateOutcome::TextReply { text } => {
            send_message(client, token, chat_id, text).await
        }
        ProcessedUpdateOutcome::SendLocalPaths {
            paths,
            response_text,
        } => {
            let paths = paths.iter().map(PathBuf::from).collect::<Vec<_>>();
            send_local_paths(client, token, chat_id, &paths).await?;
            send_message(client, token, chat_id, response_text).await
        }
    }
}

async fn try_pair(
    client: &Client,
    token: &str,
    state: &StateStore,
    chat_id: i64,
    sender_id: i64,
    text: &str,
) -> KaiResult<bool> {
    let Some(code) = text.strip_prefix("/pair ").map(str::trim) else {
        return Ok(false);
    };

    let Some(mut pending) = state.get_pending_pairing()? else {
        send_message(
            client,
            token,
            chat_id,
            "No active recovery code. Open a fresh recovery window locally first.",
        )
        .await?;
        return Ok(true);
    };

    if pending.is_expired() {
        state.clear_pending_pairing()?;
        send_message(
            client,
            token,
            chat_id,
            "Recovery code expired. Generate a fresh one locally and try again.",
        )
        .await?;
        return Ok(true);
    }

    let normalized = code.trim().to_ascii_uppercase();
    if !pending.verify(&normalized) {
        pending.consume_failed_attempt();
        if pending.remaining_attempts == 0 {
            state.clear_pending_pairing()?;
            send_message(
                client,
                token,
                chat_id,
                "Recovery code invalid. The recovery window is now closed.",
            )
            .await?;
            return Ok(true);
        }

        let attempts_left = pending.remaining_attempts;
        state.set_pending_pairing(&pending)?;
        send_message(
            client,
            token,
            chat_id,
            &format!("Recovery code mismatch. {attempts_left} attempt(s) remain."),
        )
        .await?;
        return Ok(true);
    }

    state.set_owner_user_id(sender_id)?;
    state.set_owner_chat_id(chat_id)?;
    state.clear_pending_pairing()?;
    send_message(
        client,
        token,
        chat_id,
        "Recovery complete. You can send prompts now.",
    )
    .await?;
    Ok(true)
}
