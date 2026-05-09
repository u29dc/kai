use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::state::{
    AttachmentInfo, ReplayAttachmentRef, ReplayPackage, ReplayTurn, StateStore, TurnRecord,
};
use crate::workspace::ExecutionTarget;
use chrono::Utc;

mod app_server;
mod prompt;
#[cfg(test)]
mod tests;

use self::app_server::{drain_turn_events as drain_app_server_turn_events, handshake_smoke_test};
pub use self::prompt::create_replay_package;
use self::prompt::{build_replay_prompt, build_turn_prompt};

const REPLAY_TURN_LIMIT: usize = 12;
const REPLAY_ATTACHMENT_REF_LIMIT: usize = 8;

#[derive(Debug, Clone)]
pub struct CodexTurnResult {
    pub session_id: String,
    pub response_text: String,
    pub resumed: bool,
}

#[derive(Debug, Clone)]
pub struct PreparedCodexTurn {
    pub channel: String,
    pub sender_id: i64,
    pub target: ExecutionTarget,
    pub attachments: Vec<AttachmentInfo>,
    pub prompt: String,
    pub replay_prompt: String,
    pub requested_session_id: Option<String>,
}

#[derive(Debug, Clone)]
pub struct ResumeFailure {
    pub requested_session_id: String,
    pub stale_session: bool,
    pub error: KaiError,
}

#[derive(Debug, Clone)]
pub struct AsyncCodexTurnResult {
    pub result: CodexTurnResult,
    pub resume_failure: Option<ResumeFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexProgressEvent {
    AgentMessage { text: String },
    Plan { text: String },
    CommandStarted { command: String },
    ReasoningSummary { text: String },
    StructuredActivity { text: String },
}

#[derive(Debug)]
pub enum RunningCodexTurnEvent {
    Progress(CodexProgressEvent),
    ResumeFailure(ResumeFailure),
    Completed(KaiResult<AsyncCodexTurnResult>),
}

pub struct RunningCodexTurn {
    inner: RunningCodexTurnInner,
}

enum RunningCodexTurnInner {
    AppServer(Box<app_server::RunningAppServerTurn>),
}

pub fn run_codex_turn(
    config: &LoadedConfig,
    state: &StateStore,
    target: &ExecutionTarget,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<CodexTurnResult> {
    let prepared = prepare_codex_turn(
        config,
        state,
        target,
        channel,
        sender_id,
        user_text,
        attachments,
    );
    let async_result = block_on_codex_future(app_server::run_turn_once(config.clone(), prepared?))?;
    state.set_session_binding(target, &async_result.result.session_id)?;
    Ok(async_result.result)
}

pub fn prepare_codex_turn(
    config: &LoadedConfig,
    state: &StateStore,
    target: &ExecutionTarget,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<PreparedCodexTurn> {
    let prompt = build_turn_prompt(config, target, channel, sender_id, user_text, attachments);
    let replay_prompt = build_replay_prompt(
        &prompt,
        state.get_target_replay_package(target)?,
        &state.recent_turns_for_target(target, 12)?,
    );

    Ok(PreparedCodexTurn {
        channel: channel.to_string(),
        sender_id,
        target: target.clone(),
        attachments: attachments.to_vec(),
        prompt,
        replay_prompt,
        requested_session_id: state
            .get_session_binding(target)?
            .map(|binding| binding.session_id),
    })
}

pub async fn start_codex_turn(
    config: LoadedConfig,
    prepared: PreparedCodexTurn,
) -> KaiResult<RunningCodexTurn> {
    Ok(RunningCodexTurn {
        inner: RunningCodexTurnInner::AppServer(Box::new(
            app_server::prepare_or_start_turn(config, prepared).await?,
        )),
    })
}

pub fn drain_running_codex_turn_events(turn: &mut RunningCodexTurn) -> Vec<RunningCodexTurnEvent> {
    match &mut turn.inner {
        RunningCodexTurnInner::AppServer(turn) => drain_app_server_turn_events(turn),
    }
}

pub fn cancel_codex_turn(turn: &RunningCodexTurn) -> KaiResult<()> {
    match &turn.inner {
        RunningCodexTurnInner::AppServer(turn) => {
            block_on_codex_future(app_server::cancel_turn(turn))
        }
    }
}

pub fn app_server_health_check(config: &LoadedConfig) -> KaiResult<()> {
    handshake_smoke_test(config)
}

fn block_on_codex_future<T>(
    future: impl std::future::Future<Output = KaiResult<T>>,
) -> KaiResult<T> {
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    format!("failed to build Codex runtime bridge: {error}"),
                )
            })?
            .block_on(future)
    }
}
