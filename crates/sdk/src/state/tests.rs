use std::fs;

use tempfile::tempdir;

use super::*;
use crate::config::{
    AgentConfig, ChannelConfig, CodexConfig, Config, ContextFilesConfig, LoadedConfig, MediaConfig,
    PathsConfig, RunnerConfig, RunnerProvider, TelegramConfig, TelegramProgressConfig,
    TranscriptionConfig, WorkspaceConfig, WorkspacesConfig,
};
use crate::workspace::ExecutionTarget;

fn test_config(root_app: &Path, root_work: &Path) -> LoadedConfig {
    LoadedConfig {
        config_path: root_app.join("config.toml"),
        config_exists: false,
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
            },
            workspaces: WorkspacesConfig {
                default_workspace: "main".to_string(),
                entries: std::collections::BTreeMap::from([(
                    "main".to_string(),
                    WorkspaceConfig {
                        label: Some("Main".to_string()),
                        path: root_work.display().to_string(),
                    },
                )]),
            },
        },
    }
}

fn test_target(config: &LoadedConfig) -> ExecutionTarget {
    let path = config
        .values
        .workspaces
        .entries
        .get("main")
        .expect("main workspace")
        .path
        .clone();
    ExecutionTarget {
        workspace_id: "main".to_string(),
        working_dir: path,
        provider: RunnerProvider::Codex,
    }
}

#[test]
fn state_round_trips_pending_pairing() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .set_pending_pairing(&PendingPairing::issue("ABC12345", 10, 5))
        .expect("set pending pairing");

    let pairing = store
        .get_pending_pairing()
        .expect("load pending pairing")
        .expect("pairing exists");
    assert_eq!(pairing.remaining_attempts, 5);
    assert!(pairing.verify("ABC12345"));
}

#[test]
fn processed_update_round_trips() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .set_processed_update(42, "cached reply", Some("session-1"))
        .expect("set processed update");

    let processed = store
        .get_processed_update(42)
        .expect("load processed update")
        .expect("processed update must exist");
    assert_eq!(processed.update_id, 42);
    assert_eq!(processed.response_text, "cached reply");
    assert_eq!(processed.codex_session_id.as_deref(), Some("session-1"));
}

#[test]
fn update_failure_round_trips_and_clears() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let first = store
        .record_update_failure(77, &KaiError::invalid_argument("bad attachment"))
        .expect("record first failure");
    assert_eq!(first.attempt_count, 1);
    assert_eq!(first.last_error_code, "invalid_argument");

    let second = store
        .record_update_failure(
            77,
            &KaiError::new(ErrorCode::RuntimeError, "temporary backend issue"),
        )
        .expect("record second failure");
    assert_eq!(second.attempt_count, 2);
    assert_eq!(second.last_error_code, "runtime_error");

    store.clear_update_failure(77).expect("clear failure");
    assert!(
        store
            .get_update_failure(77)
            .expect("load failure")
            .is_none()
    );
}

#[test]
fn pending_turn_queue_round_trips_and_preserves_order() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:00:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![1],
            chat_id: 1,
            sender_id: 7,
            text: "first".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue first");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-2".to_string(),
            enqueued_at: "2026-04-12T00:01:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![2],
            chat_id: 1,
            sender_id: 7,
            text: "second".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue second");

    assert_eq!(store.pending_turn_queue_len().expect("queue length"), 2);
    let first = store
        .pop_pending_turn()
        .expect("pop first")
        .expect("first turn");
    let second = store
        .pop_pending_turn()
        .expect("pop second")
        .expect("second turn");

    assert_eq!(first.id, "turn-1");
    assert_eq!(second.id, "turn-2");
    assert!(store.pop_pending_turn().expect("pop empty").is_none());
}

#[test]
fn pending_turn_queue_rejects_new_turns_past_limit() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    for index in 0..MAX_PENDING_TURNS {
        store
            .enqueue_pending_turn(&PendingTurn {
                id: format!("turn-{index}"),
                enqueued_at: "2026-04-12T00:00:00Z".to_string(),
                target: test_target(&config),
                channel: "telegram".to_string(),
                update_ids: vec![index as i64],
                chat_id: 1,
                sender_id: 7,
                text: format!("turn {index}"),
                attachments: Vec::new(),
            })
            .expect("enqueue turn");
    }

    let error = store
        .enqueue_pending_turn(&PendingTurn {
            id: "overflow".to_string(),
            enqueued_at: "2026-04-12T00:30:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![999],
            chat_id: 1,
            sender_id: 7,
            text: "overflow".to_string(),
            attachments: Vec::new(),
        })
        .expect_err("queue must reject overflow");

    assert!(matches!(error.code, ErrorCode::BlockedPrerequisite));
}

