use std::time::Duration;

const MEBIBYTE: u64 = 1024 * 1024;

pub const TELEGRAM_CLOUD_MAX_ATTACHMENT_BYTES: u64 = 20 * MEBIBYTE;
pub const MAX_ATTACHMENTS_PER_TURN: usize = 10;
pub const MAX_MEDIA_GROUP_ITEMS: usize = 10;
pub const MEDIA_GROUP_DEBOUNCE: Duration = Duration::from_millis(900);
pub const ATTACHMENT_RETENTION: Duration = Duration::from_secs(60 * 60 * 24 * 30);
pub const ATTACHMENT_CLEANUP_INTERVAL: Duration = Duration::from_secs(60 * 60 * 6);
pub const MAX_INLINE_TRANSCRIPT_CHARS: usize = 4_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum AttachmentKind {
    Animation,
    Audio,
    Document,
    Image,
    Pdf,
    Text,
    Video,
    Voice,
}

impl AttachmentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Animation => "animation",
            Self::Audio => "audio",
            Self::Document => "document",
            Self::Image => "image",
            Self::Pdf => "pdf",
            Self::Text => "text",
            Self::Video => "video",
            Self::Voice => "voice",
        }
    }

    pub fn parse(input: &str) -> Option<Self> {
        match input {
            "animation" => Some(Self::Animation),
            "audio" => Some(Self::Audio),
            "document" => Some(Self::Document),
            "image" => Some(Self::Image),
            "pdf" => Some(Self::Pdf),
            "text" => Some(Self::Text),
            "video" => Some(Self::Video),
            "voice" => Some(Self::Voice),
            _ => None,
        }
    }

    pub fn supports_native_codex_image_input(self) -> bool {
        matches!(self, Self::Image)
    }

    pub fn supports_transcription(self) -> bool {
        matches!(self, Self::Audio | Self::Video | Self::Voice)
    }

    pub fn supports_preview_frame(self) -> bool {
        matches!(self, Self::Animation | Self::Video)
    }
}

pub fn attachment_byte_limit(kind: AttachmentKind) -> u64 {
    match kind {
        AttachmentKind::Animation
        | AttachmentKind::Audio
        | AttachmentKind::Document
        | AttachmentKind::Image
        | AttachmentKind::Pdf
        | AttachmentKind::Text
        | AttachmentKind::Video
        | AttachmentKind::Voice => TELEGRAM_CLOUD_MAX_ATTACHMENT_BYTES,
    }
}

pub fn classify_document_kind(file_name: Option<&str>, mime_type: Option<&str>) -> AttachmentKind {
    let normalized_name = file_name.unwrap_or_default().to_ascii_lowercase();
    let normalized_mime = mime_type.unwrap_or_default().to_ascii_lowercase();

    if normalized_mime == "application/pdf" || normalized_name.ends_with(".pdf") {
        return AttachmentKind::Pdf;
    }

    if normalized_mime == "image/gif" || normalized_name.ends_with(".gif") {
        return AttachmentKind::Animation;
    }

    if normalized_mime.starts_with("image/")
        || has_any_extension(
            &normalized_name,
            &[".jpg", ".jpeg", ".png", ".webp", ".heic", ".heif", ".bmp"],
        )
    {
        return AttachmentKind::Image;
    }

    if normalized_mime.starts_with("text/")
        || has_any_extension(
            &normalized_name,
            &[
                ".md",
                ".txt",
                ".markdown",
                ".csv",
                ".json",
                ".jsonl",
                ".yaml",
                ".yml",
                ".toml",
                ".log",
                ".xml",
            ],
        )
    {
        return AttachmentKind::Text;
    }

    if normalized_mime.starts_with("audio/")
        || has_any_extension(
            &normalized_name,
            &[
                ".mp3", ".m4a", ".wav", ".ogg", ".flac", ".webm", ".mpga", ".mpeg",
            ],
        )
    {
        return AttachmentKind::Audio;
    }

    if normalized_mime.starts_with("video/")
        || has_any_extension(
            &normalized_name,
            &[".mp4", ".mov", ".m4v", ".webm", ".mkv", ".avi"],
        )
    {
        return AttachmentKind::Video;
    }

    AttachmentKind::Document
}

fn has_any_extension(input: &str, suffixes: &[&str]) -> bool {
    suffixes.iter().any(|suffix| input.ends_with(suffix))
}

#[cfg(test)]
mod tests {
    use super::{AttachmentKind, classify_document_kind};

    #[test]
    fn classify_document_kind_recognizes_image_documents() {
        assert_eq!(
            classify_document_kind(Some("screenshot.png"), Some("image/png")),
            AttachmentKind::Image
        );
    }

    #[test]
    fn classify_document_kind_recognizes_audio_and_video_documents() {
        assert_eq!(
            classify_document_kind(Some("note.m4a"), Some("audio/mp4")),
            AttachmentKind::Audio
        );
        assert_eq!(
            classify_document_kind(Some("clip.mp4"), Some("video/mp4")),
            AttachmentKind::Video
        );
    }

    #[test]
    fn classify_document_kind_falls_back_to_generic_document() {
        assert_eq!(
            classify_document_kind(Some("spreadsheet.xlsx"), Some("application/vnd.ms-excel")),
            AttachmentKind::Document
        );
    }
}
