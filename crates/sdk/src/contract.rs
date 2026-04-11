use serde::Serialize;

use crate::error::{ErrorCode, KaiError};

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

pub fn tool_catalog() -> ToolCatalog {
    ToolCatalog {
        tools: vec![
            tool(
                ToolSeed::new(
                    "tools",
                    "kai tools",
                    "infra",
                    "List the command catalog.",
                    "toolCatalog",
                    "kai tools",
                )
                .with_output_fields(["tools", "globalFlags"]),
            ),
            tool(
                ToolSeed::new(
                    "health",
                    "kai health",
                    "infra",
                    "Report runtime readiness and remediation hints.",
                    "healthReport",
                    "kai health",
                )
                .with_output_fields(["status", "checks"]),
            ),
            tool(
                ToolSeed::new(
                    "config.show",
                    "kai config show",
                    "config",
                    "Show effective configuration.",
                    "configShowOutput",
                    "kai config show",
                )
                .with_output_fields(["configPath", "configExists", "values"]),
            ),
            tool(
                ToolSeed::new(
                    "config.get",
                    "kai config get <key>",
                    "config",
                    "Read one effective config key.",
                    "configGetOutput",
                    "kai config get paths.root_work",
                )
                .with_parameters(vec![parameter(
                    "key",
                    "string",
                    true,
                    "Dotted config key path.",
                )])
                .with_output_fields(["key", "value"]),
            ),
            tool(
                ToolSeed::new(
                    "config.set",
                    "kai config set <key> <value>",
                    "config",
                    "Persist a config override.",
                    "configSetOutput",
                    "kai config set paths.root_work ~/Dropbox/VAULT",
                )
                .with_parameters(vec![
                    parameter("key", "string", true, "Dotted config key path."),
                    parameter("value", "string", true, "Literal value to store."),
                ])
                .with_output_fields(["configPath"])
                .with_input_schema("keyValue")
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "config.unset",
                    "kai config unset <key>",
                    "config",
                    "Remove a config override.",
                    "configUnsetOutput",
                    "kai config unset channel.telegram.owner_user_id",
                )
                .with_parameters(vec![parameter(
                    "key",
                    "string",
                    true,
                    "Dotted config key path.",
                )])
                .with_output_fields(["configPath"])
                .with_input_schema("keyOnly")
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "setup",
                    "kai setup",
                    "setup",
                    "Create app directories, config, and placeholder context files.",
                    "setupOutput",
                    "kai setup",
                )
                .with_output_fields(["configPath", "rootApp", "rootWork", "createdPaths"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "setup.telegram",
                    "kai setup telegram",
                    "setup",
                    "Open a short-lived Telegram recovery pairing window.",
                    "setupTelegramOutput",
                    "kai setup telegram",
                )
                .with_parameters(vec![parameter(
                    "recovery",
                    "bool",
                    false,
                    "Explicitly allow owner recovery pairing even when owner_user_id is pinned.",
                )])
                .with_output_fields([
                    "pairCode",
                    "botTokenEnv",
                    "expiresInMinutes",
                    "remainingAttempts",
                    "recovery",
                ])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "setup.codex",
                    "kai setup codex",
                    "setup",
                    "Verify Codex exec and resume availability.",
                    "setupCodexOutput",
                    "kai setup codex",
                )
                .with_output_fields(["binary", "execAvailable", "resumeAvailable"]),
            ),
            tool(
                ToolSeed::new(
                    "context.show",
                    "kai context show",
                    "context",
                    "Show configured context file paths and status.",
                    "contextReport",
                    "kai context show",
                )
                .with_output_fields(["entries"]),
            ),
            tool(
                ToolSeed::new(
                    "context.check",
                    "kai context check",
                    "context",
                    "Check context file readability.",
                    "contextReport",
                    "kai context check",
                )
                .with_output_fields(["entries"]),
            ),
            tool(
                ToolSeed::new(
                    "session.show",
                    "kai session show",
                    "session",
                    "Show owner pairing and active session state.",
                    "sessionView",
                    "kai session show",
                )
                .with_output_fields([
                    "ownerUserId",
                    "ownerChatId",
                    "activeSessionId",
                    "pendingPairCode",
                    "updateOffset",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "session.new",
                    "kai session new",
                    "session",
                    "Clear the active Codex session so the next turn starts fresh.",
                    "sessionView",
                    "kai session new",
                )
                .with_output_fields(["ownerUserId", "activeSessionId"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "session.set",
                    "kai session set <session-id>",
                    "session",
                    "Override the active Codex session id.",
                    "sessionView",
                    "kai session set 019d7c6a-2460-7e91-b6eb-8643f9f9930f",
                )
                .with_parameters(vec![parameter(
                    "sessionId",
                    "string",
                    true,
                    "Existing Codex session id to resume on the next turn.",
                )])
                .with_output_fields(["ownerUserId", "ownerChatId", "activeSessionId"])
                .with_input_schema("sessionId")
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "session.reset",
                    "kai session reset",
                    "session",
                    "Reset the active session pointer.",
                    "sessionView",
                    "kai session reset",
                )
                .with_output_fields(["ownerUserId", "activeSessionId"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "service.status",
                    "kai service status",
                    "service",
                    "Show background service and single-instance status.",
                    "serviceStatus",
                    "kai service status",
                )
                .with_output_fields([
                    "platform",
                    "installed",
                    "loaded",
                    "running",
                    "pid",
                    "activeMode",
                    "lock",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "service.logs",
                    "kai service logs [--tail <n>]",
                    "service",
                    "Show recent background service stdout and stderr lines.",
                    "serviceLogsOutput",
                    "kai service logs --tail 100",
                )
                .with_parameters(vec![parameter(
                    "tail",
                    "integer",
                    false,
                    "Maximum number of recent lines to include from each log file.",
                )])
                .with_output_fields([
                    "status",
                    "stdoutPath",
                    "stderrPath",
                    "stdoutTail",
                    "stderrTail",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "service.start",
                    "kai service start",
                    "service",
                    "Start the macOS background LaunchAgent.",
                    "serviceActionOutput",
                    "kai service start",
                )
                .with_output_fields(["action", "status"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "service.stop",
                    "kai service stop",
                    "service",
                    "Stop the macOS background LaunchAgent.",
                    "serviceActionOutput",
                    "kai service stop",
                )
                .with_output_fields(["action", "status"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "service.restart",
                    "kai service restart",
                    "service",
                    "Restart the macOS background LaunchAgent.",
                    "serviceActionOutput",
                    "kai service restart",
                )
                .with_output_fields(["action", "status"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "service.uninstall",
                    "kai service uninstall",
                    "service",
                    "Remove the macOS background LaunchAgent.",
                    "serviceActionOutput",
                    "kai service uninstall",
                )
                .with_output_fields(["action", "status"])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "run",
                    "kai run",
                    "runtime",
                    "Start the foreground Telegram long-polling loop.",
                    "runOutput",
                    "kai run",
                )
                .with_output_fields(["status"])
                .with_idempotent(false),
            ),
        ],
        global_flags: vec![],
    }
}

pub fn tool_spec(name: &str) -> Option<ToolSpec> {
    tool_catalog()
        .tools
        .into_iter()
        .find(|tool| tool.name == name)
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
    pub root_work: String,
    pub created_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SetupCodexOutput {
    pub binary: String,
    pub exec_available: bool,
    pub resume_available: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingPairingView {
    pub expires_at: String,
    pub remaining_attempts: u8,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionView {
    pub owner_user_id: Option<i64>,
    pub owner_chat_id: Option<i64>,
    pub active_session_id: Option<String>,
    pub pending_pairing: Option<PendingPairingView>,
    pub update_offset: i64,
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

struct ToolSeed<'a> {
    name: &'a str,
    command: &'a str,
    category: &'a str,
    description: &'a str,
    parameters: Vec<ToolParameter>,
    output_fields: Vec<&'a str>,
    output_schema: &'a str,
    input_schema: Option<&'a str>,
    idempotent: bool,
    example: &'a str,
}

impl<'a> ToolSeed<'a> {
    fn new(
        name: &'a str,
        command: &'a str,
        category: &'a str,
        description: &'a str,
        output_schema: &'a str,
        example: &'a str,
    ) -> Self {
        Self {
            name,
            command,
            category,
            description,
            parameters: Vec::new(),
            output_fields: Vec::new(),
            output_schema,
            input_schema: None,
            idempotent: true,
            example,
        }
    }

    fn with_parameters(mut self, parameters: Vec<ToolParameter>) -> Self {
        self.parameters = parameters;
        self
    }

    fn with_output_fields(mut self, output_fields: impl IntoIterator<Item = &'a str>) -> Self {
        self.output_fields = output_fields.into_iter().collect();
        self
    }

    fn with_input_schema(mut self, input_schema: &'a str) -> Self {
        self.input_schema = Some(input_schema);
        self
    }

    fn with_idempotent(mut self, idempotent: bool) -> Self {
        self.idempotent = idempotent;
        self
    }
}

fn tool(seed: ToolSeed<'_>) -> ToolSpec {
    ToolSpec {
        name: seed.name.to_string(),
        command: seed.command.to_string(),
        category: seed.category.to_string(),
        description: seed.description.to_string(),
        parameters: seed.parameters,
        output_fields: seed
            .output_fields
            .into_iter()
            .map(ToOwned::to_owned)
            .collect(),
        output_schema: seed.output_schema.to_string(),
        input_schema: seed.input_schema.map(ToOwned::to_owned),
        idempotent: seed.idempotent,
        rate_limit: None,
        example: seed.example.to_string(),
    }
}

fn parameter(name: &str, value_type: &str, required: bool, description: &str) -> ToolParameter {
    ToolParameter {
        name: name.to_string(),
        r#type: value_type.to_string(),
        required,
        description: description.to_string(),
    }
}
