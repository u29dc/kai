use super::*;

pub(super) fn initialize_schema(connection: &Connection) -> KaiResult<()> {
    connection
        .execute_batch(
            "
				CREATE TABLE IF NOT EXISTS kv (
					key TEXT PRIMARY KEY,
					value TEXT NOT NULL
				);
				CREATE TABLE IF NOT EXISTS pending_turns (
					turn_id TEXT PRIMARY KEY,
					sort_key INTEGER NOT NULL,
					payload_json TEXT NOT NULL
				);
					CREATE TABLE IF NOT EXISTS turns (
						id INTEGER PRIMARY KEY AUTOINCREMENT,
						created_at TEXT NOT NULL,
				role TEXT NOT NULL,
				channel TEXT NOT NULL,
				sender_id INTEGER,
				text TEXT NOT NULL,
				codex_session_id TEXT,
					outcome_status TEXT,
					attachments_json TEXT NOT NULL DEFAULT '[]'
				);
				CREATE TABLE IF NOT EXISTS processed_updates (
					update_id INTEGER PRIMARY KEY,
					created_at TEXT NOT NULL,
					response_text TEXT NOT NULL,
					codex_session_id TEXT
				);
                CREATE TABLE IF NOT EXISTS update_failures (
                    update_id INTEGER PRIMARY KEY,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    attempt_count INTEGER NOT NULL,
                    last_error_code TEXT NOT NULL,
                    last_message TEXT NOT NULL
                );
				",
        )
        .map_err(sql_state_error("initialize schema"))?;

    Ok(())
}

pub(super) fn migrate_pending_turn_queue_from_kv(connection: &Connection) -> KaiResult<()> {
    let existing_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM pending_turns", [], |row| row.get(0))
        .map_err(sql_state_error("count pending turns during migration"))?;
    if existing_count > 0 {
        return Ok(());
    }

    let legacy_queue = connection
        .query_row(
            "SELECT value FROM kv WHERE key = 'telegram.pending_turn_queue'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_state_error("load legacy pending turn queue"))?;

    let Some(raw_queue) = legacy_queue else {
        return Ok(());
    };

    let queue = serde_json::from_str::<Vec<PendingTurn>>(&raw_queue).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to deserialize legacy pending turn queue: {error}"),
        )
    })?;

    for (index, turn) in queue.iter().enumerate() {
        let payload_json = serde_json::to_string(turn).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize migrated pending turn payload: {error}"),
            )
        })?;
        connection
            .execute(
                "INSERT OR REPLACE INTO pending_turns (turn_id, sort_key, payload_json)
                 VALUES (?1, ?2, ?3)",
                params![turn.id, index as i64 + 1, payload_json],
            )
            .map_err(sql_state_error("migrate pending turn queue"))?;
    }

    connection
        .execute(
            "DELETE FROM kv WHERE key = 'telegram.pending_turn_queue'",
            [],
        )
        .map_err(sql_state_error("delete legacy pending turn queue"))?;
    Ok(())
}
