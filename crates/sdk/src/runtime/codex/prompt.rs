use super::*;

pub(super) fn build_turn_prompt(
    config: &LoadedConfig,
    target: &crate::workspace::ExecutionTarget,
    channel: &str,
    sender_id: i64,
    user_text: &str,
    attachments: &[AttachmentInfo],
) -> String {
    let mut sections = vec![
        "Turn envelope:".to_string(),
        format!("- channel: {channel}"),
        format!("- sender_id: {sender_id}"),
        format!("- local_timezone: {}", config.values.agent.timezone),
        format!("- workspace_id: {}", target.workspace_id),
        format!("- working_dir: {}", target.working_dir),
        format!("- root_app: {}", config.values.paths.root_app),
    ];

    if channel == "telegram" && config.values.channel.telegram.progress.enabled {
        sections.push(
            "- while working on longer tasks, emit brief intermediary progress notes in one short sentence".to_string(),
        );
        sections.push(
            "- describe what you are inspecting or checking, not raw command output".to_string(),
        );
        sections.push("- keep the complete final answer separate at the end".to_string());
    }

    append_attachment_sections(&mut sections, attachments);
    sections.push(String::new());
    sections.push("User message:".to_string());
    sections.push(user_text.to_string());
    sections.join("\n")
}

pub(super) fn build_replay_prompt(
    prompt: &str,
    replay_package: Option<ReplayPackage>,
    recent_turns: &[TurnRecord],
) -> String {
    if let Some(replay_package) = replay_package {
        let mut replay = vec![
            "Session recovery context for kai.".to_string(),
            format!("Replay package updated at: {}", replay_package.updated_at),
            "Replay summary:".to_string(),
            replay_package.summary,
        ];

        if !replay_package.recent_turns.is_empty() {
            replay.push(String::new());
            replay.push("Recent turn excerpts:".to_string());
            for turn in replay_package.recent_turns {
                replay.push(format!(
                    "[{}] {}: {}",
                    turn.created_at, turn.role, turn.text_excerpt
                ));
            }
        }

        if !replay_package.attachment_refs.is_empty() {
            replay.push(String::new());
            replay.push("Recent attachment references:".to_string());
            for attachment in replay_package.attachment_refs {
                replay.push(format!(
                    "- {} at {} (originalName={}, bytes={})",
                    attachment.kind,
                    attachment.path,
                    attachment
                        .original_name
                        .unwrap_or_else(|| "unknown".to_string()),
                    attachment.bytes
                ));
                if let Some(transcript_excerpt) = attachment.transcript_excerpt {
                    replay.push(format!("  transcript: {}", transcript_excerpt));
                }
                for artifact_path in attachment.artifact_paths {
                    replay.push(format!("  artifact: {}", artifact_path));
                }
            }
        }

        replay.push(String::new());
        replay.push("Current inbound turn:".to_string());
        replay.push(prompt.to_string());
        return replay.join("\n");
    }

    if recent_turns.is_empty() {
        return prompt.to_string();
    }

    let mut replay = vec![
        "Session recovery context for kai.".to_string(),
        "Recent turns:".to_string(),
    ];

    for turn in recent_turns {
        replay.push(format!(
            "[{}] {}: {}",
            turn.created_at,
            turn.role,
            replay_turn_excerpt(turn)
        ));
    }

    replay.push(String::new());
    replay.push("Current inbound turn:".to_string());
    replay.push(prompt.to_string());
    replay.join("\n")
}

pub fn create_replay_package(recent_turns: &[TurnRecord]) -> ReplayPackage {
    let selected_turns = recent_turns
        .iter()
        .rev()
        .take(REPLAY_TURN_LIMIT)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect::<Vec<_>>();

    let replay_turns = selected_turns
        .iter()
        .map(|turn| ReplayTurn {
            id: turn.id,
            created_at: turn.created_at.clone(),
            role: turn.role.clone(),
            text_excerpt: replay_turn_excerpt(turn),
        })
        .collect::<Vec<_>>();

    let attachment_refs = collect_replay_attachment_refs(&selected_turns);
    let summary = build_replay_summary(&replay_turns, &attachment_refs);

    ReplayPackage {
        updated_at: Utc::now().to_rfc3339(),
        summary,
        recent_turns: replay_turns,
        attachment_refs,
    }
}

