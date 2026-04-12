use super::*;

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

pub(super) async fn finish_active_turn(
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

pub(super) fn failure_notice_text(error: &KaiError, attempt_count: u32) -> String {
    if matches!(error.code, ErrorCode::InvalidArgument) {
        return format!("I couldn't handle that message: {}", error.message);
    }

    format!(
        "I hit an internal error while handling that message after {attempt_count} attempt(s). I skipped it so later messages can continue."
    )
}
