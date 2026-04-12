use pulldown_cmark::{CodeBlockKind, Event, Options, Parser, Tag, TagEnd};
use reqwest::{Client, multipart};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tokio::fs::File;
use tokio::io::AsyncWriteExt;
use tokio::time::{Instant, sleep};
use tokio_util::io::ReaderStream;
use uuid::Uuid;

use crate::app::{mobile_help_text, mobile_status_text};
use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::media::{
    ATTACHMENT_CLEANUP_INTERVAL, ATTACHMENT_RETENTION, AttachmentKind, MAX_ATTACHMENTS_PER_TURN,
    MAX_MEDIA_GROUP_ITEMS, MEDIA_GROUP_DEBOUNCE, attachment_byte_limit, classify_document_kind,
    enrich_attachment,
};
use crate::runtime::codex::{
    AsyncCodexTurnResult, ResumeFailure, RunningCodexTurn, cancel_codex_turn,
    create_replay_package, poll_running_codex_turn, prepare_codex_turn, start_codex_turn,
};
use crate::secrets::resolve_telegram_token;
use crate::state::{AttachmentInfo, NewTurn, PendingTurn, StateStore};

const TELEGRAM_RETRY_BACKOFF: Duration = Duration::from_secs(3);
const TELEGRAM_TYPING_REFRESH: Duration = Duration::from_secs(4);
const MAX_UPDATE_FAILURE_ATTEMPTS: u32 = 3;
const TELEGRAM_TEXT_LIMIT: usize = 4096;
const MAX_OUTBOUND_ATTACHMENTS_PER_REPLY: usize = 4;
const TELEGRAM_SEND_RETRY_ATTEMPTS: u32 = 3;
const TELEGRAM_SEND_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const TEXT_FRAGMENT_START_THRESHOLD_CHARS: usize = 3500;
const TEXT_FRAGMENT_MAX_TOTAL_CHARS: usize = 24_000;
const TEXT_FRAGMENT_MAX_PARTS: usize = 6;
const TEXT_FRAGMENT_MAX_ID_GAP: i64 = 5;
const TEXT_FRAGMENT_MAX_GAP: Duration = Duration::from_millis(1400);
const ACTIVE_TURN_STATE_KEY: &str = "telegram.active_turn";
const BUFFERED_MEDIA_GROUPS_STATE_KEY: &str = "telegram.buffered_media_groups";
const BUFFERED_TEXT_FRAGMENTS_STATE_KEY: &str = "telegram.buffered_text_fragments";
const PENDING_REPLY_DELIVERIES_STATE_KEY: &str = "telegram.pending_reply_deliveries";
const PROCESSED_UPDATE_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 14);
const UPDATE_FAILURE_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 7);
const MAX_TURN_ROWS: usize = 5000;
const MAX_AUDIT_LOG_BYTES: u64 = 5 * 1024 * 1024;

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
    let mut media_groups = load_buffered_media_groups(state)?;
    let mut text_fragments = load_buffered_text_fragments(state)?;
    let mut active_turn: Option<ActiveOwnerTurn> = None;
    let mut synced_menu_chat_id: Option<i64> = None;

    recover_active_turn(state)?;

    loop {
        let now = Instant::now();

        if next_cleanup_at <= now {
            if let Err(error) = run_housekeeping(state) {
                record_runtime_error(state, "telegram.housekeeping_failed", None, None, &error)?;
                eprintln!("kai housekeeping failed: {}", error.message);
            }
            next_cleanup_at = Instant::now() + ATTACHMENT_CLEANUP_INTERVAL;
        }

        let owner_chat_id = state.get_owner_chat_id()?;

        if let Err(error) = flush_pending_reply_deliveries(&client, &token, state).await {
            record_runtime_error(
                state,
                "telegram.pending_reply_delivery_failed",
                None,
                owner_chat_id,
                &error,
            )?;
            eprintln!("kai pending reply delivery failed: {}", error.message);
        }

        if owner_chat_id != synced_menu_chat_id {
            if let Some(chat_id) = owner_chat_id {
                if let Err(error) =
                    sync_command_menu_if_needed(&client, &token, state, chat_id).await
                {
                    record_runtime_error(
                        state,
                        "telegram.command_menu_sync_failed",
                        Some(chat_id),
                        owner_chat_id,
                        &error,
                    )?;
                    eprintln!("kai telegram command menu sync failed: {}", error.message);
                } else {
                    synced_menu_chat_id = Some(chat_id);
                }
            } else {
                synced_menu_chat_id = None;
            }
        }

        if let Some(turn) = active_turn.as_mut() {
            if now >= turn.next_typing_at {
                let _ = send_typing_indicator(&client, &token, turn.pending.chat_id).await;
                turn.next_typing_at = Instant::now() + TELEGRAM_TYPING_REFRESH;
            }

            if let Some(result) = poll_running_codex_turn(&mut turn.running) {
                finish_active_turn(&client, &token, config, state, turn, result).await?;
                active_turn = None;
            }
        }

        flush_ready_text_fragments(
            &client,
            &token,
            config,
            state,
            &mut text_fragments,
            &mut active_turn,
            None,
        )
        .await?;
        flush_ready_media_groups(
            &client,
            &token,
            config,
            state,
            &mut media_groups,
            &mut active_turn,
            None,
        )
        .await?;

        if active_turn.is_none() {
            maybe_start_next_pending_turn(config, state, &mut active_turn).await?;
        }

        let has_pending_queue = state.pending_turn_queue_len()? > 0;
        let poll_timeout_seconds = if active_turn.is_some()
            || has_pending_queue
            || !media_groups.is_empty()
            || !text_fragments.is_empty()
        {
            1
        } else {
            30
        };

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
                if should_ignore_message_before_buffering(config, state, &message)? {
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                    state.clear_update_failure(update.update_id)?;
                    continue;
                }

                if let Some(media_group_id) = message.media_group_id.clone() {
                    buffer_media_group(
                        &mut media_groups,
                        update.update_id,
                        message,
                        &media_group_id,
                    );
                    persist_media_groups(state, &media_groups)?;
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                    state.clear_update_failure(update.update_id)?;
                    continue;
                }

                if should_buffer_text_fragments(&message) {
                    if let Some(flushed) =
                        buffer_text_fragments(&mut text_fragments, update.update_id, message)
                    {
                        persist_text_fragments(state, &text_fragments)?;
                        if let Err(error) = process_buffered_text_fragments(
                            &client,
                            &token,
                            config,
                            state,
                            &mut active_turn,
                            &flushed,
                        )
                        .await
                        {
                            let failure_update_id = flushed
                                .update_ids
                                .last()
                                .copied()
                                .unwrap_or(flushed.last_update_id);
                            match handle_update_failure(
                                &client,
                                &token,
                                state,
                                failure_update_id,
                                Some(flushed.chat_id),
                                &error,
                            )
                            .await?
                            {
                                UpdateFailureDisposition::Advance => {}
                                UpdateFailureDisposition::Retry => {
                                    let mut retry_entry = flushed;
                                    retry_entry.ready_at = Instant::now() + TELEGRAM_RETRY_BACKOFF;
                                    text_fragments.insert(
                                        format!(
                                            "{}:{}",
                                            retry_entry.chat_id, retry_entry.sender_id
                                        ),
                                        retry_entry,
                                    );
                                    persist_text_fragments(state, &text_fragments)?;
                                }
                            }
                        }
                    } else {
                        persist_text_fragments(state, &text_fragments)?;
                    }
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                    state.clear_update_failure(update.update_id)?;
                    continue;
                }

                flush_ready_text_fragments(
                    &client,
                    &token,
                    config,
                    state,
                    &mut text_fragments,
                    &mut active_turn,
                    Some(update.update_id),
                )
                .await?;
                flush_ready_media_groups(
                    &client,
                    &token,
                    config,
                    state,
                    &mut media_groups,
                    &mut active_turn,
                    Some(update.update_id),
                )
                .await?;

                handle_message(
                    &client,
                    &token,
                    config,
                    state,
                    &mut active_turn,
                    update.update_id,
                    message,
                )
                .await
            } else {
                Ok(())
            };

            match outcome {
                Ok(()) => {
                    state.clear_update_failure(update.update_id)?;
                    offset = next_offset;
                    state.set_update_offset(offset)?;
                }
                Err(error) => match handle_update_failure(
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
                },
            }

            if active_turn.is_none() {
                maybe_start_next_pending_turn(config, state, &mut active_turn).await?;
            }
        }

        flush_ready_text_fragments(
            &client,
            &token,
            config,
            state,
            &mut text_fragments,
            &mut active_turn,
            None,
        )
        .await?;
        flush_ready_media_groups(
            &client,
            &token,
            config,
            state,
            &mut media_groups,
            &mut active_turn,
            None,
        )
        .await?;
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
            id: Uuid::new_v4().to_string(),
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

