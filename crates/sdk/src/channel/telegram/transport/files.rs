use super::*;

pub(super) async fn send_local_paths(
    client: &Client,
    token: &str,
    chat_id: i64,
    paths: &[PathBuf],
) -> KaiResult<usize> {
    let mut sent = 0_usize;

    for path in paths.iter().take(MAX_OUTBOUND_ATTACHMENTS_PER_REPLY) {
        if send_local_path(client, token, chat_id, path).await? {
            sent += 1;
        }
    }

    if sent == 0 {
        return Err(KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram rejected the requested outbound file(s)",
        ));
    }

    Ok(sent)
}

async fn send_local_path(
    client: &Client,
    token: &str,
    chat_id: i64,
    path: &Path,
) -> KaiResult<bool> {
    let metadata = fs::metadata(path).map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to inspect local file for Telegram delivery: {error}"),
        )
    })?;
    let name = path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("attachment");
    let kind = classify_document_kind(Some(name), None);
    let byte_limit = attachment_byte_limit(kind);
    if metadata.len() > byte_limit {
        return Err(KaiError::invalid_argument(format!(
            "outbound file exceeds Telegram limit: {} bytes > {}",
            metadata.len(),
            byte_limit
        )));
    }

    let mime_type = guess_mime_type(path, kind);
    let sent = match kind {
        AttachmentKind::Animation => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendAnimation",
                    "animation",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Image => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendPhoto",
                    "photo",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Video => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendVideo",
                    "video",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Voice => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendVoice",
                    "voice",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Audio => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendAudio",
                    "audio",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
                || send_uploaded_file(
                    client,
                    token,
                    chat_id,
                    OutboundUpload::new(
                        "sendDocument",
                        "document",
                        name,
                        path,
                        metadata.len(),
                        mime_type.as_deref(),
                    ),
                )
                .await?
        }
        AttachmentKind::Document | AttachmentKind::Pdf | AttachmentKind::Text => {
            send_uploaded_file(
                client,
                token,
                chat_id,
                OutboundUpload::new(
                    "sendDocument",
                    "document",
                    name,
                    path,
                    metadata.len(),
                    mime_type.as_deref(),
                ),
            )
            .await?
        }
    };

    Ok(sent)
}

async fn send_uploaded_file(
    client: &Client,
    token: &str,
    chat_id: i64,
    upload: OutboundUpload<'_>,
) -> KaiResult<bool> {
    let part = build_uploaded_part(&upload).await?;

    let form = multipart::Form::new()
        .text("chat_id", chat_id.to_string())
        .part(upload.field_name.to_string(), part);

    let response = client
        .post(format!(
            "https://api.telegram.org/bot{token}/{}",
            upload.method
        ))
        .multipart(form)
        .send()
        .await
        .map_err(http_error("send Telegram file"))?;

    let payload = response
        .json::<TelegramApiResponse<serde_json::Value>>()
        .await
        .map_err(http_error("decode Telegram file response"))?;

    if payload.ok {
        return Ok(true);
    }

    Ok(false)
}

async fn build_uploaded_part(upload: &OutboundUpload<'_>) -> KaiResult<multipart::Part> {
    let open_body = || async {
        File::open(upload.path).await.map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to open local file for Telegram delivery: {error}"),
            )
        })
    };

    let part = multipart::Part::stream_with_length(
        reqwest::Body::wrap_stream(ReaderStream::new(open_body().await?)),
        upload.bytes_len,
    )
    .file_name(upload.file_name.to_string());
    match upload.mime_type {
        Some(mime_type) => Ok(part.mime_str(mime_type).unwrap_or(
            multipart::Part::stream_with_length(
                reqwest::Body::wrap_stream(ReaderStream::new(open_body().await?)),
                upload.bytes_len,
            )
            .file_name(upload.file_name.to_string()),
        )),
        None => Ok(part),
    }
}

pub(super) fn resolve_requested_path(config: &LoadedConfig, raw: &str) -> KaiResult<PathBuf> {
    let normalized = crate::config::expand_home(raw);
    let candidate = if Path::new(&normalized).is_absolute() {
        PathBuf::from(&normalized)
    } else {
        Path::new(&config.values.paths.root_work).join(normalized)
    };

    let canonical = candidate.canonicalize().map_err(|error| {
        KaiError::new(
            ErrorCode::InvalidArgument,
            format!("failed to resolve requested path: {error}"),
        )
    })?;
    let metadata = fs::metadata(&canonical).map_err(|error| {
        KaiError::new(
            ErrorCode::InvalidArgument,
            format!("failed to inspect requested path: {error}"),
        )
    })?;
    if !metadata.is_file() {
        return Err(KaiError::invalid_argument(
            "requested path must resolve to a regular file",
        ));
    }

    let root_work = Path::new(&config.values.paths.root_work)
        .canonicalize()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to resolve root_work: {error}"),
            )
        })?;
    let root_app = Path::new(&config.values.paths.root_app)
        .canonicalize()
        .map_err(|error| {
            KaiError::new(
                ErrorCode::ConfigError,
                format!("failed to resolve root_app: {error}"),
            )
        })?;

    if !canonical.starts_with(&root_work) && !canonical.starts_with(&root_app) {
        return Err(KaiError::blocked_prerequisite(
            "requested path is outside the approved kai roots",
        ));
    }

    Ok(canonical)
}

fn guess_mime_type(path: &Path, kind: AttachmentKind) -> Option<String> {
    let lower = path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase();

    match kind {
        AttachmentKind::Animation => Some("image/gif".to_string()),
        AttachmentKind::Audio => Some(
            match lower.as_str() {
                "mp3" => "audio/mpeg",
                "m4a" => "audio/mp4",
                "wav" => "audio/wav",
                "flac" => "audio/flac",
                "ogg" => "audio/ogg",
                "webm" => "audio/webm",
                _ => "audio/mpeg",
            }
            .to_string(),
        ),
        AttachmentKind::Document => Some("application/octet-stream".to_string()),
        AttachmentKind::Image => Some(
            match lower.as_str() {
                "png" => "image/png",
                "webp" => "image/webp",
                "heic" => "image/heic",
                "heif" => "image/heif",
                "bmp" => "image/bmp",
                _ => "image/jpeg",
            }
            .to_string(),
        ),
        AttachmentKind::Pdf => Some("application/pdf".to_string()),
        AttachmentKind::Text => Some("text/plain".to_string()),
        AttachmentKind::Video => Some("video/mp4".to_string()),
        AttachmentKind::Voice => Some("audio/ogg".to_string()),
    }
}
