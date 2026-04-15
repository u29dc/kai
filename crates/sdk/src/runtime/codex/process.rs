use super::*;
use std::sync::mpsc::Sender;

#[derive(Debug)]
pub(super) struct RawCodexResult {
    pub(super) session_id: String,
    pub(super) response_text: String,
}

#[derive(Debug, Default)]
struct ParsedCodexOutput {
    session_id: Option<String>,
    response_text: Option<String>,
}

pub(super) fn build_async_command(
    config: &LoadedConfig,
    prepared: &PreparedCodexTurn,
) -> KaiResult<(TokioCommand, bool)> {
    if let Some(session_id) = &prepared.requested_session_id {
        let mut command = TokioCommand::new(&config.values.runner.codex.binary);
        command.arg("exec");
        command.arg("resume");
        command.arg("--json");
        command.arg("--skip-git-repo-check");
        command.current_dir(&prepared.target.working_dir);
        apply_codex_overrides_tokio(config, &mut command);
        apply_image_args_tokio(&mut command, &prepared.attachments);
        command.arg(session_id);
        command.arg(&prepared.prompt);
        return Ok((command, true));
    }

    let mut command = TokioCommand::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    command.arg("-C");
    command.arg(&prepared.target.working_dir);
    command.current_dir(&prepared.target.working_dir);

    for path in extra_access_paths(config, &prepared.attachments) {
        command.arg("--add-dir");
        command.arg(path);
    }

    apply_codex_overrides_tokio(config, &mut command);
    apply_image_args_tokio(&mut command, &prepared.attachments);
    command.arg(&prepared.replay_prompt);
    Ok((command, false))
}

pub(super) async fn wait_for_codex_turn(
    config: LoadedConfig,
    mut child: tokio::process::Child,
    prepared: PreparedCodexTurn,
    using_resume: bool,
    progress_sender: Sender<RunningCodexTurnEvent>,
) -> KaiResult<AsyncCodexTurnResult> {
    let stdout = child.stdout.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex CLI did not expose stdout for JSON parsing",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex CLI did not expose stderr for diagnostics",
        )
    })?;

    let stdout_progress_sender = progress_sender.clone();
    let stdout_task =
        tokio::spawn(async move { parse_jsonl_stream(stdout, stdout_progress_sender).await });
    let stderr_task = tokio::spawn(async move { read_stream_to_string(stderr).await });

    let status = child.wait().await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to wait for Codex CLI: {error}"),
        )
    })?;
    let parsed = stdout_task.await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to join Codex stdout parser: {error}"),
        )
    })??;
    let stderr_text = stderr_task.await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to join Codex stderr reader: {error}"),
        )
    })??;

    let resume_failure = if status.success() {
        None
    } else if using_resume {
        let error = codex_process_failure_from_parts(&status, &stderr_text);
        if let Some(requested_session_id) = &prepared.requested_session_id
            && is_stale_resume_error(&error)
        {
            let resume_failure = ResumeFailure {
                requested_session_id: requested_session_id.clone(),
                stale_session: true,
                error: error.clone(),
            };
            let _ =
                progress_sender.send(RunningCodexTurnEvent::ResumeFailure(resume_failure.clone()));
            let fallback =
                run_exec_async_fallback(&config, &prepared, progress_sender.clone()).await?;
            return Ok(AsyncCodexTurnResult {
                result: CodexTurnResult {
                    session_id: fallback.session_id,
                    response_text: fallback.response_text,
                    resumed: false,
                    context_snapshots: prepared.context_snapshots,
                },
                resume_failure: Some(resume_failure),
            });
        }

        return Err(error);
    } else {
        return Err(codex_process_failure_from_parts(&status, &stderr_text));
    };

    let raw = finalize_parsed_output(parsed)?;
    Ok(AsyncCodexTurnResult {
        result: CodexTurnResult {
            session_id: raw.session_id,
            response_text: raw.response_text,
            resumed: using_resume,
            context_snapshots: prepared.context_snapshots,
        },
        resume_failure,
    })
}

