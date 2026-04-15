use super::*;
use crate::config::RunnerProvider;
use crate::workspace::ExecutionTarget;

impl StateStore {
    pub fn get_owner_user_id(&self) -> KaiResult<Option<i64>> {
        self.get_json_value("telegram.owner_user_id")
    }

    pub fn set_owner_user_id(&self, user_id: i64) -> KaiResult<()> {
        self.set_json_value("telegram.owner_user_id", &user_id)
    }

    pub fn get_owner_chat_id(&self) -> KaiResult<Option<i64>> {
        self.get_json_value("telegram.owner_chat_id")
    }

    pub fn set_owner_chat_id(&self, chat_id: i64) -> KaiResult<()> {
        self.set_json_value("telegram.owner_chat_id", &chat_id)
    }

    pub fn get_pending_pairing(&self) -> KaiResult<Option<PendingPairing>> {
        self.get_json_value("telegram.pending_pairing")
    }

    pub fn set_pending_pairing(&self, pairing: &PendingPairing) -> KaiResult<()> {
        self.set_json_value("telegram.pending_pairing", pairing)?;
        self.delete_value("telegram.pending_pair_code")?;
        Ok(())
    }

    pub fn clear_pending_pairing(&self) -> KaiResult<()> {
        self.delete_value("telegram.pending_pairing")?;
        self.delete_value("telegram.pending_pair_code")
    }

    pub fn pending_pairing_view(&self) -> KaiResult<Option<PendingPairingView>> {
        Ok(self
            .get_pending_pairing()?
            .map(|pairing| PendingPairingView {
                expires_at: pairing.expires_at,
                remaining_attempts: pairing.remaining_attempts,
            }))
    }

    pub fn get_active_session_id(&self) -> KaiResult<Option<String>> {
        self.get_json_value("codex.active_session_id")
    }

    pub fn set_active_session_id(&self, session_id: &str) -> KaiResult<()> {
        self.set_json_value("codex.active_session_id", &session_id)
    }

    pub fn clear_active_session_id(&self) -> KaiResult<()> {
        self.delete_value("codex.active_session_id")
    }

    pub fn get_update_offset(&self) -> KaiResult<i64> {
        Ok(self
            .get_json_value::<i64>("telegram.update_offset")?
            .unwrap_or_default())
    }

    pub fn set_update_offset(&self, offset: i64) -> KaiResult<()> {
        self.set_json_value("telegram.update_offset", &offset)
    }

    pub fn get_replay_package(&self) -> KaiResult<Option<ReplayPackage>> {
        self.get_json_value("codex.replay_package")
    }

    pub fn set_replay_package(&self, replay_package: &ReplayPackage) -> KaiResult<()> {
        self.set_json_value("codex.replay_package", replay_package)
    }

    pub fn clear_replay_package(&self) -> KaiResult<()> {
        self.delete_value("codex.replay_package")
    }

    pub fn get_selected_workspace_id(&self) -> KaiResult<Option<String>> {
        self.get_json_value("workspace.selected_id")
    }

    pub fn set_selected_workspace_id(&self, workspace_id: &str) -> KaiResult<()> {
        self.set_json_value("workspace.selected_id", &workspace_id)
    }

    pub fn clear_selected_workspace_id(&self) -> KaiResult<()> {
        self.delete_value("workspace.selected_id")
    }

    pub fn get_session_binding(
        &self,
        target: &ExecutionTarget,
    ) -> KaiResult<Option<SessionBinding>> {
        let key = session_binding_key(target.provider, &target.workspace_id);
        let binding = self.get_json_value::<SessionBinding>(&key)?;
        if let Some(binding) = binding {
            if binding.working_dir == target.working_dir {
                return Ok(Some(binding));
            }
            self.delete_value(&key)?;
        }
        Ok(None)
    }

