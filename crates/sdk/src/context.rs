use std::fs;
use std::path::Path;

use serde::Serialize;

use crate::config::LoadedConfig;
use crate::error::{ErrorCode, KaiError, KaiResult};

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextEntry {
    pub role: String,
    pub path: String,
    pub exists: bool,
    pub readable: bool,
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextReport {
    pub entries: Vec<ContextEntry>,
}

#[derive(Debug, Clone)]
pub struct ContextBlob {
    pub role: String,
    pub path: String,
    pub content: String,
}

pub fn context_report(config: &LoadedConfig) -> ContextReport {
    let paths = [
        ("soul", config.values.context_files.soul.as_str()),
        ("memory", config.values.context_files.memory.as_str()),
        ("todo", config.values.context_files.todo.as_str()),
    ];

    let entries = paths
        .into_iter()
        .map(|(role, path)| {
            let file_path = Path::new(path);
            let metadata = fs::metadata(file_path).ok();
            let exists = metadata.is_some();
            let readable = exists && fs::read_to_string(file_path).is_ok();

            ContextEntry {
                role: role.to_string(),
                path: path.to_string(),
                exists,
                readable,
                bytes: metadata.map(|value| value.len()),
            }
        })
        .collect();

    ContextReport { entries }
}

pub fn load_context_blobs(config: &LoadedConfig) -> KaiResult<Vec<ContextBlob>> {
    let entries = [
        ("SOUL", config.values.context_files.soul.as_str()),
        ("MEMORY", config.values.context_files.memory.as_str()),
        ("TODO", config.values.context_files.todo.as_str()),
    ];
    let mut blobs = Vec::new();

    for (role, path) in entries {
        let file_path = Path::new(path);
        if !file_path.is_file() {
            continue;
        }

        let content = fs::read_to_string(file_path).map_err(|error| {
            KaiError::new(
                ErrorCode::IoError,
                format!("failed to read context file `{role}`: {error}"),
            )
        })?;

        blobs.push(ContextBlob {
            role: role.to_string(),
            path: path.to_string(),
            content,
        });
    }

    Ok(blobs)
}
