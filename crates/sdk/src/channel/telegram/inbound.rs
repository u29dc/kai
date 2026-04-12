use super::*;

pub(super) async fn handle_message(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
    update_id: i64,
    message: TelegramMessage,
) -> KaiResult<()> {
    if let Some(processed) = state.get_processed_update(update_id)? {
        return send_message(client, token, message.chat.id, &processed.response_text).await;
    }

    let Some(validated) = validate_inbound_message(client, token, config, state, &message).await?
    else {
        return Ok(());
    };

    if let Some(command) = parse_mobile_command(&validated.text) {
        return handle_mobile_command(
            client,
            token,
            config,
            state,
            active_turn,
            &validated,
            command,
        )
        .await;
    }

    let attachments = download_message_attachments(client, token, config, state, &message).await?;
    enqueue_owner_turn(
        client,
        token,
        state,
        active_turn,
        PendingTurn {
            id: stable_pending_turn_id(
                "telegram",
                validated.chat_id,
                validated.sender_id,
                &[update_id],
            ),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
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

fn parse_mobile_command(text: &str) -> Option<MobileCommand> {
    let trimmed = text.trim();
    match trimmed {
        "/help" | "help" => Some(MobileCommand::Help),
        "/status" => Some(MobileCommand::Status),
        "/new" | "/reset" => Some(MobileCommand::Reset),
        "/cancel" | "/interrupt" => Some(MobileCommand::Cancel),
        _ => trimmed
            .strip_prefix("/send ")
            .map(str::trim)
            .filter(|path| !path.is_empty())
            .map(|path| MobileCommand::Send {
                path: path.to_string(),
            }),
    }
}

async fn handle_mobile_command(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
    validated: &ValidatedInbound,
    command: MobileCommand,
) -> KaiResult<()> {
    match command {
        MobileCommand::Help => {
            send_message(client, token, validated.chat_id, &mobile_help_text()).await
        }
        MobileCommand::Status => {
            let mut status = mobile_status_text(config, state)?;
            status.push_str(&format!(
                "\nbusy: {}\nqueued_turns: {}",
                if active_turn.is_some() { "yes" } else { "no" },
                state.pending_turn_queue_len()?
            ));
            send_message(client, token, validated.chat_id, &status).await
        }
        MobileCommand::Reset => {
            if let Some(turn) = active_turn.as_mut() {
                turn.cancel_requested = true;
                let _ = cancel_codex_turn(&turn.running);
            }
            state.clear_active_session_id()?;
            state.clear_replay_package()?;
            send_message(
                client,
                token,
                validated.chat_id,
                "Cleared the active Codex session. The next queued or new message will start fresh.",
            )
            .await
        }
        MobileCommand::Cancel => {
            let Some(turn) = active_turn.as_mut() else {
                return send_message(client, token, validated.chat_id, "No active run to cancel.")
                    .await;
            };
            turn.cancel_requested = true;
            cancel_codex_turn(&turn.running)?;
            send_message(
                client,
                token,
                validated.chat_id,
                "Cancel requested. I will stop the current run.",
            )
            .await
        }
        MobileCommand::Send { path } => {
            let resolved = resolve_requested_path(config, &path)?;
            let sent = send_local_paths(client, token, validated.chat_id, &[resolved]).await?;
            send_message(
                client,
                token,
                validated.chat_id,
                &format!("Sent {} file(s).", sent),
            )
            .await
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
