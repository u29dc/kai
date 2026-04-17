use super::*;

mod active_turn;
mod pending_reply_deliveries;
mod pending_turns;
mod processed_updates;
mod update_failures;

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(super) enum StoredActiveTurnState {
    Current(ActiveTurnState),
    Legacy(PendingTurn),
}

const ACTIVE_TURN_SINGLETON: i64 = 1;

fn active_turn_state_from_legacy(value: StoredActiveTurnState) -> ActiveTurnState {
    match value {
        StoredActiveTurnState::Current(current) => current,
        StoredActiveTurnState::Legacy(pending) => pending.into(),
    }
}

fn serialize_active_turn_state(state: &ActiveTurnState) -> KaiResult<String> {
    serde_json::to_string(state).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to serialize active turn state: {error}"),
        )
    })
}

fn deserialize_active_turn_state(payload: &str) -> KaiResult<ActiveTurnState> {
    serde_json::from_str::<StoredActiveTurnState>(payload)
        .map(active_turn_state_from_legacy)
        .map_err(|error| {
            KaiError::new(
                ErrorCode::StateError,
                format!("failed to deserialize active turn state: {error}"),
            )
        })
}

fn deserialize_pending_turn(payload: &str) -> KaiResult<PendingTurn> {
    serde_json::from_str::<PendingTurn>(payload).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to deserialize pending turn payload: {error}"),
        )
    })
}

fn pending_turn_missing_target(turn: &PendingTurn) -> bool {
    turn.target.workspace_id.trim().is_empty() || turn.target.working_dir.trim().is_empty()
}

fn serialize_processed_update_outcome(outcome: &ProcessedUpdateOutcome) -> KaiResult<String> {
    serde_json::to_string(outcome).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to serialize processed update outcome: {error}"),
        )
    })
}

fn deserialize_processed_update_outcome(
    response_text: String,
    outcome_json: Option<&str>,
) -> KaiResult<ProcessedUpdateOutcome> {
    let Some(outcome_json) = outcome_json else {
        return Ok(ProcessedUpdateOutcome::TextReply {
            text: response_text,
        });
    };

    serde_json::from_str::<ProcessedUpdateOutcome>(outcome_json).map_err(|error| {
        KaiError::new(
            ErrorCode::StateError,
            format!("failed to deserialize processed update outcome: {error}"),
        )
    })
}
