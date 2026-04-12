use chrono::Utc;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};

use serde_json::Value as JsonValue;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::Command as TokioCommand;

use crate::config::LoadedConfig;
use crate::context::{ContextSnapshot, context_snapshots};
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::state::{
    AttachmentInfo, ReplayAttachmentRef, ReplayPackage, ReplayTurn, StateStore, TurnRecord,
};

mod process;
mod prompt;
#[cfg(test)]
mod tests;

use self::process::{
    build_async_command, is_stale_resume_error, run_exec, run_resume, signal_process,
    wait_for_codex_turn,
};
pub use self::prompt::create_replay_package;
use self::prompt::{build_replay_prompt, build_turn_prompt};

const REPLAY_TURN_LIMIT: usize = 12;
const REPLAY_ATTACHMENT_REF_LIMIT: usize = 8;
const USE_SYSTEM_INSTRUCTION_PROMPT: bool = false;

#[derive(Debug, Clone)]
pub struct CodexTurnResult {
    pub session_id: String,
    pub response_text: String,
    pub resumed: bool,
    pub context_snapshots: Vec<ContextSnapshot>,
}

#[derive(Debug, Clone)]
pub struct PreparedCodexTurn {
    pub channel: String,
    pub sender_id: i64,
    pub attachments: Vec<AttachmentInfo>,
    pub context_snapshots: Vec<ContextSnapshot>,
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

#[derive(Debug)]
pub struct RunningCodexTurn {
    pub pid: u32,
    receiver: Receiver<KaiResult<AsyncCodexTurnResult>>,
}

pub fn run_codex_turn(
    config: &LoadedConfig,
    state: &StateStore,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<CodexTurnResult> {
    let context_snapshots = context_snapshots(config);
    let prompt = build_turn_prompt(
        config,
        channel,
        sender_id,
        user_text,
        attachments,
        &context_snapshots,
    );

    if let Some(session_id) = state.get_active_session_id()? {
        match run_resume(config, &session_id, &prompt, attachments) {
            Ok(result) => {
                state.set_active_session_id(&result.session_id)?;
                return Ok(CodexTurnResult {
                    session_id: result.session_id,
                    response_text: result.response_text,
                    resumed: true,
                    context_snapshots,
                });
            }
            Err(error) => {
                state.append_audit_json(&serde_json::json!({
                    "timestamp": Utc::now().to_rfc3339(),
                    "event": "codex.resume_failed",
                    "requestedSessionId": session_id,
                    "staleSession": is_stale_resume_error(&error),
                    "message": error.message,
                    "hint": error.hint,
                }))?;
                if is_stale_resume_error(&error) {
                    state.clear_active_session_id()?;
                } else {
                    return Err(error);
                }
            }
        }
    }

    let replay_prompt = build_replay_prompt(
        &prompt,
        state.get_replay_package()?,
        &state.recent_turns(12)?,
    );
    let result = run_exec(config, &replay_prompt, attachments)?;
    state.set_active_session_id(&result.session_id)?;

    Ok(CodexTurnResult {
        session_id: result.session_id,
        response_text: result.response_text,
        resumed: false,
        context_snapshots,
    })
}

pub fn prepare_codex_turn(
    config: &LoadedConfig,
    state: &StateStore,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<PreparedCodexTurn> {
    let context_snapshots = context_snapshots(config);
    let prompt = build_turn_prompt(
        config,
        channel,
        sender_id,
        user_text,
        attachments,
        &context_snapshots,
    );
    let replay_prompt = build_replay_prompt(
        &prompt,
        state.get_replay_package()?,
        &state.recent_turns(12)?,
    );

    Ok(PreparedCodexTurn {
        channel: channel.to_string(),
        sender_id,
        attachments: attachments.to_vec(),
        context_snapshots,
        prompt,
        replay_prompt,
        requested_session_id: state.get_active_session_id()?,
    })
}

pub async fn start_codex_turn(
    config: LoadedConfig,
    prepared: PreparedCodexTurn,
) -> KaiResult<RunningCodexTurn> {
    let (mut command, using_resume) = build_async_command(&config, &prepared)?;
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);

    let child = command.spawn().map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to launch Codex CLI: {error}"),
        )
    })?;
    let pid = child.id().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex CLI did not expose a process id",
        )
    })?;

    let (sender, receiver) = mpsc::channel();
    tokio::spawn(async move {
        let result = wait_for_codex_turn(config, child, prepared, using_resume).await;
        let _ = sender.send(result);
    });

    Ok(RunningCodexTurn { pid, receiver })
}

pub fn poll_running_codex_turn(
    turn: &mut RunningCodexTurn,
) -> Option<KaiResult<AsyncCodexTurnResult>> {
    turn.receiver.try_recv().ok()
}

pub fn cancel_codex_turn(turn: &RunningCodexTurn) -> KaiResult<()> {
    signal_process(turn.pid, "TERM")
}
