//! Going back to an earlier prompt, to send it again changed.
//!
//! `_unharnessed/session/rewind` `{ sessionId, to }` takes the conversation
//! back to `to`, where it stood before a prompt (its turn's
//! `turnStart.rewindTo`): the next prompt follows it. What came after stays
//! in the session file on a branch of its own; the goal state follows the
//! branch, and the kernel keeps what it holds. It answers `{}`, or is
//! refused: while a turn runs (`-32603`), or when `to` is not in the
//! conversation (`-32602`, `data.details`).

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::types::session_id_params;
use super::{internal_error, jsonrpc, producer, AcpModeState, ConnectionState};

pub(super) const REWIND: &str = "_unharnessed/session/rewind";

pub(super) async fn handle_rewind(
    id: Value,
    params: Value,
    state: Arc<Mutex<ConnectionState>>,
    mode: AcpModeState,
    tx: producer::FrameSink,
) {
    let session_id = session_id_params(&params);
    let to = params
        .get("to")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Held while the conversation moves: no prompt or wake turn starts
    // meanwhile.
    let state = state.lock().await;
    let Some(entry) = state
        .session
        .as_ref()
        .filter(|entry| entry.session.id == session_id)
    else {
        let _ = tx.send(internal_error(
            &id,
            &format!("Unknown ACP session: {session_id}"),
        ));
        return;
    };
    if state.session_close_in_flight || entry.prompt_task.is_some() {
        let _ = tx.send(internal_error(
            &id,
            "The agent is working: stop it before going back.",
        ));
        return;
    }
    if let Err(error) = mode.engine.rewind_to(&to).await {
        let _ = tx.send(jsonrpc::error_response(
            &id,
            jsonrpc::INVALID_PARAMS,
            "Invalid params",
            Some(&json!({ "details": format!("{error:#}") })),
        ));
        return;
    }
    entry.session.publish_goal_update().await;
    entry.session.usage.changed().await;
    let _ = tx.send(jsonrpc::response(&id, &json!({})));
}
