use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::media::{
    ATTACHMENT_CLEANUP_INTERVAL, ATTACHMENT_RETENTION, AttachmentKind, MAX_ATTACHMENTS_PER_TURN,
    MAX_MEDIA_GROUP_ITEMS, MEDIA_GROUP_DEBOUNCE, attachment_byte_limit, classify_document_kind,
    enrich_attachment,
};
use crate::secrets::resolve_telegram_token;
use crate::state::{AttachmentInfo, StateStore};

const TELEGRAM_RETRY_BACKOFF: Duration = Duration::from_secs(3);
const TELEGRAM_TYPING_REFRESH: Duration = Duration::from_secs(4);
const MAX_UPDATE_FAILURE_ATTEMPTS: u32 = 3;

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
    let mut next_cleanup_at = Instant::now();
    let mut media_groups = HashMap::<String, BufferedMediaGroup>::new();

    loop {
        let now = Instant::now();

        if next_cleanup_at <= now {
            if let Err(error) = run_attachment_cleanup(state) {
                record_runtime_error(
                    state,
                    "telegram.attachment_cleanup_failed",
                    None,
                    None,
                    &error,
                )?;
                eprintln!("kai attachment cleanup failed: {}", error.message);
            }
            next_cleanup_at = Instant::now() + ATTACHMENT_CLEANUP_INTERVAL;
        }

        flush_ready_media_groups(&client, &token, config, state, &mut media_groups, None).await?;

        let poll_timeout_seconds = if media_groups.is_empty() { 30 } else { 1 };
        let updates = match get_updates(&client, &token, offset, poll_timeout_seconds).await {
            Ok(updates) => updates,
            Err(error) => {
                record_runtime_error(state, "telegram.poll_failed", None, None, &error)?;
                eprintln!("kai telegram poll failed: {}", error.message);
                sleep(TELEGRAM_RETRY_BACKOFF).await;
                continue;
            }
        };

        for update in updates {
            let next_offset = update.update_id + 1;
            let chat_id = update.message.as_ref().map(|message| message.chat.id);

            let outcome = if let Some(message) = update.message {
                if let Some(media_group_id) = message.media_group_id.clone() {
                    buffer_media_group(
                        &mut media_groups,
                        update.update_id,
                        message,
                        &media_group_id,
                    );
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                    state.clear_update_failure(update.update_id)?;
                    continue;
                }

                flush_ready_media_groups(
                    &client,
                    &token,
                    config,
                    state,
                    &mut media_groups,
                    Some(update.update_id),
                )
                .await?;

                handle_message(&client, &token, config, state, update.update_id, message).await
            } else {
                Ok(())
            };

            match outcome {
                Ok(()) => {
                    state.clear_update_failure(update.update_id)?;
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                }
                Err(error) => {
                    match handle_update_failure(
                        &client,
                        &token,
                        state,
                        update.update_id,
                        chat_id,
                        &error,
                    )
                    .await?
                    {
                        UpdateFailureDisposition::Advance => {
                            offset = next_offset;
                            state.set_update_offset(offset)?;
                        }
                        UpdateFailureDisposition::Retry => {
                            eprintln!(
                                "kai telegram update {} failed: {}",
                                update.update_id, error.message
                            );
                            sleep(TELEGRAM_RETRY_BACKOFF).await;
                            break;
                        }
                    }
                }
            }
        }

        flush_ready_media_groups(&client, &token, config, state, &mut media_groups, None).await?;
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
    update_id: i64,
    message: TelegramMessage,
) -> KaiResult<()> {
    let Some(validated) = validate_inbound_message(client, token, config, state, &message).await?
    else {
        return Ok(());
    };

    let attachments = download_message_attachments(client, token, config, state, &message).await?;
    process_owner_turn(
        client,
        token,
        config,
        state,
        OwnerTurnInput {
            update_ids: &[update_id],
            chat_id: validated.chat_id,
            sender_id: validated.sender_id,
            text: &validated.text,
            attachments,
        },
    )
    .await
}

#[derive(Debug)]
struct ValidatedInbound {
    chat_id: i64,
    sender_id: i64,
    text: String,
}

#[derive(Debug)]
struct BufferedMediaGroup {
    media_group_id: String,
    chat_id: i64,
    last_update_id: i64,
    ready_at: Instant,
    update_ids: Vec<i64>,
    messages: Vec<TelegramMessage>,
}

