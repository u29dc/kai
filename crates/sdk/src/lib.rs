use std::env;
use std::path::PathBuf;

use serde::Serialize;

#[derive(Debug, Clone, Serialize)]
pub struct Meta {
    pub tool: &'static str,
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

impl<T> OkEnvelope<T>
where
    T: Serialize,
{
    pub fn new(tool: &'static str, data: T) -> Self {
        Self {
            ok: true,
            data,
            meta: Meta { tool },
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct GlobalFlag {
    pub name: &'static str,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolParameter {
    pub name: &'static str,
    pub r#type: &'static str,
    pub required: bool,
    pub description: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolSpec {
    pub name: &'static str,
    pub command: &'static str,
    pub category: &'static str,
    pub description: &'static str,
    pub parameters: Vec<ToolParameter>,
    pub output_fields: Vec<&'static str>,
    pub output_schema: &'static str,
    pub input_schema: Option<&'static str>,
    pub idempotent: bool,
    pub rate_limit: Option<&'static str>,
    pub example: &'static str,
}

#[derive(Debug, Clone, Serialize)]
pub struct ToolCatalog {
    pub tools: Vec<ToolSpec>,
    pub global_flags: Vec<GlobalFlag>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthCheck {
    pub name: &'static str,
    pub ok: bool,
    pub detail: String,
    pub fix: Option<&'static str>,
}

#[derive(Debug, Clone, Serialize)]
pub struct HealthReport {
    pub status: &'static str,
    pub checks: Vec<HealthCheck>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigPaths {
    pub root_app: String,
    pub root_work: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConfigView {
    pub paths: ConfigPaths,
}

pub fn default_config() -> ConfigView {
    let root_app = detect_root_app();
    let root_work = root_app.join("work");

    ConfigView {
        paths: ConfigPaths {
            root_app: root_app.display().to_string(),
            root_work: root_work.display().to_string(),
        },
    }
}

pub fn tool_catalog() -> ToolCatalog {
    ToolCatalog {
        tools: vec![
            ToolSpec {
                name: "tools",
                command: "kai tools",
                category: "infra",
                description: "List the current command catalog.",
                parameters: vec![],
                output_fields: vec!["tools", "global_flags"],
                output_schema: "tool-catalog",
                input_schema: None,
                idempotent: true,
                rate_limit: None,
                example: "kai tools",
            },
            ToolSpec {
                name: "health",
                command: "kai health",
                category: "infra",
                description: "Report current bootstrap health checks.",
                parameters: vec![],
                output_fields: vec!["status", "checks"],
                output_schema: "health-report",
                input_schema: None,
                idempotent: true,
                rate_limit: None,
                example: "kai health",
            },
            ToolSpec {
                name: "config.show",
                command: "kai config show",
                category: "config",
                description: "Show detected default config values.",
                parameters: vec![],
                output_fields: vec!["paths"],
                output_schema: "config-view",
                input_schema: None,
                idempotent: true,
                rate_limit: None,
                example: "kai config show",
            },
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

pub fn health_report() -> HealthReport {
    let codex_path = find_in_path("codex");
    let codex_check = HealthCheck {
        name: "codex.binary",
        ok: codex_path.is_some(),
        detail: codex_path
            .as_ref()
            .map(|path| format!("found at {}", path.display()))
            .unwrap_or_else(|| "not found on PATH".to_string()),
        fix: if codex_path.is_some() {
            None
        } else {
            Some("install Codex CLI or make `codex` available on PATH")
        },
    };

    let status = if codex_check.ok { "ready" } else { "blocked" };

    HealthReport {
        status,
        checks: vec![codex_check],
    }
}

fn detect_root_app() -> PathBuf {
    if let Some(path) = env::var_os("KAI_HOME") {
        return PathBuf::from(path);
    }

    if let Some(path) = env::var_os("TOOLS_HOME") {
        return PathBuf::from(path).join("kai");
    }

    if let Some(home) = env::var_os("HOME") {
        return PathBuf::from(home).join(".tools").join("kai");
    }

    PathBuf::from(".tools").join("kai")
}

fn find_in_path(binary: &str) -> Option<PathBuf> {
    let path = env::var_os("PATH")?;

    env::split_paths(&path)
        .map(|entry| entry.join(binary))
        .find(|candidate| candidate.is_file())
}
