use super::*;

impl StateStore {
    pub fn pending_turn_preview(&self, limit: usize) -> KaiResult<Vec<PendingTurnView>> {
        Ok(self
            .pending_turn_queue()?
            .into_iter()
            .take(limit)
            .map(|turn| pending_turn_view(&turn))
            .collect())
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

    pub fn migrate_pending_turn_targets(
        &self,
        default_target: &crate::workspace::ExecutionTarget,
    ) -> KaiResult<()> {
        self.with_transaction("migrate pending turn targets", |connection| {
            let mut statement = connection
                .prepare(
                    "SELECT turn_id, sort_key, payload_json
                     FROM pending_turns
                     ORDER BY sort_key ASC, rowid ASC",
                )
                .map_err(sql_state_error("prepare pending turn target migration"))?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                })
                .map_err(sql_state_error("query pending turn target migration"))?;

            for row in rows {
                let (turn_id, sort_key, payload_json) =
                    row.map_err(sql_state_error("read pending turn target migration row"))?;
                let mut turn = deserialize_pending_turn(&payload_json)?;
                if !pending_turn_missing_target(&turn) {
                    continue;
                }
                turn.target = default_target.clone();
                connection
                    .execute(
                        "INSERT INTO pending_turns (turn_id, sort_key, payload_json)
                         VALUES (?1, ?2, ?3)
                         ON CONFLICT(turn_id) DO UPDATE
                         SET sort_key = excluded.sort_key,
                             payload_json = excluded.payload_json",
                        params![
                            turn_id,
                            sort_key,
                            serde_json::to_string(&turn).map_err(|error| {
                                KaiError::new(
                                    ErrorCode::StateError,
                                    format!(
                                        "failed to serialize migrated pending turn payload: {error}"
                                    ),
                                )
                            })?
                        ],
                    )
                    .map_err(sql_state_error("write migrated pending turn payload"))?;
            }

            let active_payload = connection
                .query_row(
                    "SELECT payload_json FROM active_turn WHERE singleton = 1",
                    [],
                    |row| row.get::<_, String>(0),
                )
                .optional()
                .map_err(sql_state_error("load active turn target migration row"))?;
            if let Some(active_payload) = active_payload {
                let mut active = deserialize_active_turn_state(&active_payload)?;
                if pending_turn_missing_target(&active.pending) {
                    active.pending.target = default_target.clone();
                    connection
                        .execute(
                            "UPDATE active_turn SET payload_json = ?1 WHERE singleton = 1",
                            [serialize_active_turn_state(&active)?],
                        )
                        .map_err(sql_state_error("write migrated active turn payload"))?;
                }
            }

            Ok(())
        })
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