    pub fn set_session_binding(&self, target: &ExecutionTarget, session_id: &str) -> KaiResult<()> {
        self.set_json_value(
            &session_binding_key(target.provider, &target.workspace_id),
            &SessionBinding {
                session_id: session_id.to_string(),
                working_dir: target.working_dir.clone(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            },
        )
    }

    pub fn clear_session_binding(&self, target: &ExecutionTarget) -> KaiResult<()> {
        self.delete_value(&session_binding_key(target.provider, &target.workspace_id))
    }

    pub fn get_target_replay_binding(
        &self,
        target: &ExecutionTarget,
    ) -> KaiResult<Option<ReplayBinding>> {
        let key = replay_binding_key(target.provider, &target.workspace_id);
        let binding = self.get_json_value::<ReplayBinding>(&key)?;
        if let Some(binding) = binding {
            if binding.working_dir == target.working_dir {
                return Ok(Some(binding));
            }
            self.delete_value(&key)?;
        }
        Ok(None)
    }

    pub fn get_target_replay_package(
        &self,
        target: &ExecutionTarget,
    ) -> KaiResult<Option<ReplayPackage>> {
        Ok(self
            .get_target_replay_binding(target)?
            .map(|binding| binding.replay_package))
    }

    pub fn set_target_replay_package(
        &self,
        target: &ExecutionTarget,
        replay_package: &ReplayPackage,
    ) -> KaiResult<()> {
        self.set_json_value(
            &replay_binding_key(target.provider, &target.workspace_id),
            &ReplayBinding {
                replay_package: replay_package.clone(),
                working_dir: target.working_dir.clone(),
                updated_at: chrono::Utc::now().to_rfc3339(),
            },
        )
    }

    pub fn clear_target_replay_package(&self, target: &ExecutionTarget) -> KaiResult<()> {
        self.delete_value(&replay_binding_key(target.provider, &target.workspace_id))
    }

    pub fn load_json_state<T>(&self, key: &str) -> KaiResult<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        self.get_json_value(key)
    }

    pub fn store_json_state<T>(&self, key: &str, value: &T) -> KaiResult<()>
    where
        T: Serialize + ?Sized,
    {
        self.set_json_value(key, value)
    }

    pub fn remove_json_state(&self, key: &str) -> KaiResult<()> {
        self.delete_value(key)
    }

    pub fn get_command_menu_hash(&self, chat_id: i64) -> KaiResult<Option<String>> {
        self.get_json_value(&format!("telegram.command_menu_hash.{chat_id}"))
    }

    pub fn set_command_menu_hash(&self, chat_id: i64, hash: &str) -> KaiResult<()> {
        self.set_json_value(&format!("telegram.command_menu_hash.{chat_id}"), &hash)
    }

    fn get_json_value<T>(&self, key: &str) -> KaiResult<Option<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        let raw = self
            .connection
            .query_row("SELECT value FROM kv WHERE key = ?1", [key], |row| {
                row.get::<_, String>(0)
            })
            .optional()
            .map_err(sql_state_error("load kv value"))?;

        raw.map(|value| {
            serde_json::from_str::<T>(&value).map_err(|error| {
                KaiError::new(
                    ErrorCode::StateError,
                    format!("failed to deserialize state value `{key}`: {error}"),
                )
            })
        })
        .transpose()
    }

    fn set_json_value<T>(&self, key: &str, value: &T) -> KaiResult<()>
    where
        T: Serialize + ?Sized,
    {
        let raw = serde_json::to_string(value).map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to serialize state value `{key}`: {error}"),
            )
        })?;

        self.connection
            .execute(
                "INSERT INTO kv (key, value) VALUES (?1, ?2)
				 ON CONFLICT(key) DO UPDATE SET value = excluded.value",
                params![key, raw],
            )
            .map_err(sql_state_error("write kv value"))?;

        Ok(())
    }

    pub(super) fn delete_value(&self, key: &str) -> KaiResult<()> {
        self.connection
            .execute("DELETE FROM kv WHERE key = ?1", [key])
            .map_err(sql_state_error("delete kv value"))?;
        Ok(())
    }
}

fn session_binding_key(provider: RunnerProvider, workspace_id: &str) -> String {
    format!("session.binding.{}.{}", provider.as_key(), workspace_id)
}

fn replay_binding_key(provider: RunnerProvider, workspace_id: &str) -> String {
    format!("session.replay.{}.{}", provider.as_key(), workspace_id)
}
