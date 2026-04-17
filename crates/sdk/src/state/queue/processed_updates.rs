use super::*;

impl StateStore {
    pub fn get_processed_update(&self, update_id: i64) -> KaiResult<Option<ProcessedUpdate>> {
        let raw = self
            .connection
            .query_row(
                "SELECT update_id, created_at, response_text, codex_session_id, outcome_json
                 FROM processed_updates
                 WHERE update_id = ?1",
                [update_id],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, Option<String>>(3)?,
                        row.get::<_, Option<String>>(4)?,
                    ))
                },
            )
            .optional()
            .map_err(sql_state_error("load processed update"))?;

        raw.map(
            |(update_id, created_at, response_text, codex_session_id, outcome_json)| {
                Ok(ProcessedUpdate {
                    update_id,
                    created_at,
                    response_text: response_text.clone(),
                    codex_session_id,
                    outcome: deserialize_processed_update_outcome(
                        response_text,
                        outcome_json.as_deref(),
                    )?,
                })
            },
        )
        .transpose()
    }

    pub fn set_processed_update(
        &self,
        update_id: i64,
        outcome: &ProcessedUpdateOutcome,
        codex_session_id: Option<&str>,
    ) -> KaiResult<()> {
        let created_at = Utc::now().to_rfc3339();
        let outcome_json = serialize_processed_update_outcome(outcome)?;
        self.connection
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
                    created_at,
                    outcome.response_text(),
                    codex_session_id,
                    outcome_json
                ],
            )
            .map_err(sql_state_error("write processed update"))?;

        Ok(())
    }
}
