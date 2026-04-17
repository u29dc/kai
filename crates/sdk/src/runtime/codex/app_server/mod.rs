use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use tokio::sync::broadcast;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

use super::{
    AsyncCodexTurnResult, CodexProgressEvent, CodexTurnResult, PreparedCodexTurn, ResumeFailure,
    RunningCodexTurnEvent,
};

mod client;
mod input;
mod policy;
mod protocol;

pub use self::client::AppServerClient;

use self::input::build_turn_input;
use self::policy::{approval_policy, sandbox_policy};
use self::protocol::{
    ItemCompletedParams, ItemInfo, ItemStartedParams, ServerNotification, ThreadResumeParams,
    ThreadStartParams, TurnCompletedParams, TurnInterruptParams, TurnStartParams,
};

pub(super) fn parse_notification(method: &str, params: serde_json::Value) -> ServerNotification {
    match method {
        "thread/started" => ServerNotification::ThreadStarted,
        "turn/started" => ServerNotification::TurnStarted,
        "turn/completed" => {
            deserialize_notification(method, params, ServerNotification::TurnCompleted)
        }
        "item/started" => deserialize_notification(method, params, ServerNotification::ItemStarted),
        "item/completed" => {
            deserialize_notification(method, params, ServerNotification::ItemCompleted)
        }
        "item/agentMessage/delta" => {
            deserialize_notification(method, params, ServerNotification::AgentMessageDelta)
        }
        "item/plan/delta" => {
            deserialize_notification(method, params, ServerNotification::PlanDelta)
        }
        "item/reasoning/summaryTextDelta" => deserialize_notification(
            method,
            params,
            ServerNotification::ReasoningSummaryTextDelta,
        ),
        "item/commandExecution/outputDelta" => ServerNotification::CommandExecutionOutputDelta,
        _ => ServerNotification::Unknown {
            method: method.to_string(),
        },
    }
}

pub struct RunningAppServerTurn {
    client: Arc<AppServerClient>,
    receiver: broadcast::Receiver<ServerNotification>,
    pending_events: Receiver<RunningCodexTurnEvent>,
    thread_id: String,
    turn_id: String,
    resumed: bool,
    context_snapshots: Vec<crate::context::ContextSnapshot>,
    response_text: Option<String>,
    completed: bool,
}

pub async fn prepare_or_start_turn(
    config: LoadedConfig,
    prepared: PreparedCodexTurn,
) -> KaiResult<RunningAppServerTurn> {
    let client = AppServerClient::shared(&config).await?;
    start_turn_with_client(client, config, prepared).await
}

pub async fn run_turn_once(
    config: LoadedConfig,
    prepared: PreparedCodexTurn,
) -> KaiResult<AsyncCodexTurnResult> {
    let client = AppServerClient::ephemeral(&config).await?;
    let mut running = start_turn_with_client(client, config, prepared).await?;

    loop {
        let events = drain_turn_events(&mut running);
        for event in events {
            if let RunningCodexTurnEvent::Completed(result) = event {
                return result;
            }
        }
        tokio::task::yield_now().await;
    }
}

pub fn drain_turn_events(turn: &mut RunningAppServerTurn) -> Vec<RunningCodexTurnEvent> {
    if turn.completed {
        return Vec::new();
    }

    let mut events = Vec::new();
    while let Ok(event) = turn.pending_events.try_recv() {
        events.push(event);
    }

    loop {
        match turn.receiver.try_recv() {
            Ok(notification) => {
                if let Some(event) = apply_notification(turn, notification) {
                    if matches!(event, RunningCodexTurnEvent::Completed(_)) {
                        turn.completed = true;
                    }
                    events.push(event);
                }
            }
            Err(broadcast::error::TryRecvError::Empty) => break,
            Err(broadcast::error::TryRecvError::Lagged(_)) => continue,
            Err(broadcast::error::TryRecvError::Closed) => {
                turn.completed = true;
                events.push(RunningCodexTurnEvent::Completed(Err(KaiError::new(
                    ErrorCode::RuntimeError,
                    "Codex App Server notification stream closed unexpectedly",
                ))));
                break;
            }
        }
    }

    events
}

