use std::path::Path;

use crate::state::{AttachmentArtifact, AttachmentInfo};

use super::protocol::TurnInputItem;

pub fn build_turn_input(prompt: &str, attachments: &[AttachmentInfo]) -> Vec<TurnInputItem> {
    let mut items = vec![TurnInputItem::Text {
        text: prompt.to_string(),
    }];

    for attachment in attachments {
        if attachment.kind.eq_ignore_ascii_case("image") {
            items.push(TurnInputItem::LocalImage {
                path: attachment.path.clone(),
            });
        }

        for artifact in &attachment.artifacts {
            if artifact_is_image(artifact) {
                items.push(TurnInputItem::LocalImage {
                    path: artifact.path.clone(),
                });
            }
        }
    }

    items
}

fn artifact_is_image(artifact: &AttachmentArtifact) -> bool {
    artifact
        .mime_type
        .as_deref()
        .is_some_and(|mime| mime.starts_with("image/"))
        || Path::new(&artifact.path)
            .extension()
            .and_then(|ext| ext.to_str())
            .is_some_and(|ext| {
                matches!(
                    ext.to_ascii_lowercase().as_str(),
                    "png" | "jpg" | "jpeg" | "gif" | "webp"
                )
            })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AttachmentArtifact, AttachmentInfo};

    fn image_attachment() -> AttachmentInfo {
        AttachmentInfo {
            kind: "image".to_string(),
            path: "/tmp/image.png".to_string(),
            original_name: Some("image.png".to_string()),
            mime_type: Some("image/png".to_string()),
            bytes: 1,
            checksum_blake3: "abc".to_string(),
            media_group_id: None,
            duration_secs: None,
            width: None,
            height: None,
            transcript_text: None,
            transcript_segments: Vec::new(),
            artifacts: vec![AttachmentArtifact {
                kind: "preview".to_string(),
                path: "/tmp/frame.jpg".to_string(),
                mime_type: Some("image/jpeg".to_string()),
                bytes: 1,
                checksum_blake3: None,
            }],
            notes: Vec::new(),
        }
    }

    #[test]
    fn build_turn_input_adds_local_images() {
        let items = build_turn_input("hello", &[image_attachment()]);
        assert_eq!(items.len(), 3);
    }
}
