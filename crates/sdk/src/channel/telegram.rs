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
use crate::runtime::agent::{
    AgentResumeFailure, AsyncAgentTurnResult, RunningAgentTurn, RunningAgentTurnEvent,
    cancel_agent_turn, create_replay_package, drain_running_agent_turn_events,
    prepare_agent_side_turn, prepare_agent_turn, selected_provider, start_agent_turn,
};
use crate::secrets::resolve_telegram_token;
use crate::state::{
    ActiveTurnState, AttachmentInfo, NewTurn, PendingReplyDelivery, PendingTurn, ProcessedUpdate,
    ProcessedUpdateOutcome, SideQueryState, StateStore,
};

mod api;
mod attachments;
mod buffers;
mod formatting;
mod inbound;
mod menu;
mod models;
mod progress;
mod store;
#[cfg(test)]
mod tests;
mod transport;
mod turns;

use self::api::*;
use self::attachments::*;
use self::buffers::*;
use self::formatting::*;
use self::inbound::*;
use self::menu::*;
use self::models::*;
use self::progress::*;
use self::store::*;
use self::transport::*;
use self::turns::*;

const TELEGRAM_RETRY_BACKOFF: Duration = Duration::from_secs(3);
const TELEGRAM_TYPING_REFRESH: Duration = Duration::from_secs(4);
const MAX_UPDATE_FAILURE_ATTEMPTS: u32 = 3;
const TELEGRAM_TEXT_LIMIT: usize = 4096;
const MAX_OUTBOUND_ATTACHMENTS_PER_REPLY: usize = 4;
const TELEGRAM_SEND_RETRY_ATTEMPTS: u32 = 3;
const TELEGRAM_SEND_RETRY_BACKOFF: Duration = Duration::from_secs(2);
const TELEGRAM_API_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const TELEGRAM_DOWNLOAD_REQUEST_TIMEOUT: Duration = Duration::from_secs(180);
const TELEGRAM_LONG_POLL_TIMEOUT_SLACK_SECS: u64 = 15;
const TEXT_FRAGMENT_START_THRESHOLD_CHARS: usize = 3500;
const TEXT_FRAGMENT_MAX_TOTAL_CHARS: usize = 24_000;
const TEXT_FRAGMENT_MAX_PARTS: usize = 6;
const TEXT_FRAGMENT_MAX_ID_GAP: i64 = 5;
const TEXT_FRAGMENT_MAX_GAP: Duration = Duration::from_millis(1400);
const BUFFERED_MEDIA_GROUPS_STATE_KEY: &str = "telegram.buffered_media_groups";
const BUFFERED_TEXT_FRAGMENTS_STATE_KEY: &str = "telegram.buffered_text_fragments";
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
    let _ = selected_provider(config)?;

    let token = telegram_token(config)?;
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to build http client: {error}"),
            )
        })?;

    let mut offset = state.get_update_offset()?;
    let mut next_cleanup_at = Instant::now();
    let mut media_groups = load_buffered_media_groups(state)?;
    let mut text_fragments = load_buffered_text_fragments(state)?;
    let mut active = ActiveTelegramTurns {
        main: None,
        side_query: None,
    };
    let mut synced_menu_chat_id: Option<i64> = None;

    let recovered_active_turn = recover_active_turn(state)?;
    if let Some(active_turn) = recovered_active_turn.as_ref() {
        mark_recovered_turn_restarting(&client, &token, config, state, active_turn).await?;
    }
    if let Some(side_query) = state.get_active_side_query()? {
        state.clear_active_side_query()?;
        let _ = send_message(
            &client,
            &token,
            side_query.chat_id,
            "Previous side query was interrupted by a restart. Send /ask again if you still need it.",
        )
        .await;
    }

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

        if let Err(error) = flush_pending_reply_deliveries(&client, &token, config, state).await {
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

        if let Some(turn) = active.main.as_mut() {
            if now >= turn.next_typing_at {
                let _ = send_typing_indicator(&client, &token, turn.pending.chat_id).await;
                turn.next_typing_at = Instant::now() + TELEGRAM_TYPING_REFRESH;
            }

            let mut completed = None;
            for event in drain_running_agent_turn_events(&mut turn.running) {
                match event {
                    RunningAgentTurnEvent::Progress(progress_event) => {
                        handle_progress_event(
                            &client,
                            &token,
                            config,
                            state,
                            turn,
                            &progress_event,
                            now,
                        )
                        .await?;
                    }
                    RunningAgentTurnEvent::ResumeFailure(resume_failure) => {
                        if resume_failure.stale_session {
                            state.clear_session_binding(&turn.pending.target)?;
                        }
                    }
                    RunningAgentTurnEvent::Completed(result) => {
                        completed = Some(result);
                    }
                }
            }

            if let Some(result) = completed {
                finish_active_turn(&client, &token, config, state, turn, result).await?;
                active.main = None;
            } else {
                maybe_send_initial_progress(&client, &token, config, state, turn, now).await?;
                maybe_send_idle_progress(&client, &token, config, state, turn, now).await?;
            }
        }

        if let Some(query) = active.side_query.as_mut() {
            if now >= query.next_typing_at {
                let _ = send_typing_indicator(&client, &token, query.state.chat_id).await;
                query.next_typing_at = Instant::now() + TELEGRAM_TYPING_REFRESH;
            }

            if maybe_timeout_side_query(&client, &token, state, query).await? {
                active.side_query = None;
            } else {
                let mut completed = None;
                for event in drain_running_agent_turn_events(&mut query.running) {
                    if let RunningAgentTurnEvent::Completed(result) = event {
                        completed = Some(result);
                    }
                }
                if let Some(result) = completed {
                    finish_active_side_query(&client, &token, config, state, query, result).await?;
                    active.side_query = None;
                }
            }
        }

        flush_ready_text_fragments(
            &client,
            &token,
            config,
            state,
            &mut text_fragments,
            &mut active,
            None,
        )
        .await?;
        flush_ready_media_groups(
            &client,
            &token,
            config,
            state,
            &mut media_groups,
            &mut active,
            None,
        )
        .await?;

        if active.main.is_none() {
            maybe_start_next_pending_turn(&client, &token, config, state, &mut active.main).await?;
        }

        let has_pending_queue = state.pending_turn_queue_len()? > 0;
        let poll_timeout_seconds = if active.main.is_some()
            || active.side_query.is_some()
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
                            &mut active,
                            &flushed,
                        )
                        .await
                        {
                            let failure_update_id = flushed
                                .update_ids
                                .last()
                                .copied()
                                .unwrap_or(update.update_id);
                            let chat_id = flushed.messages.first().map(|item| item.chat.id);
                            match handle_update_failure(
                                &client,
                                &token,
                                state,
                                failure_update_id,
                                chat_id,
                                &error,
                            )
                            .await?
                            {
                                UpdateFailureDisposition::Advance => {}
                                UpdateFailureDisposition::Retry => {
                                    let key = format!("{}:{}", flushed.chat_id, flushed.sender_id);
                                    text_fragments.insert(key, flushed);
                                    persist_text_fragments(state, &text_fragments)?;
                                    continue;
                                }
                            }
                        } else {
                            state.clear_update_failure(update.update_id)?;
                        }
                    } else {
                        persist_text_fragments(state, &text_fragments)?;
                        offset = next_offset;
                        state.set_update_offset(offset)?;
                        state.clear_update_failure(update.update_id)?;
                        continue;
                    }
                } else {
                    handle_message(
                        &client,
                        &token,
                        config,
                        state,
                        &mut active,
                        update.update_id,
                        message,
                    )
                    .await?;
                }

                offset = next_offset;
                state.set_update_offset(offset)?;
                state.clear_update_failure(update.update_id)?;
                flush_ready_text_fragments(
                    &client,
                    &token,
                    config,
                    state,
                    &mut text_fragments,
                    &mut active,
                    Some(update.update_id),
                )
                .await?;
                flush_ready_media_groups(
                    &client,
                    &token,
                    config,
                    state,
                    &mut media_groups,
                    &mut active,
                    Some(update.update_id),
                )
                .await?;
                Ok(())
            } else {
                offset = next_offset;
                state.set_update_offset(offset)?;
                state.clear_update_failure(update.update_id)?;
                Ok(())
            };

            if let Err(error) = outcome {
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
                        continue;
                    }
                    UpdateFailureDisposition::Retry => {
                        break;
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
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(10))
        .build()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to build http client: {error}"),
            )
        })?;
    send_message(&client, &token, chat_id, text).await
}
