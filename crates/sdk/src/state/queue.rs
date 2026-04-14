use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum StoredActiveTurnState {
    Current(ActiveTurnState),
    Legacy(PendingTurn),
}

const ACTIVE_TURN_SINGLETON: i64 = 1;

impl StateStore {
    pub fn get_active_turn_state(&self) -> KaiResult<Option<ActiveTurnState>> {
        let persisted = self
            .connection
            .query_row(
                "SELECT payload_json FROM active_turn WHERE singleton = ?1",
                [ACTIVE_TURN_SINGLETON],
                |row| row.get::<_, String>(0),
            )
            .optional()
            .map_err(sql_state_error("load active turn state"))?;

        if let Some(payload) = persisted {
            return deserialize_active_turn_state(&payload).map(Some);
        }

        Ok(self
            .load_json_state::<StoredActiveTurnState>(ACTIVE_TURN_STATE_KEY)?
            .map(active_turn_state_from_legacy))
    }

    pub fn set_active_turn_state(&self, state: &ActiveTurnState) -> KaiResult<()> {
        let payload_json = serialize_active_turn_state(state)?;
        self.connection
            .execute(
                "INSERT INTO active_turn (singleton, payload_json)
                 VALUES (?1, ?2)
                 ON CONFLICT(singleton) DO UPDATE
                 SET payload_json = excluded.payload_json",
                params![ACTIVE_TURN_SINGLETON, payload_json],
            )
            .map_err(sql_state_error("write active turn state"))?;
        self.delete_value(ACTIVE_TURN_STATE_KEY)
    }

    pub fn clear_active_turn_state(&self) -> KaiResult<()> {
        self.connection
            .execute(
                "DELETE FROM active_turn WHERE singleton = ?1",
                [ACTIVE_TURN_SINGLETON],
            )
            .map_err(sql_state_error("delete active turn state"))?;
        self.remove_json_state(ACTIVE_TURN_STATE_KEY)
    }

    pub fn claim_next_pending_turn(&self) -> KaiResult<Option<ActiveTurnState>> {
        self.with_transaction("claim next pending turn", |connection| {
            let next = connection
                .query_row(
                    "SELECT turn_id, payload_json
                     FROM pending_turns
                     ORDER BY sort_key ASC, rowid ASC
                     LIMIT 1",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
                )
                .optional()
                .map_err(sql_state_error("load next pending turn for claim"))?;

            let Some((turn_id, payload_json)) = next else {
                return Ok(None);
            };

            let active = deserialize_active_turn_state(&payload_json)
                .or_else(|_| deserialize_pending_turn(&payload_json).map(Into::into))?;

            connection
                .execute("DELETE FROM pending_turns WHERE turn_id = ?1", [turn_id])
                .map_err(sql_state_error("delete claimed pending turn"))?;
            connection
                .execute(
                    "INSERT INTO active_turn (singleton, payload_json)
                     VALUES (?1, ?2)
                     ON CONFLICT(singleton) DO UPDATE
                     SET payload_json = excluded.payload_json",
                    params![ACTIVE_TURN_SINGLETON, serialize_active_turn_state(&active)?],
                )
                .map_err(sql_state_error("persist claimed active turn"))?;

            Ok(Some(active))
        })
    }

