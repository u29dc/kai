use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;

use crate::app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::state::{AttachmentInfo, StateStore};

pub async fn run_telegram_loop(config: &LoadedConfig, state: &StateStore) -> KaiResult<()> {
    if !config.values.channel.telegram.enabled {
        return Err(KaiError::blocked_prerequisite(
            "telegram is disabled in config",
        ));
    }

    let token = telegram_token(config)?;
    let client = Client::builder().build().map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to build http client: {error}"),
        )
    })?;

    let mut offset = state.get_update_offset()?;

    loop {
        let updates = get_updates(&client, &token, offset).await?;
        for update in updates {
            offset = update.update_id + 1;
            state.set_update_offset(offset)?;

            if let Some(message) = update.message {
                handle_message(&client, &token, config, state, message).await?;
            }
        }
    }
}

pub async fn send_telegram_message(
    config: &LoadedConfig,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    let token = telegram_token(config)?;
    let client = Client::builder().build().map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to build http client: {error}"),
        )
    })?;
    send_message(&client, &token, chat_id, text).await
}

async fn handle_message(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    message: TelegramMessage,
) -> KaiResult<()> {
    let sender_id = match message.from.as_ref() {
        Some(user) => user.id,
        None => return Ok(()),
    };
    let chat_id = message.chat.id;

    let owner_id = config
        .values
        .channel
        .telegram
        .owner_user_id
        .or(state.get_owner_user_id()?);

    let text = message
        .text
        .clone()
        .or(message.caption.clone())
        .unwrap_or_default()
        .trim()
        .to_string();

    if owner_id.is_none() && try_pair(client, token, state, chat_id, sender_id, &text).await? {
        return Ok(());
    }

    let Some(owner_id) = owner_id.or(state.get_owner_user_id()?) else {
        send_message(
			client,
			token,
			chat_id,
			"kai is not paired yet. Run `kai setup telegram` locally, then send `/pair <code>` here.",
		)
		.await?;
        return Ok(());
    };

    if sender_id != owner_id {
        return Ok(());
    }

    match text.as_str() {
        "/help" | "help" => {
            send_message(client, token, chat_id, &mobile_help_text()).await?;
            return Ok(());
        }
        "/status" => {
            let status = mobile_status_text(config, state)?;
            send_message(client, token, chat_id, &status).await?;
            return Ok(());
        }
        "/new" | "/reset" => {
            state.clear_active_session_id()?;
            send_message(
                client,
                token,
                chat_id,
                "Cleared the active Codex session. The next message will start fresh.",
            )
            .await?;
            return Ok(());
        }
        _ => {}
    }

    let attachments = download_attachments(client, token, state, &message).await?;
    if text.is_empty() && attachments.is_empty() {
        send_message(
            client,
            token,
            chat_id,
            "I need text or a supported attachment to do anything useful.",
        )
        .await?;
        return Ok(());
    }

    let response = handle_owner_prompt(config, state, "telegram", sender_id, &text, &attachments)?;
    send_message(client, token, chat_id, &response).await
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

    let pending = state.get_pending_pair_code()?;
    let Some(pending) = pending else {
        send_message(
            client,
            token,
            chat_id,
            "No active pairing code. Run `kai setup telegram` locally first.",
        )
        .await?;
        return Ok(true);
    };

    if code != pending {
        send_message(
            client,
            token,
            chat_id,
            "Pairing code mismatch. Generate a fresh one locally and try again.",
        )
        .await?;
        return Ok(true);
    }

    state.set_owner_user_id(sender_id)?;
    state.clear_pending_pair_code()?;
    send_message(
        client,
        token,
        chat_id,
        "Pairing complete. You can send prompts now.",
    )
    .await?;
    Ok(true)
}

fn telegram_token(config: &LoadedConfig) -> KaiResult<String> {
    let key = &config.values.channel.telegram.bot_token_env;
    std::env::var(key).map_err(|_| {
        KaiError::blocked_prerequisite(format!("telegram bot token env `{key}` is not set"))
            .with_hint("export the token env var before running `kai run`")
    })
}

async fn get_updates(client: &Client, token: &str, offset: i64) -> KaiResult<Vec<TelegramUpdate>> {
    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getUpdates"))
        .query(&[
            ("offset", offset.to_string()),
            ("timeout", "30".to_string()),
            ("allowed_updates", "[\"message\"]".to_string()),
        ])
        .send()
        .await
        .map_err(http_error("poll Telegram updates"))?;

    let payload = response
        .json::<TelegramApiResponse<Vec<TelegramUpdate>>>()
        .await
        .map_err(http_error("decode Telegram updates"))?;

    if payload.ok {
        return Ok(payload.result.unwrap_or_default());
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram API returned an error".to_string()),
    ))
}