#[derive(Debug)]
struct BufferedTextFragments {
    chat_id: i64,
    sender_id: i64,
    last_update_id: i64,
    ready_at: Instant,
    update_ids: Vec<i64>,
    messages: Vec<TelegramMessage>,
}

#[derive(Debug)]
struct ActiveOwnerTurn {
    pending: PendingTurn,
    running: RunningCodexTurn,
    cancel_requested: bool,
    next_typing_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedBufferedMediaGroup {
    media_group_id: String,
    chat_id: i64,
    last_update_id: i64,
    update_ids: Vec<i64>,
    messages: Vec<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PersistedBufferedTextFragments {
    chat_id: i64,
    sender_id: i64,
    last_update_id: i64,
    update_ids: Vec<i64>,
    messages: Vec<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct PendingReplyDelivery {
    delivery_id: String,
    turn_id: String,
    chat_id: i64,
    response_text: String,
    codex_session_id: String,
    update_ids: Vec<i64>,
    attempts: u32,
    created_at: String,
}

fn load_buffered_media_groups(
    state: &StateStore,
) -> KaiResult<HashMap<String, BufferedMediaGroup>> {
    let persisted = state
        .load_json_state::<Vec<PersistedBufferedMediaGroup>>(BUFFERED_MEDIA_GROUPS_STATE_KEY)?
        .unwrap_or_default();
    let mut groups = HashMap::new();
    for entry in persisted {
        groups.insert(
            media_group_key(entry.chat_id, &entry.media_group_id),
            BufferedMediaGroup {
                media_group_id: entry.media_group_id,
                chat_id: entry.chat_id,
                last_update_id: entry.last_update_id,
                ready_at: Instant::now(),
                update_ids: entry.update_ids,
                messages: entry.messages,
            },
        );
    }
    Ok(groups)
}

fn persist_media_groups(
    state: &StateStore,
    media_groups: &HashMap<String, BufferedMediaGroup>,
) -> KaiResult<()> {
    if media_groups.is_empty() {
        return state.remove_json_state(BUFFERED_MEDIA_GROUPS_STATE_KEY);
    }

    let persisted = media_groups
        .values()
        .map(|entry| PersistedBufferedMediaGroup {
            media_group_id: entry.media_group_id.clone(),
            chat_id: entry.chat_id,
            last_update_id: entry.last_update_id,
            update_ids: entry.update_ids.clone(),
            messages: entry.messages.clone(),
        })
        .collect::<Vec<_>>();
    state.store_json_state(BUFFERED_MEDIA_GROUPS_STATE_KEY, &persisted)
}

fn load_buffered_text_fragments(
    state: &StateStore,
) -> KaiResult<HashMap<String, BufferedTextFragments>> {
    let persisted = state
        .load_json_state::<Vec<PersistedBufferedTextFragments>>(BUFFERED_TEXT_FRAGMENTS_STATE_KEY)?
        .unwrap_or_default();
    let mut buffers = HashMap::new();
    for entry in persisted {
        buffers.insert(
            format!("{}:{}", entry.chat_id, entry.sender_id),
            BufferedTextFragments {
                chat_id: entry.chat_id,
                sender_id: entry.sender_id,
                last_update_id: entry.last_update_id,
                ready_at: Instant::now(),
                update_ids: entry.update_ids,
                messages: entry.messages,
            },
        );
    }
    Ok(buffers)
}

fn persist_text_fragments(
    state: &StateStore,
    buffers: &HashMap<String, BufferedTextFragments>,
) -> KaiResult<()> {
    if buffers.is_empty() {
        return state.remove_json_state(BUFFERED_TEXT_FRAGMENTS_STATE_KEY);
    }

    let persisted = buffers
        .values()
        .map(|entry| PersistedBufferedTextFragments {
            chat_id: entry.chat_id,
            sender_id: entry.sender_id,
            last_update_id: entry.last_update_id,
            update_ids: entry.update_ids.clone(),
            messages: entry.messages.clone(),
        })
        .collect::<Vec<_>>();
    state.store_json_state(BUFFERED_TEXT_FRAGMENTS_STATE_KEY, &persisted)
}

fn recover_active_turn(state: &StateStore) -> KaiResult<()> {
    let Some(active) = state.load_json_state::<PendingTurn>(ACTIVE_TURN_STATE_KEY)? else {
        return Ok(());
    };

    if !state
        .pending_turn_queue()?
        .iter()
        .any(|queued| queued.id == active.id)
    {
        state.prepend_pending_turn(&active)?;
    }
    state.remove_json_state(ACTIVE_TURN_STATE_KEY)
}

fn run_housekeeping(state: &StateStore) -> KaiResult<()> {
    let attachment_result = state.cleanup_staged_attachments(ATTACHMENT_RETENTION)?;
    let state_result = state.cleanup_runtime_state(
        PROCESSED_UPDATE_RETENTION,
        UPDATE_FAILURE_RETENTION,
        MAX_TURN_ROWS,
        MAX_AUDIT_LOG_BYTES,
    )?;

    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "telegram.housekeeping",
        "attachments": attachment_result,
        "state": state_result,
    }))?;
    Ok(())
}

