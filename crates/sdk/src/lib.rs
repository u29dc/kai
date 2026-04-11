pub mod app;
pub mod channel;
pub mod config;
pub mod context;
pub mod contract;
pub mod error;
pub mod health;
pub mod runtime;
pub mod runtime_fs;
pub mod secrets;
pub mod service;
pub mod state;

pub use app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
pub use channel::telegram::run_telegram_loop;
pub use config::{
    ContextFilesConfig, LoadedConfig, build_default_config_file, config_value_at_key,
    default_root_app, default_root_work, ensure_config_file, expand_home, load_config,
    set_config_value, unset_config_value,
};
pub use context::{
    ContextEntry, ContextReport, ContextSnapshot, context_report, context_snapshots,
};
pub use contract::{
    ConfigGetOutput, ConfigShowOutput, GlobalFlag, HealthCheck, HealthReport, Meta, OkEnvelope,
    SessionView, SetupCodexOutput, SetupOutput, ToolCatalog, ToolParameter, ToolSpec,
    error_envelope, ok_envelope, tool_catalog, tool_spec,
};
pub use error::{ErrorCode, KaiError, KaiResult};
pub use health::health_report;
pub use runtime::codex::{CodexTurnResult, run_codex_turn};
pub use runtime_fs::{
    ensure_private_dir, ensure_private_file, harden_private_executable, harden_private_file,
    octal_mode, read_unix_mode, write_private_executable, write_private_file,
};
pub use secrets::{TelegramTokenStatus, resolve_telegram_token, telegram_token_status};
#[cfg(target_os = "macos")]
pub use secrets::{sync_telegram_token_to_keychain, telegram_token_keychain_service_name};
pub use service::{
    RunGuard, RunLockStatus, ServiceActionOutput, ServiceLogsOutput, ServiceStatus,
    acquire_run_guard, run_lock_status, service_logs, service_restart, service_start,
    service_status, service_stop, service_uninstall,
};
pub use state::{AttachmentInfo, StatePaths, StateStore, TurnRecord};
