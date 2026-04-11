use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

#[derive(Debug, Clone)]
pub struct StatePaths {
    pub attachments_dir: PathBuf,
    pub logs_dir: PathBuf,
    pub state_dir: PathBuf,
    pub db_path: PathBuf,
    pub audit_path: PathBuf,
}

#[derive(Debug)]
pub struct StateStore {
    connection: Connection,
    paths: StatePaths,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentInfo {
    pub kind: String,
    pub path: String,
    pub original_name: Option<String>,
    pub mime_type: Option<String>,
    pub bytes: u64,
    pub checksum_blake3: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRecord {
    pub id: i64,
    pub created_at: String,
    pub role: String,
    pub channel: String,
    pub sender_id: Option<i64>,
    pub text: String,
    pub codex_session_id: Option<String>,
    pub outcome_status: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

pub struct NewTurn<'a> {
    pub role: &'a str,
    pub channel: &'a str,
    pub sender_id: Option<i64>,
    pub text: &'a str,
    pub codex_session_id: Option<&'a str>,
    pub outcome_status: Option<&'a str>,
    pub attachments: &'a [AttachmentInfo],
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditRecord<'a> {
    pub timestamp: String,
    pub event: &'a str,
    pub channel: &'a str,
    pub sender_id: Option<i64>,
    pub text: &'a str,
    pub codex_session_id: Option<&'a str>,
    pub outcome_status: Option<&'a str>,
    pub attachments: &'a [AttachmentInfo],
}

impl StateStore {
    pub fn open(config: &LoadedConfig) -> KaiResult<Self> {
        let paths = state_paths(config);
        fs::create_dir_all(&paths.attachments_dir).map_err(io_state_error("attachments"))?;
        fs::create_dir_all(&paths.logs_dir).map_err(io_state_error("logs"))?;
        fs::create_dir_all(&paths.state_dir).map_err(io_state_error("state"))?;

        let connection =
            Connection::open(&paths.db_path).map_err(sql_state_error("open database"))?;
        initialize_schema(&connection)?;

        Ok(Self { connection, paths })
    }

    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    pub fn session_view(&self) -> KaiResult<crate::contract::SessionView> {
        Ok(crate::contract::SessionView {
            owner_user_id: self.get_owner_user_id()?,
            active_session_id: self.get_active_session_id()?,
            pending_pair_code: self.get_pending_pair_code()?,
            update_offset: self.get_update_offset()?,
        })
    }

    pub fn get_owner_user_id(&self) -> KaiResult<Option<i64>> {
        self.get_json_value("telegram.owner_user_id")
    }

    pub fn set_owner_user_id(&self, user_id: i64) -> KaiResult<()> {
        self.set_json_value("telegram.owner_user_id", &user_id)
    }

    pub fn get_pending_pair_code(&self) -> KaiResult<Option<String>> {
        self.get_json_value("telegram.pending_pair_code")
    }

    pub fn set_pending_pair_code(&self, code: &str) -> KaiResult<()> {
        self.set_json_value("telegram.pending_pair_code", &code)
    }

    pub fn clear_pending_pair_code(&self) -> KaiResult<()> {
        self.delete_value("telegram.pending_pair_code")
    }

    pub fn get_active_session_id(&self) -> KaiResult<Option<String>> {
        self.get_json_value("codex.active_session_id")
    }

    pub fn set_active_session_id(&self, session_id: &str) -> KaiResult<()> {
        self.set_json_value("codex.active_session_id", &session_id)
    }

    pub fn clear_active_session_id(&self) -> KaiResult<()> {
        self.delete_value("codex.active_session_id")
    }

    pub fn get_update_offset(&self) -> KaiResult<i64> {
        Ok(self
            .get_json_value::<i64>("telegram.update_offset")?
            .unwrap_or_default())
    }

    pub fn set_update_offset(&self, offset: i64) -> KaiResult<()> {
        self.set_json_value("telegram.update_offset", &offset)
    }

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

    fn get_json_value<T>(&self, key: &str) -> KaiResult<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let raw = self
            .connection
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(sql_state_error("load kv value"))?;

        raw.map(|value| {
            serde_json::from_str::<T>(&value).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to deserialize state value `{key}`: {error}"),
                )
            })
        })
        .transpose()
    }

    fn set_json_value<T>(&self, key: &str, value: &T) -> KaiResult<()>
    where
        T: Serialize,
    {
        let raw = serde_json::to_string(value).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize state value `{key}`: {error}"),
            )
        })?;

        self.connection
            .execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
				 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, raw],
            )
            .map_err(sql_state_error("write kv value"))?;

        Ok(())
    }

    fn delete_value(&self, key: &str) -> KaiResult<()> {
        self.connection
            .execute("DELETE FROM kv WHERE key = ?1", [key])
            .map_err(sql_state_error("delete kv value"))?;
        Ok(())
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

        file.write_all(line.as_bytes())
            .map_err(io_state_error("audit log"))?;
        file.write_all(b"\n").map_err(io_state_error("audit log"))?;
        Ok(())
    }
}

pub fn state_paths(config: &LoadedConfig) -> StatePaths {
    let root_app = Path::new(&config.values.paths.root_app);
    let state_dir = root_app.join("state");
    let logs_dir = root_app.join("logs");
    let attachments_dir = root_app.join("attachments");

    StatePaths {
        attachments_dir,
        logs_dir: logs_dir.clone(),
        state_dir: state_dir.clone(),
        db_path: state_dir.join("kai.sqlite"),
        audit_path: logs_dir.join("turns.jsonl"),
    }
}

fn initialize_schema(connection: &Connection) -> KaiResult<()> {
    connection
        .execute_batch(
            "
			CREATE TABLE IF NOT EXISTS kv (
				key TEXT PRIMARY KEY,
				value TEXT NOT NULL
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
			",
        )
        .map_err(sql_state_error("initialize schema"))?;

    Ok(())
}

fn io_state_error(target: &'static str) -> impl Fn(std::io::Error) -> KaiError {
    move |error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to prepare {target}: {error}"),
        )
    }
}

fn sql_state_error(action: &'static str) -> impl Fn(rusqlite::Error) -> KaiError {
    move |error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to {action}: {error}"),
        )
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, LoadedConfig,
        PathsConfig, RunnerConfig, TelegramConfig,
    };

    #[test]
    fn state_round_trips_pair_code() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");

        let config = LoadedConfig {
            config_path: root_app.join("config.toml"),
            config_exists: false,
            values: Config {
                agent: AgentConfig {
                    timezone: "Europe/London".to_string(),
                },
                channel: ChannelConfig {
                    telegram: TelegramConfig {
                        enabled: true,
                        bot_token_env: "KAI_TELEGRAM_BOT_TOKEN".to_string(),
                        owner_user_id: None,
                    },
                },
                paths: PathsConfig {
                    root_app: root_app.display().to_string(),
                    root_work: root_work.display().to_string(),
                },
                runner: RunnerConfig {
                    codex: CodexConfig {
                        binary: "codex".to_string(),
                        override_config: None,
                    },
                },
                context_files: ContextFilesConfig {
                    soul: root_app.join("SOUL.md").display().to_string(),
                    memory: root_app.join("MEMORY.md").display().to_string(),
                    todo: root_app.join("TODO.md").display().to_string(),
                },
            },
        };

        let store = StateStore::open(&config).expect("state store");
        store
            .set_pending_pair_code("ABC12345")
            .expect("set pair code");

        assert_eq!(
            store.get_pending_pair_code().expect("load pair code"),
            Some("ABC12345".to_string())
        );
    }
}
