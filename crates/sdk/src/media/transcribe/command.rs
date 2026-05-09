use std::path::Path;
use std::time::Duration;

use tokio::io::{AsyncRead, AsyncReadExt};
use tokio::process::Command;
use tokio::time::timeout;

use super::TranscriptionResult;
use crate::config::TranscriptionCommandConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

const STDERR_LIMIT_BYTES: usize = 65_536;

pub(super) async fn transcribe(
    command: &TranscriptionCommandConfig,
    file_path: &Path,
) -> KaiResult<TranscriptionResult> {
    let args = command_args(command, file_path)?;
    let mut child = Command::new(command.executable.trim())
        .args(args)
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to run transcription command: {error}"),
            )
        })?;

    let stdout = child.stdout.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "transcription command stdout was unavailable",
        )
    })?;
    let stderr = child.stderr.take().ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "transcription command stderr was unavailable",
        )
    })?;

    let stdout_task = tokio::spawn(read_limited(stdout, command.max_output_bytes));
    let stderr_task = tokio::spawn(read_limited(stderr, STDERR_LIMIT_BYTES));
    let status = match timeout(Duration::from_secs(command.timeout_secs), child.wait()).await {
        Ok(result) => result.map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to wait for transcription command: {error}"),
            )
        })?,
        Err(_) => {
            let _ = child.kill().await;
            return Err(KaiError::new(
                ErrorCode::RuntimeError,
                format!(
                    "transcription command timed out after {} seconds",
                    command.timeout_secs
                ),
            ));
        }
    };

    let stdout = join_reader(stdout_task, "stdout").await?;
    let stderr = join_reader(stderr_task, "stderr").await?;

    if stdout.truncated {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "transcription command output exceeded {} bytes",
                command.max_output_bytes
            ),
        ));
    }

    if !status.success() {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "transcription command failed with status {}: {}",
                status,
                stderr.text()
            ),
        ));
    }

    let text = stdout.text();
    if text.is_empty() {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            "transcription command returned empty output",
        ));
    }

    Ok(TranscriptionResult {
        provider: "command".to_string(),
        text,
        segments: Vec::new(),
    })
}

fn command_args(command: &TranscriptionCommandConfig, file_path: &Path) -> KaiResult<Vec<String>> {
    let file = file_path.display().to_string();
    let mut args = if command.args.trim().is_empty() {
        Vec::new()
    } else {
        split_argv_like(&command.args)?
    };

    let mut saw_file_placeholder = false;
    for arg in &mut args {
        if arg.contains("{file}") {
            *arg = arg.replace("{file}", &file);
            saw_file_placeholder = true;
        }
    }
    if !saw_file_placeholder {
        args.push(file);
    }

    Ok(args)
}

#[derive(Debug)]
struct LimitedOutput {
    bytes: Vec<u8>,
    truncated: bool,
}

impl LimitedOutput {
    fn text(&self) -> String {
        String::from_utf8_lossy(&self.bytes).trim().to_string()
    }
}

async fn join_reader(
    task: tokio::task::JoinHandle<KaiResult<LimitedOutput>>,
    stream_name: &str,
) -> KaiResult<LimitedOutput> {
    task.await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to join transcription {stream_name} reader: {error}"),
        )
    })?
}

async fn read_limited<R>(mut reader: R, max_bytes: usize) -> KaiResult<LimitedOutput>
where
    R: AsyncRead + Unpin + Send + 'static,
{
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 8192];
    let mut truncated = false;

    loop {
        let read = reader.read(&mut chunk).await.map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to read transcription command output: {error}"),
            )
        })?;
        if read == 0 {
            break;
        }

        let remaining = max_bytes.saturating_sub(bytes.len());
        if remaining > 0 {
            let keep = remaining.min(read);
            bytes.extend_from_slice(&chunk[..keep]);
        }
        if read > remaining {
            truncated = true;
        }
    }

    Ok(LimitedOutput { bytes, truncated })
}

fn split_argv_like(input: &str) -> KaiResult<Vec<String>> {
    let mut args = Vec::new();
    let mut current = String::new();
    let mut chars = input.chars().peekable();
    let mut quote: Option<char> = None;

    while let Some(char) = chars.next() {
        match (quote, char) {
            (Some(active), value) if value == active => quote = None,
            (Some(_), '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (Some(_), value) => current.push(value),
            (None, '\'' | '"') => quote = Some(char),
            (None, '\\') => {
                if let Some(next) = chars.next() {
                    current.push(next);
                }
            }
            (None, value) if value.is_whitespace() => {
                if !current.is_empty() {
                    args.push(std::mem::take(&mut current));
                }
            }
            (None, value) => current.push(value),
        }
    }

    if quote.is_some() {
        return Err(KaiError::invalid_argument(
            "transcription command arguments have an unterminated quote",
        ));
    }
    if !current.is_empty() {
        args.push(current);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn command(executable: &str, args: &str) -> TranscriptionCommandConfig {
        TranscriptionCommandConfig {
            executable: executable.to_string(),
            args: args.to_string(),
            timeout_secs: 2,
            max_output_bytes: 1_024,
        }
    }

    #[test]
    fn command_args_replace_file_placeholder_without_shell() {
        let path = Path::new("/tmp/media file.wav");
        let args = command_args(&command("/bin/cat", "--input {file}"), path).expect("args");

        assert_eq!(args, vec!["--input", "/tmp/media file.wav"]);
    }

    #[tokio::test]
    async fn command_transcription_reads_stdout() {
        let tempdir = tempdir().expect("tempdir");
        let media = tempdir.path().join("clip.txt");
        std::fs::write(&media, "hello transcript\n").expect("write media");

        let result = transcribe(&command("/bin/cat", "{file}"), &media)
            .await
            .expect("transcribe");

        assert_eq!(result.text, "hello transcript");
    }

    #[tokio::test]
    async fn command_transcription_times_out() {
        let tempdir = tempdir().expect("tempdir");
        let media = tempdir.path().join("clip.txt");
        std::fs::write(&media, "hello").expect("write media");
        let mut config = command("/usr/bin/tail", "-f {file}");
        config.timeout_secs = 1;

        let error = transcribe(&config, &media)
            .await
            .expect_err("timeout should fail");

        assert!(error.message.contains("timed out"));
    }

    #[tokio::test]
    async fn command_transcription_rejects_large_stdout() {
        let tempdir = tempdir().expect("tempdir");
        let media = tempdir.path().join("clip.txt");
        std::fs::write(&media, vec![b'x'; 1_048_576]).expect("write media");
        let mut config = command("/bin/cat", "{file}");
        config.max_output_bytes = 8;

        let error = transcribe(&config, &media)
            .await
            .expect_err("large output should fail");

        assert!(error.message.contains("exceeded 8 bytes"));
    }
}
