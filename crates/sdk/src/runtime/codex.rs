use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value as JsonValue;

use crate::config::LoadedConfig;
use crate::context::load_context_blobs;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::state::{AttachmentInfo, StateStore};

#[derive(Debug, Clone)]
pub struct CodexTurnResult {
    pub session_id: String,
    pub response_text: String,
    pub resumed: bool,
}

pub fn run_codex_turn(
    config: &LoadedConfig,
    state: &StateStore,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<CodexTurnResult> {
    let context = load_context_blobs(config)?;
    let prompt = build_turn_prompt(config, channel, sender_id, user_text, attachments, &context);

    if let Some(session_id) = state.get_active_session_id()? {
        match run_resume(config, &session_id, &prompt) {
            Ok(result) => {
                state.set_active_session_id(&result.session_id)?;
                return Ok(CodexTurnResult {
                    session_id: result.session_id,
                    response_text: result.response_text,
                    resumed: true,
                });
            }
            Err(_) => {
                state.clear_active_session_id()?;
            }
        }
    }

    let recent_turns = state.recent_turns(8)?;
    let replay_prompt = build_replay_prompt(&prompt, &recent_turns);
    let result = run_exec(config, &replay_prompt, attachments)?;
    state.set_active_session_id(&result.session_id)?;

    Ok(CodexTurnResult {
        session_id: result.session_id,
        response_text: result.response_text,
        resumed: false,
    })
}

#[derive(Debug)]
struct RawCodexResult {
    session_id: String,
    response_text: String,
}

fn run_exec(
    config: &LoadedConfig,
    prompt: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<RawCodexResult> {
    let mut command = Command::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    command.arg("-C");
    command.arg(&config.values.paths.root_work);

    for path in extra_access_paths(config, attachments) {
        command.arg("--add-dir");
        command.arg(path);
    }

    apply_codex_overrides(config, &mut command);
    apply_image_args(&mut command, attachments);
    command.arg(prompt);

    run_command(command)
}

fn run_resume(config: &LoadedConfig, session_id: &str, prompt: &str) -> KaiResult<RawCodexResult> {
    let mut command = Command::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("resume");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    apply_codex_overrides(config, &mut command);
    command.arg(session_id);
    command.arg(prompt);

    run_command(command)
}

fn run_command(mut command: Command) -> KaiResult<RawCodexResult> {
    let output = command.output().map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to launch Codex CLI: {error}"),
        )
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!("Codex CLI exited with status {}", output.status),
        )
        .with_hint(stderr));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    parse_jsonl_output(&stdout)
}

fn parse_jsonl_output(stdout: &str) -> KaiResult<RawCodexResult> {
    let mut session_id: Option<String> = None;
    let mut response_text: Option<String> = None;

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value = match serde_json::from_str::<JsonValue>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };

        if value.get("type").and_then(JsonValue::as_str) == Some("thread.started")
            && let Some(thread_id) = value.get("thread_id").and_then(JsonValue::as_str)
        {
            session_id = Some(thread_id.to_string());
        }

        if value.get("type").and_then(JsonValue::as_str) == Some("item.completed")
            && let Some(item) = value.get("item")
        {
            let is_agent_message =
                item.get("type").and_then(JsonValue::as_str) == Some("agent_message");
            if is_agent_message && let Some(text) = item.get("text").and_then(JsonValue::as_str) {
                response_text = Some(text.to_string());
            }
        }
    }

    let session_id = session_id.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex did not emit a session id in JSON output",
        )
    })?;
    let response_text = response_text.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex did not emit a completed assistant message",
        )
    })?;

    Ok(RawCodexResult {
        session_id,
        response_text,
    })
}

fn build_turn_prompt(
    config: &LoadedConfig,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
    context: &[crate::context::ContextBlob],
) -> String {
    let mut sections = vec![
		"You are replying through kai, a private owner-only Telegram portal into Codex running on the operator's machine.".to_string(),
		"Reply concisely for a phone chat unless the user clearly asks for depth.".to_string(),
		String::new(),
		"Turn envelope:".to_string(),
		format!("- channel: {channel}"),
		format!("- sender_id: {sender_id}"),
		format!("- local_timezone: {}", config.values.agent.timezone),
		"- operating_mode: reactive, owner-only".to_string(),
	];

    if attachments.is_empty() {
        sections.push("- attachments: none".to_string());
    } else {
        sections.push("- attachments:".to_string());
        for attachment in attachments {
            sections.push(format!("  - {} at {}", attachment.kind, attachment.path));
        }
    }

    for blob in context {
        sections.push(String::new());
        sections.push(format!("<{} path=\"{}\">", blob.role, blob.path));
        sections.push(blob.content.clone());
        sections.push(format!("</{}>", blob.role));
    }

    sections.push(String::new());
    sections.push("User message:".to_string());
    sections.push(user_text.to_string());

    sections.join("\n")
}

fn build_replay_prompt(prompt: &str, recent_turns: &[crate::state::TurnRecord]) -> String {
    if recent_turns.is_empty() {
        return prompt.to_string();
    }

    let mut replay = vec![
        "Session recovery context for kai.".to_string(),
        "Recent turns:".to_string(),
    ];

    for turn in recent_turns {
        replay.push(format!(
            "[{}] {}: {}",
            turn.created_at, turn.role, turn.text
        ));
    }

    replay.push(String::new());
    replay.push("Current inbound turn:".to_string());
    replay.push(prompt.to_string());
    replay.join("\n")
}

fn apply_codex_overrides(config: &LoadedConfig, command: &mut Command) {
    if let Some(override_config) = &config.values.runner.codex.override_config {
        if let Some(approval_policy) = &override_config.approval_policy {
            command.arg("-c");
            command.arg(format!("approval_policy=\"{approval_policy}\""));
        }
        if let Some(sandbox_mode) = &override_config.sandbox_mode {
            command.arg("-s");
            command.arg(sandbox_mode);
        }
    }
}

fn apply_image_args(command: &mut Command, attachments: &[AttachmentInfo]) {
    for attachment in attachments
        .iter()
        .filter(|attachment| attachment.kind == "image")
    {
        command.arg("-i");
        command.arg(&attachment.path);
    }
}

fn extra_access_paths(config: &LoadedConfig, attachments: &[AttachmentInfo]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(&config.values.paths.root_app)];

    for context_path in [
        config.values.context_files.soul.as_str(),
        config.values.context_files.memory.as_str(),
        config.values.context_files.todo.as_str(),
    ] {
        if let Some(parent) = Path::new(context_path).parent() {
            paths.push(parent.to_path_buf());
        }
    }

    for attachment in attachments {
        if let Some(parent) = Path::new(&attachment.path).parent() {
            paths.push(parent.to_path_buf());
        }
    }

    paths.sort();
    paths.dedup();
    paths
}