pub async fn cancel_turn(turn: &RunningAppServerTurn) -> KaiResult<()> {
    turn.client
        .turn_interrupt(TurnInterruptParams {
            thread_id: turn.thread_id.clone(),
            turn_id: turn.turn_id.clone(),
        })
        .await
}

pub fn handshake_smoke_test(config: &LoadedConfig) -> KaiResult<()> {
    let future = AppServerClient::initialize_smoke_test(config);
    if let Ok(handle) = tokio::runtime::Handle::try_current() {
        tokio::task::block_in_place(|| handle.block_on(future))
    } else {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| {
                KaiError::new(
                    ErrorCode::RuntimeError,
                    format!("failed to build app-server runtime: {error}"),
                )
            })?
            .block_on(future)
    }
}

async fn start_turn_with_client(
    client: Arc<AppServerClient>,
    config: LoadedConfig,
    prepared: PreparedCodexTurn,
) -> KaiResult<RunningAppServerTurn> {
    let (pending_sender, pending_events) = mpsc::channel();
    let approval_policy = approval_policy(&config);

    let (thread_id, resumed) = if let Some(requested_session_id) = &prepared.requested_session_id {
        match client
            .thread_resume(ThreadResumeParams {
                thread_id: requested_session_id.clone(),
                cwd: Some(prepared.target.working_dir.clone()),
                approval_policy: approval_policy.clone(),
            })
            .await
        {
            Ok(response) => (response.thread.id, true),
            Err(error) if is_stale_resume_error(&error) => {
                let resume_failure = ResumeFailure {
                    requested_session_id: requested_session_id.clone(),
                    stale_session: true,
                    error: error.clone(),
                };
                let _ = pending_sender.send(RunningCodexTurnEvent::ResumeFailure(resume_failure));
                let response = client
                    .thread_start(ThreadStartParams {
                        cwd: Some(prepared.target.working_dir.clone()),
                        approval_policy: approval_policy.clone(),
                        service_name: config.values.runner.codex.service_name.clone(),
                    })
                    .await?;
                (response.thread.id, false)
            }
            Err(error) => return Err(error),
        }
    } else {
        let response = client
            .thread_start(ThreadStartParams {
                cwd: Some(prepared.target.working_dir.clone()),
                approval_policy: approval_policy.clone(),
                service_name: config.values.runner.codex.service_name.clone(),
            })
            .await?;
        (response.thread.id, false)
    };

    let receiver = client.subscribe();
    let response = client
        .turn_start(TurnStartParams {
            thread_id: thread_id.clone(),
            input: build_turn_input(&prepared.prompt, &prepared.attachments),
            cwd: Some(prepared.target.working_dir.clone()),
            approval_policy,
            sandbox_policy: sandbox_policy(&config, &prepared.target, &prepared.attachments),
            summary: Some("concise".to_string()),
        })
        .await?;

    Ok(RunningAppServerTurn {
        client,
        receiver,
        pending_events,
        thread_id,
        turn_id: response.turn.id,
        resumed,
        context_snapshots: prepared.context_snapshots,
        response_text: None,
        completed: false,
    })
}

fn apply_notification(
    turn: &mut RunningAppServerTurn,
    notification: ServerNotification,
) -> Option<RunningCodexTurnEvent> {
    match notification {
        ServerNotification::ItemStarted(params) => map_item_started(turn, params),
        ServerNotification::ItemCompleted(params) => map_item_completed(turn, params),
        ServerNotification::AgentMessageDelta(params) => filter_text_delta(
            turn,
            params.thread_id,
            params.turn_id,
            CodexProgressEvent::AgentMessage { text: params.delta },
        ),
        ServerNotification::PlanDelta(params) => filter_text_delta(
            turn,
            params.thread_id,
            params.turn_id,
            CodexProgressEvent::Plan { text: params.delta },
        ),
        ServerNotification::ReasoningSummaryTextDelta(params) => filter_text_delta(
            turn,
            params.thread_id,
            params.turn_id,
            CodexProgressEvent::ReasoningSummary { text: params.delta },
        ),
        ServerNotification::CommandExecutionOutputDelta => None,
        ServerNotification::TurnCompleted(params) => map_turn_completed(turn, params),
        ServerNotification::ServerExited { message } => Some(RunningCodexTurnEvent::Completed(
            Err(KaiError::new(ErrorCode::RuntimeError, message)),
        )),
        ServerNotification::ThreadStarted | ServerNotification::TurnStarted => None,
        ServerNotification::Unknown { method } => {
            let _ = method;
            None
        }
    }
}

