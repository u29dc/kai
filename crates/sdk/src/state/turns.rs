use super::*;

impl StateStore {
    pub fn record_turn(&self, turn: NewTurn<'_>) -> KaiResult<TurnRecord> {
        let created_at = Utc::now().to_rfc3339();
        let attachments_json = serde_json::to_string(turn.attachments).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize attachments: {error}"),
            )
        })?;

        self.connection
            .execute(
                "INSERT INTO turns (
						created_at, role, channel, sender_id, text, codex_session_id, outcome_status, attachments_json
					) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    created_at,
                    turn.role,
                    turn.channel,
                    turn.sender_id,
                    turn.text,
                    turn.codex_session_id,
                    turn.outcome_status,
                    attachments_json
                ],
            )
            .map_err(sql_state_error("insert turn"))?;

        let id = self.connection.last_insert_rowid();
        let record = TurnRecord {
            id,
            created_at: created_at.clone(),
            role: turn.role.to_string(),
            channel: turn.channel.to_string(),
            sender_id: turn.sender_id,
            text: turn.text.to_string(),
            codex_session_id: turn.codex_session_id.map(ToOwned::to_owned),
            outcome_status: turn.outcome_status.map(ToOwned::to_owned),
            attachments: turn.attachments.to_vec(),
        };

        self.append_audit(&AuditRecord {
            timestamp: created_at,
            event: "turn.recorded",
            channel: turn.channel,
            sender_id: turn.sender_id,
            text: turn.text,
            codex_session_id: turn.codex_session_id,
            outcome_status: turn.outcome_status,
            attachments: turn.attachments,
        })?;

        Ok(record)
    }

    pub fn recent_turns(&self, limit: usize) -> KaiResult<Vec<TurnRecord>> {
        let mut statement = self
            .connection
            .prepare(
                "SELECT id, created_at, role, channel, sender_id, text, codex_session_id, outcome_status, attachments_json
                 FROM turns
                 ORDER BY id DESC
                 LIMIT ?1",
            )
            .map_err(sql_state_error("prepare recent turns"))?;

        let rows = statement
            .query_map([limit as i64], |row| {
                let attachments_json: String = row.get(8)?;
                let attachments = serde_json::from_str::<Vec<AttachmentInfo>>(&attachments_json)
                    .unwrap_or_default();

                Ok(TurnRecord {
                    id: row.get(0)?,
                    created_at: row.get(1)?,
                    role: row.get(2)?,
                    channel: row.get(3)?,
                    sender_id: row.get(4)?,
                    text: row.get(5)?,
                    codex_session_id: row.get(6)?,
                    outcome_status: row.get(7)?,
                    attachments,
                })
            })
            .map_err(sql_state_error("query recent turns"))?;

        let mut turns = Vec::new();
        for row in rows {
            turns.push(row.map_err(sql_state_error("read recent turn"))?);
        }
        turns.reverse();
        Ok(turns)
    }

    pub fn append_audit_json(&self, value: &JsonValue) -> KaiResult<()> {
        let serialized = serde_json::to_string(value).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize audit payload: {error}"),
            )
        })?;
        self.append_line(&serialized)
    }

    fn append_audit<T>(&self, value: &T) -> KaiResult<()>
    where
        T: Serialize,
    {
        let serialized = serde_json::to_string(value).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize audit record: {error}"),
            )
        })?;
        self.append_line(&serialized)
    }

    fn append_line(&self, line: &str) -> KaiResult<()> {
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.paths.audit_path)
            .map_err(io_state_error("audit log"))?;
        ensure_private_file(&self.paths.audit_path)?;

        file.write_all(line.as_bytes())
            .map_err(io_state_error("audit log"))?;
        file.write_all(b"\n").map_err(io_state_error("audit log"))?;
        Ok(())
    }
}
