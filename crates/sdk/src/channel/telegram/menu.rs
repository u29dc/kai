use super::*;

pub(super) async fn sync_command_menu_if_needed(
    client: &Client,
    token: &str,
    state: &StateStore,
    chat_id: i64,
) -> KaiResult<()> {
    let commands = telegram_menu_commands();
    let hash_input = commands
        .iter()
        .map(|command| format!("{}:{}", command.command, command.description))
        .collect::<Vec<_>>()
        .join("|");
    let current_hash = blake3::hash(hash_input.as_bytes()).to_hex().to_string();
    if state.get_command_menu_hash(chat_id)?.as_deref() == Some(current_hash.as_str()) {
        return Ok(());
    }

    let scope = serde_json::json!({
        "type": "chat",
        "chat_id": chat_id,
    });

    let _ = client
        .post(format!(
            "https://api.telegram.org/bot{token}/deleteMyCommands"
        ))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&serde_json::json!({ "scope": scope }))
        .send()
        .await;

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/setMyCommands"))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&serde_json::json!({
            "scope": scope,
            "commands": commands,
        }))
        .send()
        .await
        .map_err(http_error("sync Telegram command menu"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram command menu response"))?;

    if !payload.ok {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            payload
                .description
                .unwrap_or_else(|| "Telegram setMyCommands failed".to_string()),
        ));
    }

    state.set_command_menu_hash(chat_id, &current_hash)?;
    Ok(())
}

fn telegram_menu_commands() -> Vec<TelegramMenuCommand> {
    vec![
        TelegramMenuCommand::new("status", "Show current session and queue status"),
        TelegramMenuCommand::new("new", "Start the next turn with a fresh session"),
        TelegramMenuCommand::new("cancel", "Cancel the current running turn"),
        TelegramMenuCommand::new("send", "Send a local file back to this chat"),
        TelegramMenuCommand::new("help", "Show the available mobile commands"),
    ]
}