    pub fn recover_active_turn(&self) -> KaiResult<Option<ActiveTurnState>> {
        self.with_transaction("recover active turn", |connection| {
            let raw = connection
                .query_row(
                    "SELECT payload_json FROM active_turn WHERE singleton = ?1",
                    [ACTIVE_TURN_SINGLETON],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_state_error("load active turn for recovery"))?;

            let Some(payload_json) = raw else {
                return Ok(None);
            };

            let active = deserialize_active_turn_state(&payload_json)?;
            let exists = connection
                .query_row(
                    "SELECT 1 FROM pending_turns WHERE turn_id = ?1 LIMIT 1",
                    [active.pending.id.as_str()],
                    |_| Ok(()),
                )
                .optional()
                .map_err(sql_state_error("check recovered pending turn existence"))?
                .is_some();

            if !exists {
                let pending_payload = serde_json::to_string(&active.pending).map_err(|error| {
                    KaiError::new(
                        ErrorCode::StateError,
                        format!("failed to serialize recovered pending turn payload: {error}"),
                    )
                })?;
                let sort_key: i64 = connection
                    .query_row(
                        "SELECT COALESCE(MIN(sort_key), 1) - 1 FROM pending_turns",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(sql_state_error("compute recovered pending turn sort key"))?;
                connection
                    .execute(
                        "INSERT INTO pending_turns (turn_id, sort_key, payload_json)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(turn_id) DO UPDATE
                         SET sort_key = excluded.sort_key,
                             payload_json = excluded.payload_json",
                        params![active.pending.id.as_str(), sort_key, pending_payload],
                    )
                    .map_err(sql_state_error("restore recovered pending turn"))?;
            }

            connection
                .execute(
                    "DELETE FROM active_turn WHERE singleton = ?1",
                    [ACTIVE_TURN_SINGLETON],
                )
                .map_err(sql_state_error("clear recovered active turn"))?;

            Ok(Some(active))
        })
    }

    pub fn pending_turn_preview(&self, limit: usize) -> KaiResult<Vec<PendingTurnView>> {
        Ok(self
            .pending_turn_queue()?
            .into_iter()
            .take(limit)
            .map(|turn| pending_turn_view(&turn))
            .collect())
    }

    pub fn get_active_pending_turn(&self) -> KaiResult<Option<PendingTurn>> {
        Ok(self.get_active_turn_state()?.map(|state| state.pending))
    }

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
            for update_id in &delivery.update_ids {
                connection
                    .execute(
                        "INSERT INTO processed_updates (update_id, created_at, response_text, codex_session_id)
                         VALUES (?1, ?2, ?3, ?4)
                         ON CONFLICT(update_id) DO UPDATE
                         SET created_at = excluded.created_at,
                             response_text = excluded.response_text,
                             codex_session_id = excluded.codex_session_id",
                        params![
                            update_id,
                            created_at.as_str(),
                            delivery.response_text.as_str(),
                            delivery.codex_session_id.as_str()
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

    pub fn pending_turn_queue(&self) -> KaiResult<Vec<PendingTurn>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT payload_json
                 FROM pending_turns
                 ORDER BY sort_key ASC, rowid ASC",
            )
            .map_err(sql_state_error("prepare pending turn queue"))?;

        let rows = statement
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(sql_state_error("query pending turn queue"))?;

        let mut queue = Vec::new();
        for row in rows {
            let payload = row.map_err(sql_state_error("read pending turn queue row"))?;
            let turn = serde_json::from_str::<PendingTurn>(&payload).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to deserialize pending turn payload: {error}"),
                )
            })?;
            queue.push(turn);
        }
        Ok(queue)
    }

    pub fn pending_turn_queue_len(&self) -> KaiResult<usize> {
        let count: i64 = self
            .connection
            .query_row("SELECT COUNT(*) FROM pending_turns", [], |row| row.get(0))
            .map_err(sql_state_error("count pending turns"))?;
        Ok(count as usize)
    }

    pub fn enqueue_pending_turn(&self, turn: &PendingTurn) -> KaiResult<usize> {
        if !self.pending_turn_exists(&turn.id)?
            && self.pending_turn_queue_len()? >= MAX_PENDING_TURNS
        {
            return Err(KaiError::blocked_prerequisite(format!(
                "pending turn queue is full (limit: {MAX_PENDING_TURNS})"
            ))
            .with_hint(
                "wait for the queue to drain or cancel the running turn before adding more",
            ));
        }
        let sort_key: i64 = self
            .connection
            .query_row(
                "SELECT COALESCE(MAX(sort_key), 0) + 1 FROM pending_turns",
                [],
                |row| row.get(0),
            )
            .map_err(sql_state_error("compute pending turn enqueue key"))?;
        self.insert_pending_turn(turn, sort_key)?;
        self.pending_turn_queue_len()
    }

    pub fn prepend_pending_turn(&self, turn: &PendingTurn) -> KaiResult<usize> {
        if !self.pending_turn_exists(&turn.id)?
            && self.pending_turn_queue_len()? >= MAX_PENDING_TURNS
        {
            return Err(KaiError::blocked_prerequisite(format!(
                "pending turn queue is full (limit: {MAX_PENDING_TURNS})"
            ))
            .with_hint(
                "wait for the queue to drain or cancel the running turn before adding more",
            ));
        }
        let sort_key: i64 = self
            .connection
            .query_row(
                "SELECT COALESCE(MIN(sort_key), 1) - 1 FROM pending_turns",
                [],
                |row| row.get(0),
            )
            .map_err(sql_state_error("compute pending turn prepend key"))?;
        self.insert_pending_turn(turn, sort_key)?;
        self.pending_turn_queue_len()
    }

    pub fn pop_pending_turn(&self) -> KaiResult<Option<PendingTurn>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT turn_id, payload_json
                 FROM pending_turns
                 ORDER BY sort_key ASC, rowid ASC
                 LIMIT 1",
            )
            .map_err(sql_state_error("prepare pop pending turn"))?;

        let next = statement
            .query_row([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .optional()
            .map_err(sql_state_error("load next pending turn"))?;

        let Some((turn_id, payload_json)) = next else {
            return Ok(None);
        };

        self.connection
            .execute("DELETE FROM pending_turns WHERE turn_id = ?1", [turn_id])
            .map_err(sql_state_error("delete popped pending turn"))?;

        let turn = serde_json::from_str::<PendingTurn>(&payload_json).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to deserialize pending turn payload: {error}"),
            )
        })?;
        Ok(Some(turn))
    }

    pub fn clear_pending_turn_queue(&self) -> KaiResult<()> {
        self.connection
            .execute("DELETE FROM pending_turns", [])
            .map_err(sql_state_error("clear pending turn queue"))?;
        self.delete_value("telegram.pending_turn_queue")
    }

    pub fn get_processed_update(&self, update_id: i64) -> KaiResult<Option<ProcessedUpdate>> {
        self.connection
            .query_row(
                "SELECT update_id, created_at, response_text, codex_session_id
                 FROM processed_updates
                 WHERE update_id = ?1",
                [update_id],
                |row| {
                    Ok(ProcessedUpdate {
                        update_id: row.get(0)?,
                        created_at: row.get(1)?,
                        response_text: row.get(2)?,
                        codex_session_id: row.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(sql_state_error("load processed update"))
    }

    pub fn set_processed_update(
        &self,
        update_id: i64,
        response_text: &str,
        codex_session_id: Option<&str>,
    ) -> KaiResult<()> {
        let created_at = Utc::now().to_rfc3339();
        self.connection
            .execute(
                "INSERT INTO processed_updates (update_id, created_at, response_text, codex_session_id)
                 VALUES (?1, ?2, ?3, ?4)
                 ON CONFLICT(update_id) DO UPDATE
                 SET created_at = excluded.created_at,
                     response_text = excluded.response_text,
                     codex_session_id = excluded.codex_session_id",
                params![update_id, created_at, response_text, codex_session_id],
            )
            .map_err(sql_state_error("write processed update"))?;

        Ok(())
    }

    pub fn get_update_failure(&self, update_id: i64) -> KaiResult<Option<UpdateFailureState>> {
        self.connection
            .query_row(
                "SELECT update_id, created_at, updated_at, attempt_count, last_error_code, last_message
                 FROM update_failures
                 WHERE update_id = ?1",
                [update_id],
                |row| {
                    Ok(UpdateFailureState {
                        update_id: row.get(0)?,
                        created_at: row.get(1)?,
                        updated_at: row.get(2)?,
                        attempt_count: row.get(3)?,
                        last_error_code: row.get(4)?,
                        last_message: row.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(sql_state_error("load update failure"))
    }

    pub fn record_update_failure(
        &self,
        update_id: i64,
        error: &KaiError,
    ) -> KaiResult<UpdateFailureState> {
        let now = Utc::now().to_rfc3339();
        let prior = self.get_update_failure(update_id)?;
        let attempt_count = prior
            .as_ref()
            .map(|failure| failure.attempt_count.saturating_add(1))
            .unwrap_or(1);
        let created_at = prior
            .as_ref()
            .map(|failure| failure.created_at.clone())
            .unwrap_or_else(|| now.clone());
        let last_error_code = serde_json::to_string(&error.code)
            .unwrap_or_else(|_| "\"runtime_error\"".to_string())
            .trim_matches('"')
            .to_string();
        let last_message = redact_text(&error.message);

        self.connection
            .execute(
                "INSERT INTO update_failures (
                    update_id, created_at, updated_at, attempt_count, last_error_code, last_message
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)
                 ON CONFLICT(update_id) DO UPDATE
                 SET updated_at = excluded.updated_at,
                     attempt_count = excluded.attempt_count,
                     last_error_code = excluded.last_error_code,
                     last_message = excluded.last_message",
                params![
                    update_id,
                    created_at,
                    now,
                    attempt_count,
                    last_error_code,
                    last_message
                ],
            )
            .map_err(sql_state_error("write update failure"))?;

        self.get_update_failure(update_id)?.ok_or_else(|| {
            KaiError::new(
                ErrorCode::StateError,
                format!("missing persisted update failure for update {update_id}"),
            )
        })
    }

    pub fn clear_update_failure(&self, update_id: i64) -> KaiResult<()> {
        self.connection
            .execute(
                "DELETE FROM update_failures WHERE update_id = ?1",
                [update_id],
            )
            .map_err(sql_state_error("delete update failure"))?;
        Ok(())
    }

    fn insert_pending_turn(&self, turn: &PendingTurn, sort_key: i64) -> KaiResult<()> {
        let payload_json = serde_json::to_string(turn).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize pending turn payload: {error}"),
            )
        })?;
        self.connection
            .execute(
                "INSERT INTO pending_turns (turn_id, sort_key, payload_json)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(turn_id) DO UPDATE
                 SET sort_key = excluded.sort_key,
                     payload_json = excluded.payload_json",
                params![turn.id, sort_key, payload_json],
            )
            .map_err(sql_state_error("insert pending turn"))?;
        Ok(())
    }

    fn pending_turn_exists(&self, turn_id: &str) -> KaiResult<bool> {
        Ok(self
            .connection
            .query_row(
                "SELECT 1 FROM pending_turns WHERE turn_id = ?1 LIMIT 1",
                [turn_id],
                |_| Ok(()),
            )
            .optional()
            .map_err(sql_state_error("check pending turn existence"))?
            .is_some())
    }
}

fn active_turn_state_from_legacy(value: StoredActiveTurnState) -> ActiveTurnState {
    match value {
        StoredActiveTurnState::Current(current) => current,
        StoredActiveTurnState::Legacy(pending) => pending.into(),
    }
}

fn serialize_active_turn_state(state: &ActiveTurnState) -> KaiResult<String> {
    serde_json::to_string(state).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to serialize active turn state: {error}"),
        )
    })
}

fn deserialize_active_turn_state(payload: &str) -> KaiResult<ActiveTurnState> {
    serde_json::from_str::<StoredActiveTurnState>(payload)
        .map(active_turn_state_from_legacy)
        .map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to deserialize active turn state: {error}"),
            )
        })
}

fn deserialize_pending_turn(payload: &str) -> KaiResult<PendingTurn> {
    serde_json::from_str::<PendingTurn>(payload).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to deserialize pending turn payload: {error}"),
        )
    })
}
