use super::*;

pub(super) fn telegram_token(config: &LoadedConfig) -> KaiResult<String> {
    resolve_telegram_token(config)
}

pub(super) async fn get_updates(
    client: &Client,
    token: &str,
    offset: i64,
    timeout_seconds: u64,
) -> KaiResult<Vec<TelegramUpdate>> {
    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getUpdates"))
        .timeout(Duration::from_secs(
            timeout_seconds + TELEGRAM_LONG_POLL_TIMEOUT_SLACK_SECS,
        ))
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

pub(super) fn http_error(action: &'static str) -> impl Fn(reqwest::Error) -> KaiError {
    move |error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to {action}: {error}"),
        )
    }
}
