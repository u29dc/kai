use serde::Serialize;

use crate::error::{ErrorCode, KaiError};

mod catalog;
#[cfg(test)]
mod tests;

pub use self::catalog::{tool_catalog, tool_spec};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    pub tool: String,
}

impl Meta {
    pub fn new(tool: impl Into<String>) -> Self {
        Self { tool: tool.into() }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct OkEnvelope<T>
where
    T: Serialize,
{
    pub ok: bool,
    pub data: T,
    pub meta: Meta,
}

pub fn ok_envelope<T>(tool: impl Into<String>, data: T) -> OkEnvelope<T>
where
    T: Serialize,
{
    OkEnvelope {
        ok: true,
        data,
        meta: Meta::new(tool),
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorEnvelope {
    pub ok: bool,
    pub error: ErrorInfo,
    pub meta: Meta,
}

#[derive(Debug, Clone, Serialize)]
pub struct ErrorInfo {
    pub code: ErrorCode,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<String>,
}

pub fn error_envelope(tool: impl Into<String>, error: KaiError) -> ErrorEnvelope {
    error.into_envelope(tool)
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GlobalFlag {
    pub name: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolParameter {
    pub name: String,
    pub r#type: String,
    pub required: bool,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolSpec {
    pub name: String,
    pub command: String,
    pub category: String,
    pub description: String,
    pub parameters: Vec<ToolParameter>,
    pub output_fields: Vec<String>,
    pub output_schema: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_schema: Option<String>,
    pub idempotent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub rate_limit: Option<String>,
    pub example: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCatalog {
    pub tools: Vec<ToolSpec>,
    pub global_flags: Vec<GlobalFlag>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthCheck {
    pub name: String,
    pub ok: bool,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fix: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthReport {
    pub status: String,
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigShowOutput {
    pub config_path: String,
    pub config_exists: bool,
    pub values: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigGetOutput {
    pub key: String,
    pub value: serde_json::Value,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupOutput {
    pub config_path: String,
    pub root_app: String,
    pub default_workspace_id: String,
    pub created_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigMigrationOutput {
    pub config_path: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_path: Option<String>,
    pub migrated: bool,
    pub default_workspace_id: String,
    pub removed_legacy_keys: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceView {
    pub id: String,
    pub label: String,
    pub path: String,
    pub is_default: bool,
    pub selected: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub active_session_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceStatusOutput {
    pub provider: String,
    pub default_workspace_id: String,
    pub selected_workspace_id: String,
    pub selected_workspace_path: String,
    pub workspaces: Vec<WorkspaceView>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCodexOutput {
    pub binary: String,
    pub exec_available: bool,
    pub resume_available: bool,
    pub app_server_available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairingView {
    pub expires_at: String,
    pub remaining_attempts: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingTurnView {
    pub id: String,
    pub enqueued_at: String,
    pub provider: String,
    pub workspace_id: String,
    pub working_dir: String,
    pub chat_id: i64,
    pub sender_id: i64,
    pub update_count: usize,
    pub attachment_count: usize,
    pub text_excerpt: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SideQueryView {
    pub id: String,
    pub started_at: String,
    pub provider: String,
    pub workspace_id: String,
    pub working_dir: String,
    pub chat_id: i64,
    pub sender_id: i64,
    pub text_excerpt: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub owner_user_id: Option<i64>,
    pub owner_chat_id: Option<i64>,
    pub provider: String,
    pub transport: String,
    pub default_workspace_id: String,
    pub selected_workspace_id: String,
    pub selected_workspace_path: String,
    pub workspaces: Vec<WorkspaceView>,
    pub active_session_id: Option<String>,
    pub pending_pairing: Option<PendingPairingView>,
    pub update_offset: i64,
    pub queue_limit: usize,
    pub queued_turns: usize,
    pub queued_preview: Vec<PendingTurnView>,
    pub active_turn: Option<PendingTurnView>,
    pub active_side_query: Option<SideQueryView>,
    pub pending_reply_deliveries: usize,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServiceStatusOutput {
    pub platform: String,
    pub label: String,
    pub installed: bool,
    pub loaded: bool,
    pub running: bool,
    pub pid: Option<u32>,
    pub active_mode: String,
    pub plist_path: Option<String>,
    pub stdout_path: String,
    pub stderr_path: String,
    pub lock: serde_json::Value,
}
