use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver};

use serde_json::Value as JsonValue;
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
    ThreadStartParams, TurnCompletedParams, TurnInterruptParams, TurnStartParams, WebSearchAction,
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
        "item/commandExecution/outputDelta" => deserialize_notification(
            method,
            params,
            ServerNotification::CommandExecutionOutputDelta,
        ),
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
        ServerNotification::CommandExecutionOutputDelta(params) => {
            let _ = (
                matches_turn(turn, &params.thread_id, &params.turn_id),
                &params.item_id,
                &params.stream,
                &params.delta,
            );
            None
        }
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
        ItemInfo::WebSearch { query, action, .. } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: describe_web_search(query, action),
            },
        )),
        ItemInfo::McpToolCall {
            server,
            tool,
            arguments,
            ..
        } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: describe_tool_call(&format!("{server}.{tool}"), arguments),
            },
        )),
        ItemInfo::DynamicToolCall {
            tool, arguments, ..
        } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: describe_tool_call(&tool, arguments),
            },
        )),
        ItemInfo::FileChange { changes, .. } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: describe_file_change(&changes),
            },
        )),
        ItemInfo::ImageView { path, .. } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: describe_image_view(&path),
            },
        )),
        ItemInfo::ContextCompaction { .. } => Some(RunningCodexTurnEvent::Progress(
            CodexProgressEvent::StructuredActivity {
                text: "Compacting context.".to_string(),
            },
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
        ItemInfo::FileChange { id, .. }
        | ItemInfo::McpToolCall { id, .. }
        | ItemInfo::DynamicToolCall { id, .. }
        | ItemInfo::WebSearch { id, .. }
        | ItemInfo::ImageView { id, .. }
        | ItemInfo::ContextCompaction { id } => {
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

fn describe_web_search(query: Option<String>, action: Option<WebSearchAction>) -> String {
    match action {
        Some(WebSearchAction::Search { query, queries }) => {
            let query = query.or_else(|| queries.and_then(|queries| queries.into_iter().next()));
            format_search_query(query.as_deref())
                .unwrap_or_else(|| "Searching the web.".to_string())
        }
        Some(WebSearchAction::OpenPage { url }) => {
            if let Some(host) = url.as_deref().and_then(short_url_host) {
                format!("Opening {host}.")
            } else {
                "Opening a page.".to_string()
            }
        }
        Some(WebSearchAction::FindInPage { url, pattern }) => {
            let pattern = format_detail(pattern.as_deref());
            match (url.as_deref().and_then(short_url_host), pattern) {
                (Some(host), Some(pattern)) => {
                    format!("Searching within {host} for \"{pattern}\".")
                }
                (Some(host), None) => format!("Searching within {host}."),
                (None, Some(pattern)) => format!("Searching a page for \"{pattern}\"."),
                (None, None) => "Searching within a page.".to_string(),
            }
        }
        Some(WebSearchAction::Unknown) | None => format_search_query(query.as_deref())
            .unwrap_or_else(|| "Searching the web.".to_string()),
    }
}

fn describe_tool_call(title: &str, arguments: Option<JsonValue>) -> String {
    let title = collapse_whitespace(title);
    let detail = arguments.as_ref().and_then(summarize_json_hint);
    match detail {
        Some(detail) => format!("Calling {title} for \"{detail}\"."),
        None => format!("Calling {title}."),
    }
}

fn describe_file_change(changes: &[self::protocol::FileChange]) -> String {
    let mut files = changes
        .iter()
        .filter_map(|change| short_path_label(&change.path))
        .collect::<Vec<_>>();
    files.sort();
    files.dedup();

    match files.as_slice() {
        [] => "Preparing a patch.".to_string(),
        [file] => format!("Preparing a patch for {file}."),
        [first, second, ..] => format!("Preparing patches for {first} and {second}."),
    }
}

fn describe_image_view(path: &str) -> String {
    short_path_label(path)
        .map(|label| format!("Inspecting {label}."))
        .unwrap_or_else(|| "Inspecting an image.".to_string())
}

fn format_search_query(query: Option<&str>) -> Option<String> {
    let query = format_detail(query)?;
    Some(format!("Searching the web for \"{query}\"."))
}

fn summarize_json_hint(value: &JsonValue) -> Option<String> {
    const PRIORITY_KEYS: &[&str] = &[
        "query", "q", "pattern", "path", "file", "url", "name", "command", "title",
    ];

    match value {
        JsonValue::String(text) => format_detail(Some(text)),
        JsonValue::Array(items) => items.iter().find_map(summarize_json_hint),
        JsonValue::Object(map) => {
            for key in PRIORITY_KEYS {
                if let Some(value) = map.get(*key).and_then(summarize_json_hint) {
                    return Some(value);
                }
            }
            map.values().find_map(summarize_json_hint)
        }
        JsonValue::Null | JsonValue::Bool(_) | JsonValue::Number(_) => None,
    }
}

fn short_url_host(url: &str) -> Option<String> {
    let trimmed = url.trim();
    if trimmed.is_empty() {
        return None;
    }

    let without_scheme = trimmed
        .strip_prefix("https://")
        .or_else(|| trimmed.strip_prefix("http://"))
        .unwrap_or(trimmed);
    let host = without_scheme
        .split(['/', '?', '#'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    Some(host.to_string())
}

fn short_path_label(path: &str) -> Option<String> {
    let trimmed = path.trim();
    if trimmed.is_empty() {
        return None;
    }

    Path::new(trimmed)
        .file_name()
        .and_then(|value| value.to_str())
        .map(|value| value.to_string())
        .or_else(|| Some(trimmed.to_string()))
}

fn format_detail(input: Option<&str>) -> Option<String> {
    let text = input.map(collapse_whitespace)?;
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return None;
    }

    let max_chars = 80;
    let mut output = String::new();
    for (index, ch) in trimmed.chars().enumerate() {
        if index >= max_chars {
            break;
        }
        output.push(ch);
    }
    let output = output.trim();
    if output.is_empty() {
        return None;
    }

    if trimmed.chars().count() > max_chars {
        Some(format!("{output}..."))
    } else {
        Some(output.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parse_notification_deserializes_command_output_delta_payload() {
        let notification = parse_notification(
            "item/commandExecution/outputDelta",
            json!({
                "threadId": "thread-1",
                "turnId": "turn-1",
                "itemId": "item-1",
                "stream": "stdout",
                "delta": "Compiling kai"
            }),
        );

        match notification {
            ServerNotification::CommandExecutionOutputDelta(params) => {
                assert_eq!(params.thread_id, "thread-1");
                assert_eq!(params.turn_id, "turn-1");
                assert_eq!(params.item_id, "item-1");
                assert_eq!(params.delta.as_deref(), Some("Compiling kai"));
            }
            other => panic!("unexpected notification: {other:?}"),
        }
    }

    #[test]
    fn structured_activity_descriptions_are_human_readable() {
        assert_eq!(
            describe_web_search(Some("codex app server".to_string()), None),
            "Searching the web for \"codex app server\"."
        );
        assert_eq!(
            describe_tool_call(
                "github.search_issues",
                Some(json!({ "query": "progress updates" }))
            ),
            "Calling github.search_issues for \"progress updates\"."
        );
        assert_eq!(
            describe_file_change(&[self::protocol::FileChange {
                path: "/tmp/progress.rs".to_string(),
            }]),
            "Preparing a patch for progress.rs."
        );
        assert_eq!(
            describe_image_view("/tmp/screenshot.png"),
            "Inspecting screenshot.png."
        );
    }
}