async fn validate_inbound_message(
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

    match text.as_str() {
        "/help" | "help" => {
            send_message(client, token, chat_id, &mobile_help_text()).await?;
            return Ok(None);
        }
        "/status" => {
            let status = mobile_status_text(config, state)?;
            send_message(client, token, chat_id, &status).await?;
            return Ok(None);
        }
        "/new" | "/reset" => {
            state.clear_active_session_id()?;
            state.clear_replay_package()?;
            send_message(
                client,
                token,
                chat_id,
                "Cleared the active Codex session. The next message will start fresh.",
            )
            .await?;
            return Ok(None);
        }
        _ => {}
    }

    Ok(Some(ValidatedInbound {
        chat_id,
        sender_id,
        text,
    }))
}

async fn process_owner_turn(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    input: OwnerTurnInput<'_>,
) -> KaiResult<()> {
    for update_id in input.update_ids {
        if let Some(processed) = state.get_processed_update(*update_id)? {
            send_message(client, token, input.chat_id, &processed.response_text).await?;
            return Ok(());
        }
    }

    let typing_client = client.clone();
    let typing_token = token.to_string();
    let chat_id = input.chat_id;
    let typing_handle = tokio::spawn(async move {
        loop {
            let _ = send_typing_indicator(&typing_client, &typing_token, chat_id).await;
            sleep(TELEGRAM_TYPING_REFRESH).await;
        }
    });

    let result = async {
        if input.text.is_empty() && input.attachments.is_empty() {
            send_message(
                client,
                token,
                input.chat_id,
                "I need text or a supported attachment to do anything useful.",
            )
            .await?;
            return Ok(None);
        }

        let response = handle_owner_prompt(
            config,
            state,
            "telegram",
            input.sender_id,
            input.text,
            &input.attachments,
        )?;
        Ok(Some(response))
    }
    .await;

    typing_handle.abort();
    let Some(response) = result? else {
        return Ok(());
    };

    let session_id = state.get_active_session_id()?;
    for update_id in input.update_ids {
        state.set_processed_update(*update_id, &response, session_id.as_deref())?;
    }
    send_message(client, token, input.chat_id, &response).await
}

struct OwnerTurnInput<'a> {
    update_ids: &'a [i64],
    chat_id: i64,
    sender_id: i64,
    text: &'a str,
    attachments: Vec<AttachmentInfo>,
}

fn buffer_media_group(
    media_groups: &mut HashMap<String, BufferedMediaGroup>,
    update_id: i64,
    message: TelegramMessage,
    media_group_id: &str,
) {
    let key = media_group_key(message.chat.id, media_group_id);
    let entry = media_groups
        .entry(key)
        .or_insert_with(|| BufferedMediaGroup {
            media_group_id: media_group_id.to_string(),
            chat_id: message.chat.id,
            last_update_id: update_id,
            ready_at: Instant::now() + MEDIA_GROUP_DEBOUNCE,
            update_ids: Vec::new(),
            messages: Vec::new(),
        });

    entry.chat_id = message.chat.id;
    entry.last_update_id = update_id;
    entry.ready_at = Instant::now() + MEDIA_GROUP_DEBOUNCE;

    if entry.update_ids.contains(&update_id) {
        return;
    }

    if entry.messages.len() < MAX_MEDIA_GROUP_ITEMS {
        entry.update_ids.push(update_id);
        entry.messages.push(message);
    }
}

async fn flush_ready_media_groups(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    media_groups: &mut HashMap<String, BufferedMediaGroup>,
    current_update_id: Option<i64>,
) -> KaiResult<()> {
    let now = Instant::now();
    let ready_keys = media_groups
        .iter()
        .filter_map(|(key, entry)| {
            let force_flush = current_update_id.is_some_and(|value| value > entry.last_update_id);
            let due = now >= entry.ready_at
                || force_flush
                || entry.messages.len() >= MAX_MEDIA_GROUP_ITEMS;
            due.then_some(key.clone())
        })
        .collect::<Vec<_>>();

    for key in ready_keys {
        let Some(mut entry) = media_groups.remove(&key) else {
            continue;
        };

        match process_buffered_media_group(client, token, config, state, &entry).await {
            Ok(()) => {
                state.clear_update_failure(entry.last_update_id)?;
            }
            Err(error) => match handle_update_failure(
                client,
                token,
                state,
                entry.last_update_id,
                Some(entry.chat_id),
                &error,
            )
            .await?
            {
                UpdateFailureDisposition::Advance => {
                    continue;
                }
                UpdateFailureDisposition::Retry => {
                    entry.ready_at = Instant::now() + TELEGRAM_RETRY_BACKOFF;
                    media_groups.insert(key, entry);
                }
            },
        }
    }

    Ok(())
}

