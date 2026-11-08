//! Kernel host-request handlers for the bundled goal skill: the
//! `snake_case` bridge the Python REPL skill calls. Port of
//! handleGoalHostRequest in agent-session.ts.

use serde_json::Value;

use crate::goals::{goal_host_response, GoalHostResponse, GoalState, GoalStatus};
use crate::session::manager::SessionManager;

use super::goal_driver::GoalDriver;

/// Handle a `goal.*` host request. All goal state stays host-side; the kernel
/// only sees the serialized `snake_case` response.
///
/// # Errors
///
/// Returns an error when the request payload's fields are invalid, the
/// objective or budget fails validation, the request type is unknown, or a
/// goal-state persist fails.
pub fn handle_goal_host_request(
    request_type: &str,
    payload: &Value,
    driver: &mut GoalDriver,
    session: &mut SessionManager,
) -> anyhow::Result<GoalHostResponse> {
    let record = payload.as_object().cloned().unwrap_or_default();
    match request_type {
        // The creation-based timer: the served state reads the goal's age
        // fresh from `created_at` on every read.
        "goal.get" => Ok(goal_host_response(
            &driver.state_with_creation_elapsed(),
            false,
        )),
        "goal.create" => {
            let Some(objective) = record.get("objective").and_then(Value::as_str) else {
                anyhow::bail!("goal.create objective must be a string");
            };
            let token_budget = match record.get("token_budget") {
                None | Some(Value::Null) => None,
                Some(value) => {
                    let budget = value.as_u64().ok_or_else(|| {
                        anyhow::anyhow!("goal.create token_budget must be an integer when provided")
                    })?;
                    Some(budget)
                }
            };
            let goal = create_goal_from_host(driver, session, objective, token_budget)?;
            Ok(goal_host_response(&goal, false))
        }
        "goal.complete" => {
            let goal = complete_goal_from_host(driver, session)?;
            Ok(goal_host_response(&goal, true))
        }
        "goal.block" => {
            let reason = record
                .get("reason")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|reason| !reason.is_empty());
            let Some(reason) = reason else {
                anyhow::bail!("goal.block reason must be a non-empty string");
            };
            let goal = block_goal_from_host(driver, session, reason)?;
            Ok(goal_host_response(&goal, false))
        }
        _ => anyhow::bail!("unknown goal request type \"{request_type}\""),
    }
}

fn create_goal_from_host(
    driver: &mut GoalDriver,
    session: &mut SessionManager,
    objective: &str,
    token_budget: Option<u64>,
) -> anyhow::Result<GoalState> {
    match driver.state().status {
        GoalStatus::Active => anyhow::bail!(
            "cannot create a new goal because this thread already has an active goal; run `await goal.complete()` when it is achieved, or ask the user to clear it with /goal clear"
        ),
        GoalStatus::Paused => anyhow::bail!(
            "cannot create a new goal because a paused goal exists; ask the user to resume it with /goal resume or clear it with /goal clear"
        ),
        GoalStatus::Blocked => anyhow::bail!(
            "cannot create a new goal because a blocked goal exists; ask the user to resume it with /goal resume or clear it with /goal clear"
        ),
        GoalStatus::BudgetLimited => anyhow::bail!(
            "cannot create a new goal because a budget-limited goal exists; ask the user to resume it with /goal resume or clear it with /goal clear"
        ),
        // Idle, or a terminal record (complete / error): start fresh.
        GoalStatus::Idle | GoalStatus::Complete | GoalStatus::Error => {
            driver.start(session, objective, token_budget)
        }
    }
}

fn complete_goal_from_host(
    driver: &mut GoalDriver,
    session: &mut SessionManager,
) -> anyhow::Result<GoalState> {
    if driver.state().objective.is_none() || driver.state().status == GoalStatus::Idle {
        anyhow::bail!("cannot complete goal because this thread has no goal");
    }
    driver.complete(session)?;
    Ok(driver.state_with_creation_elapsed())
}

fn block_goal_from_host(
    driver: &mut GoalDriver,
    session: &mut SessionManager,
    reason: &str,
) -> anyhow::Result<GoalState> {
    if driver.state().status != GoalStatus::Active {
        anyhow::bail!("cannot block goal because this thread has no active goal");
    }
    driver.block(session, reason)?;
    Ok(driver.state_with_creation_elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::manager::SessionManager;
    use serde_json::json;

    fn persisted_session() -> SessionManager {
        let dir = tempfile::TempDir::new().unwrap();
        let session_dir = dir.path().join("session");
        std::fs::create_dir_all(&session_dir).unwrap();
        let mut session = SessionManager::in_memory(dir.path());
        session.materialize_session_file(Some(session_dir));
        session
    }

    #[test]
    fn goal_host_requests() {
        let mut session = persisted_session();
        let mut driver = GoalDriver::new();
        // goal.get with no goal.
        let response =
            handle_goal_host_request("goal.get", &json!({}), &mut driver, &mut session).unwrap();
        assert!(response.goal.is_none());
        // goal.create.
        let response = handle_goal_host_request(
            "goal.create",
            &json!({ "objective": "ship it", "token_budget": 5000 }),
            &mut driver,
            &mut session,
        )
        .unwrap();
        let goal = response.goal.unwrap();
        assert_eq!(goal.objective, "ship it");
        assert_eq!(goal.token_budget, Some(5000));
        assert_eq!(goal.status, GoalStatus::Active);
        assert_eq!(response.remaining_tokens, Some(5000));
        // Creating while active is rejected with the exact TS error.
        let error = handle_goal_host_request(
            "goal.create",
            &json!({ "objective": "another" }),
            &mut driver,
            &mut session,
        )
        .unwrap_err();
        assert!(error.to_string().contains("already has an active goal"));
        // Validation errors from the kernel payload.
        let error = handle_goal_host_request("goal.create", &json!({}), &mut driver, &mut session)
            .unwrap_err();
        assert_eq!(error.to_string(), "goal.create objective must be a string");
        let error = handle_goal_host_request(
            "goal.create",
            &json!({ "objective": "x", "token_budget": "lots" }),
            &mut driver,
            &mut session,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("token_budget must be an integer"));
        // goal.complete carries the completion budget report.
        let response =
            handle_goal_host_request("goal.complete", &json!({}), &mut driver, &mut session)
                .unwrap();
        assert_eq!(response.goal.unwrap().status, GoalStatus::Complete);
        assert!(response
            .completion_budget_report
            .as_deref()
            .unwrap()
            .starts_with("Goal achieved."));
        // Completing with no goal errors.
        let mut bare = GoalDriver::new();
        let mut other_session = persisted_session();
        let error =
            handle_goal_host_request("goal.complete", &json!({}), &mut bare, &mut other_session)
                .unwrap_err();
        assert_eq!(
            error.to_string(),
            "cannot complete goal because this thread has no goal"
        );
        // Unknown types.
        let error = handle_goal_host_request("goal.nope", &json!({}), &mut driver, &mut session)
            .unwrap_err();
        assert!(error.to_string().contains("unknown goal request type"));
    }
}
