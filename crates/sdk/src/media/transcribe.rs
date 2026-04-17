use std::path::Path;
use std::sync::OnceLock;
use std::time::Duration;

use reqwest::{Client, multipart};
use serde::{Deserialize, Serialize};
use tokio::fs::File;
use tokio::process::Command;
use tokio_util::io::ReaderStream;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::secrets::resolve_groq_api_key;

static GROQ_CLIENT: OnceLock<Result<Client, String>> = OnceLock::new();

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptSegment {
    pub start_secs: Option<f32>,
    pub end_secs: Option<f32>,
    pub text: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TranscriptionResult {
    pub provider: String,
    pub text: String,
    #[serde(default)]
    pub segments: Vec<TranscriptSegment>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TranscriptionProviderStatus {
    Disabled,
    Ready { provider: String },
    Misconfigured { provider: String, detail: String },
}

pub fn transcription_provider_status(
    config: &LoadedConfig,
) -> KaiResult<TranscriptionProviderStatus> {
    let provider = config.values.media.transcription.provider.trim();

    if provider.eq_ignore_ascii_case("none") || provider.is_empty() {
        return Ok(TranscriptionProviderStatus::Disabled);
    }

    if provider.eq_ignore_ascii_case("groq") {
        let env_key = &config.values.media.transcription.groq_api_key_env;
        let api_key = resolve_groq_api_key(config)?;
        if api_key
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            return Ok(TranscriptionProviderStatus::Ready {
                provider: "groq".to_string(),
            });
        }

        return Ok(TranscriptionProviderStatus::Misconfigured {
            provider: "groq".to_string(),
            detail: format!(
                "Groq transcription is selected but `{env_key}` is not available in env or Keychain"
            ),
        });
    }

    if provider.eq_ignore_ascii_case("command") {
        return if config
            .values
            .media
            .transcription
            .command
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
        {
            Ok(TranscriptionProviderStatus::Ready {
                provider: "command".to_string(),
            })
        } else {
            Ok(TranscriptionProviderStatus::Misconfigured {
                provider: "command".to_string(),
                detail: "command transcription is selected but no command template is configured"
                    .to_string(),
            })
        };
    }

    Ok(TranscriptionProviderStatus::Misconfigured {
        provider: provider.to_string(),
        detail: format!("unsupported transcription provider `{provider}`"),
    })
}

pub async fn transcribe_file(
    config: &LoadedConfig,
    file_path: &Path,
    mime_type: Option<&str>,
) -> KaiResult<Option<TranscriptionResult>> {
    match transcription_provider_status(config)? {
        TranscriptionProviderStatus::Disabled => Ok(None),
        TranscriptionProviderStatus::Ready { provider } if provider == "groq" => {
            let api_key = resolve_groq_api_key(config)?.ok_or_else(|| {
                KaiError::blocked_prerequisite("Groq transcription secret is unavailable")
            })?;
            let model = &config.values.media.transcription.groq_model;
            let result = transcribe_with_groq(&api_key, model, file_path, mime_type).await?;
            Ok(Some(result))
        }
        TranscriptionProviderStatus::Ready { provider } if provider == "command" => {
            let template = config
                .values
                .media
                .transcription
                .command
                .as_deref()
                .ok_or_else(|| {
                    KaiError::blocked_prerequisite(
                        "command transcription is selected but no command is configured",
                    )
                })?;
            let result = transcribe_with_command(template, file_path).await?;
            Ok(Some(result))
        }
        TranscriptionProviderStatus::Ready { provider } => Err(KaiError::new(
            ErrorCode::ConfigError,
            format!("unsupported transcription provider `{provider}`"),
        )),
        TranscriptionProviderStatus::Misconfigured { detail, .. } => {
            Err(KaiError::blocked_prerequisite(detail))
        }
    }
}

async fn transcribe_with_groq(
    api_key: &str,
    model: &str,
    file_path: &Path,
    mime_type: Option<&str>,
) -> KaiResult<TranscriptionResult> {
    let file = File::open(file_path).await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to read media file for Groq transcription: {error}"),
        )
    })?;
    let file_bytes = file.metadata().await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to inspect media file for Groq transcription: {error}"),
        )
    })?;
    let file_name = file_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("media.bin");

    let stream = reqwest::Body::wrap_stream(ReaderStream::new(file));
    let mut part = multipart::Part::stream_with_length(stream, file_bytes.len())
        .file_name(file_name.to_string());
    if let Some(mime_type) = mime_type
        && !mime_type.trim().is_empty()
    {
        part = part.mime_str(mime_type).map_err(|error| {
            KaiError::new(
                ErrorCode::InvalidArgument,
                format!("unsupported media MIME type `{mime_type}`: {error}"),
            )
        })?;
    }

    let form = multipart::Form::new()
        .text("model", model.to_string())
        .text("response_format", "verbose_json".to_string())
        .part("file", part);

    let response = groq_client()?
        .post("https://api.groq.com/openai/v1/audio/transcriptions")
        .timeout(Duration::from_secs(180))
        .bearer_auth(api_key)
        .multipart(form)
        .send()
        .await
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to call Groq speech-to-text API: {error}"),
            )
        })?;

    let status = response.status();
    let body = response.text().await.map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to read Groq speech-to-text response: {error}"),
        )
    })?;

    if !status.is_success() {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!("Groq speech-to-text request failed with status {status}: {body}"),
        ));
    }

    let payload = serde_json::from_str::<GroqTranscriptionResponse>(&body).map_err(|error| {
        KaiError::new(
            ErrorCode::RuntimeError,
            format!("failed to parse Groq speech-to-text response: {error}"),
        )
    })?;

    let text = payload.text.trim().to_string();
    if text.is_empty() {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            "Groq speech-to-text returned an empty transcript",
        ));
    }

    Ok(TranscriptionResult {
        provider: "groq".to_string(),
        text,
        segments: payload
            .segments
            .unwrap_or_default()
            .into_iter()
            .map(|segment| TranscriptSegment {
                start_secs: segment.start,
                end_secs: segment.end,
                text: segment.text,
            })
            .collect(),
    })
}

async fn transcribe_with_command(
    template: &str,
    file_path: &Path,
) -> KaiResult<TranscriptionResult> {
    let file = shell_quote(&file_path.display().to_string());
    let command = if template.contains("{file}") {
        template.replace("{file}", &file)
    } else {
        format!("{template} {file}")
    };

    let output = Command::new("/bin/zsh")
        .arg("-lc")
        .arg(&command)
        .output()
        .await
        .map_err(|error| {
            KaiError::new(
                ErrorCode::RuntimeError,
                format!("failed to run transcription command: {error}"),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            format!(
                "transcription command failed with status {}: {stderr}",
                output.status
            ),
        ));
    }

    let text = String::from_utf8_lossy(&output.stdout).trim().to_string();
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

fn groq_client() -> KaiResult<&'static Client> {
    match GROQ_CLIENT.get_or_init(|| {
        Client::builder()
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|error| format!("failed to build Groq speech-to-text client: {error}"))
    }) {
        Ok(client) => Ok(client),
        Err(message) => Err(KaiError::new(ErrorCode::RuntimeError, message.clone())),
    }
}

fn shell_quote(input: &str) -> String {
    format!("'{}'", input.replace('\'', "'\\''"))
}

#[derive(Debug, Deserialize)]
struct GroqTranscriptionResponse {
    text: String,
    #[serde(default)]
    segments: Option<Vec<GroqTranscriptionSegment>>,
}

#[derive(Debug, Deserialize)]
struct GroqTranscriptionSegment {
    start: Option<f32>,
    end: Option<f32>,
    text: String,
}