async fn process_buffered_media_group(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    entry: &BufferedMediaGroup,
) -> KaiResult<()> {
    let first_message = entry.messages.first().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("empty media group buffer for {}", entry.media_group_id),
        )
    })?;

    let validated = validate_inbound_message(client, token, config, state, first_message).await?;
    let Some(validated) = validated else {
        return Ok(());
    };

    for message in &entry.messages {
        if message.chat.kind != "private" || message.chat.id != validated.chat_id {
            return Err(KaiError::invalid_argument(
                "media group contains messages from multiple chats",
            ));
        }

        let sender_id =
            message.from.as_ref().map(|user| user.id).ok_or_else(|| {
                KaiError::invalid_argument("media group message is missing sender")
            })?;
        if sender_id != validated.sender_id {
            return Err(KaiError::invalid_argument(
                "media group contains messages from multiple senders",
            ));
        }
    }

    let text = merge_media_group_text(&entry.messages, &validated.text);
    let mut attachments = Vec::new();
    for message in &entry.messages {
        attachments
            .extend(download_message_attachments(client, token, config, state, message).await?);
    }

    if attachments.len() > MAX_ATTACHMENTS_PER_TURN {
        return Err(KaiError::invalid_argument(format!(
            "too many attachments in one message: max {MAX_ATTACHMENTS_PER_TURN}"
        )));
    }

    process_owner_turn(
        client,
        token,
        config,
        state,
        OwnerTurnInput {
            update_ids: &entry.update_ids,
            chat_id: validated.chat_id,
            sender_id: validated.sender_id,
            text: &text,
            attachments,
        },
    )
    .await
}

fn merge_media_group_text(messages: &[TelegramMessage], fallback_text: &str) -> String {
    let mut texts = messages
        .iter()
        .filter_map(|message| {
            message
                .caption
                .as_deref()
                .or(message.text.as_deref())
                .map(str::trim)
        })
        .filter(|text| !text.is_empty())
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if texts.is_empty() && !fallback_text.trim().is_empty() {
        texts.push(fallback_text.trim().to_string());
    }

    texts.dedup();
    texts.join("\n\n")
}

