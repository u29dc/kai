use super::*;

#[derive(Debug)]
pub(super) struct ValidatedInbound {
    pub(super) chat_id: i64,
    pub(super) sender_id: i64,
    pub(super) text: String,
}

#[derive(Debug)]
pub(super) struct BufferedMediaGroup {
    pub(super) media_group_id: String,
    pub(super) chat_id: i64,
    pub(super) last_update_id: i64,
    pub(super) ready_at: Instant,
    pub(super) update_ids: Vec<i64>,
    pub(super) messages: Vec<TelegramMessage>,
}

#[derive(Debug)]
pub(super) struct BufferedTextFragments {
    pub(super) chat_id: i64,
    pub(super) sender_id: i64,
    pub(super) last_update_id: i64,
    pub(super) ready_at: Instant,
    pub(super) update_ids: Vec<i64>,
    pub(super) messages: Vec<TelegramMessage>,
}

#[derive(Debug)]
pub(super) struct ActiveOwnerTurn {
    pub(super) pending: PendingTurn,
    pub(super) running: RunningCodexTurn,
    pub(super) cancel_requested: bool,
    pub(super) next_typing_at: Instant,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PersistedBufferedMediaGroup {
    pub(super) media_group_id: String,
    pub(super) chat_id: i64,
    pub(super) last_update_id: i64,
    pub(super) update_ids: Vec<i64>,
    pub(super) messages: Vec<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PersistedBufferedTextFragments {
    pub(super) chat_id: i64,
    pub(super) sender_id: i64,
    pub(super) last_update_id: i64,
    pub(super) update_ids: Vec<i64>,
    pub(super) messages: Vec<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct PendingReplyDelivery {
    pub(super) delivery_id: String,
    pub(super) turn_id: String,
    pub(super) chat_id: i64,
    pub(super) response_text: String,
    pub(super) codex_session_id: String,
    pub(super) update_ids: Vec<i64>,
    pub(super) attempts: u32,
    pub(super) created_at: String,
}

#[derive(Debug)]
pub(super) enum MobileCommand {
    Help,
    Status,
    Reset,
    Cancel,
    Send { path: String },
}

pub(super) struct DownloadRequest {
    pub(super) file_id: String,
    pub(super) original_name: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) kind: AttachmentKind,
    pub(super) declared_size: Option<u64>,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
    pub(super) duration_secs: Option<u32>,
    pub(super) media_group_id: Option<String>,
}

pub(super) struct OutboundUpload<'a> {
    pub(super) method: &'a str,
    pub(super) field_name: &'a str,
    pub(super) file_name: &'a str,
    pub(super) path: &'a Path,
    pub(super) bytes_len: u64,
    pub(super) mime_type: Option<&'a str>,
}

impl<'a> OutboundUpload<'a> {
    pub(super) fn new(
        method: &'a str,
        field_name: &'a str,
        file_name: &'a str,
        path: &'a Path,
        bytes_len: u64,
        mime_type: Option<&'a str>,
    ) -> Self {
        Self {
            method,
            field_name,
            file_name,
            path,
            bytes_len,
            mime_type,
        }
    }
}

pub(super) enum UpdateFailureDisposition {
    Advance,
    Retry,
}

#[derive(Debug, Deserialize)]
pub(super) struct TelegramApiResponse<T> {
    pub(super) ok: bool,
    pub(super) result: Option<T>,
    pub(super) description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramUpdate {
    pub(super) update_id: i64,
    pub(super) message: Option<TelegramMessage>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramMessage {
    pub(super) from: Option<TelegramUser>,
    pub(super) chat: TelegramChat,
    pub(super) text: Option<String>,
    pub(super) caption: Option<String>,
    pub(super) document: Option<TelegramDocument>,
    pub(super) photo: Option<Vec<TelegramPhotoSize>>,
    pub(super) audio: Option<TelegramAudio>,
    pub(super) voice: Option<TelegramVoice>,
    pub(super) video: Option<TelegramVideo>,
    pub(super) video_note: Option<TelegramVideoNote>,
    pub(super) animation: Option<TelegramAnimation>,
    pub(super) media_group_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramUser {
    pub(super) id: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramChat {
    pub(super) id: i64,
    #[serde(rename = "type")]
    pub(super) kind: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramDocument {
    pub(super) file_id: String,
    pub(super) file_name: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramPhotoSize {
    pub(super) file_id: String,
    pub(super) file_unique_id: String,
    pub(super) width: u32,
    pub(super) height: u32,
    pub(super) file_size: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramAudio {
    pub(super) file_id: String,
    pub(super) file_name: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) file_size: Option<u64>,
    pub(super) duration: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramVoice {
    pub(super) file_id: String,
    pub(super) file_unique_id: String,
    pub(super) mime_type: Option<String>,
    pub(super) file_size: Option<u64>,
    pub(super) duration: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramVideo {
    pub(super) file_id: String,
    pub(super) file_name: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) file_size: Option<u64>,
    pub(super) duration: Option<u32>,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramVideoNote {
    pub(super) file_id: String,
    pub(super) file_unique_id: String,
    pub(super) file_size: Option<u64>,
    pub(super) duration: Option<u32>,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct TelegramAnimation {
    pub(super) file_id: String,
    pub(super) file_name: Option<String>,
    pub(super) mime_type: Option<String>,
    pub(super) file_size: Option<u64>,
    pub(super) duration: Option<u32>,
    pub(super) width: Option<u32>,
    pub(super) height: Option<u32>,
}

#[derive(Debug, Deserialize)]
pub(super) struct TelegramFile {
    pub(super) file_path: Option<String>,
    pub(super) file_size: Option<u64>,
}

#[derive(Debug, Serialize)]
pub(super) struct SendMessageRequest {
    pub(super) chat_id: i64,
    pub(super) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(super) parse_mode: Option<String>,
}

#[derive(Debug, Serialize)]
pub(super) struct SendChatActionRequest {
    pub(super) chat_id: i64,
    pub(super) action: String,
}

#[derive(Debug, Serialize)]
pub(super) struct TelegramMenuCommand {
    pub(super) command: String,
    pub(super) description: String,
}

impl TelegramMenuCommand {
    pub(super) fn new(command: &str, description: &str) -> Self {
        Self {
            command: command.to_string(),
            description: description.to_string(),
        }
    }
}

pub(super) enum ListKind {
    Unordered,
    Ordered(u64),
}

impl ListKind {
    pub(super) fn new(start: Option<u64>) -> Self {
        match start {
            Some(value) => Self::Ordered(value),
            None => Self::Unordered,
        }
    }
}
