use std::collections::HashMap;
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
    agent_message_buffers: HashMap<String, DeltaProgressBuffer>,
    plan_buffers: HashMap<String, DeltaProgressBuffer>,
    reasoning_buffers: HashMap<String, DeltaProgressBuffer>,
    completed: bool,
}

#[derive(Default)]
struct DeltaProgressBuffer {
    text: String,
    last_emitted: Option<String>,
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
        agent_message_buffers: HashMap::new(),
        plan_buffers: HashMap::new(),
        reasoning_buffers: HashMap::new(),
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
        ServerNotification::AgentMessageDelta(params) => {
            let snapshot = append_progress_delta(
                &mut turn.agent_message_buffers,
                &params.item_id,
                &params.delta,
                18,
                28,
            );
            filter_text_delta(
                turn,
                params.thread_id,
                params.turn_id,
                snapshot.map(|text| CodexProgressEvent::AgentMessage { text }),
            )
        }
        ServerNotification::PlanDelta(params) => {
            let snapshot = append_progress_delta(
                &mut turn.plan_buffers,
                &params.item_id,
                &params.delta,
                12,
                20,
            );
            filter_text_delta(
                turn,
                params.thread_id,
                params.turn_id,
                snapshot.map(|text| CodexProgressEvent::Plan { text }),
            )
        }
        ServerNotification::ReasoningSummaryTextDelta(params) => {
            let key = format!("{}:{}", params.item_id, params.summary_index);
            let snapshot =
                append_progress_delta(&mut turn.reasoning_buffers, &key, &params.delta, 18, 28);
            filter_text_delta(
                turn,
                params.thread_id,
                params.turn_id,
                snapshot.map(|text| CodexProgressEvent::ReasoningSummary { text }),
            )
        }
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

    match params.item {
        ItemInfo::AgentMessage { id, text, phase } => {
            turn.agent_message_buffers.remove(&id);
            if phase.as_deref() == Some("final_answer") && !text.trim().is_empty() {
                turn.response_text = Some(text);
            }
        }
        ItemInfo::Plan { id, text } => {
            turn.plan_buffers.remove(&id);
            if !text.trim().is_empty() {
                return Some(RunningCodexTurnEvent::Progress(CodexProgressEvent::Plan {
                    text,
                }));
            }
        }
        ItemInfo::Reasoning { id, summary } => {
            turn.reasoning_buffers
                .retain(|key, _| !key.starts_with(&format!("{id}:")));
            let text = summary
                .into_iter()
                .map(|part| part.trim().to_string())
                .filter(|part| !part.is_empty())
                .collect::<Vec<_>>()
                .join(" ");
            if !text.is_empty() {
                return Some(RunningCodexTurnEvent::Progress(
                    CodexProgressEvent::ReasoningSummary { text },
                ));
            }
        }
        ItemInfo::CommandExecution { id, .. } => {
            let _ = id;
        }
        ItemInfo::Unknown => {}
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
    progress: Option<CodexProgressEvent>,
) -> Option<RunningCodexTurnEvent> {
    if !matches_turn(turn, &thread_id, &turn_id) {
        return None;
    }
    progress.map(RunningCodexTurnEvent::Progress)
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

fn append_progress_delta(
    buffers: &mut HashMap<String, DeltaProgressBuffer>,
    key: &str,
    delta: &str,
    min_chars: usize,
    min_growth: usize,
) -> Option<String> {
    let buffer = buffers.entry(key.to_string()).or_default();
    buffer.text.push_str(delta);

    let candidate = collapse_whitespace(&buffer.text);
    if candidate.chars().count() < min_chars {
        return None;
    }

    if buffer.last_emitted.as_deref() == Some(candidate.as_str()) {
        return None;
    }

    let last_len = buffer
        .last_emitted
        .as_ref()
        .map(|text| text.chars().count())
        .unwrap_or(0);
    let current_len = candidate.chars().count();
    let grew_by = current_len.saturating_sub(last_len);
    let ends_cleanly = candidate.ends_with(['.', '!', '?', ':']);
    if !ends_cleanly && grew_by < min_growth {
        return None;
    }

    buffer.last_emitted = Some(candidate.clone());
    Some(candidate)
}

fn collapse_whitespace(input: &str) -> String {
    input.split_whitespace().collect::<Vec<_>>().join(" ")
}