fn append_attachment_sections(sections: &mut Vec<String>, attachments: &[AttachmentInfo]) {
    if attachments.is_empty() {
        sections.push("- attachments: none".to_string());
        return;
    }

    sections.push("- attachments:".to_string());
    for (index, attachment) in attachments.iter().enumerate() {
        sections.push(format!(
            "  - [{}] {} at {} (mime={}, bytes={}, originalName={})",
            index + 1,
            attachment.kind,
            attachment.path,
            attachment.mime_type.as_deref().unwrap_or("unknown"),
            attachment.bytes,
            attachment.original_name.as_deref().unwrap_or("unknown")
        ));
        if let Some(duration_secs) = attachment.duration_secs {
            sections.push(format!("    durationSecs: {duration_secs}"));
        }
        if let Some(media_group_id) = &attachment.media_group_id {
            sections.push(format!("    mediaGroupId: {media_group_id}"));
        }
        if !attachment.artifacts.is_empty() {
            sections.push("    artifacts:".to_string());
            for artifact in &attachment.artifacts {
                sections.push(format!(
                    "      - {} at {} (mime={}, bytes={})",
                    artifact.kind,
                    artifact.path,
                    artifact.mime_type.as_deref().unwrap_or("unknown"),
                    artifact.bytes
                ));
            }
        }
        if !attachment.notes.is_empty() {
            sections.push("    notes:".to_string());
            for note in &attachment.notes {
                sections.push(format!("      - {note}"));
            }
        }
    }

    let transcript_sections = attachments
        .iter()
        .filter_map(|attachment| {
            attachment.transcript_text.as_ref().map(|transcript| {
                format!(
                    "<ATTACHMENT_TRANSCRIPT kind=\"{}\" path=\"{}\">\n{}\n</ATTACHMENT_TRANSCRIPT>",
                    attachment.kind, attachment.path, transcript
                )
            })
        })
        .collect::<Vec<_>>();

    if !transcript_sections.is_empty() {
        sections.push(String::new());
        sections.push("Attachment transcripts:".to_string());
        sections.extend(transcript_sections);
    }
}

fn excerpt_text(input: &str) -> String {
    const MAX_EXCERPT_CHARS: usize = 240;

    let excerpt = input.trim().replace('\n', " ");
    let mut chars = excerpt.chars();
    let truncated = chars.clone().count() > MAX_EXCERPT_CHARS;
    let collected = chars.by_ref().take(MAX_EXCERPT_CHARS).collect::<String>();
    if truncated {
        format!("{collected}...")
    } else {
        collected
    }
}

fn replay_turn_excerpt(turn: &TurnRecord) -> String {
    if !turn.text.trim().is_empty() {
        return excerpt_text(&turn.text);
    }

    let transcript = turn
        .attachments
        .iter()
        .find_map(|attachment| attachment.transcript_text.as_deref());
    if let Some(transcript) = transcript {
        return format!("[attachment transcript] {}", excerpt_text(transcript));
    }

    if turn.attachments.is_empty() {
        return String::new();
    }

    let labels = turn
        .attachments
        .iter()
        .map(|attachment| attachment.kind.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    format!("[attachments] {labels}")
}

fn collect_replay_attachment_refs(source_turns: &[&TurnRecord]) -> Vec<ReplayAttachmentRef> {
    let mut refs = Vec::new();

    for source_turn in source_turns {
        for attachment in &source_turn.attachments {
            if refs
                .iter()
                .any(|existing: &ReplayAttachmentRef| existing.path == attachment.path)
            {
                continue;
            }

            refs.push(ReplayAttachmentRef {
                kind: attachment.kind.clone(),
                path: attachment.path.clone(),
                original_name: attachment.original_name.clone(),
                bytes: attachment.bytes,
                transcript_excerpt: attachment
                    .transcript_text
                    .as_ref()
                    .map(|text| excerpt_text(text)),
                artifact_paths: attachment
                    .artifacts
                    .iter()
                    .map(|artifact| artifact.path.clone())
                    .collect(),
            });

            if refs.len() >= REPLAY_ATTACHMENT_REF_LIMIT {
                return refs;
            }
        }
    }

    refs
}

fn build_replay_summary(
    replay_turns: &[ReplayTurn],
    attachment_refs: &[ReplayAttachmentRef],
) -> String {
    if replay_turns.is_empty() {
        return "No prior turns recorded.".to_string();
    }

    let mut lines = vec![
        format!("Turns captured: {}", replay_turns.len()),
        "Latest conversation state:".to_string(),
    ];

    for turn in replay_turns.iter().rev().take(6).rev() {
        lines.push(format!(
            "- [{}] {}: {}",
            turn.created_at, turn.role, turn.text_excerpt
        ));
    }

    if !attachment_refs.is_empty() {
        lines.push(format!("Recent attachment refs: {}", attachment_refs.len()));
    }

    lines.join("\n")
}
