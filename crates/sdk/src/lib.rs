pub mod app;
pub mod channel;
pub mod config;
pub mod context;
pub mod contract;
pub mod error;
pub mod health;
pub mod media;
pub mod redaction;
pub mod runtime;
pub mod runtime_fs;
pub mod secrets;
pub mod service;
pub mod state;

pub use app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
pub use channel::telegram::run_telegram_loop;
pub use config::{
    ContextFilesConfig, LoadedConfig, MediaConfig, RunnerProvider, TranscriptionConfig,
    build_default_config_file, config_value_at_key, default_root_app, default_root_work,
    ensure_config_file, expand_home, load_config, set_config_value, unset_config_value,
};
pub use context::{
    ContextEntry, ContextReport, ContextSnapshot, context_report, context_snapshots,
};
pub use contract::{
    ConfigGetOutput, ConfigShowOutput, GlobalFlag, HealthCheck, HealthReport, Meta, OkEnvelope,
    PendingTurnView, SessionView, SetupCodexOutput, SetupOutput, ToolCatalog, ToolParameter,
    ToolSpec, error_envelope, ok_envelope, tool_catalog, tool_spec,
};
pub use error::{ErrorCode, KaiError, KaiResult};
pub use health::health_report;
pub use media::{
    ATTACHMENT_CLEANUP_INTERVAL, ATTACHMENT_RETENTION, AttachmentKind, MAX_ATTACHMENTS_PER_TURN,
    MAX_MEDIA_GROUP_ITEMS, MEDIA_GROUP_DEBOUNCE, TELEGRAM_CLOUD_MAX_ATTACHMENT_BYTES,
    TranscriptSegment, TranscriptionProviderStatus, attachment_byte_limit, classify_document_kind,
    enrich_attachment, transcription_provider_status,
};
pub use redaction::{redact_json_value, redact_optional_text, redact_text};
pub use runtime::agent::{AgentTurnResult, run_agent_turn};
pub use runtime::codex::{CodexTurnResult, run_codex_turn};
pub use runtime_fs::{
    ensure_private_dir, ensure_private_file, harden_private_executable, harden_private_file,
    octal_mode, read_unix_mode, write_private_executable, write_private_file,
};
pub use secrets::{
    GroqApiKeyStatus, TelegramTokenStatus, groq_api_key_status, resolve_groq_api_key,
    resolve_telegram_token, telegram_token_status,
};
#[cfg(target_os = "macos")]
pub use secrets::{
    groq_api_key_keychain_service_name, sync_groq_api_key_to_keychain,
    sync_telegram_token_to_keychain, telegram_token_keychain_service_name,
};
pub use service::{
    RunGuard, RunLockStatus, ServiceActionOutput, ServiceLogsOutput, ServiceStatus,
    acquire_run_guard, run_lock_status, service_logs, service_restart, service_start,
    service_status, service_stop, service_uninstall,
};
pub use state::{AttachmentInfo, MAX_PENDING_TURNS, StatePaths, StateStore, TurnRecord};