fn pending_reply_deliveries(state: &StateStore) -> KaiResult<Vec<PendingReplyDelivery>> {
    Ok(state
        .load_json_state::<Vec<PendingReplyDelivery>>(PENDING_REPLY_DELIVERIES_STATE_KEY)?
        .unwrap_or_default())
}

fn store_pending_reply_deliveries(
    state: &StateStore,
    deliveries: &[PendingReplyDelivery],
) -> KaiResult<()> {
    if deliveries.is_empty() {
        return state.remove_json_state(PENDING_REPLY_DELIVERIES_STATE_KEY);
    }
    state.store_json_state(PENDING_REPLY_DELIVERIES_STATE_KEY, deliveries)
}

fn enqueue_pending_reply_delivery(
    state: &StateStore,
    delivery: PendingReplyDelivery,
) -> KaiResult<()> {
    let mut deliveries = pending_reply_deliveries(state)?;
    deliveries.push(delivery);
    store_pending_reply_deliveries(state, &deliveries)
}

async fn flush_pending_reply_deliveries(
    client: &Client,
    token: &str,
    state: &StateStore,
) -> KaiResult<()> {
    let mut deliveries = pending_reply_deliveries(state)?;
    if deliveries.is_empty() {
        return Ok(());
    }

    let mut remaining = Vec::new();
    for mut delivery in deliveries.drain(..) {
        match send_message_with_retry(client, token, delivery.chat_id, &delivery.response_text)
            .await
        {
            Ok(()) => {
                for update_id in &delivery.update_ids {
                    state.set_processed_update(
                        *update_id,
                        &delivery.response_text,
                        Some(&delivery.codex_session_id),
                    )?;
                }
                state.append_audit_json(&serde_json::json!({
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "event": "telegram.reply_delivered",
                    "deliveryId": delivery.delivery_id,
                    "turnId": delivery.turn_id,
                    "chatId": delivery.chat_id,
                    "attempts": delivery.attempts + 1,
                    "updateIds": delivery.update_ids,
                }))?;
            }
            Err(error) => {
                delivery.attempts = delivery.attempts.saturating_add(1);
                state.append_audit_json(&serde_json::json!({
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "event": "telegram.reply_delivery_retry",
                    "deliveryId": delivery.delivery_id,
                    "turnId": delivery.turn_id,
                    "chatId": delivery.chat_id,
                    "attempts": delivery.attempts,
                    "message": error.message,
                    "hint": error.hint,
                }))?;
                remaining.push(delivery);
            }
        }
    }

    store_pending_reply_deliveries(state, &remaining)
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

    Ok(Some(ValidatedInbound {
        chat_id,
        sender_id,
        text,
    }))
}

