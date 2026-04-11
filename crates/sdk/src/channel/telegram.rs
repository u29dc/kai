use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::fs;
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, sleep};
use uuid::Uuid;

use crate::app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::secrets::resolve_telegram_token;
use crate::state::{AttachmentInfo, StateStore};

const TELEGRAM_RETRY_BACKOFF: Duration = Duration::from_secs(3);
const TELEGRAM_TYPING_REFRESH: Duration = Duration::from_secs(4);
const ATTACHMENT_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 30);
const ATTACHMENT_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60 * 6);
const MAX_UPDATE_FAILURE_ATTEMPTS: u32 = 3;
const MAX_ATTACHMENT_COUNT: usize = 3;
const MAX_IMAGE_BYTES: u64 = 10 * 1024 * 1024;
const MAX_PDF_BYTES: u64 = 20 * 1024 * 1024;
const MAX_TEXT_BYTES: u64 = 1024 * 1024;

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

    loop {
        if next_cleanup_at <= Instant::now() {
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

        let updates = match get_updates(&client, &token, offset).await {
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
    if message.chat.kind != "private" {
        return Ok(());
    }

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
    let owner_chat_id = state.get_owner_chat_id()?;

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
        return Ok(());
    };

    if sender_id != owner_id {
        return Ok(());
    }

    if let Some(expected_chat_id) = owner_chat_id {
        if chat_id != expected_chat_id {
            return Ok(());
        }
    } else {
        state.set_owner_chat_id(chat_id)?;
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
            state.clear_replay_package()?;
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

    if let Some(processed) = state.get_processed_update(update_id)? {
        send_message(client, token, chat_id, &processed.response_text).await?;
        return Ok(());
    }

    let typing_client = client.clone();
    let typing_token = token.to_string();
    let typing_handle = tokio::spawn(async move {
        loop {
            let _ = send_typing_indicator(&typing_client, &typing_token, chat_id).await;
            sleep(TELEGRAM_TYPING_REFRESH).await;
        }
    });

    let result = async {
        let attachments = download_attachments(client, token, state, &message).await?;
        if text.is_empty() && attachments.is_empty() {
            send_message(
                client,
                token,
                chat_id,
                "I need text or a supported attachment to do anything useful.",
            )
            .await?;
            return Ok(None);
        }

        let response =
            handle_owner_prompt(config, state, "telegram", sender_id, &text, &attachments)?;
        Ok(Some(response))
    }
    .await;

    typing_handle.abort();
    let Some(response) = result? else {
        return Ok(());
    };

    state.set_processed_update(
        update_id,
        &response,
        state.get_active_session_id()?.as_deref(),
    )?;
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
                DownloadRequest {
                    file_id: document.file_id.as_str(),
                    original_name: document.file_name.clone(),
                    mime_type: document.mime_type.clone(),
                    kind: determine_document_kind(document),
                    declared_size: document.file_size,
                },
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
                DownloadRequest {
                    file_id: best.file_id.as_str(),
                    original_name: Some(format!("photo-{}.jpg", best.file_unique_id)),
                    mime_type: Some("image/jpeg".to_string()),
                    kind: "image".to_string(),
                    declared_size: best.file_size,
                },
            )
            .await?,
        );
    }

    if attachments.len() > MAX_ATTACHMENT_COUNT {
        return Err(KaiError::invalid_argument(format!(
            "too many attachments in one message: max {MAX_ATTACHMENT_COUNT}"
        )));
    }

    Ok(attachments)
}

struct DownloadRequest<'a> {
    file_id: &'a str,
    original_name: Option<String>,
    mime_type: Option<String>,
    kind: String,
    declared_size: Option<u64>,
}

async fn download_file(
    client: &Client,
    token: &str,
    state: &StateStore,
    request: DownloadRequest<'_>,
) -> KaiResult<AttachmentInfo> {
    let DownloadRequest {
        file_id,
        original_name,
        mime_type,
        kind,
        declared_size,
    } = request;

    let byte_limit = attachment_byte_limit(&kind);
    if let Some(size) = declared_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{kind} attachment exceeds limit: {size} bytes > {byte_limit}"
        )));
    }

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

    if let Some(size) = file.file_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{kind} attachment exceeds limit: {size} bytes > {byte_limit}"
        )));
    }

    let file_path = file.file_path.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram did not return a downloadable file path",
        )
    })?;

    let safe_name = sanitize_filename(original_name.as_deref().unwrap_or(file_id));
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
                "{kind} attachment exceeds limit while downloading: {bytes} bytes > {byte_limit}"
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

    Ok(AttachmentInfo {
        kind,
        path: local_path.display().to_string(),
        original_name,
        mime_type,
        bytes,
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

fn attachment_byte_limit(kind: &str) -> u64 {
    match kind {
        "image" => MAX_IMAGE_BYTES,
        "pdf" => MAX_PDF_BYTES,
        "text" => MAX_TEXT_BYTES,
        _ => MAX_TEXT_BYTES,
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
    file_size: Option<u64>,
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
