use super::*;

pub(super) fn should_buffer_text_fragments(message: &TelegramMessage) -> bool {
    let Some(text) = message.text.as_deref() else {
        return false;
    };

    let trimmed = text.trim();
    !trimmed.starts_with('/') && trimmed.chars().count() >= TEXT_FRAGMENT_START_THRESHOLD_CHARS
}

pub(super) fn buffer_text_fragments(
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

pub(super) async fn flush_ready_text_fragments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    buffers: &mut HashMap<String, BufferedTextFragments>,
    active: &mut ActiveTelegramTurns,
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
        match process_buffered_text_fragments(client, token, config, state, active, &entry).await {
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

pub(super) async fn process_buffered_text_fragments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active: &mut ActiveTelegramTurns,
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

    if let Some(command) = parse_mobile_command(&text) {
        handle_mobile_command(client, token, config, state, active, &validated, command).await?;
        return Ok(());
    }

    enqueue_owner_turn(
        client,
        token,
        state,
        &mut active.main,
        PendingTurn {
            id: stable_pending_turn_id(
                "telegram",
                validated.chat_id,
                validated.sender_id,
                &entry.update_ids,
            ),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
            target: crate::workspace::execution_target(config, state)?,
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

pub(super) fn buffer_media_group(
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

pub(super) async fn flush_ready_media_groups(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    media_groups: &mut HashMap<String, BufferedMediaGroup>,
    active: &mut ActiveTelegramTurns,
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

        match process_buffered_media_group(client, token, config, state, active, &entry).await {
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
    active: &mut ActiveTelegramTurns,
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
    if let Some(command) = parse_mobile_command(&text) {
        handle_mobile_command(client, token, config, state, active, &validated, command).await?;
        return Ok(());
    }

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
        &mut active.main,
        PendingTurn {
            id: stable_pending_turn_id(
                "telegram",
                validated.chat_id,
                validated.sender_id,
                &entry.update_ids,
            ),
            enqueued_at: chrono::Utc::now().to_rfc3339(),
            target: crate::workspace::execution_target(config, state)?,
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

pub(super) fn media_group_key(chat_id: i64, media_group_id: &str) -> String {
    format!("{chat_id}:{media_group_id}")
}