fn should_ignore_message_before_buffering(
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

#[derive(Debug)]
enum MobileCommand {
    Help,
    Status,
    Reset,
    Cancel,
    Send { path: String },
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

async fn enqueue_owner_turn(
    client: &Client,
    token: &str,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
    pending: PendingTurn,
) -> KaiResult<()> {
    let position = state.enqueue_pending_turn(&pending)?;
    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "telegram.turn_queued",
        "turnId": pending.id,
        "chatId": pending.chat_id,
        "senderId": pending.sender_id,
        "queuePosition": position,
        "attachmentCount": pending.attachments.len(),
    }))?;

    if (active_turn.is_some() || position > 1)
        && let Err(error) = send_message(
            client,
            token,
            pending.chat_id,
            &format!("Queued. Position {}.", position),
        )
        .await
    {
        state.append_audit_json(&serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": "telegram.queue_notice_failed",
            "turnId": pending.id,
            "chatId": pending.chat_id,
            "message": error.message,
            "hint": error.hint,
        }))?;
    }

    Ok(())
}

async fn maybe_start_next_pending_turn(
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
) -> KaiResult<()> {
    if active_turn.is_some() {
        return Ok(());
    }

    let Some(pending) = state.pop_pending_turn()? else {
        return Ok(());
    };
    state.store_json_state(ACTIVE_TURN_STATE_KEY, &pending)?;

    if pending.text.is_empty() && pending.attachments.is_empty() {
        state.remove_json_state(ACTIVE_TURN_STATE_KEY)?;
        return Ok(());
    }

    let prepared = prepare_codex_turn(
        config,
        state,
        &pending.channel,
        pending.sender_id,
        &pending.text,
        &pending.attachments,
    )
    .inspect_err(|_| {
        let _ = state.remove_json_state(ACTIVE_TURN_STATE_KEY);
        let _ = state.prepend_pending_turn(&pending);
    })?;

    let running = match start_codex_turn(config.clone(), prepared).await {
        Ok(running) => running,
        Err(error) => {
            let _ = state.remove_json_state(ACTIVE_TURN_STATE_KEY);
            let _ = state.prepend_pending_turn(&pending);
            return Err(error);
        }
    };
    state.record_turn(NewTurn {
        role: "user",
        channel: &pending.channel,
        sender_id: Some(pending.sender_id),
        text: &pending.text,
        codex_session_id: state.get_active_session_id()?.as_deref(),
        outcome_status: Some("received"),
        attachments: &pending.attachments,
    })?;
    *active_turn = Some(ActiveOwnerTurn {
        pending,
        running,
        cancel_requested: false,
        next_typing_at: Instant::now(),
    });
    Ok(())
}

