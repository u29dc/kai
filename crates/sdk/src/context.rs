use std::fs::{self, File};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::config::LoadedConfig;

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

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContextSnapshot {
    pub role: String,
    pub path: String,
    pub exists: bool,
    pub readable: bool,
    pub bytes: Option<u64>,
}

pub fn context_report(config: &LoadedConfig) -> ContextReport {
    let paths = [
        ("soul", config.values.context_files.soul.as_str()),
        ("memory", config.values.context_files.memory.as_str()),
    ];

    let entries = paths
        .into_iter()
        .map(|(role, path)| {
            let file_path = Path::new(path);
            let metadata = fs::metadata(file_path).ok();
            let exists = metadata.is_some();
            let readable = exists && File::open(file_path).is_ok();

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

pub fn context_snapshots(config: &LoadedConfig) -> Vec<ContextSnapshot> {
    context_report(config)
        .entries
        .into_iter()
        .map(|entry| ContextSnapshot {
            role: entry.role.to_uppercase(),
            path: entry.path,
            exists: entry.exists,
            readable: entry.readable,
            bytes: entry.bytes,
        })
        .collect()
}
