use super::*;

mod files;
mod message;

pub(super) async fn send_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    message::send_message(client, token, chat_id, text).await
}

pub(super) async fn send_message_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<()> {
    message::send_message_with_retry(client, token, chat_id, text).await
}

pub(super) async fn send_message_chunk_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    message::send_message_chunk_with_retry(client, token, chat_id, text).await
}

pub(super) async fn send_status_message(
    client: &Client,
    token: &str,
    chat_id: i64,
    text: &str,
) -> KaiResult<i64> {
    message::send_status_message(client, token, chat_id, text).await
}

pub(super) async fn edit_message_text_with_retry(
    client: &Client,
    token: &str,
    chat_id: i64,
    message_id: i64,
    text: &str,
) -> KaiResult<()> {
    message::edit_message_text_with_retry(client, token, chat_id, message_id, text).await
}

pub(super) async fn send_typing_indicator(
    client: &Client,
    token: &str,
    chat_id: i64,
) -> KaiResult<()> {
    message::send_typing_indicator(client, token, chat_id).await
}

#[cfg(test)]
pub(super) fn should_retry_telegram_send(error: &KaiError) -> bool {
    message::should_retry_telegram_send(error)
}

pub(super) fn is_telegram_edit_target_lost(error: &KaiError) -> bool {
    message::is_telegram_edit_target_lost(error)
}

pub(super) async fn send_local_paths(
    client: &Client,
    token: &str,
    chat_id: i64,
    paths: &[PathBuf],
) -> KaiResult<usize> {
    files::send_local_paths(client, token, chat_id, paths).await
}

pub(super) fn resolve_requested_path(
    config: &LoadedConfig,
    state: &StateStore,
    raw: &str,
) -> KaiResult<PathBuf> {
    files::resolve_requested_path(config, state, raw)
}