async fn finish_active_turn(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &ActiveOwnerTurn,
    result: KaiResult<AsyncCodexTurnResult>,
) -> KaiResult<()> {
    match result {
        Ok(async_result) => {
            finalize_successful_turn(client, token, config, state, active_turn, async_result)
                .await?;
            state.remove_json_state(ACTIVE_TURN_STATE_KEY)
        }
        Err(error) => {
            state.remove_json_state(ACTIVE_TURN_STATE_KEY)?;
            if active_turn.cancel_requested {
                state.append_audit_json(&serde_json::json!({
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "event": "turn.cancelled",
                    "turnId": active_turn.pending.id,
                    "chatId": active_turn.pending.chat_id,
                    "senderId": active_turn.pending.sender_id,
                    "message": error.message,
                    "hint": error.hint,
                }))?;
                return Ok(());
            }

            record_runtime_error(
                state,
                "telegram.turn_failed",
                Some(active_turn.pending.chat_id),
                Some(active_turn.pending.sender_id),
                &error,
            )?;
            if let Err(notice_error) = send_message_with_retry(
                client,
                token,
                active_turn.pending.chat_id,
                &format!(
                    "I hit an internal error while running that turn: {}",
                    error.message
                ),
            )
            .await
            {
                state.append_audit_json(&serde_json::json!({
                    "timestamp": chrono::Utc::now().to_rfc3339(),
                    "event": "telegram.turn_failure_notice_failed",
                    "turnId": active_turn.pending.id,
                    "chatId": active_turn.pending.chat_id,
                    "message": notice_error.message,
                    "hint": notice_error.hint,
                }))?;
            }
            Ok(())
        }
    }
}

async fn finalize_successful_turn(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &ActiveOwnerTurn,
    async_result: AsyncCodexTurnResult,
) -> KaiResult<()> {
    if let Some(ResumeFailure {
        requested_session_id,
        stale_session,
        error,
    }) = async_result.resume_failure
    {
        state.append_audit_json(&serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": "codex.resume_failed",
            "requestedSessionId": requested_session_id,
            "staleSession": stale_session,
            "message": error.message,
            "hint": error.hint,
        }))?;
    }

    let result = async_result.result;
    state.set_active_session_id(&result.session_id)?;
    state.record_turn(NewTurn {
        role: "assistant",
        channel: &active_turn.pending.channel,
        sender_id: None,
        text: &result.response_text,
        codex_session_id: Some(&result.session_id),
        outcome_status: Some(if result.resumed { "resumed" } else { "fresh" }),
        attachments: &[],
    })?;

    let replay_package = create_replay_package(&result.context_snapshots, &state.recent_turns(24)?);
    state.set_replay_package(&replay_package)?;

    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "turn.completed",
        "turnId": active_turn.pending.id,
        "channel": active_turn.pending.channel,
        "senderId": active_turn.pending.sender_id,
        "codexSessionId": result.session_id,
        "resumed": result.resumed,
        "attachmentCount": active_turn.pending.attachments.len(),
    }))?;

    enqueue_pending_reply_delivery(
        state,
        PendingReplyDelivery {
            delivery_id: Uuid::new_v4().to_string(),
            turn_id: active_turn.pending.id.clone(),
            chat_id: active_turn.pending.chat_id,
            response_text: result.response_text.clone(),
            codex_session_id: result.session_id.clone(),
            update_ids: active_turn.pending.update_ids.clone(),
            attempts: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
        },
    )?;

    let _ = config;
    flush_pending_reply_deliveries(client, token, state).await?;

    Ok(())
}

fn should_buffer_text_fragments(message: &TelegramMessage) -> bool {
    let Some(text) = message.text.as_deref() else {
        return false;
    };

    let trimmed = text.trim();
    !trimmed.starts_with('/') && trimmed.chars().count() >= TEXT_FRAGMENT_START_THRESHOLD_CHARS
}

fn buffer_text_fragments(
    buffers: &mut HashMap<String, BufferedTextFragments>,
    update_id: i64,
    message: TelegramMessage,
) -> Option<BufferedTextFragments> {
    let sender_id = message.from.as_ref().map(|user| user.id)?;
    let key = format!("{}:{sender_id}", message.chat.id);
    let mut flushed = None;
    let entry = buffers.entry(key).or_insert_with(|| BufferedTextFragments {
        chat_id: message.chat.id,
        sender_id,
        last_update_id: update_id,
        ready_at: Instant::now() + TEXT_FRAGMENT_MAX_GAP,
        update_ids: Vec::new(),
        messages: Vec::new(),
    });

    if let Some(last_message) = entry.messages.last() {
        let id_gap = update_id - entry.last_update_id;
        let current_chars = entry
            .messages
            .iter()
            .filter_map(|item| item.text.as_deref())
            .map(str::chars)
            .map(Iterator::count)
            .sum::<usize>();
        let next_chars = current_chars
            + message
                .text
                .as_deref()
                .map(str::chars)
                .map(Iterator::count)
                .unwrap_or_default();
        let appendable = last_message.chat.id == message.chat.id
            && entry.sender_id == sender_id
            && id_gap > 0
            && id_gap <= TEXT_FRAGMENT_MAX_ID_GAP
            && entry.messages.len() < TEXT_FRAGMENT_MAX_PARTS
            && next_chars <= TEXT_FRAGMENT_MAX_TOTAL_CHARS;
        if !appendable {
            flushed = Some(BufferedTextFragments {
                chat_id: entry.chat_id,
                sender_id: entry.sender_id,
                last_update_id: entry.last_update_id,
                ready_at: entry.ready_at,
                update_ids: std::mem::take(&mut entry.update_ids),
                messages: std::mem::take(&mut entry.messages),
            });
        }
    }

    entry.chat_id = message.chat.id;
    entry.sender_id = sender_id;
    entry.last_update_id = update_id;
    entry.ready_at = Instant::now() + TEXT_FRAGMENT_MAX_GAP;
    entry.update_ids.push(update_id);
    entry.messages.push(message);
    flushed
}

