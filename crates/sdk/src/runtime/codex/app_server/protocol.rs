use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct InitializeParams {
    pub client_info: ClientInfo,
    pub capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientInfo {
    pub name: String,
    pub title: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilities {
    pub experimental_api: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct InitializeResponse {}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeParams {
    pub thread_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadStartResponse {
    pub thread: ThreadInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadResumeResponse {
    pub thread: ThreadInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ThreadInfo {
    pub id: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartParams {
    pub thread_id: String,
    pub input: Vec<TurnInputItem>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub approval_policy: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sandbox_policy: Option<SandboxPolicy>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum TurnInputItem {
    Text { text: String },
    LocalImage { path: String },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum SandboxPolicy {
    DangerFullAccess,
    ReadOnly {
        access: ReadOnlyAccess,
        network_access: bool,
    },
    WorkspaceWrite {
        writable_roots: Vec<String>,
        read_only_access: ReadOnlyAccess,
        network_access: bool,
    },
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReadOnlyAccess {
    FullAccess,
    Restricted {
        include_platform_defaults: bool,
        readable_roots: Vec<String>,
    },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnStartResponse {
    pub turn: TurnInfo,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInterruptParams {
    pub thread_id: String,
    pub turn_id: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnInfo {
    pub id: String,
    pub status: String,
    #[serde(default)]
    pub error: Option<TurnError>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnError {
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnCompletedParams {
    pub thread_id: String,
    pub turn: TurnInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemStartedParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ItemInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ItemCompletedParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item: ItemInfo,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ItemInfo {
    AgentMessage {
        text: String,
        #[serde(default)]
        phase: Option<String>,
    },
    CommandExecution {
        command: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub delta: String,
}

#[derive(Debug, Clone)]
pub enum ServerNotification {
    ThreadStarted,
    TurnStarted,
    TurnCompleted(TurnCompletedParams),
    ItemStarted(ItemStartedParams),
    ItemCompleted(ItemCompletedParams),
    AgentMessageDelta(TextDeltaParams),
    PlanDelta(TextDeltaParams),
    ReasoningSummaryTextDelta(ReasoningSummaryTextDeltaParams),
    CommandExecutionOutputDelta,
    Unknown { method: String },
    ServerExited { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryTextDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseErrorPayload {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub data: Option<JsonValue>,
}
