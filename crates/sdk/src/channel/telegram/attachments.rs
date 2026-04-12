use super::*;

pub(super) async fn download_message_attachments(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    message: &TelegramMessage,
) -> KaiResult<Vec<AttachmentInfo>> {
    let requests = collect_download_requests(message);
    if requests.len() > MAX_ATTACHMENTS_PER_TURN {
        return Err(KaiError::invalid_argument(format!(
            "too many attachments in one message: max {MAX_ATTACHMENTS_PER_TURN}"
        )));
    }

    let mut attachments = Vec::new();
    for request in requests {
        attachments.push(download_file(client, token, config, state, request).await?);
    }

    Ok(attachments)
}

fn collect_download_requests(message: &TelegramMessage) -> Vec<DownloadRequest> {
    let mut requests = Vec::new();
    let media_group_id = message.media_group_id.clone();

    if let Some(document) = &message.document {
        requests.push(DownloadRequest {
            file_id: document.file_id.clone(),
            original_name: document.file_name.clone(),
            mime_type: document.mime_type.clone(),
            kind: classify_document_kind(
                document.file_name.as_deref(),
                document.mime_type.as_deref(),
            ),
            declared_size: document.file_size,
            width: None,
            height: None,
            duration_secs: None,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(photo) = &message.photo
        && let Some(best) = photo.iter().max_by_key(|item| item.file_size.unwrap_or(0))
    {
        requests.push(DownloadRequest {
            file_id: best.file_id.clone(),
            original_name: Some(format!("photo-{}.jpg", best.file_unique_id)),
            mime_type: Some("image/jpeg".to_string()),
            kind: AttachmentKind::Image,
            declared_size: best.file_size,
            width: Some(best.width),
            height: Some(best.height),
            duration_secs: None,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(audio) = &message.audio {
        requests.push(DownloadRequest {
            file_id: audio.file_id.clone(),
            original_name: audio.file_name.clone(),
            mime_type: audio.mime_type.clone(),
            kind: AttachmentKind::Audio,
            declared_size: audio.file_size,
            width: None,
            height: None,
            duration_secs: audio.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(voice) = &message.voice {
        requests.push(DownloadRequest {
            file_id: voice.file_id.clone(),
            original_name: Some(format!("voice-{}.ogg", voice.file_unique_id)),
            mime_type: voice.mime_type.clone().or(Some("audio/ogg".to_string())),
            kind: AttachmentKind::Voice,
            declared_size: voice.file_size,
            width: None,
            height: None,
            duration_secs: voice.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(video) = &message.video {
        requests.push(DownloadRequest {
            file_id: video.file_id.clone(),
            original_name: video.file_name.clone(),
            mime_type: video.mime_type.clone(),
            kind: AttachmentKind::Video,
            declared_size: video.file_size,
            width: video.width,
            height: video.height,
            duration_secs: video.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(video_note) = &message.video_note {
        requests.push(DownloadRequest {
            file_id: video_note.file_id.clone(),
            original_name: Some(format!("video-note-{}.mp4", video_note.file_unique_id)),
            mime_type: Some("video/mp4".to_string()),
            kind: AttachmentKind::Video,
            declared_size: video_note.file_size,
            width: video_note.width,
            height: video_note.height,
            duration_secs: video_note.duration,
            media_group_id: media_group_id.clone(),
        });
    }

    if let Some(animation) = &message.animation {
        requests.push(DownloadRequest {
            file_id: animation.file_id.clone(),
            original_name: animation.file_name.clone(),
            mime_type: animation.mime_type.clone(),
            kind: AttachmentKind::Animation,
            declared_size: animation.file_size,
            width: animation.width,
            height: animation.height,
            duration_secs: animation.duration,
            media_group_id,
        });
    }

    requests
}

async fn download_file(
    client: &Client,
    token: &str,
    config: &LoadedConfig,
    state: &StateStore,
    request: DownloadRequest,
) -> KaiResult<AttachmentInfo> {
    let DownloadRequest {
        file_id,
        original_name,
        mime_type,
        kind,
        declared_size,
        width,
        height,
        duration_secs,
        media_group_id,
    } = request;

    let byte_limit = attachment_byte_limit(kind);
    if let Some(size) = declared_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{} attachment exceeds limit: {size} bytes > {byte_limit}",
            kind.as_str()
        )));
    }

    let response = client
        .get(format!("https://api.telegram.org/bot{token}/getFile"))
        .query(&[("file_id", file_id.as_str())])
        .send()
        .await
        .map_err(http_error("request Telegram file metadata"))?;

    let payload = response
        .json::<TelegramApiResponse<TelegramFile>>()
        .await
        .map_err(http_error("decode Telegram file metadata"))?;

    let file = payload.result.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            payload
                .description
                .unwrap_or_else(|| "Telegram getFile failed".to_string()),
        )
    })?;

    if let Some(size) = file.file_size
        && size > byte_limit
    {
        return Err(KaiError::invalid_argument(format!(
            "{} attachment exceeds limit: {size} bytes > {byte_limit}",
            kind.as_str()
        )));
    }

    let file_path = file.file_path.ok_or_else(|| {
        KaiError::new(
            ErrorCode::RuntimeError,
            "Telegram did not return a downloadable file path",
        )
    })?;

    let safe_name = sanitize_filename(original_name.as_deref().unwrap_or(&file_id));
    let storage_name = format!("{}-{}", Uuid::new_v4().simple(), safe_name);
    let local_path = state.paths().attachments_dir.join(storage_name);
    let partial_path = local_path.with_extension("part");

    let mut response = client
        .get(format!(
            "https://api.telegram.org/file/bot{token}/{file_path}"
        ))
        .send()
        .await
        .map_err(http_error("download Telegram file"))?;

    let mut file = File::create(&partial_path).await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to create attachment on disk: {error}"),
        )
    })?;

    let mut hasher = blake3::Hasher::new();
    let mut bytes = 0_u64;

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(http_error("read Telegram file body"))?
    {
        bytes += chunk.len() as u64;
        if bytes > byte_limit {
            let _ = fs::remove_file(&partial_path);
            return Err(KaiError::invalid_argument(format!(
                "{} attachment exceeds limit while downloading: {bytes} bytes > {byte_limit}",
                kind.as_str()
            )));
        }

        hasher.update(&chunk);
        file.write_all(&chunk).await.map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to write attachment to disk: {error}"),
            )
        })?;
    }

    file.flush().await.map_err(|error| {
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to flush attachment to disk: {error}"),
        )
    })?;

    fs::rename(&partial_path, &local_path).map_err(|error| {
        let _ = fs::remove_file(&partial_path);
        KaiError::new(
            ErrorCode::IoError,
            format!("failed to finalize attachment on disk: {error}"),
        )
    })?;

    let checksum_blake3 = hasher.finalize().to_hex().to_string();

    let mut attachment = AttachmentInfo {
        kind: kind.as_str().to_string(),
        path: local_path.display().to_string(),
        original_name,
        mime_type,
        bytes,
        checksum_blake3,
        media_group_id,
        duration_secs,
        width,
        height,
        transcript_text: None,
        transcript_segments: Vec::new(),
        artifacts: Vec::new(),
        notes: Vec::new(),
    };
    enrich_attachment(config, &mut attachment).await?;

    Ok(attachment)
}

fn sanitize_filename(input: &str) -> String {
    let mut output = input
        .chars()
        .map(|character| match character {
            'a'..='z' | 'A'..='Z' | '0'..='9' | '.' | '-' | '_' => character,
            _ => '_',
        })
        .collect::<String>();

    if output.len() > 96 {
        output.truncate(96);
    }

    if output.is_empty() {
        output = "attachment".to_string();
    }

    output
}
