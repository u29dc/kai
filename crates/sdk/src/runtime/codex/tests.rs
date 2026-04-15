use super::*;
use crate::config::RunnerProvider;

#[test]
fn stale_resume_detection_matches_missing_rollout_errors() {
    let error = KaiError::new(ErrorCode::RuntimeError, "Codex CLI exited with status 1").with_hint(
        "Error: thread/resume: thread/resume failed: no rollout found for thread id 123",
    );
    assert!(is_stale_resume_error(&error));
}

#[test]
fn stale_resume_detection_does_not_match_generic_backend_errors() {
    let error = KaiError::new(ErrorCode::RuntimeError, "Codex CLI exited with status 1")
        .with_hint("temporary auth failure");
    assert!(!is_stale_resume_error(&error));
}

#[test]
fn replay_package_includes_attachment_refs() {
    let turns = vec![
        TurnRecord {
            id: 1,
            created_at: "2026-04-11T20:00:00Z".to_string(),
            provider: RunnerProvider::Codex,
            workspace_id: "vault".to_string(),
            working_dir: "/tmp/work".to_string(),
            role: "user".to_string(),
            channel: "telegram".to_string(),
            sender_id: Some(1),
            text: "inspect this".to_string(),
            codex_session_id: Some("session-1".to_string()),
            outcome_status: Some("received".to_string()),
            attachments: vec![AttachmentInfo {
                kind: "pdf".to_string(),
                path: "/tmp/report.pdf".to_string(),
                original_name: Some("report.pdf".to_string()),
                mime_type: Some("application/pdf".to_string()),
                bytes: 42,
                checksum_blake3: "abc".to_string(),
                media_group_id: None,
                duration_secs: None,
                width: None,
                height: None,
                transcript_text: None,
                transcript_segments: Vec::new(),
                artifacts: Vec::new(),
                notes: Vec::new(),
            }],
        },
        TurnRecord {
            id: 2,
            created_at: "2026-04-11T20:01:00Z".to_string(),
            provider: RunnerProvider::Codex,
            workspace_id: "vault".to_string(),
            working_dir: "/tmp/work".to_string(),
            role: "assistant".to_string(),
            channel: "telegram".to_string(),
            sender_id: None,
            text: "done".to_string(),
            codex_session_id: Some("session-1".to_string()),
            outcome_status: Some("fresh".to_string()),
            attachments: vec![],
        },
    ];

    let replay = create_replay_package(&[], &turns);
    assert_eq!(replay.attachment_refs.len(), 1);
    assert_eq!(replay.attachment_refs[0].path, "/tmp/report.pdf");
    assert!(replay.summary.contains("Recent attachment refs: 1"));
}
