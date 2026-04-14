use super::*;

pub(super) async fn send_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    send_message_with_retry(client, token, chat_id, text).await
}

pub(super) async fn send_status_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    send_status_message_with_retry(client, token, chat_id, text).await
}

pub(super) async fn send_message_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    if text.trim().is_empty() {
        return Ok(());
    }

    for chunk in split_response_text(text) {
        let _ = send_message_chunk_with_retry(client, token, chat_id, &chunk).await?;
    }

    Ok(())
}

pub(super) async fn send_message_chunk_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let mut last_error: Option<KaiError> = None;
    for attempt in 0..TELEGRAM_SEND_RETRY_ATTEMPTS {
        match send_message_chunk(client, token, chat_id, text).await {
            Ok(message_id) => return Ok(message_id),
            Err(error) if should_retry_telegram_send(&error) => {
                last_error = Some(error);
                if attempt + 1 < TELEGRAM_SEND_RETRY_ATTEMPTS {
                    sleep(TELEGRAM_SEND_RETRY_BACKOFF).await;
                    continue;
                }
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error
        .unwrap_or_else(|| KaiError::new(ErrorCode::RuntimeError, "Telegram sendMessage failed")))
}

pub(super) async fn send_status_message_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let text = truncate_single_message_text(text);
    if text.trim().is_empty() {
        return Err(KaiError::invalid_argument(
            "Telegram status messages must not be empty",
        ));
    }

    let mut last_error: Option<KaiError> = None;
    for attempt in 0..TELEGRAM_SEND_RETRY_ATTEMPTS {
        match send_status_message_chunk(client, token, chat_id, &text).await {
            Ok(message_id) => return Ok(message_id),
            Err(error) if should_retry_telegram_send(&error) => {
                last_error = Some(error);
                if attempt + 1 < TELEGRAM_SEND_RETRY_ATTEMPTS {
                    sleep(TELEGRAM_SEND_RETRY_BACKOFF).await;
                    continue;
                }
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram status message send failed",
        )
    }))
}

pub(super) async fn edit_message_text_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> KaiResult<()> {
    let text = truncate_single_message_text(text);
    if text.trim().is_empty() {
        return Ok(());
    }

    let mut last_error: Option<KaiError> = None;
    for attempt in 0..TELEGRAM_SEND_RETRY_ATTEMPTS {
        match edit_message_text(client, token, chat_id, message_id, &text).await {
            Ok(()) => return Ok(()),
            Err(error) if should_retry_telegram_send(&error) => {
                last_error = Some(error);
                if attempt + 1 < TELEGRAM_SEND_RETRY_ATTEMPTS {
                    sleep(TELEGRAM_SEND_RETRY_BACKOFF).await;
                    continue;
                }
            }
            Err(error) => return Err(error),
        }
    }

    Err(last_error.unwrap_or_else(|| {
        KaiError::new(ErrorCode::RuntimeError, "Telegram editMessageText failed")
    }))
}

async fn send_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let formatted = format_telegram_html(text);
    if formatted.chars().count() > TELEGRAM_TEXT_LIMIT {
        return send_plain_text_message_chunk(client, token, chat_id, text).await;
    }

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&SendMessageRequest {
            chat_id,
            text: formatted.clone(),
            parse_mode: Some("HTML".to_string()),
        })
        .send()
        .await
        .map_err(http_error("send Telegram message"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramSentMessage>>()
        .await
        .map_err(http_error("decode Telegram sendMessage response"))?;

    if payload.ok {
        return payload
            .result
            .map(|message| message.message_id)
            .ok_or_else(|| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    "Telegram message send was missing a message id",
                )
            });
    }

    let description = payload
        .description
        .unwrap_or_else(|| "Telegram sendMessage failed".to_string());

    if is_telegram_html_parse_error(&description) {
        return send_plain_text_message_chunk(client, token, chat_id, text).await;
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

async fn send_status_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let formatted = format_telegram_html(text);
    if formatted.chars().count() > TELEGRAM_TEXT_LIMIT {
        return send_plain_status_message_chunk(client, token, chat_id, text).await;
    }

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&SendMessageRequest {
            chat_id,
            text: formatted.clone(),
            parse_mode: Some("HTML".to_string()),
        })
        .send()
        .await
        .map_err(http_error("send Telegram status message"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramSentMessage>>()
        .await
        .map_err(http_error("decode Telegram status response"))?;

    if payload.ok {
        return payload
            .result
            .map(|message| message.message_id)
            .ok_or_else(|| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    "Telegram status message was missing a message id",
                )
            });
    }

    let description = payload
        .description
        .unwrap_or_else(|| "Telegram sendMessage failed".to_string());

    if is_telegram_html_parse_error(&description) {
        return send_plain_status_message_chunk(client, token, chat_id, text).await;
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

async fn send_plain_text_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&SendMessageRequest {
            chat_id,
            text: text.to_string(),
            parse_mode: None,
        })
        .send()
        .await
        .map_err(http_error("send Telegram plain-text message"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramSentMessage>>()
        .await
        .map_err(http_error("decode Telegram plain-text response"))?;

    if payload.ok {
        return payload
            .result
            .map(|message| message.message_id)
            .ok_or_else(|| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    "Telegram plain-text send was missing a message id",
                )
            });
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram plain-text sendMessage failed".to_string()),
    ))
}

