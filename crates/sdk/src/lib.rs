pub mod app;
pub mod channel;
pub mod config;
pub mod context;
pub mod contract;
pub mod error;
pub mod health;
pub mod runtime;
pub mod state;

pub use app::{handle_owner_prompt, mobile_help_text, mobile_status_text};
pub use channel::telegram::run_telegram_loop;
pub use config::{
    ContextFilesConfig, LoadedConfig, build_default_config_file, config_value_at_key,
    default_root_app, default_root_work, ensure_config_file, expand_home, load_config,
    set_config_value, unset_config_value,
};
pub use context::{ContextBlob, ContextEntry, ContextReport, context_report, load_context_blobs};
pub use contract::{
    ConfigGetOutput, ConfigShowOutput, GlobalFlag, HealthCheck, HealthReport, Meta, OkEnvelope,
    SessionView, SetupCodexOutput, SetupOutput, ToolCatalog, ToolParameter, ToolSpec,
    error_envelope, ok_envelope, tool_catalog, tool_spec,
};
pub use error::{ErrorCode, KaiError, KaiResult};
pub use health::health_report;
pub use runtime::codex::{CodexTurnResult, run_codex_turn};
pub use state::{AttachmentInfo, StatePaths, StateStore, TurnRecord};
