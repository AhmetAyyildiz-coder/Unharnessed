//! The session runtime: shared goal-driver state with kernel host-handler
//! registration. This is the wiring layer that binds the goal host requests
//! (kernel bridge) to a live session, the Rust equivalent of the
//! `AgentSession` host-request controllers.

use std::sync::Arc;

use serde_json::Value;
use tokio::sync::Mutex;

use crate::goals::GoalHostResponse;
use crate::kernel::shared::{host_handler, HostRequestHandlers};
use crate::session::manager::SessionManager;

use super::goal_driver::GoalDriver;
use super::host_requests::handle_goal_host_request;

/// The host-side purge of queued goal-context turns (TS
/// `_clearQueuedGoalContexts` at the `_completeGoalFromHost` site): the
/// queue lanes live in the daemon worker, so the completing kernel host
/// request invokes this seam instead — dropping a continuation queued
/// while the goal was completing.
pub type QueuedGoalContextPurge = Arc<dyn Fn() + Send + Sync>;

/// Session-scoped runtime state the kernel bridge reaches.
pub struct SessionRuntime {
    goal_driver: Arc<Mutex<GoalDriver>>,
    /// Invoked after a kernel `goal.complete` settles the goal (TS
    /// `_completeGoalFromHost` -> `_clearQueuedGoalContexts`); `None` when
    /// the embedding owns no queued goal contexts.
    goal_complete_purge: Option<QueuedGoalContextPurge>,
}

impl SessionRuntime {
    /// Build a runtime, rehydrating any persisted goal.
    #[must_use]
    pub fn new(session: &SessionManager) -> Self {
        Self {
            goal_driver: Arc::new(Mutex::new(GoalDriver::load_persisted(session))),
            goal_complete_purge: None,
        }
    }

    /// Set the post-completion purge seam (the daemon worker's queue
    /// purge).
    pub fn set_goal_complete_purge(&mut self, purge: QueuedGoalContextPurge) {
        self.goal_complete_purge = Some(purge);
    }

    #[must_use]
    pub fn goal_driver(&self) -> &Arc<Mutex<GoalDriver>> {
        &self.goal_driver
    }

    /// Register the `goal.*` handlers onto a handler map.
    pub fn register_host_handlers(
        &self,
        session: Arc<Mutex<SessionManager>>,
        handlers: &mut HostRequestHandlers,
    ) {
        let driver = self.goal_driver.clone();
        let goal_session = session.clone();
        handlers.register(
            "goal.get",
            host_handler(move |payload| {
                let driver = driver.clone();
                let session = goal_session.clone();
                Box::pin(async move {
                    let mut driver = driver.lock().await;
                    let mut session = session.lock().await;
                    let response = handle_goal_host_request(
                        payload
                            .data
                            .get("type")
                            .and_then(Value::as_str)
                            .unwrap_or("goal.get"),
                        &payload.data,
                        &mut driver,
                        &mut session,
                    )?;
                    host_ok(&response)
                })
            }),
        );
        let driver = self.goal_driver.clone();
        let goal_session = session.clone();
        handlers.register(
            "goal.create",
            host_handler(move |payload| {
                let driver = driver.clone();
                let session = goal_session.clone();
                Box::pin(async move {
                    let mut driver = driver.lock().await;
                    let mut session = session.lock().await;
                    let response = handle_goal_host_request(
                        "goal.create",
                        &payload.data,
                        &mut driver,
                        &mut session,
                    )?;
                    host_ok(&response)
                })
            }),
        );
        let driver = self.goal_driver.clone();
        let goal_session = session;
        // TS `_completeGoalFromHost` clears the queued goal contexts: a
        // continuation queued behind the completing turn (e.g. an owed
        // continuation delivered mid-turn) never runs post-completion.
        let goal_complete_purge = self.goal_complete_purge.clone();
        handlers.register(
            "goal.complete",
            host_handler(move |payload| {
                let driver = driver.clone();
                let session = goal_session.clone();
                let purge = goal_complete_purge.clone();
                Box::pin(async move {
                    let response = {
                        let mut driver = driver.lock().await;
                        let mut session = session.lock().await;
                        handle_goal_host_request(
                            "goal.complete",
                            &payload.data,
                            &mut driver,
                            &mut session,
                        )?
                    };
                    if let Some(purge) = purge {
                        purge();
                    }
                    host_ok(&response)
                })
            }),
        );
    }
}

fn host_ok(response: &GoalHostResponse) -> anyhow::Result<Value> {
    serde_json::to_value(response).map_err(anyhow::Error::new)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::kernel::shared::HostRequestPayload;

    fn persisted_session() -> SessionManager {
        let dir = tempfile::TempDir::new().unwrap();
        let session_dir = dir.path().join("session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut session = SessionManager::in_memory(dir.path());
        session.materialize_session_file(Some(session_dir));
        session
    }

    fn payload(data: Value) -> HostRequestPayload {
        HostRequestPayload {
            data,
            cell_source_code: None,
        }
    }

    #[tokio::test]
    async fn goal_handlers_round_trip_through_the_registry() {
        let session = Arc::new(Mutex::new(persisted_session()));
        let runtime = SessionRuntime::new(&*session.lock().await);
        let mut handlers = HostRequestHandlers::default();
        runtime.register_host_handlers(session.clone(), &mut handlers);

        // goal.create via the registry.
        let create = handlers.get("goal.create").unwrap().clone();
        let response = create(payload(serde_json::json!({
            "type": "goal.create",
            "objective": "finish the port",
            "token_budget": 1000
        })))
        .await
        .unwrap();
        assert_eq!(response["goal"]["status"], "active");
        assert_eq!(response["goal"]["objective"], "finish the port");
        // The goal state persisted to the session.
        let persisted = session.lock().await;
        let goal_entries: Vec<&pa_types::session::FileEntry> = persisted
            .get_all_entries()
            .iter()
            .filter(|entry| {
                matches!(entry, pa_types::session::FileEntry::Custom { payload, .. }
                if payload.custom_type == crate::goals::GOAL_STATE_CUSTOM_TYPE)
            })
            .collect();
        assert_eq!(goal_entries.len(), 1);
        drop(persisted);

        // goal.get sees the same driver state.
        let get = handlers.get("goal.get").unwrap().clone();
        let response = get(payload(serde_json::json!({ "type": "goal.get" })))
            .await
            .unwrap();
        assert_eq!(response["goal"]["objective"], "finish the port");
        assert_eq!(response["remaining_tokens"], 1000);

        // goal.complete carries the budget report.
        let complete = handlers.get("goal.complete").unwrap().clone();
        let response = complete(payload(serde_json::json!({ "type": "goal.complete" })))
            .await
            .unwrap();
        assert_eq!(response["goal"]["status"], "complete");
        assert!(response["completion_budget_report"].is_string());
    }
}
