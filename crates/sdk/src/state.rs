use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{Duration as ChronoDuration, Utc};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

use crate::config::{LoadedConfig, RunnerProvider};
use crate::context::ContextSnapshot;
use crate::contract::{
    PendingPairingView, PendingTurnView, SessionView, WorkspaceStatusOutput, WorkspaceView,
};
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::redaction::{redact_json_value, redact_text};
use crate::runtime_fs::{ensure_private_dir, ensure_private_file};
use crate::workspace::{ExecutionTarget, configured_workspaces, execution_target};

mod cleanup;
mod kv;
mod queue;
mod schema;
#[cfg(test)]
mod tests;
mod turns;

use self::schema::{
    initialize_schema, migrate_active_turn_state_from_kv, migrate_pending_reply_deliveries_from_kv,
    migrate_pending_turn_queue_from_kv,
};

pub const MAX_PENDING_TURNS: usize = 24;
const ACTIVE_TURN_STATE_KEY: &str = "telegram.active_turn";
const PENDING_REPLY_DELIVERIES_STATE_KEY: &str = "telegram.pending_reply_deliveries";

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
pub struct AttachmentArtifact {
    pub kind: String,
    pub path: String,
    #[serde(default)]
    pub mime_type: Option<String>,
    pub bytes: u64,
    #[serde(default)]
    pub checksum_blake3: Option<String>,
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
    #[serde(default)]
    pub media_group_id: Option<String>,
    #[serde(default)]
    pub duration_secs: Option<u32>,
    #[serde(default)]
    pub width: Option<u32>,
    #[serde(default)]
    pub height: Option<u32>,
    #[serde(default)]
    pub transcript_text: Option<String>,
    #[serde(default)]
    pub transcript_segments: Vec<crate::media::TranscriptSegment>,
    #[serde(default)]
    pub artifacts: Vec<AttachmentArtifact>,
    #[serde(default)]
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnRecord {
    pub id: i64,
    pub created_at: String,
    pub provider: RunnerProvider,
    pub workspace_id: String,
    pub working_dir: String,
    pub role: String,
    pub channel: String,
    pub sender_id: Option<i64>,
    pub text: String,
    pub codex_session_id: Option<String>,
    pub outcome_status: Option<String>,
    pub attachments: Vec<AttachmentInfo>,
}

pub struct NewTurn<'a> {
    pub provider: RunnerProvider,
    pub workspace_id: &'a str,
    pub working_dir: &'a str,
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
    #[serde(default)]
    pub transcript_excerpt: Option<String>,
    #[serde(default)]
    pub artifact_paths: Vec<String>,
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
pub struct PendingTurn {
    pub id: String,
    pub enqueued_at: String,
    #[serde(default)]
    pub target: ExecutionTarget,
    pub channel: String,
    pub update_ids: Vec<i64>,
    pub chat_id: i64,
    pub sender_id: i64,
    pub text: String,
    pub attachments: Vec<AttachmentInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ActiveTurnState {
    pub pending: PendingTurn,
    #[serde(default)]
    pub status_message_id: Option<i64>,
}

impl From<PendingTurn> for ActiveTurnState {
    fn from(pending: PendingTurn) -> Self {
        Self {
            pending,
            status_message_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingReplyDelivery {
    pub delivery_id: String,
    pub turn_id: String,
    pub chat_id: i64,
    pub response_text: String,
    pub codex_session_id: String,
    #[serde(default)]
    pub status_message_id: Option<i64>,
    pub update_ids: Vec<i64>,
    pub attempts: u32,
    pub created_at: String,
    #[serde(default)]
    pub next_chunk_index: usize,
    #[serde(default)]
    pub sent_message_ids: Vec<i64>,
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StateCleanupResult {
    pub removed_old_processed_updates: usize,
    pub removed_old_update_failures: usize,
    pub removed_old_turns: usize,
    pub audit_compacted: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct AuditRecord<'a> {
    pub timestamp: String,
    pub event: &'a str,
    pub provider: RunnerProvider,
    pub workspace_id: &'a str,
    pub working_dir: &'a str,
    pub channel: &'a str,
    pub sender_id: Option<i64>,
    pub text: &'a str,
    pub codex_session_id: Option<&'a str>,
    pub outcome_status: Option<&'a str>,
    pub attachments: &'a [AttachmentInfo],
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionBinding {
    pub session_id: String,
    pub working_dir: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReplayBinding {
    pub replay_package: ReplayPackage,
    pub working_dir: String,
    pub updated_at: String,
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
        migrate_pending_turn_queue_from_kv(&connection)?;
        migrate_active_turn_state_from_kv(&connection)?;
        migrate_pending_reply_deliveries_from_kv(&connection)?;

        let store = Self { connection, paths };
        store.migrate_legacy_target_state(config)?;
        Ok(store)
    }

    pub fn paths(&self) -> &StatePaths {
        &self.paths
    }

    pub(crate) fn with_transaction<T>(
        &self,
        action: &'static str,
        operation: impl FnOnce(&Connection) -> KaiResult<T>,
    ) -> KaiResult<T> {
        self.connection
            .execute_batch("BEGIN IMMEDIATE TRANSACTION")
            .map_err(sql_state_error(action))?;

        match operation(&self.connection) {
            Ok(value) => {
                self.connection
                    .execute_batch("COMMIT")
                    .map_err(sql_state_error(action))?;
                Ok(value)
            }
            Err(error) => {
                let _ = self.connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    }

    fn migrate_legacy_target_state(&self, config: &LoadedConfig) -> KaiResult<()> {
        let default_workspace = config
            .values
            .workspaces
            .entries
            .get(config.values.workspaces.default_workspace.as_str())
            .ok_or_else(|| {
                KaiError::new(
                    ErrorCode::ConfigError,
                    format!(
                        "missing default workspace `{}` during state migration",
                        config.values.workspaces.default_workspace
                    ),
                )
            })?;
        let default_target = ExecutionTarget {
            workspace_id: config.values.workspaces.default_workspace.clone(),
            working_dir: default_workspace.path.clone(),
            provider: RunnerProvider::Codex,
        };

        if let Some(session_id) = self.get_active_session_id()? {
            if self.get_session_binding(&default_target)?.is_none() {
                self.set_session_binding(&default_target, &session_id)?;
            }
            self.clear_active_session_id()?;
        }

        if let Some(replay_package) = self.get_replay_package()? {
            if self.get_target_replay_package(&default_target)?.is_none() {
                self.set_target_replay_package(&default_target, &replay_package)?;
            }
            self.clear_replay_package()?;
        }

        self.migrate_pending_turn_targets(&default_target)?;
        self.connection
            .execute(
                "UPDATE turns
                 SET provider = 'codex',
                     workspace_id = CASE WHEN workspace_id = '' THEN ?1 ELSE workspace_id END,
                     working_dir = CASE WHEN working_dir = '' THEN ?2 ELSE working_dir END
                 WHERE workspace_id = '' OR working_dir = '' OR provider = ''",
                params![default_target.workspace_id, default_target.working_dir],
            )
            .map_err(sql_state_error("migrate turn targets"))?;

        Ok(())
    }

    pub fn workspace_status_output(
        &self,
        config: &LoadedConfig,
    ) -> KaiResult<WorkspaceStatusOutput> {
        let target = execution_target(config, self)?;
        let workspaces = configured_workspaces(config)?
            .into_iter()
            .map(|workspace| {
                let workspace_target = ExecutionTarget {
                    workspace_id: workspace.id.clone(),
                    working_dir: workspace.path.clone(),
                    provider: target.provider,
                };
                Ok(WorkspaceView {
                    id: workspace.id.clone(),
                    label: workspace.label,
                    path: workspace.path.clone(),
                    is_default: workspace.id == config.values.workspaces.default_workspace,
                    selected: workspace.id == target.workspace_id,
                    active_session_id: self
                        .get_session_binding(&workspace_target)?
                        .map(|binding| binding.session_id),
                })
            })
            .collect::<KaiResult<Vec<_>>>()?;

        Ok(WorkspaceStatusOutput {
            provider: target.provider.as_key().to_string(),
            default_workspace_id: config.values.workspaces.default_workspace.clone(),
            selected_workspace_id: target.workspace_id,
            selected_workspace_path: target.working_dir,
            workspaces,
        })
    }

    pub fn session_view(&self, config: &LoadedConfig) -> KaiResult<SessionView> {
        let target = execution_target(config, self)?;
        let workspace_status = self.workspace_status_output(config)?;

        Ok(SessionView {
            owner_user_id: self.get_owner_user_id()?,
            owner_chat_id: self.get_owner_chat_id()?,
            provider: target.provider.as_key().to_string(),
            transport: config.values.runner.codex.transport.as_key().to_string(),
            default_workspace_id: workspace_status.default_workspace_id.clone(),
            selected_workspace_id: workspace_status.selected_workspace_id.clone(),
            selected_workspace_path: workspace_status.selected_workspace_path.clone(),
            workspaces: workspace_status.workspaces,
            active_session_id: self
                .get_session_binding(&target)?
                .map(|binding| binding.session_id),
            pending_pairing: self.pending_pairing_view()?,
            update_offset: self.get_update_offset()?,
            queue_limit: MAX_PENDING_TURNS,
            queued_turns: self.pending_turn_queue_len()?,
            queued_preview: self.pending_turn_preview(5)?,
            active_turn: self
                .get_active_pending_turn()?
                .map(|turn| pending_turn_view(&turn)),
            pending_reply_deliveries: self.pending_reply_delivery_count()?,
        })
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

fn pending_turn_view(turn: &PendingTurn) -> PendingTurnView {
    PendingTurnView {
        id: turn.id.clone(),
        enqueued_at: turn.enqueued_at.clone(),
        provider: turn.target.provider.as_key().to_string(),
        workspace_id: turn.target.workspace_id.clone(),
        working_dir: turn.target.working_dir.clone(),
        chat_id: turn.chat_id,
        sender_id: turn.sender_id,
        update_count: turn.update_ids.len(),
        attachment_count: turn.attachments.len(),
        text_excerpt: truncate_turn_text(&turn.text),
    }
}

fn truncate_turn_text(input: &str) -> String {
    const MAX_CHARS: usize = 120;
    let mut chars = input.chars();
    let truncated = chars.by_ref().take(MAX_CHARS).collect::<String>();
    if chars.next().is_some() {
        format!("{truncated}...")
    } else {
        truncated
    }
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
