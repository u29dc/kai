use tempfile::tempdir;

use super::*;
use crate::config::{
    AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, LoadedConfig, MediaConfig,
    PathsConfig, RunnerConfig, RunnerProvider, TelegramConfig, TelegramProgressConfig,
    TranscriptionConfig,
};

fn test_config(root_app: &Path, root_work: &Path) -> LoadedConfig {
    LoadedConfig {
        config_path: root_app.join("config.toml"),
        config_exists: true,
        values: Config {
            agent: AgentConfig {
                timezone: "Europe/London".to_string(),
            },
            channel: ChannelConfig {
                telegram: TelegramConfig {
                    enabled: true,
                    bot_token_env: "KAI_TELEGRAM_BOT_TOKEN".to_string(),
                    owner_user_id: None,
                    progress: TelegramProgressConfig {
                        enabled: true,
                        edit_interval_ms: 2500,
                        idle_update_secs: 8,
                    },
                },
            },
            media: MediaConfig {
                transcription: TranscriptionConfig {
                    provider: "groq".to_string(),
                    groq_api_key_env: "GROQ_API_KEY".to_string(),
                    groq_model: "whisper-large-v3-turbo".to_string(),
                    command: None,
                },
            },
            paths: PathsConfig {
                root_app: root_app.display().to_string(),
                root_work: root_work.display().to_string(),
            },
            runner: RunnerConfig {
                provider: RunnerProvider::Codex,
                codex: CodexConfig {
                    binary: "codex".to_string(),
                    override_config: None,
                },
            },
            context_files: ContextFilesConfig {
                soul: root_app.join("SOUL.md").display().to_string(),
                memory: root_app.join("MEMORY.md").display().to_string(),
                todo: root_app.join("TODO.md").display().to_string(),
            },
        },
    }
}

#[test]
fn run_lock_status_reports_stale_after_release() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    {
        let _guard = acquire_run_guard(&config).expect("acquire run guard");
        let status = run_lock_status(&config).expect("run lock status while running");
        assert!(status.locked);
        assert!(status.pid.is_some());
        assert!(!status.stale);
    }

    let status = run_lock_status(&config).expect("run lock status after release");
    assert!(!status.locked);
    assert!(status.pid.is_none());
    assert!(!status.stale);
}

#[cfg(target_os = "macos")]
#[test]
fn render_macos_plist_contains_required_fields() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);
    let runner = root_app.join("bin").join("service-run.sh");

    let plist = launchd::render_macos_plist(&config, &runner);

    assert!(plist.contains("<key>Label</key>"));
    assert!(plist.contains(MAC_LABEL));
    assert!(plist.contains("service-run.sh"));
    assert!(plist.contains("service.stdout.log"));
    assert!(plist.contains("service.stderr.log"));
    assert!(!plist.contains("KAI_TELEGRAM_BOT_TOKEN"));
    assert!(!plist.contains("secret-token"));
}

#[cfg(target_os = "macos")]
#[test]
fn render_service_runner_uses_keychain_lookup() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);
    let binary = root_app.join("kai");

    let runner = launchd::render_service_runner(&config, &binary);

    assert!(runner.contains("find-generic-password"));
    assert!(runner.contains("KAI_TELEGRAM_BOT_TOKEN"));
    assert!(runner.contains("ai.kai.telegram.bot-token"));
    assert!(runner.contains("exec"));
}
