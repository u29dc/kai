use std::collections::BTreeSet;
use std::path::Path;

use crate::config::LoadedConfig;
use crate::state::AttachmentInfo;
use crate::workspace::ExecutionTarget;

use super::protocol::{ReadOnlyAccess, SandboxPolicy};

pub fn approval_policy(config: &LoadedConfig) -> Option<String> {
    Some(
        config
            .values
            .runner
            .codex
            .override_config
            .as_ref()
            .and_then(|override_config| override_config.approval_policy.clone())
            .unwrap_or_else(|| "never".to_string()),
    )
}

pub fn sandbox_policy(
    config: &LoadedConfig,
    target: &ExecutionTarget,
    attachments: &[AttachmentInfo],
) -> Option<SandboxPolicy> {
    let override_mode = config
        .values
        .runner
        .codex
        .override_config
        .as_ref()
        .and_then(|override_config| override_config.sandbox_mode.clone());

    let writable_roots = writable_roots(config, target);
    let readable_roots = readable_roots(attachments, &writable_roots);
    let read_only_access = if readable_roots.is_empty() {
        ReadOnlyAccess::FullAccess
    } else {
        ReadOnlyAccess::Restricted {
            include_platform_defaults: true,
            readable_roots,
        }
    };

    match override_mode.as_deref().map(normalize_mode) {
        Some("danger-full-access") => Some(SandboxPolicy::DangerFullAccess),
        Some("read-only") => Some(SandboxPolicy::ReadOnly {
            access: read_only_access,
            network_access: false,
        }),
        Some("workspace-write") | None => Some(SandboxPolicy::WorkspaceWrite {
            writable_roots,
            read_only_access,
            network_access: true,
        }),
        Some(_) => Some(SandboxPolicy::WorkspaceWrite {
            writable_roots,
            read_only_access,
            network_access: true,
        }),
    }
}

fn writable_roots(config: &LoadedConfig, target: &ExecutionTarget) -> Vec<String> {
    let mut roots = BTreeSet::new();
    roots.insert(target.working_dir.clone());
    roots.insert(config.values.paths.root_app.clone());
    roots.into_iter().collect()
}

fn readable_roots(attachments: &[AttachmentInfo], writable_roots: &[String]) -> Vec<String> {
    let writable_roots = writable_roots.iter().collect::<BTreeSet<_>>();
    let mut roots = BTreeSet::new();

    for attachment in attachments {
        push_parent(&mut roots, &attachment.path);
        for artifact in &attachment.artifacts {
            push_parent(&mut roots, &artifact.path);
        }
    }

    roots
        .into_iter()
        .filter(|root| !writable_roots.contains(root))
        .collect()
}

fn push_parent(roots: &mut BTreeSet<String>, path: &str) {
    if let Some(parent) = Path::new(path).parent() {
        roots.insert(parent.display().to_string());
    }
}

fn normalize_mode(input: &str) -> &str {
    match input.trim().to_ascii_lowercase().as_str() {
        "dangerfullaccess" | "danger-full-access" | "danger_full_access" => "danger-full-access",
        "readonly" | "read-only" | "read_only" => "read-only",
        "workspacewrite" | "workspace-write" | "workspace_write" => "workspace-write",
        _ => input,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, CodexConfig, Config, MediaConfig, PathsConfig, RunnerConfig,
        RunnerProvider, TelegramConfig, TelegramProgressConfig, TranscriptionConfig,
        WorkspaceConfig, WorkspacesConfig,
    };
    use std::collections::BTreeMap;
    use std::path::PathBuf;

    fn test_config() -> LoadedConfig {
        LoadedConfig {
            config_path: PathBuf::from("/tmp/kai.toml"),
            config_exists: true,
            values: Config {
                agent: AgentConfig {
                    timezone: "Europe/London".to_string(),
                },
                channel: ChannelConfig {
                    telegram: TelegramConfig {
                        enabled: true,
                        bot_token_env: "KAI_TELEGRAM_BOT_TOKEN".to_string(),
                        owner_user_id: None,
                        progress: TelegramProgressConfig {
                            enabled: true,
                            edit_interval_ms: 2500,
                            idle_update_secs: 8,
                        },
                    },
                },
                media: MediaConfig {
                    transcription: TranscriptionConfig {
                        provider: "groq".to_string(),
                        groq_api_key_env: "GROQ_API_KEY".to_string(),
                        groq_model: "whisper-large-v3-turbo".to_string(),
                        command: None,
                    },
                },
                paths: PathsConfig {
                    root_app: "/tmp/kai".to_string(),
                },
                runner: RunnerConfig {
                    codex: CodexConfig {
                        binary: "codex".to_string(),
                        service_name: Some("kai".to_string()),
                        override_config: None,
                    },
                },
                workspaces: WorkspacesConfig {
                    default_workspace: "main".to_string(),
                    entries: BTreeMap::from([(
                        "main".to_string(),
                        WorkspaceConfig {
                            label: Some("Main".to_string()),
                            path: "/tmp/work".to_string(),
                        },
                    )]),
                },
            },
        }
    }

    #[test]
    fn sandbox_policy_defaults_to_workspace_write() {
        let policy = sandbox_policy(
            &test_config(),
            &ExecutionTarget {
                workspace_id: "main".to_string(),
                working_dir: "/tmp/work".to_string(),
                provider: RunnerProvider::Codex,
            },
            &[],
        )
        .expect("policy");

        match policy {
            SandboxPolicy::WorkspaceWrite { writable_roots, .. } => {
                assert!(writable_roots.iter().any(|root| root == "/tmp/work"));
                assert!(writable_roots.iter().any(|root| root == "/tmp/kai"));
            }
            _ => panic!("unexpected policy"),
        }
    }
}