fn map_item_started(
    turn: &RunningAppServerTurn,
    params: ItemStartedParams,
) -> Option<RunningCodexTurnEvent> {
    if !matches_turn(turn, &params.thread_id, &params.turn_id) {
        return None;
    }

    match params.item {
        ItemInfo::CommandExecution { command, .. } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::CommandStarted { command },
        )),
        _ => None,
    }
}

fn map_item_completed(
    turn: &mut RunningAppServerTurn,
    params: ItemCompletedParams,
) -> Option<RunningCodexTurnEvent> {
    if !matches_turn(turn, &params.thread_id, &params.turn_id) {
        return None;
    }

    if let ItemInfo::AgentMessage { text, phase, .. } = params.item
        && phase.as_deref() == Some("final_answer")
        && !text.trim().is_empty()
    {
        turn.response_text = Some(text);
    }

    None
}

fn map_turn_completed(
    turn: &mut RunningAppServerTurn,
    params: TurnCompletedParams,
) -> Option<RunningCodexTurnEvent> {
    if !matches_turn(turn, &params.thread_id, &params.turn.id) {
        return None;
    }

    if !params.turn.status.eq_ignore_ascii_case("completed") {
        let mut error = KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "Codex App Server turn ended with status {}",
                params.turn.status
            ),
        );
        if let Some(turn_error) = params.turn.error.and_then(|error| error.message) {
            error = error.with_hint(turn_error);
        }
        return Some(RunningCodexTurnEvent::Completed(Err(error)));
    }

    let response_text = turn.response_text.clone().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex App Server completed without a final assistant message",
        )
    });

    Some(RunningCodexTurnEvent::Completed(response_text.map(
        |response_text| AsyncCodexTurnResult {
            result: CodexTurnResult {
                session_id: turn.thread_id.clone(),
                response_text,
                resumed: turn.resumed,
                context_snapshots: turn.context_snapshots.clone(),
            },
            resume_failure: None,
        },
    )))
}

fn filter_text_delta(
    turn: &RunningAppServerTurn,
    thread_id: String,
    turn_id: String,
    progress: CodexProgressEvent,
) -> Option<RunningCodexTurnEvent> {
    if !matches_turn(turn, &thread_id, &turn_id) {
        return None;
    }
    Some(RunningCodexTurnEvent::Progress(progress))
}

fn matches_turn(turn: &RunningAppServerTurn, thread_id: &str, turn_id: &str) -> bool {
    turn.thread_id == thread_id && turn.turn_id == turn_id
}

fn is_stale_resume_error(error: &KaiError) -> bool {
    let mut text = error.message.to_ascii_lowercase();
    if let Some(hint) = &error.hint {
        text.push('\n');
        text.push_str(&hint.to_ascii_lowercase());
    }

    text.contains("no rollout found for thread id")
        || text.contains("thread not found")
        || text.contains("unknown thread")
        || text.contains("session not found")
        || (text.contains("thread/resume failed") && text.contains("not found"))
}

fn deserialize_notification<T>(
    method: &str,
    params: serde_json::Value,
    build: impl FnOnce(T) -> ServerNotification,
) -> ServerNotification
where
    T: serde::de::DeserializeOwned,
{
    match serde_json::from_value::<T>(params.clone()) {
        Ok(parsed) => build(parsed),
        Err(_) => ServerNotification::Unknown {
            method: method.to_string(),
        },
    }
}
