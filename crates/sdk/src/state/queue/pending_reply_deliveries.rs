use super::*;

impl StateStore {
    pub fn pending_reply_delivery_count(&self) -> KaiResult<usize> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM pending_reply_deliveries", [], |row| {
                row.get(0)
            })
            .map_err(sql_state_error("count pending reply deliveries"))?;
        Ok(count as usize)
    }

    pub fn pending_reply_deliveries(&self) -> KaiResult<Vec<PendingReplyDelivery>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT delivery_id, turn_id, chat_id, response_text, codex_session_id,
                        status_message_id, update_ids_json, attempts, created_at,
                        next_chunk_index, sent_message_ids_json
                 FROM pending_reply_deliveries
                 ORDER BY created_at ASC, rowid ASC",
            )
            .map_err(sql_state_error("prepare pending reply deliveries"))?;

        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, Option<i64>>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, u32>(7)?,
                    row.get::<_, String>(8)?,
                    row.get::<_, i64>(9)?,
                    row.get::<_, String>(10)?,
                ))
            })
            .map_err(sql_state_error("query pending reply deliveries"))?;

        let mut deliveries = Vec::new();
        for row in rows {
            let (
                delivery_id,
                turn_id,
                chat_id,
                response_text,
                codex_session_id,
                status_message_id,
                update_ids_json,
                attempts,
                created_at,
                next_chunk_index,
                sent_message_ids_json,
            ) = row.map_err(sql_state_error("read pending reply delivery"))?;

            let update_ids =
                serde_json::from_str::<Vec<i64>>(&update_ids_json).map_err(|error| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("failed to deserialize pending reply update ids: {error}"),
                    )
                })?;
            let sent_message_ids = serde_json::from_str::<Vec<i64>>(&sent_message_ids_json)
                .map_err(|error| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("failed to deserialize pending reply message ids: {error}"),
                    )
                })?;

            deliveries.push(PendingReplyDelivery {
                delivery_id,
                turn_id,
                chat_id,
                response_text,
                codex_session_id,
                status_message_id,
                update_ids,
                attempts,
                created_at,
                next_chunk_index: next_chunk_index as usize,
                sent_message_ids,
            });
        }

        Ok(deliveries)
    }

    pub fn enqueue_pending_reply_delivery(&self, delivery: &PendingReplyDelivery) -> KaiResult<()> {
        let update_ids_json = serde_json::to_string(&delivery.update_ids).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize pending reply update ids: {error}"),
            )
        })?;
        let sent_message_ids_json =
            serde_json::to_string(&delivery.sent_message_ids).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to serialize pending reply message ids: {error}"),
                )
            })?;

        self.with_transaction("write pending reply delivery", |connection| {
            connection
                .execute(
                    "INSERT INTO pending_reply_deliveries (
                        delivery_id,
                        turn_id,
                        chat_id,
                        response_text,
                        codex_session_id,
                        status_message_id,
                        update_ids_json,
                        attempts,
                        created_at,
                        next_chunk_index,
                        sent_message_ids_json
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                     ON CONFLICT(turn_id) DO UPDATE
                     SET delivery_id = excluded.delivery_id,
                         chat_id = excluded.chat_id,
                         response_text = excluded.response_text,
                         codex_session_id = excluded.codex_session_id,
                         status_message_id = excluded.status_message_id,
                         update_ids_json = excluded.update_ids_json,
                         attempts = excluded.attempts,
                         created_at = excluded.created_at,
                         next_chunk_index = excluded.next_chunk_index,
                         sent_message_ids_json = excluded.sent_message_ids_json",
                    params![
                        delivery.delivery_id.as_str(),
                        delivery.turn_id.as_str(),
                        delivery.chat_id,
                        delivery.response_text.as_str(),
                        delivery.codex_session_id.as_str(),
                        delivery.status_message_id,
                        update_ids_json,
                        delivery.attempts,
                        delivery.created_at.as_str(),
                        delivery.next_chunk_index as i64,
                        sent_message_ids_json
                    ],
                )
                .map_err(sql_state_error("insert pending reply delivery"))?;

            let active = connection
                .query_row(
                    "SELECT payload_json FROM active_turn WHERE singleton = ?1",
                    [ACTIVE_TURN_SINGLETON],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_state_error(
                    "load active turn during reply delivery enqueue",
                ))?;
            if let Some(payload_json) = active {
                let active = deserialize_active_turn_state(&payload_json)?;
                if active.pending.id == delivery.turn_id {
                    connection
                        .execute(
                            "DELETE FROM active_turn WHERE singleton = ?1",
                            [ACTIVE_TURN_SINGLETON],
                        )
                        .map_err(sql_state_error(
                            "clear active turn during reply delivery enqueue",
                        ))?;
                }
            }

            Ok(())
        })?;
        self.delete_value(PENDING_REPLY_DELIVERIES_STATE_KEY)
    }

    pub fn record_pending_reply_delivery_chunk(
        &self,
        delivery_id: &str,
        next_chunk_index: usize,
        sent_message_id: i64,
    ) -> KaiResult<()> {
        self.with_transaction("record pending reply delivery chunk", |connection| {
            let sent_message_ids_json = connection
                .query_row(
                    "SELECT sent_message_ids_json
                     FROM pending_reply_deliveries
                     WHERE delivery_id = ?1",
                    [delivery_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_state_error("load pending reply delivery messages"))?
                .ok_or_else(|| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("missing pending reply delivery `{delivery_id}`"),
                    )
                })?;

            let mut sent_message_ids = serde_json::from_str::<Vec<i64>>(&sent_message_ids_json)
                .map_err(|error| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("failed to deserialize pending reply message ids: {error}"),
                    )
                })?;
            sent_message_ids.push(sent_message_id);

            connection
                .execute(
                    "UPDATE pending_reply_deliveries
                     SET next_chunk_index = ?2,
                         sent_message_ids_json = ?3
                     WHERE delivery_id = ?1",
                    params![
                        delivery_id,
                        next_chunk_index as i64,
                        serde_json::to_string(&sent_message_ids).map_err(|error| {
                            KaiError::new(
                                ErrorCode::StateError,
                                format!(
                                    "failed to serialize updated pending reply message ids: {error}"
                                ),
                            )
                        })?
                    ],
                )
                .map_err(sql_state_error("update pending reply delivery progress"))?;
            Ok(())
        })
    }

    pub fn increment_pending_reply_delivery_attempts(&self, delivery_id: &str) -> KaiResult<u32> {
        self.with_transaction("increment pending reply delivery attempts", |connection| {
            let attempts = connection
                .query_row(
                    "SELECT attempts
                     FROM pending_reply_deliveries
                     WHERE delivery_id = ?1",
                    [delivery_id],
                    |row| row.get::<_, u32>(0),
                )
                .optional()
                .map_err(sql_state_error("load pending reply delivery attempts"))?
                .ok_or_else(|| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("missing pending reply delivery `{delivery_id}`"),
                    )
                })?
                .saturating_add(1);

            connection
                .execute(
                    "UPDATE pending_reply_deliveries
                     SET attempts = ?2
                     WHERE delivery_id = ?1",
                    params![delivery_id, attempts],
                )
                .map_err(sql_state_error("update pending reply delivery attempts"))?;

            Ok(attempts)
        })
    }

    pub fn finalize_pending_reply_delivery(
        &self,
        delivery: &PendingReplyDelivery,
    ) -> KaiResult<()> {
        self.with_transaction("finalize pending reply delivery", |connection| {
            let created_at = Utc::now().to_rfc3339();
            let outcome_json =
                serialize_processed_update_outcome(&ProcessedUpdateOutcome::TextReply {
                    text: delivery.response_text.clone(),
                })?;
            for update_id in &delivery.update_ids {
                connection
                    .execute(
                        "INSERT INTO processed_updates (update_id, created_at, response_text, codex_session_id, outcome_json)
                         VALUES (?1, ?2, ?3, ?4, ?5)
                         ON CONFLICT(update_id) DO UPDATE
                         SET created_at = excluded.created_at,
                             response_text = excluded.response_text,
                             codex_session_id = excluded.codex_session_id,
                             outcome_json = excluded.outcome_json",
                        params![
                            update_id,
                            created_at.as_str(),
                            delivery.response_text.as_str(),
                            delivery.codex_session_id.as_str(),
                            outcome_json.as_str()
                        ],
                    )
                    .map_err(sql_state_error("write processed update during delivery finalize"))?;
            }

            connection
                .execute(
                    "DELETE FROM pending_reply_deliveries WHERE delivery_id = ?1",
                    [delivery.delivery_id.as_str()],
                )
                .map_err(sql_state_error("delete finalized pending reply delivery"))?;

            Ok(())
        })
    }
}