async fn send_plain_status_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&SendMessageRequest {
            chat_id,
            text: text.to_string(),
            parse_mode: None,
        })
        .send()
        .await
        .map_err(http_error("send Telegram plain-text status message"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramSentMessage>>()
        .await
        .map_err(http_error("decode Telegram plain-text status response"))?;

    if payload.ok {
        return payload
            .result
            .map(|message| message.message_id)
            .ok_or_else(|| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    "Telegram status message was missing a message id",
                )
            });
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram plain-text status sendMessage failed".to_string()),
    ))
}

async fn edit_message_text(
    client: &Client,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> KaiResult<()> {
    let formatted = format_telegram_html(text);
    if formatted.chars().count() > TELEGRAM_TEXT_LIMIT {
        return edit_plain_text_message(client, token, chat_id, message_id, text).await;
    }

    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/editMessageText"
        ))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&EditMessageTextRequest {
            chat_id,
            message_id,
            text: formatted.clone(),
            parse_mode: Some("HTML".to_string()),
        })
        .send()
        .await
        .map_err(http_error("edit Telegram message"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram editMessageText response"))?;

    if payload.ok {
        return Ok(());
    }

    let description = payload
        .description
        .unwrap_or_else(|| "Telegram editMessageText failed".to_string());
    if is_telegram_message_not_modified(&description) {
        return Ok(());
    }
    if is_telegram_html_parse_error(&description) {
        return edit_plain_text_message(client, token, chat_id, message_id, text).await;
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

async fn edit_plain_text_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> KaiResult<()> {
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/editMessageText"
        ))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
        .json(&EditMessageTextRequest {
            chat_id,
            message_id,
            text: text.to_string(),
            parse_mode: None,
        })
        .send()
        .await
        .map_err(http_error("edit Telegram plain-text message"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error(
            "decode Telegram plain-text editMessageText response",
        ))?;

    if payload.ok {
        return Ok(());
    }

    let description = payload
        .description
        .unwrap_or_else(|| "Telegram plain-text editMessageText failed".to_string());
    if is_telegram_message_not_modified(&description) {
        return Ok(());
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

pub(super) async fn send_typing_indicator(
    client: &Client,
    token: &str,
    chat_id: i64,
) -> KaiResult<()> {
    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/sendChatAction"
        ))
        .timeout(TELEGRAM_API_REQUEST_TIMEOUT)
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

fn is_telegram_html_parse_error(description: &str) -> bool {
    let normalized = description.to_ascii_lowercase();
    normalized.contains("can't parse entities")
        || normalized.contains("unsupported start tag")
        || normalized.contains("unexpected end tag")
        || normalized.contains("can't find end tag")
}

fn is_telegram_message_not_modified(description: &str) -> bool {
    description
        .to_ascii_lowercase()
        .contains("message is not modified")
}

pub(super) fn is_telegram_edit_target_lost(error: &KaiError) -> bool {
    let mut haystacks = vec![error.message.to_ascii_lowercase()];
    if let Some(hint) = &error.hint {
        haystacks.push(hint.to_ascii_lowercase());
    }

    haystacks.iter().any(|value| {
        value.contains("message to edit not found")
            || value.contains("message can't be edited")
            || value.contains("message can not be edited")
    })
}

fn truncate_single_message_text(text: &str) -> String {
    const SINGLE_MESSAGE_LIMIT: usize = 3800;
    let mut output = String::new();
    for (count, ch) in text.chars().enumerate() {
        if count >= SINGLE_MESSAGE_LIMIT {
            break;
        }
        output.push(ch);
    }
    output.trim().to_string()
}

pub(super) fn should_retry_telegram_send(error: &KaiError) -> bool {
    let mut haystacks = vec![error.message.to_ascii_lowercase()];
    if let Some(hint) = &error.hint {
        haystacks.push(hint.to_ascii_lowercase());
    }

    haystacks.iter().any(|value| {
        value.contains("timeout")
            || value.contains("timed out")
            || value.contains("connection")
            || value.contains("connect")
            || value.contains("dns")
            || value.contains("socket")
            || value.contains("temporar")
            || value.contains("too many requests")
            || value.contains("429")
            || value.contains("502")
            || value.contains("503")
            || value.contains("504")
    })
}