async fn run_exec_async_fallback(
    config: &LoadedConfig,
    prepared: &PreparedCodexTurn,
    progress_sender: Sender<RunningCodexTurnEvent>,
) -> KaiResult<RawCodexResult> {
    let mut command = TokioCommand::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    command.arg("-C");
    command.arg(&prepared.target.working_dir);
    command.current_dir(&prepared.target.working_dir);

    for path in extra_access_paths(config, &prepared.attachments) {
        command.arg("--add-dir");
        command.arg(path);
    }

    apply_codex_overrides_tokio(config, &mut command);
    apply_image_args_tokio(&mut command, &prepared.attachments);
    command.arg(&prepared.replay_prompt);
    command.stdout(Stdio::piped());
    command.stderr(Stdio::piped());
    command.kill_on_drop(true);

    let mut child = command.spawn().map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to launch Codex CLI: {error}"),
        )
    })?;
    let stdout = child.stdout.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex CLI fallback run did not expose stdout for JSON parsing",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex CLI fallback run did not expose stderr for diagnostics",
        )
    })?;
    let stdout_progress_sender = progress_sender.clone();
    let stdout_task =
        tokio::spawn(async move { parse_jsonl_stream(stdout, stdout_progress_sender).await });
    let stderr_task = tokio::spawn(async move { read_stream_to_string(stderr).await });

    let status = child.wait().await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to wait for Codex CLI: {error}"),
        )
    })?;
    let parsed = stdout_task.await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to join Codex fallback stdout parser: {error}"),
        )
    })??;
    let stderr_text = stderr_task.await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to join Codex fallback stderr reader: {error}"),
        )
    })??;

    if !status.success() {
        return Err(codex_process_failure_from_parts(&status, &stderr_text));
    }

    finalize_parsed_output(parsed)
}

fn codex_process_failure_from_parts(status: &std::process::ExitStatus, stderr: &str) -> KaiError {
    KaiError::new(
        ErrorCode::RuntimeError,
        format!("Codex CLI exited with status {status}"),
    )
    .with_hint(stderr.trim())
}

pub(super) fn run_exec(
    config: &LoadedConfig,
    target: &crate::workspace::ExecutionTarget,
    prompt: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<RawCodexResult> {
    let mut command = Command::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    command.arg("-C");
    command.arg(&target.working_dir);
    command.current_dir(&target.working_dir);

    for path in extra_access_paths(config, attachments) {
        command.arg("--add-dir");
        command.arg(path);
    }

    apply_codex_overrides(config, &mut command);
    apply_image_args(&mut command, attachments);
    command.arg(prompt);

    run_command(command)
}

pub(super) fn run_resume(
    config: &LoadedConfig,
    target: &crate::workspace::ExecutionTarget,
    session_id: &str,
    prompt: &str,
    attachments: &[AttachmentInfo],
) -> KaiResult<RawCodexResult> {
    let mut command = Command::new(&config.values.runner.codex.binary);
    command.arg("exec");
    command.arg("resume");
    command.arg("--json");
    command.arg("--skip-git-repo-check");
    command.current_dir(&target.working_dir);
    apply_codex_overrides(config, &mut command);
    apply_image_args(&mut command, attachments);
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

async fn parse_jsonl_stream<R>(
    reader: R,
    progress_sender: Sender<RunningCodexTurnEvent>,
) -> KaiResult<ParsedCodexOutput>
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut parsed = ParsedCodexOutput::default();

    while let Some(line) = lines.next_line().await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to read Codex JSON stream: {error}"),
        )
    })? {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        let value = match serde_json::from_str::<JsonValue>(trimmed) {
            Ok(value) => value,
            Err(_) => continue,
        };
        apply_codex_json_value(&mut parsed, &value, Some(&progress_sender));
    }

    Ok(parsed)
}

