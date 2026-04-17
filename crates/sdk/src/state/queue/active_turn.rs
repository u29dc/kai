use super::*;

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

    pub fn get_active_pending_turn(&self) -> KaiResult<Option<PendingTurn>> {
        Ok(self.get_active_turn_state()?.map(|state| state.pending))
    }
}
