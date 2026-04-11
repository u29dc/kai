use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::LoadedConfig;
use crate::context::ContextSnapshot;
use crate::contract::PendingPairingView;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime_fs::{ensure_private_dir, ensure_private_file};

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayTurn {
    pub id: i64,
    pub created_at: String,
    pub role: String,
    pub text_excerpt: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayAttachmentRef {
    pub kind: String,
    pub path: String,
    pub original_name: Option<String>,
    pub bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayPackage {
    pub updated_at: String,
    pub summary: String,
    pub context: Vec<ContextSnapshot>,
    pub recent_turns: Vec<ReplayTurn>,
    pub attachment_refs: Vec<ReplayAttachmentRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessedUpdate {
    pub update_id: i64,
    pub created_at: String,
    pub response_text: String,
    pub codex_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairing {
    pub code_hash_blake3: String,
    pub created_at: String,
    pub expires_at: String,
    pub remaining_attempts: u8,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateFailureState {
    pub update_id: i64,
    pub created_at: String,
    pub updated_at: String,
    pub attempt_count: u32,
    pub last_error_code: String,
    pub last_message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentCleanupResult {
    pub scanned_files: usize,
    pub removed_partial_files: usize,
    pub removed_stale_files: usize,
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
        ensure_private_dir(Path::new(&config.values.paths.root_app))?;
        ensure_private_dir(&paths.attachments_dir)?;
        ensure_private_dir(&paths.logs_dir)?;
        ensure_private_dir(&paths.state_dir)?;

        let connection =
            Connection::open(&paths.db_path).map_err(sql_state_error("open database"))?;
        ensure_private_file(&paths.db_path)?;
        ensure_private_file(&paths.audit_path)?;
        initialize_schema(&connection)?;

        Ok(Self { connection, paths })
    }

    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    pub fn session_view(&self) -> KaiResult<crate::contract::SessionView> {
        Ok(crate::contract::SessionView {
            owner_user_id: self.get_owner_user_id()?,
            owner_chat_id: self.get_owner_chat_id()?,
            active_session_id: self.get_active_session_id()?,
            pending_pairing: self.pending_pairing_view()?,
            update_offset: self.get_update_offset()?,
        })
    }

    pub fn get_owner_user_id(&self) -> KaiResult<Option<i64>> {
        self.get_json_value("telegram.owner_user_id")
    }

    pub fn set_owner_user_id(&self, user_id: i64) -> KaiResult<()> {
        self.set_json_value("telegram.owner_user_id", &user_id)
    }

    pub fn get_owner_chat_id(&self) -> KaiResult<Option<i64>> {
        self.get_json_value("telegram.owner_chat_id")
    }

    pub fn set_owner_chat_id(&self, chat_id: i64) -> KaiResult<()> {
        self.set_json_value("telegram.owner_chat_id", &chat_id)
    }

    pub fn get_pending_pairing(&self) -> KaiResult<Option<PendingPairing>> {
        self.get_json_value("telegram.pending_pairing")
    }

    pub fn set_pending_pairing(&self, pairing: &PendingPairing) -> KaiResult<()> {
        self.set_json_value("telegram.pending_pairing", pairing)?;
        self.delete_value("telegram.pending_pair_code")?;
        Ok(())
    }

    pub fn clear_pending_pairing(&self) -> KaiResult<()> {
        self.delete_value("telegram.pending_pairing")?;
        self.delete_value("telegram.pending_pair_code")
    }

    pub fn pending_pairing_view(&self) -> KaiResult<Option<PendingPairingView>> {
        Ok(self
            .get_pending_pairing()?
            .map(|pairing| PendingPairingView {
                expires_at: pairing.expires_at,
                remaining_attempts: pairing.remaining_attempts,
            }))
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

    pub fn get_replay_package(&self) -> KaiResult<Option<ReplayPackage>> {
        self.get_json_value("codex.replay_package")
    }

    pub fn set_replay_package(&self, replay_package: &ReplayPackage) -> KaiResult<()> {
        self.set_json_value("codex.replay_package", replay_package)
    }

    pub fn clear_replay_package(&self) -> KaiResult<()> {
        self.delete_value("codex.replay_package")
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
                    error.message
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

    pub fn cleanup_staged_attachments(
        &self,
        retention: Duration,
    ) -> KaiResult<AttachmentCleanupResult> {
        let mut scanned_files = 0_usize;
        let mut removed_partial_files = 0_usize;
        let mut removed_stale_files = 0_usize;

        let entries = match fs::read_dir(&self.paths.attachments_dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return Ok(AttachmentCleanupResult {
                    scanned_files,
                    removed_partial_files,
                    removed_stale_files,
                });
            }
            Err(error) => {
                return Err(KaiError::new(
                    ErrorCode::IoError,
                    format!("failed to read attachments directory: {error}"),
                ));
            }
        };

        let now = SystemTime::now();
        for entry in entries {
            let entry = entry.map_err(io_state_error("scan attachments directory"))?;
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            scanned_files += 1;

            if path.extension().and_then(|value| value.to_str()) == Some("part") {
                fs::remove_file(&path).map_err(io_state_error("remove partial attachment"))?;
                removed_partial_files += 1;
                continue;
            }

            let metadata = fs::metadata(&path).map_err(io_state_error("inspect attachment"))?;
            let modified = metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH);
            let age = now
                .duration_since(modified)
                .unwrap_or_else(|_| Duration::from_secs(0));

            if age >= retention {
                fs::remove_file(&path).map_err(io_state_error("remove stale attachment"))?;
                removed_stale_files += 1;
            }
        }

        Ok(AttachmentCleanupResult {
            scanned_files,
            removed_partial_files,
            removed_stale_files,
        })
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
        ensure_private_file(&self.paths.audit_path)?;

        file.write_all(line.as_bytes())
            .map_err(io_state_error("audit log"))?;
        file.write_all(b"\n").map_err(io_state_error("audit log"))?;
        Ok(())
    }
}

impl PendingPairing {
    pub fn issue(code: &str, ttl_minutes: i64, max_attempts: u8) -> Self {
        let created_at = Utc::now();
        let expires_at = created_at + ChronoDuration::minutes(ttl_minutes);

        Self {
            code_hash_blake3: blake3::hash(code.as_bytes()).to_hex().to_string(),
            created_at: created_at.to_rfc3339(),
            expires_at: expires_at.to_rfc3339(),
            remaining_attempts: max_attempts,
        }
    }

    pub fn is_expired(&self) -> bool {
        chrono::DateTime::parse_from_rfc3339(&self.expires_at)
            .map(|value| value.with_timezone(&Utc) <= Utc::now())
            .unwrap_or(true)
    }

    pub fn verify(&self, code: &str) -> bool {
        self.code_hash_blake3 == blake3::hash(code.as_bytes()).to_hex().to_string()
    }

    pub fn consume_failed_attempt(&mut self) {
        self.remaining_attempts = self.remaining_attempts.saturating_sub(1);
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
    use std::fs;

    use tempfile::tempdir;

    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, LoadedConfig,
        PathsConfig, RunnerConfig, TelegramConfig,
    };

    fn test_config(root_app: &Path, root_work: &Path) -> LoadedConfig {
        LoadedConfig {
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
        }
    }

    #[test]
    fn state_round_trips_pending_pairing() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);

        let store = StateStore::open(&config).expect("state store");
        store
            .set_pending_pairing(&PendingPairing::issue("ABC12345", 10, 5))
            .expect("set pending pairing");

        let pairing = store
            .get_pending_pairing()
            .expect("load pending pairing")
            .expect("pairing exists");
        assert_eq!(pairing.remaining_attempts, 5);
        assert!(pairing.verify("ABC12345"));
    }

    #[test]
    fn processed_update_round_trips() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);

        let store = StateStore::open(&config).expect("state store");
        store
            .set_processed_update(42, "cached reply", Some("session-1"))
            .expect("set processed update");

        let processed = store
            .get_processed_update(42)
            .expect("load processed update")
            .expect("processed update must exist");
        assert_eq!(processed.update_id, 42);
        assert_eq!(processed.response_text, "cached reply");
        assert_eq!(processed.codex_session_id.as_deref(), Some("session-1"));
    }

    #[test]
    fn update_failure_round_trips_and_clears() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);

        let store = StateStore::open(&config).expect("state store");
        let first = store
            .record_update_failure(77, &KaiError::invalid_argument("bad attachment"))
            .expect("record first failure");
        assert_eq!(first.attempt_count, 1);
        assert_eq!(first.last_error_code, "invalid_argument");

        let second = store
            .record_update_failure(
                77,
                &KaiError::new(ErrorCode::RuntimeError, "temporary backend issue"),
            )
            .expect("record second failure");
        assert_eq!(second.attempt_count, 2);
        assert_eq!(second.last_error_code, "runtime_error");

        store.clear_update_failure(77).expect("clear failure");
        assert!(
            store
                .get_update_failure(77)
                .expect("load failure")
                .is_none()
        );
    }

    #[test]
    fn cleanup_staged_attachments_removes_partial_and_stale_files() {
        let tempdir = tempdir().expect("tempdir");
        let root_app = tempdir.path().join("kai-home");
        let root_work = tempdir.path().join("work");
        let config = test_config(&root_app, &root_work);

        let store = StateStore::open(&config).expect("state store");
        let partial = store.paths().attachments_dir.join("dangling.bin.part");
        let stale = store.paths().attachments_dir.join("stale.txt");
        let fresh = store.paths().attachments_dir.join("fresh.txt");

        fs::write(&partial, b"partial").expect("write partial");
        fs::write(&stale, b"old").expect("write stale");
        fs::write(&fresh, b"new").expect("write fresh");

        let old_time = filetime::FileTime::from_unix_time(1, 0);
        filetime::set_file_mtime(&stale, old_time).expect("age stale file");

        let result = store
            .cleanup_staged_attachments(Duration::from_secs(60))
            .expect("cleanup attachments");
        assert_eq!(result.removed_partial_files, 1);
        assert_eq!(result.removed_stale_files, 1);
        assert!(!partial.exists());
        assert!(!stale.exists());
        assert!(fresh.exists());
    }
}