#[test]
fn pending_turn_queue_replaces_duplicate_turn_id_without_growth() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:00:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![1],
            chat_id: 1,
            sender_id: 7,
            text: "first".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue first");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:05:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![2],
            chat_id: 1,
            sender_id: 7,
            text: "replacement".to_string(),
            attachments: Vec::new(),
        })
        .expect("replace duplicate");

    assert_eq!(store.pending_turn_queue_len().expect("queue length"), 1);
    let turn = store
        .pop_pending_turn()
        .expect("pop turn")
        .expect("queued turn");
    assert_eq!(turn.update_ids, vec![2]);
    assert_eq!(turn.text, "replacement");
}

#[test]
fn session_view_includes_queue_metadata() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .set_owner_user_id(1000000001)
        .expect("set owner user id");
    store
        .set_owner_chat_id(1000000001)
        .expect("set owner chat id");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:00:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![1, 2],
            chat_id: 1000000001,
            sender_id: 1000000001,
            text: "hello from queue".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue turn");

    let session = store.session_view(&config).expect("session view");
    assert_eq!(session.provider, "codex");
    assert_eq!(session.default_workspace_id, "main");
    assert_eq!(session.selected_workspace_id, "main");
    assert_eq!(
        session.selected_workspace_path,
        root_work.display().to_string()
    );
    assert_eq!(session.workspaces.len(), 1);
    assert_eq!(session.workspaces[0].id, "main");
    assert!(session.workspaces[0].selected);
    assert_eq!(session.queue_limit, MAX_PENDING_TURNS);
    assert_eq!(session.queued_turns, 1);
    assert_eq!(session.pending_reply_deliveries, 0);
    assert_eq!(session.queued_preview.len(), 1);
    assert_eq!(session.queued_preview[0].id, "turn-1");
    assert_eq!(session.queued_preview[0].update_count, 2);
}

#[test]
fn active_turn_state_round_trips_with_status_message_id() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let pending = PendingTurn {
        id: "turn-1".to_string(),
        enqueued_at: "2026-04-12T00:00:00Z".to_string(),
        target: test_target(&config),
        channel: "telegram".to_string(),
        update_ids: vec![1],
        chat_id: 1,
        sender_id: 7,
        text: "hello".to_string(),
        attachments: Vec::new(),
    };
    store
        .set_active_turn_state(&ActiveTurnState {
            pending: pending.clone(),
            status_message_id: Some(42),
        })
        .expect("set active turn state");

    let active = store
        .get_active_turn_state()
        .expect("load active turn state")
        .expect("active turn state");
    assert_eq!(active.pending.id, pending.id);
    assert_eq!(active.status_message_id, Some(42));
    assert_eq!(
        store
            .get_active_pending_turn()
            .expect("load active pending turn")
            .expect("active pending turn")
            .id,
        pending.id
    );
}

#[test]
fn active_turn_state_reads_legacy_pending_turn_payload() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let pending = PendingTurn {
        id: "turn-legacy".to_string(),
        enqueued_at: "2026-04-12T00:00:00Z".to_string(),
        target: test_target(&config),
        channel: "telegram".to_string(),
        update_ids: vec![9],
        chat_id: 1,
        sender_id: 7,
        text: "legacy".to_string(),
        attachments: Vec::new(),
    };
    store
        .store_json_state("telegram.active_turn", &pending)
        .expect("store legacy active turn");

    let active = store
        .get_active_turn_state()
        .expect("load active turn state")
        .expect("active turn state");
    assert_eq!(active.pending.id, pending.id);
    assert_eq!(active.status_message_id, None);
}

