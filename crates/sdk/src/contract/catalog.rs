use super::*;

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
                    "kai config get workspaces.vault.path",
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
                    "kai config set workspaces.vault.path ~/Dropbox/VAULT",
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
                    "config.migrate",
                    "kai config migrate",
                    "config",
                    "Rewrite legacy config into the workspace-based format.",
                    "configMigrationOutput",
                    "kai config migrate",
                )
                .with_output_fields([
                    "configPath",
                    "backupPath",
                    "migrated",
                    "defaultWorkspaceId",
                    "removedLegacyKeys",
                ])
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
                .with_output_fields([
                    "configPath",
                    "rootApp",
                    "defaultWorkspaceId",
                    "createdPaths",
                ])
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
                    "Check Codex CLI availability.",
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
                    "Show configured context file status.",
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
                    "Validate configured context file access.",
                    "contextReport",
                    "kai context check",
                )
                .with_output_fields(["entries"]),
            ),
            tool(
                ToolSeed::new(
                    "workspace.list",
                    "kai workspace list",
                    "workspace",
                    "List configured workspaces and current selection.",
                    "workspaceStatusOutput",
                    "kai workspace list",
                )
                .with_output_fields([
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "workspace.show",
                    "kai workspace show",
                    "workspace",
                    "Show the current workspace selection.",
                    "workspaceStatusOutput",
                    "kai workspace show",
                )
                .with_output_fields([
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "workspace.select",
                    "kai workspace select <workspace_id>",
                    "workspace",
                    "Select the workspace used for subsequent turns.",
                    "workspaceStatusOutput",
                    "kai workspace select vault",
                )
                .with_parameters(vec![parameter(
                    "workspace_id",
                    "string",
                    true,
                    "Configured workspace id.",
                )])
                .with_output_fields([
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                ])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "session.show",
                    "kai session show",
                    "session",
                    "Show owner/session/runtime state for the current workspace target.",
                    "sessionView",
                    "kai session show",
                )
                .with_output_fields([
                    "ownerUserId",
                    "ownerChatId",
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                    "activeSessionId",
                    "pendingPairing",
                    "updateOffset",
                    "queueLimit",
                    "queuedTurns",
                    "queuedPreview",
                    "activeTurn",
                    "pendingReplyDeliveries",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "session.set",
                    "kai session set <session_id>",
                    "session",
                    "Override the active session id for the current workspace target.",
                    "sessionView",
                    "kai session set 019d7c6a-2460-7e91-b6eb-8643f9f9930f",
                )
                .with_parameters(vec![parameter(
                    "session_id",
                    "string",
                    true,
                    "Existing Codex session id to resume.",
                )])
                .with_output_fields([
                    "ownerUserId",
                    "ownerChatId",
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                    "activeSessionId",
                    "pendingPairing",
                    "updateOffset",
                    "queueLimit",
                    "queuedTurns",
                    "queuedPreview",
                    "activeTurn",
                    "pendingReplyDeliveries",
                ])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "session.new",
                    "kai session new",
                    "session",
                    "Clear the current workspace session so the next turn starts fresh.",
                    "sessionView",
                    "kai session new",
                )
                .with_output_fields([
                    "ownerUserId",
                    "ownerChatId",
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                    "activeSessionId",
                    "pendingPairing",
                    "updateOffset",
                    "queueLimit",
                    "queuedTurns",
                    "queuedPreview",
                    "activeTurn",
                    "pendingReplyDeliveries",
                ])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "session.reset",
                    "kai session reset",
                    "session",
                    "Alias for `session new`.",
                    "sessionView",
                    "kai session reset",
                )
                .with_output_fields([
                    "ownerUserId",
                    "ownerChatId",
                    "provider",
                    "defaultWorkspaceId",
                    "selectedWorkspaceId",
                    "selectedWorkspacePath",
                    "workspaces",
                    "activeSessionId",
                    "pendingPairing",
                    "updateOffset",
                    "queueLimit",
                    "queuedTurns",
                    "queuedPreview",
                    "activeTurn",
                    "pendingReplyDeliveries",
                ])
                .with_idempotent(false),
            ),
            tool(
                ToolSeed::new(
                    "service.status",
                    "kai service status",
                    "service",
                    "Inspect background service status.",
                    "serviceStatusOutput",
                    "kai service status",
                )
                .with_output_fields([
                    "platform",
                    "label",
                    "installed",
                    "loaded",
                    "running",
                    "pid",
                    "activeMode",
                    "plistPath",
                    "stdoutPath",
                    "stderrPath",
                    "lock",
                ]),
            ),
            tool(
                ToolSeed::new(
                    "service.logs",
                    "kai service logs --tail <n>",
                    "service",
                    "Show background service log tails.",
                    "serviceLogsOutput",
                    "kai service logs --tail 50",
                )
                .with_parameters(vec![parameter(
                    "tail",
                    "number",
                    false,
                    "Number of recent lines to return per stream.",
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
                    "Start the background LaunchAgent.",
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
                    "Stop the background LaunchAgent.",
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
                    "Restart the background LaunchAgent.",
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
                    "Unload and remove the LaunchAgent.",
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
                    "Run the Telegram loop in the foreground.",
                    "runStatus",
                    "kai run",
                )
                .with_output_fields(["status", "help", "rootApp", "mode"])
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