fn media_group_key(chat_id: i64, media_group_id: &str) -> String {
    format!("{chat_id}:{media_group_id}")
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

fn telegram_token(config: &LoadedConfig) -> KaiResult<String> {
    resolve_telegram_token(config)
}

async fn get_updates(
    client: &Client,
    token: &str,
    offset: i64,
    timeout_seconds: u64,
) -> KaiResult<Vec<TelegramUpdate>> {
    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getUpdates"))
        .query(&[
            ("offset", offset.to_string()),
            ("timeout", timeout_seconds.to_string()),
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
    let formatted = format_telegram_html(text);
    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&SendMessageRequest {
            chat_id,
            text: formatted.clone(),
            parse_mode: Some("HTML".to_string()),
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

    let description = payload
        .description
        .unwrap_or_else(|| "Telegram sendMessage failed".to_string());

    if is_telegram_html_parse_error(&description) {
        let plain_response = client
            .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
            .json(&SendMessageRequest {
                chat_id,
                text: text.to_string(),
                parse_mode: None,
            })
            .send()
            .await
            .map_err(http_error("send Telegram plain-text fallback"))?;

        let plain_payload = plain_response
            .json::<TelegramApiResponse<serde_json::Value>>()
            .await
            .map_err(http_error("decode Telegram plain-text fallback response"))?;

        if plain_payload.ok {
            return Ok(());
        }

        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            plain_payload
                .description
                .unwrap_or_else(|| "Telegram plain-text fallback failed".to_string()),
        ));
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

async fn send_typing_indicator(client: &Client, token: &str, chat_id: i64) -> KaiResult<()> {
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/sendChatAction"
        ))
        .json(&SendChatActionRequest {
            chat_id,
            action: "typing".to_string(),
        })
        .send()
        .await
        .map_err(http_error("send Telegram typing indicator"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram sendChatAction response"))?;

    if payload.ok {
        return Ok(());
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram sendChatAction failed".to_string()),
    ))
}

async fn download_message_attachments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    message: &TelegramMessage,
) -> KaiResult<Vec<AttachmentInfo>> {
    let requests = collect_download_requests(message);
    if requests.len() > MAX_ATTACHMENTS_PER_TURN {
        return Err(KaiError::invalid_argument(format!(
            "too many attachments in one message: max {MAX_ATTACHMENTS_PER_TURN}"
        )));
    }

    let mut attachments = Vec::new();
    for request in requests {
        attachments.push(download_file(client, token, config, state, request).await?);
    }

    Ok(attachments)
}

struct DownloadRequest {
    file_id: String,
    original_name: Option<String>,
    mime_type: Option<String>,
    kind: AttachmentKind,
    declared_size: Option<u64>,
    width: Option<u32>,
    height: Option<u32>,
    duration_secs: Option<u32>,
    media_group_id: Option<String>,
}

fn collect_download_requests(message: &TelegramMessage) -> Vec<DownloadRequest> {
    let mut requests = Vec::new();
    let media_group_id = message.media_group_id.clone();

    if let Some(document) = &message.document {
        requests.push(DownloadRequest {
            file_id: document.file_id.clone(),
            original_name: document.file_name.clone(),
            mime_type: document.mime_type.clone(),
            kind: classify_document_kind(
                document.file_name.as_deref(),
                document.mime_type.as_deref(),
            ),
            declared_size: document.file_size,
            width: None,
            height: None,
            duration_secs: None,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(photo) = &message.photo
        && let Some(best) = photo.iter().max_by_key(|item| item.file_size.unwrap_or(0))
    {
        requests.push(DownloadRequest {
            file_id: best.file_id.clone(),
            original_name: Some(format!("photo-{}.jpg", best.file_unique_id)),
            mime_type: Some("image/jpeg".to_string()),
            kind: AttachmentKind::Image,
            declared_size: best.file_size,
            width: Some(best.width),
            height: Some(best.height),
            duration_secs: None,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(audio) = &message.audio {
        requests.push(DownloadRequest {
            file_id: audio.file_id.clone(),
            original_name: audio.file_name.clone(),
            mime_type: audio.mime_type.clone(),
            kind: AttachmentKind::Audio,
            declared_size: audio.file_size,
            width: None,
            height: None,
            duration_secs: audio.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(voice) = &message.voice {
        requests.push(DownloadRequest {
            file_id: voice.file_id.clone(),
            original_name: Some(format!("voice-{}.ogg", voice.file_unique_id)),
            mime_type: voice.mime_type.clone().or(Some("audio/ogg".to_string())),
            kind: AttachmentKind::Voice,
            declared_size: voice.file_size,
            width: None,
            height: None,
            duration_secs: voice.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(video) = &message.video {
        requests.push(DownloadRequest {
            file_id: video.file_id.clone(),
            original_name: video.file_name.clone(),
            mime_type: video.mime_type.clone(),
            kind: AttachmentKind::Video,
            declared_size: video.file_size,
            width: video.width,
            height: video.height,
            duration_secs: video.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(video_note) = &message.video_note {
        requests.push(DownloadRequest {
            file_id: video_note.file_id.clone(),
            original_name: Some(format!("video-note-{}.mp4", video_note.file_unique_id)),
            mime_type: Some("video/mp4".to_string()),
            kind: AttachmentKind::Video,
            declared_size: video_note.file_size,
            width: video_note.width,
            height: video_note.height,
            duration_secs: video_note.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(animation) = &message.animation {
        requests.push(DownloadRequest {
            file_id: animation.file_id.clone(),
            original_name: animation.file_name.clone(),
            mime_type: animation.mime_type.clone(),
            kind: AttachmentKind::Animation,
            declared_size: animation.file_size,
            width: animation.width,
            height: animation.height,
            duration_secs: animation.duration,
            media_group_id,
        });
    }

    requests
}

async fn download_file(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    request: DownloadRequest,
) -> KaiResult<AttachmentInfo> {
    let DownloadRequest {
        file_id,
        original_name,
        mime_type,
        kind,
        declared_size,
        width,
        height,
        duration_secs,
        media_group_id,
    } = request;

    let byte_limit = attachment_byte_limit(kind);
    if let Some(size) = declared_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{} attachment exceeds limit: {size} bytes > {byte_limit}",
            kind.as_str()
        )));
    }

    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id.as_str())])
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

    if let Some(size) = file.file_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{} attachment exceeds limit: {size} bytes > {byte_limit}",
            kind.as_str()
        )));
    }

    let file_path = file.file_path.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram did not return a downloadable file path",
        )
    })?;

    let safe_name = sanitize_filename(original_name.as_deref().unwrap_or(&file_id));
    let storage_name = format!("{}-{}", Uuid::new_v4().simple(), safe_name);
    let local_path = state.paths().attachments_dir.join(storage_name);
    let partial_path = local_path.with_extension("part");

    let mut response = client
        .get(format!(
            "https://api.telegram.org/file/bot{token}/{file_path}"
        ))
        .send()
        .await
        .map_err(http_error("download Telegram file"))?;

    let mut file = File::create(&partial_path).await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to create attachment on disk: {error}"),
        )
    })?;

    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0_u64;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(http_error("read Telegram file body"))?
    {
        bytes += chunk.len() as u64;
        if bytes > byte_limit {
            let _ = fs::remove_file(&partial_path);
            return Err(KaiError::invalid_argument(format!(
                "{} attachment exceeds limit while downloading: {bytes} bytes > {byte_limit}",
                kind.as_str()
            )));
        }

        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to write attachment to disk: {error}"),
            )
        })?;
    }

    file.flush().await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to flush attachment to disk: {error}"),
        )
    })?;

    fs::rename(&partial_path, &local_path).map_err(|error| {
        let _ = fs::remove_file(&partial_path);
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to finalize attachment on disk: {error}"),
        )
    })?;

    let checksum_blake3 = hasher.finalize().to_hex().to_string();

    let mut attachment = AttachmentInfo {
        kind: kind.as_str().to_string(),
        path: local_path.display().to_string(),
        original_name,
        mime_type,
        bytes,
        checksum_blake3,
        media_group_id,
        duration_secs,
        width,
        height,
        transcript_text: None,
        transcript_segments: Vec::new(),
        artifacts: Vec::new(),
        notes: Vec::new(),
    };
    enrich_attachment(config, &mut attachment).await?;

    Ok(attachment)
}