#[test]
fn claim_next_pending_turn_moves_queue_head_into_active_state() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:00:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![1],
            chat_id: 1,
            sender_id: 7,
            text: "first".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue first");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-2".to_string(),
            enqueued_at: "2026-04-12T00:01:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![2],
            chat_id: 1,
            sender_id: 7,
            text: "second".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue second");

    let active = store
        .claim_next_pending_turn()
        .expect("claim pending turn")
        .expect("active turn");
    assert_eq!(active.pending.id, "turn-1");
    assert_eq!(store.pending_turn_queue_len().expect("queue length"), 1);
    assert_eq!(
        store
            .get_active_turn_state()
            .expect("get active turn state")
            .expect("persisted active turn")
            .pending
            .id,
        "turn-1"
    );
}

#[test]
fn recover_active_turn_requeues_claimed_turn_once() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .enqueue_pending_turn(&PendingTurn {
            id: "turn-1".to_string(),
            enqueued_at: "2026-04-12T00:00:00Z".to_string(),
            target: test_target(&config),
            channel: "telegram".to_string(),
            update_ids: vec![1],
            chat_id: 1,
            sender_id: 7,
            text: "first".to_string(),
            attachments: Vec::new(),
        })
        .expect("enqueue turn");
    let claimed = store
        .claim_next_pending_turn()
        .expect("claim turn")
        .expect("claimed turn");
    let recovered = store
        .recover_active_turn()
        .expect("recover active turn")
        .expect("recovered turn");

    assert_eq!(claimed.pending.id, recovered.pending.id);
    assert!(
        store
            .get_active_turn_state()
            .expect("load active")
            .is_none()
    );
    assert_eq!(store.pending_turn_queue_len().expect("queue length"), 1);
    assert!(
        store
            .recover_active_turn()
            .expect("recover again should succeed")
            .is_none()
    );
}

#[test]
fn pending_reply_delivery_round_trips_progress_and_clears_matching_active_turn() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let pending = PendingTurn {
        id: "turn-1".to_string(),
        enqueued_at: "2026-04-12T00:00:00Z".to_string(),
        target: test_target(&config),
        channel: "telegram".to_string(),
        update_ids: vec![1, 2],
        chat_id: 1,
        sender_id: 7,
        text: "hello".to_string(),
        attachments: Vec::new(),
    };
    store
        .set_active_turn_state(&ActiveTurnState {
            pending: pending.clone(),
            status_message_id: Some(42),
        })
        .expect("set active turn state");
    store
        .enqueue_pending_reply_delivery(&PendingReplyDelivery {
            delivery_id: "delivery-1".to_string(),
            turn_id: pending.id.clone(),
            chat_id: pending.chat_id,
            response_text: "first\nsecond".to_string(),
            codex_session_id: "session-1".to_string(),
            status_message_id: Some(42),
            update_ids: pending.update_ids.clone(),
            attempts: 0,
            created_at: "2026-04-12T00:05:00Z".to_string(),
            next_chunk_index: 0,
            sent_message_ids: Vec::new(),
        })
        .expect("enqueue pending reply delivery");

    assert!(
        store
            .get_active_turn_state()
            .expect("load active turn")
            .is_none()
    );
    assert_eq!(
        store
            .pending_reply_delivery_count()
            .expect("pending reply delivery count"),
        1
    );

    store
        .record_pending_reply_delivery_chunk("delivery-1", 1, 777)
        .expect("record first chunk");
    let attempts = store
        .increment_pending_reply_delivery_attempts("delivery-1")
        .expect("increment attempts");
    assert_eq!(attempts, 1);

    let deliveries = store
        .pending_reply_deliveries()
        .expect("load pending reply deliveries");
    assert_eq!(deliveries.len(), 1);
    assert_eq!(deliveries[0].next_chunk_index, 1);
    assert_eq!(deliveries[0].sent_message_ids, vec![777]);
    assert_eq!(deliveries[0].attempts, 1);
}

#[test]
fn finalize_pending_reply_delivery_marks_updates_processed_and_removes_delivery() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let delivery = PendingReplyDelivery {
        delivery_id: "delivery-1".to_string(),
        turn_id: "turn-1".to_string(),
        chat_id: 1,
        response_text: "done".to_string(),
        codex_session_id: "session-1".to_string(),
        status_message_id: Some(42),
        update_ids: vec![11, 12],
        attempts: 0,
        created_at: "2026-04-12T00:05:00Z".to_string(),
        next_chunk_index: 1,
        sent_message_ids: vec![777],
    };
    store
        .enqueue_pending_reply_delivery(&delivery)
        .expect("enqueue delivery");

    store
        .finalize_pending_reply_delivery(&delivery)
        .expect("finalize delivery");

    assert_eq!(
        store
            .pending_reply_delivery_count()
            .expect("pending reply delivery count"),
        0
    );
    let processed = store
        .get_processed_update(11)
        .expect("processed update")
        .expect("processed update must exist");
    assert_eq!(processed.response_text, "done");
    assert_eq!(processed.codex_session_id.as_deref(), Some("session-1"));
}