async fn send_message(client: &Client, token: &str, chat_id: i64, text: &str) -> KaiResult<()> {
    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&SendMessageRequest {
            chat_id,
            text: text.to_string(),
        })
        .send()
        .await
        .map_err(http_error("send Telegram message"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram sendMessage response"))?;

    if payload.ok {
        return Ok(());
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram sendMessage failed".to_string()),
    ))
}

async fn download_attachments(
    client: &Client,
    token: &str,
    state: &StateStore,
    message: &TelegramMessage,
) -> KaiResult<Vec<AttachmentInfo>> {
    let mut attachments = Vec::new();

    if let Some(document) = &message.document
        && is_supported_document(document)
    {
        attachments.push(
            download_file(
                client,
                token,
                state,
                &document.file_id,
                document.file_name.clone(),
                document.mime_type.clone(),
                determine_document_kind(document),
            )
            .await?,
        );
    }

    if let Some(photo) = &message.photo
        && let Some(best) = photo.iter().max_by_key(|item| item.file_size.unwrap_or(0))
    {
        attachments.push(
            download_file(
                client,
                token,
                state,
                &best.file_id,
                Some(format!("photo-{}.jpg", best.file_unique_id)),
                Some("image/jpeg".to_string()),
                "image".to_string(),
            )
            .await?,
        );
    }

    Ok(attachments)
}

async fn download_file(
    client: &Client,
    token: &str,
    state: &StateStore,
    file_id: &str,
    original_name: Option<String>,
    mime_type: Option<String>,
    kind: String,
) -> KaiResult<AttachmentInfo> {
    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id)])
        .send()
        .await
        .map_err(http_error("request Telegram file metadata"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramFile>>()
        .await
        .map_err(http_error("decode Telegram file metadata"))?;

    let file = payload.result.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            payload
                .description
                .unwrap_or_else(|| "Telegram getFile failed".to_string()),
        )
    })?;

    let file_path = file.file_path.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram did not return a downloadable file path",
        )
    })?;

    let safe_name = sanitize_filename(original_name.as_deref().unwrap_or(file_id));
    let local_path = state.paths().attachments_dir.join(safe_name);
    let bytes = client
        .get(format!(
            "https://api.telegram.org/file/bot{token}/{file_path}"
        ))
        .send()
        .await
        .map_err(http_error("download Telegram file"))?
        .bytes()
        .await
        .map_err(http_error("read Telegram file body"))?;

    fs::write(&local_path, &bytes).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to write attachment to disk: {error}"),
        )
    })?;

    let checksum_blake3 = blake3::hash(&bytes).to_hex().to_string();

    Ok(AttachmentInfo {
        kind,
        path: local_path.display().to_string(),
        original_name,
        mime_type,
        bytes: bytes.len() as u64,
        checksum_blake3,
    })
}

fn is_supported_document(document: &TelegramDocument) -> bool {
    matches!(determine_document_kind(document).as_str(), "pdf" | "text")
}

fn determine_document_kind(document: &TelegramDocument) -> String {
    let file_name = document
        .file_name
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();
    let mime_type = document
        .mime_type
        .as_deref()
        .unwrap_or_default()
        .to_lowercase();

    if mime_type == "application/pdf" || file_name.ends_with(".pdf") {
        return "pdf".to_string();
    }

    if mime_type.starts_with("text/")
        || file_name.ends_with(".md")
        || file_name.ends_with(".txt")
        || file_name.ends_with(".markdown")
    {
        return "text".to_string();
    }

    "unsupported".to_string()
}

fn sanitize_filename(input: &str) -> String {
    let mut output = input
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();

    if output.is_empty() {
        output = "attachment".to_string();
    }

    output
}

fn http_error(action: &'static str) -> impl Fn(reqwest::Error) -> KaiError {
    move |error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to {action}: {error}"),
        )
    }
}

#[derive(Debug, Deserialize)]
struct TelegramApiResponse<T> {
    ok: bool,
    result: Option<T>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Deserialize)]
struct TelegramMessage {
    from: Option<TelegramUser>,
    chat: TelegramChat,
    text: Option<String>,
    caption: Option<String>,
    document: Option<TelegramDocument>,
    photo: Option<Vec<TelegramPhotoSize>>,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
    file_unique_id: String,
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
}