fn sanitize_filename(input: &str) -> String {
    let mut output = input
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();

    if output.len() > 96 {
        output.truncate(96);
    }

    if output.is_empty() {
        output = "attachment".to_string();
    }

    output
}

fn is_telegram_html_parse_error(description: &str) -> bool {
    let normalized = description.to_ascii_lowercase();
    normalized.contains("can't parse entities")
        || normalized.contains("unsupported start tag")
        || normalized.contains("unexpected end tag")
        || normalized.contains("can't find end tag")
}

fn format_telegram_html(input: &str) -> String {
    let mut output = String::new();
    let mut list_stack = Vec::new();
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TASKLISTS);
    options.insert(Options::ENABLE_TABLES);

    for event in Parser::new_ext(input, options) {
        match event {
            Event::Start(tag) => match tag {
                Tag::Paragraph => {}
                Tag::Heading { .. } => output.push_str("<b>"),
                Tag::BlockQuote(_) => output.push_str("<blockquote>"),
                Tag::CodeBlock(kind) => {
                    if !output.ends_with('\n') && !output.is_empty() {
                        output.push('\n');
                    }
                    output.push_str("<pre>");
                    if let CodeBlockKind::Fenced(language) = kind
                        && !language.trim().is_empty()
                    {
                        output.push_str(&escape_html(language.as_ref()));
                        output.push('\n');
                    }
                }
                Tag::Strong => output.push_str("<b>"),
                Tag::Emphasis => output.push_str("<i>"),
                Tag::Strikethrough => output.push_str("<s>"),
                Tag::Link { dest_url, .. } => {
                    output.push_str("<a href=\"");
                    output.push_str(&escape_html_attr(dest_url.as_ref()));
                    output.push_str("\">");
                }
                Tag::List(start) => list_stack.push(ListKind::new(start)),
                Tag::Item => {
                    if !output.ends_with('\n') && !output.is_empty() {
                        output.push('\n');
                    }
                    let prefix = match list_stack.last_mut() {
                        Some(ListKind::Ordered(next)) => {
                            let current = *next;
                            *next += 1;
                            format!("{current}. ")
                        }
                        _ => "• ".to_string(),
                    };
                    output.push_str(&prefix);
                }
                _ => {}
            },
            Event::End(tag) => match tag {
                TagEnd::Paragraph => push_block_break(&mut output),
                TagEnd::Heading(_) => {
                    output.push_str("</b>");
                    push_block_break(&mut output);
                }
                TagEnd::BlockQuote(_) => {
                    output.push_str("</blockquote>");
                    push_block_break(&mut output);
                }
                TagEnd::CodeBlock => {
                    while output.ends_with('\n') {
                        output.pop();
                    }
                    output.push_str("</pre>");
                    push_block_break(&mut output);
                }
                TagEnd::Strong => output.push_str("</b>"),
                TagEnd::Emphasis => output.push_str("</i>"),
                TagEnd::Strikethrough => output.push_str("</s>"),
                TagEnd::Link => output.push_str("</a>"),
                TagEnd::List(_) => {
                    list_stack.pop();
                    push_block_break(&mut output);
                }
                TagEnd::Item => {}
                _ => {}
            },
            Event::Text(text) | Event::InlineHtml(text) | Event::Html(text) => {
                output.push_str(&escape_html(text.as_ref()));
            }
            Event::Code(text) => {
                output.push_str("<code>");
                output.push_str(&escape_html(text.as_ref()));
                output.push_str("</code>");
            }
            Event::SoftBreak | Event::HardBreak => output.push('\n'),
            Event::Rule => {
                if !output.ends_with('\n') && !output.is_empty() {
                    output.push('\n');
                }
                output.push_str("────────");
                push_block_break(&mut output);
            }
            Event::TaskListMarker(checked) => {
                output.push_str(if checked { "[x] " } else { "[ ] " });
            }
            Event::FootnoteReference(name) => {
                output.push('[');
                output.push_str(&escape_html(name.as_ref()));
                output.push(']');
            }
            _ => {}
        }
    }

    output.trim_end().to_string()
}

