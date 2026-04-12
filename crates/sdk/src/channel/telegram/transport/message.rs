use super::*;

pub(super) async fn send_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    send_message_with_retry(client, token, chat_id, text).await
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
        let mut last_error: Option<KaiError> = None;
        for attempt in 0..TELEGRAM_SEND_RETRY_ATTEMPTS {
            match send_message_chunk(client, token, chat_id, &chunk).await {
                Ok(()) => {
                    last_error = None;
                    break;
                }
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
        if let Some(error) = last_error {
            return Err(error);
        }
    }

    Ok(())
}

async fn send_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    let formatted = format_telegram_html(text);
    if formatted.chars().count() > TELEGRAM_TEXT_LIMIT {
        return send_plain_text_message_chunk(client, token, chat_id, text).await;
    }

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
        return send_plain_text_message_chunk(client, token, chat_id, text).await;
    }

    Err(KaiError::new(ErrorCode::RuntimeError, description))
}

async fn send_plain_text_message_chunk(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    let response = client
        .post(format!("https://api.telegram.org/bot{token}/sendMessage"))
        .json(&SendMessageRequest {
            chat_id,
            text: text.to_string(),
            parse_mode: None,
        })
        .send()
        .await
        .map_err(http_error("send Telegram plain-text message"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram plain-text response"))?;

    if payload.ok {
        return Ok(());
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        payload
            .description
            .unwrap_or_else(|| "Telegram plain-text sendMessage failed".to_string()),
    ))
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
