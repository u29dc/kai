use super::*;

impl StateStore {
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
}