fn escape_html(input: &str) -> String {
    input
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn escape_html_attr(input: &str) -> String {
    escape_html(input).replace('"', "&quot;")
}

fn push_block_break(output: &mut String) {
    while output.ends_with('\n') {
        output.pop();
    }
    if !output.is_empty() {
        output.push('\n');
        output.push('\n');
    }
}

fn http_error(action: &'static str) -> impl Fn(reqwest::Error) -> KaiError {
    move |error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to {action}: {error}"),
        )
    }
}

fn record_runtime_error(
    state: &StateStore,
    event: &str,
    update_id: Option<i64>,
    chat_id: Option<i64>,
    error: &KaiError,
) -> KaiResult<()> {
    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": event,
        "updateId": update_id,
        "chatId": chat_id,
        "errorCode": error.code,
        "message": error.message,
        "hint": error.hint,
    }))
}

fn run_attachment_cleanup(state: &StateStore) -> KaiResult<()> {
    let result = state.cleanup_staged_attachments(ATTACHMENT_RETENTION)?;
    if result.removed_partial_files > 0 || result.removed_stale_files > 0 {
        state.append_audit_json(&serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": "telegram.attachment_cleanup",
            "scannedFiles": result.scanned_files,
            "removedPartialFiles": result.removed_partial_files,
            "removedStaleFiles": result.removed_stale_files,
        }))?;
    }
    Ok(())
}

enum UpdateFailureDisposition {
    Advance,
    Retry,
}

async fn handle_update_failure(
    client: &Client,
    token: &str,
    state: &StateStore,
    update_id: i64,
    chat_id: Option<i64>,
    error: &KaiError,
) -> KaiResult<UpdateFailureDisposition> {
    let failure = state.record_update_failure(update_id, error)?;
    let should_skip = should_skip_failed_update(error, failure.attempt_count);

    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "telegram.update_failed",
        "updateId": update_id,
        "chatId": chat_id,
        "errorCode": error.code,
        "message": error.message,
        "hint": error.hint,
        "attemptCount": failure.attempt_count,
        "skipUpdate": should_skip,
    }))?;

    if !should_skip {
        return Ok(UpdateFailureDisposition::Retry);
    }

    if let Some(chat_id) = chat_id {
        let notice = failure_notice_text(error, failure.attempt_count);
        if let Err(notice_error) = send_message(client, token, chat_id, &notice).await {
            record_runtime_error(
                state,
                "telegram.update_skip_notice_failed",
                Some(update_id),
                Some(chat_id),
                &notice_error,
            )?;
        }
    }

    state.clear_update_failure(update_id)?;
    Ok(UpdateFailureDisposition::Advance)
}