async fn flush_ready_text_fragments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    buffers: &mut HashMap<String, BufferedTextFragments>,
    active_turn: &mut Option<ActiveOwnerTurn>,
    current_update_id: Option<i64>,
) -> KaiResult<()> {
    let now = Instant::now();
    let ready_keys = buffers
        .iter()
        .filter_map(|(key, entry)| {
            let force_flush = current_update_id.is_some_and(|value| value > entry.last_update_id);
            let due = now >= entry.ready_at
                || force_flush
                || entry.messages.len() >= TEXT_FRAGMENT_MAX_PARTS;
            due.then_some(key.clone())
        })
        .collect::<Vec<_>>();

    for key in ready_keys {
        let Some(mut entry) = buffers.remove(&key) else {
            continue;
        };
        persist_text_fragments(state, buffers)?;
        match process_buffered_text_fragments(client, token, config, state, active_turn, &entry)
            .await
        {
            Ok(()) => {
                if let Some(last_update_id) = entry.update_ids.last().copied() {
                    state.clear_update_failure(last_update_id)?;
                }
            }
            Err(error) => {
                let chat_id = entry.messages.first().map(|message| message.chat.id);
                let failure_update_id = entry
                    .update_ids
                    .last()
                    .copied()
                    .unwrap_or(entry.last_update_id);
                match handle_update_failure(
                    client,
                    token,
                    state,
                    failure_update_id,
                    chat_id,
                    &error,
                )
                .await?
                {
                    UpdateFailureDisposition::Advance => {}
                    UpdateFailureDisposition::Retry => {
                        entry.ready_at = Instant::now() + TELEGRAM_RETRY_BACKOFF;
                        buffers.insert(key, entry);
                        persist_text_fragments(state, buffers)?;
                    }
                }
            }
        }
    }

    Ok(())
}

