use super::*;
use tokio::task::JoinSet;

const ATTACHMENT_ENRICH_CONCURRENCY: usize = 3;
const SIDE_QUERY_TIMEOUT: Duration = Duration::from_secs(60 * 30);

pub(super) fn stable_pending_turn_id(
    channel: &str,
    chat_id: i64,
    sender_id: i64,
    update_ids: &[i64],
) -> String {
    let mut hasher = blake3::Hasher::new();
    hasher.update(channel.as_bytes());
    hasher.update(b":");
    hasher.update(chat_id.to_string().as_bytes());
    hasher.update(b":");
    hasher.update(sender_id.to_string().as_bytes());
    hasher.update(b":");
    for update_id in update_ids {
        hasher.update(update_id.to_string().as_bytes());
        hasher.update(b",");
    }
    format!("turn-{}", hasher.finalize().to_hex())
}

pub(super) async fn enqueue_owner_turn(
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

pub(super) async fn maybe_start_next_pending_turn(
    _client: &Client,
    _token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &mut Option<ActiveOwnerTurn>,
) -> KaiResult<()> {
    if active_turn.is_some() {
        return Ok(());
    }

    let Some(active_state) = state.claim_next_pending_turn()? else {
        return Ok(());
    };
    let mut pending = active_state.pending;

    if pending.text.is_empty() && pending.attachments.is_empty() {
        state.clear_active_turn_state()?;
        return Ok(());
    }

    enrich_pending_turn_attachments(config, &mut pending)
        .await
        .inspect_err(|_| {
            let _ = state.clear_active_turn_state();
            let _ = state.prepend_pending_turn(&pending);
        })?;
    state.set_active_turn_state(&ActiveTurnState {
        pending: pending.clone(),
        status_message_id: None,
    })?;

    let prepared = prepare_agent_turn(
        config,
        state,
        &pending.target,
        &pending.channel,
        pending.sender_id,
        &pending.text,
        &pending.attachments,
    )
    .inspect_err(|_| {
        let _ = state.clear_active_turn_state();
        let _ = state.prepend_pending_turn(&pending);
    })?;

    let running = match start_agent_turn(config.clone(), prepared).await {
        Ok(running) => running,
        Err(error) => {
            let _ = state.clear_active_turn_state();
            let _ = state.prepend_pending_turn(&pending);
            return Err(error);
        }
    };
    state.record_turn(NewTurn {
        provider: pending.target.provider,
        workspace_id: &pending.target.workspace_id,
        working_dir: &pending.target.working_dir,
        role: "user",
        channel: &pending.channel,
        sender_id: Some(pending.sender_id),
        text: &pending.text,
        codex_session_id: state
            .get_session_binding(&pending.target)?
            .as_ref()
            .map(|binding| binding.session_id.as_str()),
        outcome_status: Some("received"),
        attachments: &pending.attachments,
    })?;
    let progress_variant_seed = progress_variant_seed(&pending.id);
    *active_turn = Some(ActiveOwnerTurn {
        pending,
        running,
        cancel_requested: false,
        next_typing_at: Instant::now(),
        status_message_id: None,
        progress: TurnProgressState {
            last_event_at: Instant::now(),
            last_visible_update_at: Instant::now()
                - Duration::from_millis(config.values.channel.telegram.progress.edit_interval_ms),
            initial_progress_due_at: Instant::now() + initial_progress_delay(),
            initial_progress_sent: false,
            last_sent_text: None,
            semantic_update_count: 0,
            idle_update_count: 0,
            update_count: 0,
            edit_interval_ms: config.values.channel.telegram.progress.edit_interval_ms,
            variant_seed: progress_variant_seed,
        },
    });
    Ok(())
}

pub(super) async fn finish_active_turn(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &ActiveOwnerTurn,
    result: KaiResult<AsyncAgentTurnResult>,
) -> KaiResult<()> {
    match result {
        Ok(async_result) => {
            finalize_successful_turn(client, token, config, state, active_turn, async_result).await
        }
        Err(error) => {
            state.clear_active_turn_state()?;
            if active_turn.cancel_requested {
                mark_turn_canceled(client, token, config, state, active_turn).await?;
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
            mark_turn_failed(client, token, config, state, active_turn).await?;
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

pub(super) async fn start_side_query(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_side_query: &mut Option<ActiveSideQuery>,
    validated: &ValidatedInbound,
    prompt: String,
) -> KaiResult<ProcessedUpdateOutcome> {
    if let Some(query) = active_side_query.as_ref() {
        let text = format!(
            "A side query is already running in {}. Wait for it to finish or use /cancel ask.",
            query.state.target.workspace_id
        );
        send_message(client, token, validated.chat_id, &text).await?;
        return Ok(ProcessedUpdateOutcome::TextReply { text });
    }

    let target = crate::workspace::execution_target(config, state)?;
    let query_state = SideQueryState {
        id: Uuid::new_v4().to_string(),
        started_at: chrono::Utc::now().to_rfc3339(),
        target,
        chat_id: validated.chat_id,
        sender_id: validated.sender_id,
        text: prompt.clone(),
        status: "working".to_string(),
    };
    state.set_active_side_query(&query_state)?;

    let prepared = prepare_agent_side_turn(
        config,
        state,
        &query_state.target,
        "telegram.side",
        validated.sender_id,
        &prompt,
        &[],
    )
    .inspect_err(|_| {
        let _ = state.clear_active_side_query();
    })?;
    let running = match start_agent_turn(config.clone(), prepared).await {
        Ok(running) => running,
        Err(error) => {
            let _ = state.clear_active_side_query();
            return Err(error);
        }
    };

    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "telegram.side_query_started",
        "sideQueryId": query_state.id,
        "provider": query_state.target.provider.as_key(),
        "workspaceId": query_state.target.workspace_id,
        "workingDir": query_state.target.working_dir,
        "chatId": query_state.chat_id,
        "senderId": query_state.sender_id,
    }))?;

    *active_side_query = Some(ActiveSideQuery {
        state: query_state.clone(),
        running,
        cancel_requested: false,
        started_at_instant: Instant::now(),
        next_typing_at: Instant::now(),
    });

    let text = format!(
        "Side query started in {}. It will run alongside the main queue.",
        query_state.target.workspace_id
    );
    send_message(client, token, validated.chat_id, &text).await?;
    Ok(ProcessedUpdateOutcome::TextReply { text })
}

pub(super) async fn finish_active_side_query(
    client: &Client,
    token: &str,
    _config: &LoadedConfig,
    state: &StateStore,
    query: &ActiveSideQuery,
    result: KaiResult<AsyncAgentTurnResult>,
) -> KaiResult<()> {
    state.clear_active_side_query()?;
    match result {
        Ok(async_result) => {
            let result = async_result.result;
            state.append_audit_json(&serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event": "telegram.side_query_completed",
                "sideQueryId": query.state.id,
                "provider": query.state.target.provider.as_key(),
                "workspaceId": query.state.target.workspace_id,
                "workingDir": query.state.target.working_dir,
                "chatId": query.state.chat_id,
                "senderId": query.state.sender_id,
                "codexSessionId": result.session_id,
                "resumed": result.resumed,
            }))?;
            send_message_with_retry(
                client,
                token,
                query.state.chat_id,
                &format!(
                    "Side query result ({})\n\n{}",
                    query.state.target.workspace_id, result.response_text
                ),
            )
            .await
        }
        Err(error) if query.cancel_requested => {
            state.append_audit_json(&serde_json::json!({
                "timestamp": chrono::Utc::now().to_rfc3339(),
                "event": "telegram.side_query_cancelled",
                "sideQueryId": query.state.id,
                "message": error.message,
                "hint": error.hint,
            }))?;
            send_message(client, token, query.state.chat_id, "Side query canceled.").await
        }
        Err(error) => {
            record_runtime_error(
                state,
                "telegram.side_query_failed",
                None,
                Some(query.state.chat_id),
                &error,
            )?;
            send_message_with_retry(
                client,
                token,
                query.state.chat_id,
                &format!("Side query failed: {}", error.message),
            )
            .await
        }
    }
}

