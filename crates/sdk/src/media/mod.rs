pub mod policy;
pub mod transcribe;

use std::env;
use std::path::{Path, PathBuf};

use tokio::process::Command;
use uuid::Uuid;

use crate::config::LoadedConfig;
use crate::error::KaiResult;
use crate::runtime_fs::ensure_private_file;
use crate::state::{AttachmentArtifact, AttachmentInfo};

pub use policy::{
    ATTACHMENT_CLEANUP_INTERVAL, ATTACHMENT_RETENTION, AttachmentKind, MAX_ATTACHMENTS_PER_TURN,
    MAX_INLINE_TRANSCRIPT_CHARS, MAX_MEDIA_GROUP_ITEMS, MEDIA_GROUP_DEBOUNCE,
    TELEGRAM_CLOUD_MAX_ATTACHMENT_BYTES, attachment_byte_limit, classify_document_kind,
};
pub use transcribe::{
    TranscriptSegment, TranscriptionProviderStatus, transcription_provider_status,
};

pub async fn enrich_attachment(
    config: &LoadedConfig,
    attachment: &mut AttachmentInfo,
) -> KaiResult<()> {
    let Some(kind) = AttachmentKind::parse(&attachment.kind) else {
        return Ok(());
    };

    if kind.supports_preview_frame()
        && let Err(error) = maybe_extract_preview_frame(attachment).await
    {
        attachment.notes.push(format!(
            "preview frame extraction skipped: {}",
            error.message
        ));
    }

    if kind.supports_transcription()
        && let Err(error) = maybe_attach_transcript(config, attachment).await
    {
        attachment
            .notes
            .push(format!("transcription unavailable: {}", error.message));
    }

    Ok(())
}

fn ffmpeg_available() -> bool {
    let path = match env::var_os("PATH") {
        Some(value) => value,
        None => return false,
    };

    env::split_paths(&path)
        .map(|entry| entry.join("ffmpeg"))
        .any(|candidate| candidate.is_file())
}

async fn maybe_extract_preview_frame(attachment: &mut AttachmentInfo) -> KaiResult<()> {
    if !ffmpeg_available() {
        attachment
            .notes
            .push("ffmpeg is not available; skipped preview frame extraction".to_string());
        return Ok(());
    }

    let output_path = derived_artifact_path(&attachment.path, "frame", "jpg");
    let output = Command::new("ffmpeg")
        .args([
            "-v",
            "error",
            "-y",
            "-i",
            attachment.path.as_str(),
            "-frames:v",
            "1",
            output_path.to_string_lossy().as_ref(),
        ])
        .output()
        .await
        .map_err(|error| {
            crate::error::KaiError::new(
                crate::error::ErrorCode::RuntimeError,
                format!("failed to launch ffmpeg: {error}"),
            )
        })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        attachment.notes.push(format!(
            "ffmpeg preview extraction failed: {}",
            if stderr.is_empty() {
                "unknown error".to_string()
            } else {
                stderr
            }
        ));
        return Ok(());
    }

    let artifact = build_artifact("image_frame", &output_path, Some("image/jpeg"))?;
    attachment.artifacts.push(artifact);
    Ok(())
}

async fn maybe_attach_transcript(
    config: &LoadedConfig,
    attachment: &mut AttachmentInfo,
) -> KaiResult<()> {
    let result = match transcribe::transcribe_file(
        config,
        Path::new(&attachment.path),
        attachment.mime_type.as_deref(),
    )
    .await?
    {
        Some(result) => result,
        None => return Ok(()),
    };

    let transcript_text = result.text.trim();
    if transcript_text.is_empty() {
        return Ok(());
    }

    let transcript_path = derived_artifact_path(&attachment.path, "transcript", "txt");
    let mut rendered = transcript_text.to_string();
    if !result.segments.is_empty() {
        let lines = result
            .segments
            .iter()
            .map(|segment| match (segment.start_secs, segment.end_secs) {
                (Some(start), Some(end)) => {
                    format!("[{start:.2}s-{end:.2}s] {}", segment.text.trim())
                }
                _ => segment.text.trim().to_string(),
            })
            .collect::<Vec<_>>();
        rendered = lines.join("\n");
    }

    std::fs::write(&transcript_path, rendered.as_bytes()).map_err(|error| {
        crate::error::KaiError::new(
            crate::error::ErrorCode::IoError,
            format!("failed to write transcript artifact: {error}"),
        )
    })?;
    ensure_private_file(&transcript_path)?;

    attachment.transcript_text = Some(truncate_inline_transcript(transcript_text));
    attachment.artifacts.push(build_artifact(
        "transcript",
        &transcript_path,
        Some("text/plain"),
    )?);
    Ok(())
}

fn derived_artifact_path(source_path: &str, label: &str, extension: &str) -> PathBuf {
    let source = Path::new(source_path);
    let parent = source
        .parent()
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| PathBuf::from("."));
    let stem = source
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let safe_stem = sanitize_component(stem);
    parent.join(format!(
        "{}-{}-{}.{}",
        safe_stem,
        label,
        Uuid::new_v4().simple(),
        extension
    ))
}

fn build_artifact(
    kind: &str,
    path: &Path,
    mime_type: Option<&str>,
) -> KaiResult<AttachmentArtifact> {
    let bytes = std::fs::read(path).map_err(|error| {
        crate::error::KaiError::new(
            crate::error::ErrorCode::IoError,
            format!("failed to read derived media artifact: {error}"),
        )
    })?;
    let checksum = blake3::hash(&bytes).to_hex().to_string();

    Ok(AttachmentArtifact {
        kind: kind.to_string(),
        path: path.display().to_string(),
        mime_type: mime_type.map(ToOwned::to_owned),
        bytes: bytes.len() as u64,
        checksum_blake3: Some(checksum),
    })
}

fn sanitize_component(input: &str) -> String {
    let mut output = input
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();

    if output.is_empty() {
        output = "attachment".to_string();
    }

    if output.len() > 48 {
        output.truncate(48);
    }

    output
}

fn truncate_inline_transcript(input: &str) -> String {
    let trimmed = input.trim();
    if trimmed.chars().count() <= MAX_INLINE_TRANSCRIPT_CHARS {
        return trimmed.to_string();
    }

    let excerpt = trimmed
        .chars()
        .take(MAX_INLINE_TRANSCRIPT_CHARS)
        .collect::<String>();
    format!("{excerpt}...")
}

#[cfg(test)]
mod tests {
    use super::truncate_inline_transcript;
    use crate::media::policy::MAX_INLINE_TRANSCRIPT_CHARS;

    #[test]
    fn truncate_inline_transcript_respects_limit() {
        let input = "a".repeat(MAX_INLINE_TRANSCRIPT_CHARS + 10);
        let output = truncate_inline_transcript(&input);
        assert!(output.len() > MAX_INLINE_TRANSCRIPT_CHARS);
        assert!(output.ends_with("..."));
    }
}