fn should_skip_failed_update(error: &KaiError, attempt_count: u32) -> bool {
    is_terminal_update_error(error) || attempt_count >= MAX_UPDATE_FAILURE_ATTEMPTS
}

fn is_terminal_update_error(error: &KaiError) -> bool {
    matches!(
        error.code,
        ErrorCode::InvalidArgument
            | ErrorCode::BlockedPrerequisite
            | ErrorCode::ConfigError
            | ErrorCode::ToolNotFound
    )
}

fn failure_notice_text(error: &KaiError, attempt_count: u32) -> String {
    if matches!(error.code, ErrorCode::InvalidArgument) {
        return format!("I couldn't handle that message: {}", error.message);
    }

    format!(
        "I hit an internal error while handling that message after {attempt_count} attempt(s). I skipped it so later messages can continue."
    )
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
    audio: Option<TelegramAudio>,
    voice: Option<TelegramVoice>,
    video: Option<TelegramVideo>,
    video_note: Option<TelegramVideoNote>,
    animation: Option<TelegramAnimation>,
    media_group_id: Option<String>,
}

#[derive(Debug, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
    file_unique_id: String,
    width: u32,
    height: u32,
    file_size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct TelegramAudio {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TelegramVoice {
    file_id: String,
    file_unique_id: String,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TelegramVideo {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TelegramVideoNote {
    file_id: String,
    file_unique_id: String,
    file_size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TelegramAnimation {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Deserialize)]
struct TelegramFile {
    file_path: Option<String>,
    file_size: Option<u64>,
}

#[derive(Debug, Serialize)]
struct SendMessageRequest {
    chat_id: i64,
    text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
struct SendChatActionRequest {
    chat_id: i64,
    action: String,
}

enum ListKind {
    Unordered,
    Ordered(u64),
}

impl ListKind {
    fn new(start: Option<u64>) -> Self {
        match start {
            Some(value) => Self::Ordered(value),
            None => Self::Unordered,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{failure_notice_text, format_telegram_html, should_skip_failed_update};
    use crate::error::{ErrorCode, KaiError};

    #[test]
    fn format_telegram_html_renders_inline_code_and_bold() {
        let input = "Use `rg` and **be precise**.";
        let output = format_telegram_html(input);
        assert_eq!(output, "Use <code>rg</code> and <b>be precise</b>.");
    }

    #[test]
    fn format_telegram_html_renders_fenced_code_block() {
        let input = "Example:\n```rust\nlet x = 1 < 2;\n```\nDone.";
        let output = format_telegram_html(input);
        assert_eq!(
            output,
            "Example:\n\n<pre>rust\nlet x = 1 &lt; 2;</pre>\n\nDone."
        );
    }

    #[test]
    fn format_telegram_html_escapes_raw_html() {
        let input = "<b>unsafe</b> `ok`";
        let output = format_telegram_html(input);
        assert_eq!(output, "&lt;b&gt;unsafe&lt;/b&gt; <code>ok</code>");
    }

    #[test]
    fn format_telegram_html_renders_lists_and_links() {
        let input = "- one\n- two\n\n[site](https://example.com)";
        let output = format_telegram_html(input);
        assert_eq!(
            output,
            "• one\n• two\n\n<a href=\"https://example.com\">site</a>"
        );
    }

    #[test]
    fn invalid_argument_updates_are_skipped_immediately() {
        let error = KaiError::invalid_argument("too many attachments");
        assert!(should_skip_failed_update(&error, 1));
        assert_eq!(
            failure_notice_text(&error, 1),
            "I couldn't handle that message: too many attachments"
        );
    }

    #[test]
    fn retryable_errors_are_skipped_after_threshold() {
        let error = KaiError::new(ErrorCode::RuntimeError, "temporary backend issue");
        assert!(!should_skip_failed_update(&error, 1));
        assert!(should_skip_failed_update(&error, 3));
    }
}
