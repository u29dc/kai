use super::*;

pub(super) fn load_buffered_media_groups(
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

pub(super) fn persist_media_groups(
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

pub(super) fn load_buffered_text_fragments(
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

pub(super) fn persist_text_fragments(
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

pub(super) fn recover_active_turn(state: &StateStore) -> KaiResult<Option<ActiveTurnState>> {
    state.recover_active_turn()
}

pub(super) fn run_housekeeping(state: &StateStore) -> KaiResult<()> {
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

pub(super) fn enqueue_pending_reply_delivery(
    state: &StateStore,
    delivery: PendingReplyDelivery,
) -> KaiResult<()> {
    state.enqueue_pending_reply_delivery(&delivery)
}

pub(super) async fn flush_pending_reply_deliveries(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
) -> KaiResult<()> {
    let deliveries = state.pending_reply_deliveries()?;
    if deliveries.is_empty() {
        return Ok(());
    }

    for mut delivery in deliveries {
        if delivery.response_text.trim().is_empty() {
            state.finalize_pending_reply_delivery(&delivery)?;
            state.append_audit_json(&serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event": "telegram.reply_delivered",
                "deliveryId": delivery.delivery_id,
                "turnId": delivery.turn_id,
                "chatId": delivery.chat_id,
                "attempts": delivery.attempts,
                "updateIds": delivery.update_ids,
                "chunksSent": 0,
            }))?;
            continue;
        }

        let chunks = split_response_text(&delivery.response_text);
        let mut completed = true;
        for (index, chunk) in chunks.iter().enumerate().skip(delivery.next_chunk_index) {
            match send_message_chunk_with_retry(client, token, delivery.chat_id, chunk).await {
                Ok(message_id) => {
                    state.record_pending_reply_delivery_chunk(
                        &delivery.delivery_id,
                        index + 1,
                        message_id,
                    )?;
                    delivery.next_chunk_index = index + 1;
                    delivery.sent_message_ids.push(message_id);
                }
                Err(error) => {
                    let attempts =
                        state.increment_pending_reply_delivery_attempts(&delivery.delivery_id)?;
                    state.append_audit_json(&serde_json::json!({
                        "timestamp": chrono::Utc::now().to_rfc3339(),
                        "event": "telegram.reply_delivery_retry",
                        "deliveryId": delivery.delivery_id,
                        "turnId": delivery.turn_id,
                        "chatId": delivery.chat_id,
                        "attempts": attempts,
                        "nextChunkIndex": delivery.next_chunk_index,
                        "message": error.message,
                        "hint": error.hint,
                    }))?;
                    completed = false;
                    break;
                }
            }
        }

        if !completed {
            continue;
        }

        state.finalize_pending_reply_delivery(&delivery)?;
        mark_progress_done(
            client,
            token,
            config,
            state,
            delivery.chat_id,
            delivery.status_message_id,
        )
        .await?;
        state.append_audit_json(&serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": "telegram.reply_delivered",
            "deliveryId": delivery.delivery_id,
            "turnId": delivery.turn_id,
            "chatId": delivery.chat_id,
            "attempts": delivery.attempts.saturating_add(1),
            "updateIds": delivery.update_ids,
            "chunksSent": delivery.next_chunk_index,
        }))?;
    }

    Ok(())
}
