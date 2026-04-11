use serde::Serialize;

use crate::contract::{ErrorEnvelope, ErrorInfo, Meta};

pub type KaiResult<T> = Result<T, KaiError>;

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    BlockedPrerequisite,
    ConfigError,
    InvalidArgument,
    IoError,
    RuntimeError,
    StateError,
    ToolNotFound,
}

#[derive(Debug, Clone)]
pub struct KaiError {
    pub code: ErrorCode,
    pub message: String,
    pub hint: Option<String>,
    pub blocked: bool,
}

impl KaiError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            hint: None,
            blocked: false,
        }
    }

    pub fn with_hint(mut self, hint: impl Into<String>) -> Self {
        self.hint = Some(hint.into());
        self
    }

    pub fn blocked(mut self) -> Self {
        self.blocked = true;
        self
    }

    pub fn invalid_argument(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::InvalidArgument, message)
    }

    pub fn blocked_prerequisite(message: impl Into<String>) -> Self {
        Self::new(ErrorCode::BlockedPrerequisite, message).blocked()
    }

    pub fn tool_not_found(name: &str) -> Self {
        Self::new(ErrorCode::ToolNotFound, format!("unknown tool: {name}"))
    }

    pub fn into_envelope(self, tool: impl Into<String>) -> ErrorEnvelope {
        ErrorEnvelope {
            ok: false,
            error: ErrorInfo {
                code: self.code,
                message: self.message,
                hint: self.hint,
            },
            meta: Meta::new(tool),
        }
    }
}
