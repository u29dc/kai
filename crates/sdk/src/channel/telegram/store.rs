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
    let Some(active) = state.get_active_turn_state()? else {
        return Ok(None);
    };

    if !state
        .pending_turn_queue()?
        .iter()
        .any(|queued| queued.id == active.pending.id)
    {
        state.prepend_pending_turn(&active.pending)?;
    }
    state.clear_active_turn_state()?;
    Ok(Some(active))
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

pub(super) fn enqueue_pending_reply_delivery(
    state: &StateStore,
    delivery: PendingReplyDelivery,
) -> KaiResult<()> {
    let mut deliveries = pending_reply_deliveries(state)?;
    deliveries.retain(|existing| existing.turn_id != delivery.turn_id);
    deliveries.push(delivery);
    store_pending_reply_deliveries(state, &deliveries)
}

pub(super) async fn flush_pending_reply_deliveries(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
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