async fn process_buffered_text_fragments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
    entry: &BufferedTextFragments,
) -> KaiResult<()> {
    let first_message = entry
        .messages
        .first()
        .ok_or_else(|| KaiError::new(ErrorCode::RuntimeError, "empty text fragment buffer"))?;
    let validated = validate_inbound_message(client, token, config, state, first_message).await?;
    let Some(validated) = validated else {
        return Ok(());
    };

    let text = entry
        .messages
        .iter()
        .filter_map(|message| message.text.as_deref())
        .map(str::trim)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join("\n");

    enqueue_owner_turn(
        client,
        token,
        state,
        active_turn,
        PendingTurn {
            id: Uuid::new_v4().to_string(),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
            channel: "telegram".to_string(),
            update_ids: entry.update_ids.clone(),
            chat_id: validated.chat_id,
            sender_id: validated.sender_id,
            text,
            attachments: Vec::new(),
        },
    )
    .await
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
    active_turn: &mut Option<ActiveOwnerTurn>,
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
        persist_media_groups(state, media_groups)?;

        match process_buffered_media_group(client, token, config, state, active_turn, &entry).await
        {
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
                    persist_media_groups(state, media_groups)?;
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
    active_turn: &mut Option<ActiveOwnerTurn>,
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

    enqueue_owner_turn(
        client,
        token,
        state,
        active_turn,
        PendingTurn {
            id: Uuid::new_v4().to_string(),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
            channel: "telegram".to_string(),
            update_ids: entry.update_ids.clone(),
            chat_id: validated.chat_id,
            sender_id: validated.sender_id,
            text,
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
    send_message_with_retry(client, token, chat_id, text).await
}

async fn send_message_with_retry(
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

async fn send_local_paths(
    client: &Client,
    token: &str,
    chat_id: i64,
    paths: &[PathBuf],
) -> KaiResult<usize> {
    let mut sent = 0_usize;

    for path in paths.iter().take(MAX_OUTBOUND_ATTACHMENTS_PER_REPLY) {
        if send_local_path(client, token, chat_id, path).await? {
            sent += 1;
        }
    }

    if sent == 0 {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram rejected the requested outbound file(s)",
        ));
    }

    Ok(sent)
}

async fn send_local_path(
    client: &Client,
    token: &str,
    chat_id: i64,
    path: &Path,
) -> KaiResult<bool> {
    let metadata = fs::metadata(path).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to inspect local file for Telegram delivery: {error}"),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let kind = classify_document_kind(Some(name), None);
    let byte_limit = attachment_byte_limit(kind);
    if metadata.len() > byte_limit {
        return Err(KaiError::invalid_argument(format!(
            "outbound file exceeds Telegram limit: {} bytes > {}",
            metadata.len(),
            byte_limit
        )));
    }

    let mime_type = guess_mime_type(path, kind);
    let sent = match kind {
        AttachmentKind::Animation => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendAnimation",
                    "animation",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Image => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendPhoto",
                    "photo",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Video => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendVideo",
                    "video",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Voice => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendVoice",
                    "voice",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Audio => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendAudio",
                    "audio",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Document | AttachmentKind::Pdf | AttachmentKind::Text => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendDocument",
                    "document",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
        }
    };

    Ok(sent)
}

async fn send_uploaded_file(
    client: &Client,
    token: &str,
    chat_id: i64,
    upload: OutboundUpload<'_>,
) -> KaiResult<bool> {
    let part = build_uploaded_part(&upload).await?;

    let form = multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part(upload.field_name.to_string(), part);

    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/{}",
            upload.method
        ))
        .multipart(form)
        .send()
        .await
        .map_err(http_error("send Telegram file"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram file response"))?;

    if payload.ok {
        return Ok(true);
    }

    Ok(false)
}

async fn build_uploaded_part(upload: &OutboundUpload<'_>) -> KaiResult<multipart::Part> {
    let open_body = || async {
        File::open(upload.path).await.map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to open local file for Telegram delivery: {error}"),
            )
        })
    };

    let part = multipart::Part::stream_with_length(
        reqwest::Body::wrap_stream(ReaderStream::new(open_body().await?)),
        upload.bytes_len,
    )
    .file_name(upload.file_name.to_string());
    match upload.mime_type {
        Some(mime_type) => Ok(part.mime_str(mime_type).unwrap_or(
            multipart::Part::stream_with_length(
                reqwest::Body::wrap_stream(ReaderStream::new(open_body().await?)),
                upload.bytes_len,
            )
            .file_name(upload.file_name.to_string()),
        )),
        None => Ok(part),
    }
}

struct OutboundUpload<'a> {
    method: &'a str,
    field_name: &'a str,
    file_name: &'a str,
    path: &'a Path,
    bytes_len: u64,
    mime_type: Option<&'a str>,
}

impl<'a> OutboundUpload<'a> {
    fn new(
        method: &'a str,
        field_name: &'a str,
        file_name: &'a str,
        path: &'a Path,
        bytes_len: u64,
        mime_type: Option<&'a str>,
    ) -> Self {
        Self {
            method,
            field_name,
            file_name,
            path,
            bytes_len,
            mime_type,
        }
    }
}

async fn sync_command_menu_if_needed(
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
        .json(&serde_json::json!({ "scope": scope }))
        .send()
        .await;

    let response = client
        .post(format!("https://api.telegram.org/bot{token}/setMyCommands"))
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

fn split_response_text(text: &str) -> Vec<String> {
    if text.chars().count() <= TELEGRAM_TEXT_LIMIT {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text.to_string();

    while !remaining.is_empty() {
        if remaining.chars().count() <= TELEGRAM_TEXT_LIMIT {
            chunks.push(remaining);
            break;
        }

        let boundary = floor_char_boundary(&remaining, TELEGRAM_TEXT_LIMIT.min(remaining.len()));
        let split_at = remaining[..boundary]
            .rfind('\n')
            .filter(|index| *index > 0)
            .unwrap_or(boundary);

        let mut chunk = remaining[..split_at].to_string();
        let mut next_remaining = remaining[split_at..].to_string();
        if next_remaining.starts_with('\n') {
            next_remaining = next_remaining[1..].to_string();
        }

        let fence_count = chunk
            .lines()
            .filter(|line| line.trim_start().starts_with("```"))
            .count();
        if fence_count % 2 == 1 {
            chunk.push('\n');
            chunk.push_str("```");
            if !next_remaining.is_empty() {
                next_remaining = format!("```\n{next_remaining}");
            }
        }

        chunks.push(chunk);
        remaining = next_remaining;
    }

    chunks
}

fn floor_char_boundary(input: &str, mut index: usize) -> usize {
    while index > 0 && !input.is_char_boundary(index) {
        index -= 1;
    }
    index
}

fn resolve_requested_path(config: &LoadedConfig, raw: &str) -> KaiResult<PathBuf> {
    let normalized = crate::config::expand_home(raw);
    let candidate = if Path::new(&normalized).is_absolute() {
        PathBuf::from(&normalized)
    } else {
        Path::new(&config.values.paths.root_work).join(normalized)
    };

    let canonical = candidate.canonicalize().map_err(|error| {
        KaiError::new(
            ErrorCode::InvalidArgument,
            format!("failed to resolve requested path: {error}"),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        KaiError::new(
            ErrorCode::InvalidArgument,
            format!("failed to inspect requested path: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(KaiError::invalid_argument(
            "requested path must resolve to a regular file",
        ));
    }

    let root_work = Path::new(&config.values.paths.root_work)
        .canonicalize()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to resolve root_work: {error}"),
            )
        })?;
    let root_app = Path::new(&config.values.paths.root_app)
        .canonicalize()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to resolve root_app: {error}"),
            )
        })?;

    if !canonical.starts_with(&root_work) && !canonical.starts_with(&root_app) {
        return Err(KaiError::blocked_prerequisite(
            "requested path is outside the approved kai roots",
        ));
    }

    Ok(canonical)
}

