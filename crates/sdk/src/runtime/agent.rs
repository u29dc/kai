use crate::config::{LoadedConfig, RunnerProvider};
use crate::context::ContextSnapshot;
use crate::error::{KaiError, KaiResult};
use crate::state::{AttachmentInfo, StateStore};
use crate::workspace::ExecutionTarget;

use super::codex;

pub use super::codex::create_replay_package;

#[derive(Debug, Clone)]
pub struct AgentTurnResult {
    pub provider: RunnerProvider,
    pub session_id: String,
    pub response_text: String,
    pub resumed: bool,
    pub context_snapshots: Vec<ContextSnapshot>,
}

#[derive(Debug, Clone)]
pub struct PreparedAgentTurn {
    pub provider: RunnerProvider,
    inner: PreparedAgentTurnInner,
}

#[derive(Debug, Clone)]
enum PreparedAgentTurnInner {
    Codex(codex::PreparedCodexTurn),
}

#[derive(Debug, Clone)]
pub struct AgentResumeFailure {
    pub provider: RunnerProvider,
    pub requested_session_id: String,
    pub stale_session: bool,
    pub error: KaiError,
}

#[derive(Debug, Clone)]
pub struct AsyncAgentTurnResult {
    pub result: AgentTurnResult,
    pub resume_failure: Option<AgentResumeFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentProgressEvent {
    AgentMessage { text: String },
    Plan { text: String },
    CommandStarted { command: String },
    ReasoningSummary { text: String },
    StructuredActivity { text: String },
}

#[derive(Debug)]
pub enum RunningAgentTurnEvent {
    Progress(AgentProgressEvent),
    ResumeFailure(AgentResumeFailure),
    Completed(KaiResult<AsyncAgentTurnResult>),
}

pub enum RunningAgentTurn {
    Codex(codex::RunningCodexTurn),
}

pub fn selected_provider(config: &LoadedConfig) -> KaiResult<RunnerProvider> {
    match config.values.runner.provider {
        RunnerProvider::Codex => Ok(RunnerProvider::Codex),
        RunnerProvider::Claude => Err(KaiError::blocked_prerequisite(
            "runner.provider `claude` is not available yet",
        )
        .with_hint("keep `runner.provider = \"codex\"` until the Claude adapter lands")),
    }
}

pub fn run_agent_turn(
    config: &LoadedConfig,
    state: &StateStore,
    target: &ExecutionTarget,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<AgentTurnResult> {
    match selected_provider(config)? {
        RunnerProvider::Codex => Ok(map_turn_result(
            RunnerProvider::Codex,
            codex::run_codex_turn(
                config,
                state,
                target,
                channel,
                sender_id,
                user_text,
                attachments,
            )?,
        )),
        RunnerProvider::Claude => unreachable!("unsupported provider filtered above"),
    }
}

pub fn prepare_agent_turn(
    config: &LoadedConfig,
    state: &StateStore,
    target: &ExecutionTarget,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<PreparedAgentTurn> {
    match selected_provider(config)? {
        RunnerProvider::Codex => Ok(PreparedAgentTurn {
            provider: RunnerProvider::Codex,
            inner: PreparedAgentTurnInner::Codex(codex::prepare_codex_turn(
                config,
                state,
                target,
                channel,
                sender_id,
                user_text,
                attachments,
            )?),
        }),
        RunnerProvider::Claude => unreachable!("unsupported provider filtered above"),
    }
}

pub async fn start_agent_turn(
    config: LoadedConfig,
    prepared: PreparedAgentTurn,
) -> KaiResult<RunningAgentTurn> {
    match prepared.inner {
        PreparedAgentTurnInner::Codex(prepared) => Ok(RunningAgentTurn::Codex(
            codex::start_codex_turn(config, prepared).await?,
        )),
    }
}

pub fn drain_running_agent_turn_events(turn: &mut RunningAgentTurn) -> Vec<RunningAgentTurnEvent> {
    match turn {
        RunningAgentTurn::Codex(turn) => codex::drain_running_codex_turn_events(turn)
            .into_iter()
            .map(map_running_event)
            .collect(),
    }
}

pub fn cancel_agent_turn(turn: &RunningAgentTurn) -> KaiResult<()> {
    match turn {
        RunningAgentTurn::Codex(turn) => codex::cancel_codex_turn(turn),
    }
}

fn map_running_event(event: codex::RunningCodexTurnEvent) -> RunningAgentTurnEvent {
    match event {
        codex::RunningCodexTurnEvent::Progress(event) => {
            RunningAgentTurnEvent::Progress(map_progress_event(event))
        }
        codex::RunningCodexTurnEvent::ResumeFailure(failure) => {
            RunningAgentTurnEvent::ResumeFailure(map_resume_failure(RunnerProvider::Codex, failure))
        }
        codex::RunningCodexTurnEvent::Completed(result) => {
            RunningAgentTurnEvent::Completed(result.map(|result| {
                AsyncAgentTurnResult {
                    result: map_turn_result(RunnerProvider::Codex, result.result),
                    resume_failure: result
                        .resume_failure
                        .map(|failure| map_resume_failure(RunnerProvider::Codex, failure)),
                }
            }))
        }
    }
}

fn map_turn_result(provider: RunnerProvider, result: codex::CodexTurnResult) -> AgentTurnResult {
    AgentTurnResult {
        provider,
        session_id: result.session_id,
        response_text: result.response_text,
        resumed: result.resumed,
        context_snapshots: result.context_snapshots,
    }
}

fn map_resume_failure(
    provider: RunnerProvider,
    failure: codex::ResumeFailure,
) -> AgentResumeFailure {
    AgentResumeFailure {
        provider,
        requested_session_id: failure.requested_session_id,
        stale_session: failure.stale_session,
        error: failure.error,
    }
}

fn map_progress_event(event: codex::CodexProgressEvent) -> AgentProgressEvent {
    match event {
        codex::CodexProgressEvent::AgentMessage { text } => {
            AgentProgressEvent::AgentMessage { text }
        }
        codex::CodexProgressEvent::Plan { text } => AgentProgressEvent::Plan { text },
        codex::CodexProgressEvent::CommandStarted { command } => {
            AgentProgressEvent::CommandStarted { command }
        }
        codex::CodexProgressEvent::ReasoningSummary { text } => {
            AgentProgressEvent::ReasoningSummary { text }
        }
        codex::CodexProgressEvent::StructuredActivity { text } => {
            AgentProgressEvent::StructuredActivity { text }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{
        AgentConfig, ChannelConfig, Config, ContextFilesConfig, MediaConfig, PathsConfig,
        RunnerConfig, TelegramConfig, TelegramProgressConfig, TranscriptionConfig, WorkspaceConfig,
        WorkspacesConfig,
    };

    #[test]
    fn selected_provider_blocks_unimplemented_claude() {
        let config = LoadedConfig {
            config_path: "/tmp/kai.toml".into(),
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
                    provider: RunnerProvider::Claude,
                    codex: crate::config::CodexConfig {
                        binary: "codex".to_string(),
                        transport: crate::config::CodexTransport::AppServer,
                        service_name: Some("kai".to_string()),
                        override_config: None,
                    },
                },
                context_files: ContextFilesConfig {
                    soul: "/tmp/kai/SOUL.md".to_string(),
                    memory: "/tmp/kai/MEMORY.md".to_string(),
                },
                workspaces: WorkspacesConfig {
                    default_workspace: "main".to_string(),
                    entries: std::collections::BTreeMap::from([(
                        "main".to_string(),
                        WorkspaceConfig {
                            label: Some("Main".to_string()),
                            path: "/tmp/work".to_string(),
                        },
                    )]),
                },
            },
            config_exists: false,
        };

        let error = selected_provider(&config).expect_err("claude should be blocked in phase 1");
        assert!(matches!(
            error.code,
            crate::error::ErrorCode::BlockedPrerequisite
        ));
        assert!(error.message.contains("claude"));
    }
}