async fn read_stream_to_string<R>(reader: R) -> KaiResult<String>
where
    R: AsyncRead + Unpin,
{
    let mut lines = BufReader::new(reader).lines();
    let mut chunks = Vec::new();
    while let Some(line) = lines.next_line().await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to read Codex stderr stream: {error}"),
        )
    })? {
        chunks.push(line);
    }
    Ok(chunks.join("\n"))
}

fn apply_codex_json_value(
    parsed: &mut ParsedCodexOutput,
    value: &JsonValue,
    progress_sender: Option<&Sender<RunningCodexTurnEvent>>,
) {
    if value.get("type").and_then(JsonValue::as_str) == Some("thread.started")
        && let Some(thread_id) = value.get("thread_id").and_then(JsonValue::as_str)
    {
        parsed.session_id = Some(thread_id.to_string());
    }

    if value.get("type").and_then(JsonValue::as_str) == Some("item.started")
        && let Some(item) = value.get("item")
    {
        let item_type = item.get("type").and_then(JsonValue::as_str);
        if item_type == Some("command_execution")
            && let Some(command) = item.get("command").and_then(JsonValue::as_str)
        {
            send_progress_event(
                progress_sender,
                CodexProgressEvent::CommandStarted {
                    command: command.to_string(),
                },
            );
        }
    }

    if value.get("type").and_then(JsonValue::as_str) == Some("item.completed")
        && let Some(item) = value.get("item")
    {
        let item_type = item.get("type").and_then(JsonValue::as_str);
        let text = item.get("text").and_then(JsonValue::as_str);

        match item_type {
            Some("agent_message") => {
                if let Some(text) = text {
                    parsed.response_text = Some(text.to_string());
                    send_progress_event(
                        progress_sender,
                        CodexProgressEvent::AgentMessage {
                            text: text.to_string(),
                        },
                    );
                }
            }
            Some("plan") => {
                if let Some(text) = text {
                    send_progress_event(
                        progress_sender,
                        CodexProgressEvent::Plan {
                            text: text.to_string(),
                        },
                    );
                }
            }
            Some("reasoning") => {
                if let Some(text) = text {
                    send_progress_event(
                        progress_sender,
                        CodexProgressEvent::ReasoningSummary {
                            text: text.to_string(),
                        },
                    );
                }
            }
            _ => {}
        }
    }
}

