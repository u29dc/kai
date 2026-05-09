use std::time::{SystemTime, UNIX_EPOCH};

use super::*;

pub fn migrate_config_to_workspaces() -> KaiResult<ConfigMigrationResult> {
    let config_path = discover_config_path();
    migrate_config_to_workspaces_at(&config_path)
}

pub fn migrate_config_to_workspaces_at(config_path: &Path) -> KaiResult<ConfigMigrationResult> {
    if !config_path.is_file() {
        ensure_config_file(config_path)?;
        return Ok(ConfigMigrationResult {
            config_path: config_path.to_path_buf(),
            backup_path: None,
            migrated: false,
            default_workspace_id: "main".to_string(),
            removed_legacy_keys: Vec::new(),
        });
    }

    harden_private_file(config_path)?;
    let raw = fs::read_to_string(config_path).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to read config file: {error}"),
        )
    })?;
    let legacy_keys = legacy_config_keys(&raw)?;
    let mut document = DocumentMut::from_str(&raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to parse config file for migration: {error}"),
        )
    })?;

    let default_workspace_id = ensure_workspace_document_layout(&mut document)?;
    let mut removed_legacy_keys = Vec::new();
    for key in [
        "paths.root_work",
        "context_files.todo",
        "context_files.soul",
        "context_files.memory",
        "runner.provider",
        "runner.codex.transport",
    ] {
        if remove_document_value(&mut document, key).is_ok() {
            removed_legacy_keys.push(key.to_string());
        }
    }
    if legacy_keys
        .iter()
        .any(|key| key == "media.transcription.command")
        && remove_document_value(&mut document, "media.transcription.command").is_ok()
    {
        removed_legacy_keys.push("media.transcription.command".to_string());
    }

    let backup_path = if legacy_keys.is_empty() && removed_legacy_keys.is_empty() {
        None
    } else {
        let backup_path = backup_config_path(config_path);
        fs::copy(config_path, &backup_path).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to write config backup: {error}"),
            )
        })?;
        harden_private_file(&backup_path)?;
        write_document(config_path, &mut document)?;
        Some(backup_path)
    };

    Ok(ConfigMigrationResult {
        config_path: config_path.to_path_buf(),
        backup_path: backup_path.clone(),
        migrated: backup_path.is_some(),
        default_workspace_id,
        removed_legacy_keys,
    })
}

pub(super) fn legacy_config_keys(raw: &str) -> KaiResult<Vec<String>> {
    let document = DocumentMut::from_str(raw).map_err(|error| {
        KaiError::new(
            ErrorCode::ConfigError,
            format!("failed to inspect config file: {error}"),
        )
    })?;

    let mut keys = Vec::new();
    if document
        .get("paths")
        .and_then(|item| item.get("root_work"))
        .is_some()
    {
        keys.push("paths.root_work".to_string());
    }
    for key in ["todo", "soul", "memory"] {
        if document
            .get("context_files")
            .and_then(|item| item.get(key))
            .is_some()
        {
            keys.push(format!("context_files.{key}"));
        }
    }
    if document
        .get("runner")
        .and_then(|item| item.get("provider"))
        .is_some()
    {
        keys.push("runner.provider".to_string());
    }
    if document
        .get("runner")
        .and_then(|item| item.get("codex"))
        .and_then(|item| item.get("transport"))
        .is_some()
    {
        keys.push("runner.codex.transport".to_string());
    }
    if document
        .get("media")
        .and_then(|item| item.get("transcription"))
        .and_then(|item| item.get("command"))
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_str())
        .is_some()
    {
        keys.push("media.transcription.command".to_string());
    }
    let has_workspaces = document
        .get("workspaces")
        .and_then(|item| item.get("default"))
        .is_some();
    if !has_workspaces {
        keys.push("workspaces.default".to_string());
    }
    keys.sort();
    keys.dedup();
    Ok(keys)
}

fn ensure_workspace_document_layout(document: &mut DocumentMut) -> KaiResult<String> {
    let existing_default = document
        .get("workspaces")
        .and_then(|item| item.get("default"))
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned);
    if let Some(default_workspace_id) = existing_default {
        return Ok(default_workspace_id);
    }

    let workspace_path = document
        .get("paths")
        .and_then(|item| item.get("root_work"))
        .and_then(|item| item.as_value())
        .and_then(|value| value.as_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "~/.tools/kai/work".to_string());
    let workspace_id = derive_workspace_id(&workspace_path);
    let workspace_label = derive_workspace_label(&workspace_id);
    set_document_value(document, "workspaces.default", &workspace_id)?;
    set_document_value(
        document,
        &format!("workspaces.{workspace_id}.label"),
        &workspace_label,
    )?;
    set_document_value(
        document,
        &format!("workspaces.{workspace_id}.path"),
        &workspace_path,
    )?;
    Ok(workspace_id)
}

fn derive_workspace_id(path: &str) -> String {
    let file_name = expand_home(path)
        .file_name()
        .and_then(|value| value.to_str())
        .map(ToOwned::to_owned)
        .unwrap_or_else(|| "main".to_string());
    let mut id = file_name
        .chars()
        .map(|char| {
            if char.is_ascii_alphanumeric() {
                char.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    while id.contains("--") {
        id = id.replace("--", "-");
    }
    if id.is_empty() || id == "default" {
        "main".to_string()
    } else {
        id
    }
}

fn derive_workspace_label(id: &str) -> String {
    if id == "main" {
        return "Main".to_string();
    }

    id.split('-')
        .filter(|segment| !segment.is_empty())
        .map(|segment| {
            let mut chars = segment.chars();
            match chars.next() {
                Some(first) => {
                    let mut label = String::new();
                    label.push(first.to_ascii_uppercase());
                    label.push_str(chars.as_str());
                    label
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn backup_config_path(config_path: &Path) -> PathBuf {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .unwrap_or(0);
    let base_name = config_path
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("config.toml");
    config_path.with_file_name(format!("{base_name}.bak.{timestamp}"))
}