fn guess_mime_type(path: &Path, kind: AttachmentKind) -> Option<String> {
    let lower = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match kind {
        AttachmentKind::Animation => Some("image/gif".to_string()),
        AttachmentKind::Audio => Some(
            match lower.as_str() {
                "mp3" => "audio/mpeg",
                "m4a" => "audio/mp4",
                "wav" => "audio/wav",
                "flac" => "audio/flac",
                "ogg" => "audio/ogg",
                "webm" => "audio/webm",
                _ => "audio/mpeg",
            }
            .to_string(),
        ),
        AttachmentKind::Document => Some("application/octet-stream".to_string()),
        AttachmentKind::Image => Some(
            match lower.as_str() {
                "png" => "image/png",
                "webp" => "image/webp",
                "heic" => "image/heic",
                "heif" => "image/heif",
                "bmp" => "image/bmp",
                _ => "image/jpeg",
            }
            .to_string(),
        ),
        AttachmentKind::Pdf => Some("application/pdf".to_string()),
        AttachmentKind::Text => Some("text/plain".to_string()),
        AttachmentKind::Video => Some("video/mp4".to_string()),
        AttachmentKind::Voice => Some("audio/ogg".to_string()),
    }
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

fn should_retry_telegram_send(error: &KaiError) -> bool {
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramUpdate {
    update_id: i64,
    message: Option<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramUser {
    id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramChat {
    id: i64,
    #[serde(rename = "type")]
    kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramDocument {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramPhotoSize {
    file_id: String,
    file_unique_id: String,
    width: u32,
    height: u32,
    file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramAudio {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramVoice {
    file_id: String,
    file_unique_id: String,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramVideo {
    file_id: String,
    file_name: Option<String>,
    mime_type: Option<String>,
    file_size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TelegramVideoNote {
    file_id: String,
    file_unique_id: String,
    file_size: Option<u64>,
    duration: Option<u32>,
    width: Option<u32>,
    height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
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

#[derive(Debug, Serialize)]
struct TelegramMenuCommand {
    command: String,
    description: String,
}

impl TelegramMenuCommand {
    fn new(command: &str, description: &str) -> Self {
        Self {
            command: command.to_string(),
            description: description.to_string(),
        }
    }
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
    use super::{
        TELEGRAM_TEXT_LIMIT, failure_notice_text, format_telegram_html, should_retry_telegram_send,
        should_skip_failed_update, split_response_text,
    };
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
    fn split_response_text_splits_long_messages() {
        let input = "a".repeat(TELEGRAM_TEXT_LIMIT + 20);
        let chunks = split_response_text(&input);
        assert_eq!(chunks.len(), 2);
        assert!(
            chunks
                .iter()
                .all(|chunk| chunk.chars().count() <= TELEGRAM_TEXT_LIMIT)
        );
    }

    #[test]
    fn split_response_text_balances_fenced_code_blocks() {
        let input = format!(
            "{}\n```rust\n{}\n```",
            "a".repeat(TELEGRAM_TEXT_LIMIT - 20),
            "let x = 1;"
        );
        let chunks = split_response_text(&input);
        assert!(chunks.len() >= 2);
        for chunk in chunks {
            let fences = chunk
                .lines()
                .filter(|line| line.trim_start().starts_with("```"))
                .count();
            assert_eq!(fences % 2, 0);
        }
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

    #[test]
    fn telegram_send_retry_classifier_matches_common_retryable_errors() {
        let error = KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram API returned 429 Too Many Requests",
        );
        assert!(should_retry_telegram_send(&error));
        let error = KaiError::new(
            ErrorCode::RuntimeError,
            "failed to send Telegram message: connection reset by peer",
        );
        assert!(should_retry_telegram_send(&error));
        let error = KaiError::new(ErrorCode::RuntimeError, "bot was blocked by the user");
        assert!(!should_retry_telegram_send(&error));
    }
}