fn finalize_parsed_output(parsed: ParsedCodexOutput) -> KaiResult<RawCodexResult> {
    let session_id = parsed.session_id.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Codex did not emit a session id in JSON output",
        )
    })?;
    let response_text = parsed.response_text.ok_or_else(|| {
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

fn parse_jsonl_output(stdout: &str) -> KaiResult<RawCodexResult> {
    let mut parsed = ParsedCodexOutput::default();

    for line in stdout.lines().filter(|line| !line.trim().is_empty()) {
        let value = match serde_json::from_str::<JsonValue>(line) {
            Ok(value) => value,
            Err(_) => continue,
        };
        apply_codex_json_value(&mut parsed, &value, None);
    }

    finalize_parsed_output(parsed)
}

fn send_progress_event(
    progress_sender: Option<&Sender<RunningCodexTurnEvent>>,
    event: CodexProgressEvent,
) {
    if let Some(progress_sender) = progress_sender {
        let _ = progress_sender.send(RunningCodexTurnEvent::Progress(event));
    }
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

fn apply_codex_overrides_tokio(config: &LoadedConfig, command: &mut TokioCommand) {
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
    for path in image_input_paths(attachments) {
        command.arg("-i");
        command.arg(path);
    }
}

fn apply_image_args_tokio(command: &mut TokioCommand, attachments: &[AttachmentInfo]) {
    for path in image_input_paths(attachments) {
        command.arg("-i");
        command.arg(path);
    }
}

fn extra_access_paths(config: &LoadedConfig, attachments: &[AttachmentInfo]) -> Vec<PathBuf> {
    let mut paths = vec![PathBuf::from(&config.values.paths.root_app)];

    for context_path in [
        config.values.context_files.soul.as_str(),
        config.values.context_files.memory.as_str(),
    ] {
        if let Some(parent) = Path::new(context_path).parent() {
            paths.push(parent.to_path_buf());
        }
    }

    for attachment in attachments {
        if let Some(parent) = Path::new(&attachment.path).parent() {
            paths.push(parent.to_path_buf());
        }
        for artifact in &attachment.artifacts {
            if let Some(parent) = Path::new(&artifact.path).parent() {
                paths.push(parent.to_path_buf());
            }
        }
    }

    paths.sort();
    paths.dedup();
    paths
}

pub(super) fn is_stale_resume_error(error: &KaiError) -> bool {
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

fn image_input_paths(attachments: &[AttachmentInfo]) -> Vec<&str> {
    let mut paths = Vec::new();

    for attachment in attachments {
        if attachment.kind == "image" {
            paths.push(attachment.path.as_str());
        }

        for artifact in attachment.artifacts.iter().filter(|artifact| {
            artifact.kind == "image_frame"
                || artifact
                    .mime_type
                    .as_deref()
                    .is_some_and(|mime| mime.starts_with("image/"))
        }) {
            paths.push(artifact.path.as_str());
        }
    }

    paths
}

pub(super) fn signal_process(pid: u32, signal: &str) -> KaiResult<()> {
    let status = Command::new("kill")
        .arg(format!("-{signal}"))
        .arg(pid.to_string())
        .status()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to signal Codex process {pid}: {error}"),
            )
        })?;

    if status.success() {
        return Ok(());
    }

    Err(KaiError::new(
        ErrorCode::RuntimeError,
        format!("failed to signal Codex process {pid} with {signal}"),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    #[test]
    fn apply_codex_json_value_emits_progress_events_and_tracks_final_message() {
        let (sender, receiver) = mpsc::channel();
        let mut parsed = ParsedCodexOutput::default();

        for line in [
            r#"{"type":"thread.started","thread_id":"thread-1"}"#,
            r#"{"type":"item.started","item":{"id":"item-1","type":"command_execution","command":"sed -n '1,10p' AGENTS.md"}}"#,
            r#"{"type":"item.completed","item":{"id":"item-2","type":"agent_message","text":"Inspecting AGENTS.md now."}}"#,
            r#"{"type":"item.completed","item":{"id":"item-3","type":"plan","text":"check config\ncheck runtime"}}"#,
            r#"{"type":"item.completed","item":{"id":"item-4","type":"reasoning","text":"hidden summary"}}"#,
            r#"{"type":"item.completed","item":{"id":"item-5","type":"agent_message","text":"Final reply"}}"#,
        ] {
            let value = serde_json::from_str::<JsonValue>(line).expect("json value");
            apply_codex_json_value(&mut parsed, &value, Some(&sender));
        }

        assert_eq!(parsed.session_id.as_deref(), Some("thread-1"));
        assert_eq!(parsed.response_text.as_deref(), Some("Final reply"));

        let events = receiver
            .try_iter()
            .map(|event| match event {
                RunningCodexTurnEvent::Progress(progress) => progress,
                RunningCodexTurnEvent::ResumeFailure(_) => {
                    panic!("unexpected resume failure event")
                }
                RunningCodexTurnEvent::Completed(_) => panic!("unexpected completed event"),
            })
            .collect::<Vec<_>>();
        assert_eq!(
            events,
            vec![
                CodexProgressEvent::CommandStarted {
                    command: "sed -n '1,10p' AGENTS.md".to_string(),
                },
                CodexProgressEvent::AgentMessage {
                    text: "Inspecting AGENTS.md now.".to_string(),
                },
                CodexProgressEvent::Plan {
                    text: "check config\ncheck runtime".to_string(),
                },
                CodexProgressEvent::ReasoningSummary {
                    text: "hidden summary".to_string(),
                },
                CodexProgressEvent::AgentMessage {
                    text: "Final reply".to_string(),
                },
            ]
        );
    }
}
