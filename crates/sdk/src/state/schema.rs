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
                    CREATE TABLE IF NOT EXISTS active_turn (
                        singleton INTEGER PRIMARY KEY CHECK (singleton = 1),
                        payload_json TEXT NOT NULL
                    );
						CREATE TABLE IF NOT EXISTS turns (
							id INTEGER PRIMARY KEY AUTOINCREMENT,
							created_at TEXT NOT NULL,
					provider TEXT NOT NULL DEFAULT 'codex',
					workspace_id TEXT NOT NULL DEFAULT '',
					working_dir TEXT NOT NULL DEFAULT '',
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
                    CREATE TABLE IF NOT EXISTS pending_reply_deliveries (
                        delivery_id TEXT PRIMARY KEY,
                        turn_id TEXT NOT NULL UNIQUE,
                        chat_id INTEGER NOT NULL,
                        response_text TEXT NOT NULL,
                        codex_session_id TEXT NOT NULL,
                        status_message_id INTEGER,
                        update_ids_json TEXT NOT NULL,
                        attempts INTEGER NOT NULL DEFAULT 0,
                        created_at TEXT NOT NULL,
                        next_chunk_index INTEGER NOT NULL DEFAULT 0,
                        sent_message_ids_json TEXT NOT NULL DEFAULT '[]'
                    );
					",
        )
        .map_err(sql_state_error("initialize schema"))?;
    ensure_column(
        connection,
        "turns",
        "provider",
        "TEXT NOT NULL DEFAULT 'codex'",
    )?;
    ensure_column(
        connection,
        "turns",
        "workspace_id",
        "TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        connection,
        "turns",
        "working_dir",
        "TEXT NOT NULL DEFAULT ''",
    )?;

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

pub(super) fn migrate_active_turn_state_from_kv(connection: &Connection) -> KaiResult<()> {
    let existing_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM active_turn", [], |row| row.get(0))
        .map_err(sql_state_error("count active turn rows during migration"))?;
    if existing_count > 0 {
        return Ok(());
    }

    let legacy_active = connection
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            [ACTIVE_TURN_STATE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_state_error("load legacy active turn state"))?;

    let Some(raw_active) = legacy_active else {
        return Ok(());
    };

    let active = serde_json::from_str::<super::queue::StoredActiveTurnState>(&raw_active)
        .map(|value| match value {
            super::queue::StoredActiveTurnState::Current(current) => current,
            super::queue::StoredActiveTurnState::Legacy(pending) => pending.into(),
        })
        .map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to deserialize legacy active turn state: {error}"),
            )
        })?;

    let payload_json = serde_json::to_string(&active).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to serialize migrated active turn state: {error}"),
        )
    })?;

    connection
        .execute(
            "INSERT OR REPLACE INTO active_turn (singleton, payload_json) VALUES (1, ?1)",
            [payload_json],
        )
        .map_err(sql_state_error("migrate active turn state"))?;
    connection
        .execute("DELETE FROM kv WHERE key = ?1", [ACTIVE_TURN_STATE_KEY])
        .map_err(sql_state_error("delete legacy active turn state"))?;
    Ok(())
}

pub(super) fn migrate_pending_reply_deliveries_from_kv(connection: &Connection) -> KaiResult<()> {
    let existing_count: i64 = connection
        .query_row("SELECT COUNT(*) FROM pending_reply_deliveries", [], |row| {
            row.get(0)
        })
        .map_err(sql_state_error(
            "count pending reply deliveries during migration",
        ))?;
    if existing_count > 0 {
        return Ok(());
    }

    let legacy_deliveries = connection
        .query_row(
            "SELECT value FROM kv WHERE key = ?1",
            [PENDING_REPLY_DELIVERIES_STATE_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(sql_state_error("load legacy pending reply deliveries"))?;

    let Some(raw_deliveries) = legacy_deliveries else {
        return Ok(());
    };

    let deliveries =
        serde_json::from_str::<Vec<PendingReplyDelivery>>(&raw_deliveries).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to deserialize legacy pending reply deliveries: {error}"),
            )
        })?;

    for delivery in deliveries {
        let update_ids_json = serde_json::to_string(&delivery.update_ids).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize migrated delivery update ids: {error}"),
            )
        })?;
        let sent_message_ids_json =
            serde_json::to_string(&delivery.sent_message_ids).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to serialize migrated delivery message ids: {error}"),
                )
            })?;

        connection
            .execute(
                "INSERT OR REPLACE INTO pending_reply_deliveries (
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
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
                params![
                    delivery.delivery_id,
                    delivery.turn_id,
                    delivery.chat_id,
                    delivery.response_text,
                    delivery.codex_session_id,
                    delivery.status_message_id,
                    update_ids_json,
                    delivery.attempts,
                    delivery.created_at,
                    delivery.next_chunk_index as i64,
                    sent_message_ids_json
                ],
            )
            .map_err(sql_state_error("migrate pending reply delivery"))?;
    }

    connection
        .execute(
            "DELETE FROM kv WHERE key = ?1",
            [PENDING_REPLY_DELIVERIES_STATE_KEY],
        )
        .map_err(sql_state_error("delete legacy pending reply deliveries"))?;
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    definition: &str,
) -> KaiResult<()> {
    let mut statement = connection
        .prepare(&format!("PRAGMA table_info({table})"))
        .map_err(sql_state_error("inspect schema"))?;
    let rows = statement
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(sql_state_error("inspect schema"))?;
    for row in rows {
        if row.map_err(sql_state_error("inspect schema"))? == column {
            return Ok(());
        }
    }

    connection
        .execute(
            &format!("ALTER TABLE {table} ADD COLUMN {column} {definition}"),
            [],
        )
        .map_err(sql_state_error("migrate schema"))?;
    Ok(())
}
