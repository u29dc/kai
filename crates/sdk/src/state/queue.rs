use super::*;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum StoredActiveTurnState {
    Current(ActiveTurnState),
    Legacy(PendingTurn),
}

impl StateStore {
    pub fn get_active_turn_state(&self) -> KaiResult<Option<ActiveTurnState>> {
        Ok(self
            .load_json_state::<StoredActiveTurnState>(ACTIVE_TURN_STATE_KEY)?
            .map(|value| match value {
                StoredActiveTurnState::Current(current) => current,
                StoredActiveTurnState::Legacy(pending) => pending.into(),
            }))
    }

    pub fn set_active_turn_state(&self, state: &ActiveTurnState) -> KaiResult<()> {
        self.store_json_state(ACTIVE_TURN_STATE_KEY, state)
    }

    pub fn clear_active_turn_state(&self) -> KaiResult<()> {
        self.remove_json_state(ACTIVE_TURN_STATE_KEY)
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
        Ok(self
            .load_json_state::<Vec<PendingReplyDelivery>>(PENDING_REPLY_DELIVERIES_STATE_KEY)?
            .unwrap_or_default()
            .len())
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
