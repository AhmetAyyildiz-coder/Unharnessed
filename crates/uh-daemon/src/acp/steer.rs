//! A message for the running turn, now rather than after it.
//!
//! `_unharnessed/session/steer` `{ sessionId, prompt, turn }` (the blocks
//! of a `session/prompt`, and the running turn's `promptTurnId`) steers
//! that turn: the agent reads the message before its next model call. It answers `{}` once the message waits;
//! when the agent takes it, a `turnStart` with origin `steer` (and its
//! `rewindTo`) says so, and what follows belongs to it. The turn may end
//! before taking it: the message goes with the turn, the client sends it
//! again. It is refused when that turn no longer runs or is being stopped
//! (`-32603`), and for blocks, images or a command that cannot join a turn
//! (`-32602`, `data.details`).

use std::sync::Arc;

use serde_json::{json, Value};
use tokio::sync::Mutex;

use super::prompt::image_refusal;
use super::session::{prompt_block_error, AdmittedPrompt};
use super::types::PromptParams;
use super::{internal_error, jsonrpc, producer, AcpModeState, ConnectionState};

pub(super) const STEER: &str = "_unharnessed/session/steer";

pub(super) async fn handle_steer(
    id: Value,
    params: Value,
    state: Arc<Mutex<ConnectionState>>,
    mode: AcpModeState,
    tx: producer::FrameSink,
) {
    let turn = params.get("turn").and_then(Value::as_u64);
    let params = PromptParams::parse(&params);
    // Held while the message joins: the turn cannot end in between, its
    // end drops what it did not take.
    let state = state.lock().await;
    let Some(entry) = state
        .session
        .as_ref()
        .filter(|entry| entry.session.id == params.session_id)
    else {
        let _ = tx.send(internal_error(
            &id,
            &format!("Unknown ACP session: {}", params.session_id),
        ));
        return;
    };
    if state.session_close_in_flight
        || entry.prompt_task.is_none()
        || entry.session.cancel_requested()
        || turn != Some(entry.session.producer().active_prompt_turn().await)
    {
        let _ = tx.send(internal_error(
            &id,
            "The turn is over: send it as a prompt.",
        ));
        return;
    }
    let prompt = match AdmittedPrompt::parse(&params.prompt) {
        Ok(prompt) => prompt,
        Err(error) => {
            let _ = tx.send(prompt_block_error(&id, &error));
            return;
        }
    };
    let refused = |details: String| {
        jsonrpc::error_response(
            &id,
            jsonrpc::INVALID_PARAMS,
            "Invalid params",
            Some(&json!({ "details": details })),
        )
    };
    if let Some(refusal) = image_refusal(&mode, &prompt.images) {
        let _ = tx.send(refused(refusal));
        return;
    }
    let images = prompt
        .images
        .into_iter()
        .map(|image| uh_agent::types::ImageContent {
            data: image.data,
            mime_type: image.mime_type,
        })
        .collect();
    match mode
        .engine
        .session
        .steer_message(&prompt.text, images)
        .await
    {
        Ok(message) => {
            // Known before the agent can take it.
            if let uh_agent::types::AgentMessage::Standard(uh_agent::types::Message::User(user)) =
                &message
            {
                entry.session.steered(user.timestamp);
            }
            entry.session.agent().steer(message);
            let _ = tx.send(jsonrpc::response(&id, &json!({})));
        }
        Err(error) => {
            let _ = tx.send(refused(format!("{error:#}")));
        }
    }
}