pub(super) async fn maybe_timeout_side_query(
    client: &Client,
    token: &str,
    state: &StateStore,
    query: &mut ActiveSideQuery,
) -> KaiResult<bool> {
    if query.started_at_instant.elapsed() < SIDE_QUERY_TIMEOUT {
        return Ok(false);
    }

    query.cancel_requested = true;
    let _ = cancel_agent_turn(&query.running);
    state.clear_active_side_query()?;
    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "telegram.side_query_timeout",
        "sideQueryId": query.state.id,
        "chatId": query.state.chat_id,
        "workspaceId": query.state.target.workspace_id,
    }))?;
    send_message(
        client,
        token,
        query.state.chat_id,
        "Side query timed out and was canceled.",
    )
    .await?;
    Ok(true)
}

async fn finalize_successful_turn(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    active_turn: &ActiveOwnerTurn,
    async_result: AsyncAgentTurnResult,
) -> KaiResult<()> {
    if let Some(AgentResumeFailure {
        requested_session_id,
        stale_session,
        error,
        ..
    }) = async_result.resume_failure
    {
        state.append_audit_json(&serde_json::json!({
            "timestamp": chrono::Utc::now().to_rfc3339(),
            "event": "codex.resume_failed",
            "provider": active_turn.pending.target.provider.as_key(),
            "workspaceId": active_turn.pending.target.workspace_id,
            "requestedSessionId": requested_session_id,
            "staleSession": stale_session,
            "message": error.message,
            "hint": error.hint,
        }))?;
    }

    let result = async_result.result;
    state.set_session_binding(&active_turn.pending.target, &result.session_id)?;
    state.record_turn(NewTurn {
        provider: active_turn.pending.target.provider,
        workspace_id: &active_turn.pending.target.workspace_id,
        working_dir: &active_turn.pending.target.working_dir,
        role: "assistant",
        channel: &active_turn.pending.channel,
        sender_id: None,
        text: &result.response_text,
        codex_session_id: Some(&result.session_id),
        outcome_status: Some(if result.resumed { "resumed" } else { "fresh" }),
        attachments: &[],
    })?;

    let replay_package =
        create_replay_package(&state.recent_turns_for_target(&active_turn.pending.target, 24)?);
    state.set_target_replay_package(&active_turn.pending.target, &replay_package)?;

    state.append_audit_json(&serde_json::json!({
        "timestamp": chrono::Utc::now().to_rfc3339(),
        "event": "turn.completed",
        "turnId": active_turn.pending.id,
        "provider": active_turn.pending.target.provider.as_key(),
        "workspaceId": active_turn.pending.target.workspace_id,
        "workingDir": active_turn.pending.target.working_dir,
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
            status_message_id: active_turn.status_message_id,
            update_ids: active_turn.pending.update_ids.clone(),
            attempts: 0,
            created_at: chrono::Utc::now().to_rfc3339(),
            next_chunk_index: 0,
            sent_message_ids: Vec::new(),
        },
    )?;

    flush_pending_reply_deliveries(client, token, config, state).await?;

    Ok(())
}

pub(super) fn record_runtime_error(
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

pub(super) async fn handle_update_failure(
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

pub(super) fn should_skip_failed_update(error: &KaiError, attempt_count: u32) -> bool {
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

async fn enrich_pending_turn_attachments(
    config: &LoadedConfig,
    pending: &mut PendingTurn,
) -> KaiResult<()> {
    if pending.attachments.is_empty() {
        return Ok(());
    }

    let mut in_flight = JoinSet::new();
    let mut attachments = std::mem::take(&mut pending.attachments);
    let concurrency = attachments.len().clamp(1, ATTACHMENT_ENRICH_CONCURRENCY);
    let mut next_index = 0usize;
    let total = attachments.len();

    while next_index < total && in_flight.len() < concurrency {
        let index = next_index;
        let config = config.clone();
        let mut attachment = attachments[index].clone();
        in_flight.spawn(async move {
            enrich_attachment(&config, &mut attachment).await?;
            Ok::<_, KaiError>((index, attachment))
        });
        next_index += 1;
    }

    while let Some(result) = in_flight.join_next().await {
        let (index, attachment) = result.map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("attachment enrichment task failed: {error}"),
            )
        })??;
        attachments[index] = attachment;

        if next_index < total {
            let index = next_index;
            let config = config.clone();
            let mut attachment = attachments[index].clone();
            in_flight.spawn(async move {
                enrich_attachment(&config, &mut attachment).await?;
                Ok::<_, KaiError>((index, attachment))
            });
            next_index += 1;
        }
    }

    pending.attachments = attachments;
    Ok(())
}

pub(super) fn failure_notice_text(error: &KaiError, attempt_count: u32) -> String {
    if matches!(error.code, ErrorCode::InvalidArgument) {
        return format!("I couldn't handle that message: {}", error.message);
    }

    format!(
        "I hit an internal error while handling that message after {attempt_count} attempt(s). I skipped it so later messages can continue."
    )
}