#[test]
fn cleanup_staged_attachments_removes_partial_and_stale_files() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    let partial = store.paths().attachments_dir.join("dangling.bin.part");
    let stale = store.paths().attachments_dir.join("stale.txt");
    let fresh = store.paths().attachments_dir.join("fresh.txt");

    fs::write(&partial, b"partial").expect("write partial");
    fs::write(&stale, b"old").expect("write stale");
    fs::write(&fresh, b"new").expect("write fresh");

    let old_time = filetime::FileTime::from_unix_time(1, 0);
    filetime::set_file_mtime(&stale, old_time).expect("age stale file");

    let result = store
        .cleanup_staged_attachments(Duration::from_secs(60))
        .expect("cleanup attachments");
    assert_eq!(result.removed_partial_files, 1);
    assert_eq!(result.removed_stale_files, 1);
    assert!(!partial.exists());
    assert!(!stale.exists());
    assert!(fresh.exists());
}

#[test]
fn cleanup_runtime_state_prunes_old_rows_and_compacts_audit() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .connection
        .execute(
            "INSERT INTO processed_updates (update_id, created_at, response_text, codex_session_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![1_i64, "2000-01-01T00:00:00Z", "old", Option::<String>::None],
        )
        .expect("insert old processed update");
    store
        .connection
        .execute(
            "INSERT INTO update_failures (update_id, created_at, updated_at, attempt_count, last_error_code, last_message)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                7_i64,
                "2000-01-01T00:00:00Z",
                "2000-01-01T00:00:00Z",
                1_i64,
                "runtime_error",
                "old failure"
            ],
        )
        .expect("insert old update failure");

    let working_dir = test_target(&config).working_dir;
    for index in 0..4 {
        store
            .record_turn(NewTurn {
                provider: RunnerProvider::Codex,
                workspace_id: "main",
                working_dir: &working_dir,
                role: "user",
                channel: "telegram",
                sender_id: Some(1),
                text: &format!("turn-{index}"),
                codex_session_id: None,
                outcome_status: Some("received"),
                attachments: &[],
            })
            .expect("record turn");
    }

    fs::write(
        store.paths().audit_path.clone(),
        "line-1\nline-2\nline-3\nline-4\nline-5\n",
    )
    .expect("write audit log");

    let result = store
        .cleanup_runtime_state(Duration::from_secs(1), Duration::from_secs(1), 2, 18)
        .expect("cleanup runtime state");
    assert_eq!(result.removed_old_processed_updates, 1);
    assert_eq!(result.removed_old_update_failures, 1);
    assert_eq!(result.removed_old_turns, 2);
    assert!(result.audit_compacted);
    assert!(
        store
            .get_processed_update(1)
            .expect("processed update lookup")
            .is_none()
    );
    assert!(store.recent_turns(10).expect("recent turns").len() <= 2);
}

#[test]
fn audit_log_redacts_secret_like_strings() {
    let tempdir = tempdir().expect("tempdir");
    let root_app = tempdir.path().join("kai-home");
    let root_work = tempdir.path().join("work");
    let config = test_config(&root_app, &root_work);

    let store = StateStore::open(&config).expect("state store");
    store
        .append_audit_json(&serde_json::json!({
            "event": "test",
            "message": "Authorization: Bearer secret-value",
            "token": "[REDACTED-TELEGRAM-TOKEN]",
            "url": "https://example.com?api_key=gsk_secret",
        }))
        .expect("append audit");

    let raw = fs::read_to_string(store.paths().audit_path.clone()).expect("read audit");
    assert!(raw.contains("[REDACTED]"));
    assert!(!raw.contains("AAFbluiDk8KPd83dPhcNdXr0XbHbJali72A"));
    assert!(!raw.contains("Bearer secret-value"));
    assert!(!raw.contains("api_key=gsk_secret"));
}
