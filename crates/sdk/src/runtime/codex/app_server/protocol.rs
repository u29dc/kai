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
        id: String,
        text: String,
        #[serde(default)]
        phase: Option<String>,
    },
    Plan {
        id: String,
        text: String,
    },
    Reasoning {
        id: String,
        #[serde(default)]
        summary: Vec<String>,
    },
    CommandExecution {
        id: String,
        command: String,
    },
    FileChange {
        id: String,
        #[serde(default)]
        changes: Vec<FileChange>,
    },
    McpToolCall {
        id: String,
        server: String,
        tool: String,
        #[serde(default)]
        arguments: Option<JsonValue>,
    },
    DynamicToolCall {
        id: String,
        tool: String,
        #[serde(default)]
        arguments: Option<JsonValue>,
    },
    WebSearch {
        id: String,
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        action: Option<WebSearchAction>,
    },
    ImageView {
        id: String,
        path: String,
    },
    ContextCompaction {
        id: String,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileChange {
    pub path: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum WebSearchAction {
    Search {
        #[serde(default)]
        query: Option<String>,
        #[serde(default)]
        queries: Option<Vec<String>>,
    },
    #[serde(alias = "open_page")]
    OpenPage {
        #[serde(default)]
        url: Option<String>,
    },
    #[serde(alias = "find_in_page")]
    FindInPage {
        #[serde(default)]
        url: Option<String>,
        #[serde(default)]
        pattern: Option<String>,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TextDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
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
    CommandExecutionOutputDelta(CommandExecutionOutputDeltaParams),
    Unknown { method: String },
    ServerExited { message: String },
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningSummaryTextDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    pub summary_index: i64,
    pub delta: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandExecutionOutputDeltaParams {
    pub thread_id: String,
    pub turn_id: String,
    pub item_id: String,
    #[serde(default)]
    pub stream: Option<String>,
    #[serde(default)]
    pub delta: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResponseErrorPayload {
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub data: Option<JsonValue>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn item_info_deserializes_structured_activity_items() {
        let web_search = serde_json::json!({
            "type": "webSearch",
            "id": "item-web",
            "query": "codex app server",
            "action": {
                "type": "search",
                "query": "codex app server"
            }
        });
        let parsed = serde_json::from_value::<ItemInfo>(web_search).expect("web search item");
        match parsed {
            ItemInfo::WebSearch { query, action, .. } => {
                assert_eq!(query.as_deref(), Some("codex app server"));
                assert!(matches!(
                    action,
                    Some(WebSearchAction::Search { query: Some(_), .. })
                ));
            }
            other => panic!("unexpected item: {other:?}"),
        }

        let mcp_tool = serde_json::json!({
            "type": "mcpToolCall",
            "id": "item-mcp",
            "server": "github",
            "tool": "search_issues",
            "arguments": { "query": "codex" }
        });
        let parsed = serde_json::from_value::<ItemInfo>(mcp_tool).expect("mcp tool item");
        match parsed {
            ItemInfo::McpToolCall {
                server,
                tool,
                arguments,
                ..
            } => {
                assert_eq!(server, "github");
                assert_eq!(tool, "search_issues");
                assert_eq!(
                    arguments
                        .as_ref()
                        .and_then(|value| value.get("query"))
                        .and_then(JsonValue::as_str),
                    Some("codex")
                );
            }
            other => panic!("unexpected item: {other:?}"),
        }

        let file_change = serde_json::json!({
            "type": "fileChange",
            "id": "item-file",
            "changes": [
                { "path": "/tmp/progress.rs" },
                { "path": "/tmp/mod.rs" }
            ]
        });
        let parsed = serde_json::from_value::<ItemInfo>(file_change).expect("file change item");
        match parsed {
            ItemInfo::FileChange { changes, .. } => {
                assert_eq!(changes.len(), 2);
                assert_eq!(changes[0].path, "/tmp/progress.rs");
            }
            other => panic!("unexpected item: {other:?}"),
        }
    }

    #[test]
    fn command_output_delta_deserializes() {
        let params = serde_json::json!({
            "threadId": "thread-1",
            "turnId": "turn-1",
            "itemId": "item-1",
            "stream": "stdout",
            "delta": "Compiling kai v0.1.0"
        });
        let parsed =
            serde_json::from_value::<CommandExecutionOutputDeltaParams>(params).expect("delta");
        assert_eq!(parsed.thread_id, "thread-1");
        assert_eq!(parsed.turn_id, "turn-1");
        assert_eq!(parsed.item_id, "item-1");
        assert_eq!(parsed.stream.as_deref(), Some("stdout"));
        assert_eq!(parsed.delta.as_deref(), Some("Compiling kai v0.1.0"));
    }
}
