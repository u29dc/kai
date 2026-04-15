use serde::{Deserialize, Serialize};

use crate::config::{LoadedConfig, RunnerProvider};
use crate::error::{ErrorCode, KaiError, KaiResult};
use crate::runtime::agent::selected_provider;
use crate::state::StateStore;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionTarget {
    pub workspace_id: String,
    pub working_dir: String,
    pub provider: RunnerProvider,
}

#[derive(Debug, Clone)]
pub struct WorkspaceSpec {
    pub id: String,
    pub label: String,
    pub path: String,
}

pub fn configured_workspaces(config: &LoadedConfig) -> KaiResult<Vec<WorkspaceSpec>> {
    let mut workspaces = config
        .values
        .workspaces
        .entries
        .iter()
        .map(|(id, workspace)| WorkspaceSpec {
            id: id.clone(),
            label: workspace.label.clone().unwrap_or_else(|| id.clone()),
            path: workspace.path.clone(),
        })
        .collect::<Vec<_>>();
    workspaces.sort_by(|left, right| left.id.cmp(&right.id));
    if workspaces.is_empty() {
        return Err(KaiError::new(
            ErrorCode::ConfigError,
            "at least one workspace must be configured",
        ));
    }
    Ok(workspaces)
}

pub fn default_workspace(config: &LoadedConfig) -> KaiResult<WorkspaceSpec> {
    workspace_by_id(config, &config.values.workspaces.default_workspace)
}

pub fn workspace_by_id(config: &LoadedConfig, workspace_id: &str) -> KaiResult<WorkspaceSpec> {
    let workspace = config
        .values
        .workspaces
        .entries
        .get(workspace_id)
        .ok_or_else(|| {
            KaiError::invalid_argument(format!("unknown workspace `{workspace_id}`"))
                .with_hint("use `kai workspace list` to inspect configured workspaces")
        })?;

    Ok(WorkspaceSpec {
        id: workspace_id.to_string(),
        label: workspace
            .label
            .clone()
            .unwrap_or_else(|| workspace_id.to_string()),
        path: workspace.path.clone(),
    })
}

pub fn selected_workspace(config: &LoadedConfig, state: &StateStore) -> KaiResult<WorkspaceSpec> {
    if let Some(workspace_id) = state.get_selected_workspace_id()?
        && config
            .values
            .workspaces
            .entries
            .contains_key(workspace_id.as_str())
    {
        return workspace_by_id(config, &workspace_id);
    }

    default_workspace(config)
}

pub fn execution_target(config: &LoadedConfig, state: &StateStore) -> KaiResult<ExecutionTarget> {
    let workspace = selected_workspace(config, state)?;
    Ok(ExecutionTarget {
        workspace_id: workspace.id,
        working_dir: workspace.path,
        provider: selected_provider(config)?,
    })
}
