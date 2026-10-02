//! Namespaced `_meta` payloads for unharnessed capabilities that ACP has no
//! native concept for (cwd reporting, quiescence observation, correlation).
//!
//! ACP reserves `_meta` on capability objects, notifications, and content
//! blocks so agents can carry non-standard data. Vanilla ACP clients ignore
//! these keys; an unharnessed-aware client reads them. Non-standard fields
//! never appear at an ACP object root.

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

/// The namespace key of every unharnessed `_meta` payload.
pub const PRIME_AGENT_META_NAMESPACE: &str = "unharnessed";

/// A client-requested cwd that differs from the agent's actual startup cwd.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnharnessedCwdMeta {
    pub requested: String,
    pub actual: String,
}

/// Observed autonomous-continuation count at a completion point.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnharnessedQuiescenceMeta {
    pub remaining_autonomous_continuations: u64,
}

/// Producer-side ordering of an update inside its prompt turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnharnessedEventPhase {
    /// Ordinary streamed work.
    #[serde(rename = "event")]
    Event,
    /// The correlated boundary in front of a prompt response.
    #[serde(rename = "responseBoundary")]
    ResponseBoundary,
    /// The final settled state after the response boundary.
    #[serde(rename = "terminalQuiescence")]
    TerminalQuiescence,
}

/// The outcome carried by a response boundary and terminal envelope. ACP
/// transport stop reasons (including `end_turn`) are never a causal
/// completion signal, so this is deliberately only `result` and `error`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UnharnessedOutcome {
    #[serde(rename = "result")]
    Result,
    #[serde(rename = "error")]
    Error,
}

/// The unharnessed payload under the `_meta` namespace key.
///
/// `prompt_turn_id` is allocated when ACP accepts a prompt, never inferred
/// from whichever prompt happens to be running when an update is delivered;
/// `0` means a session-scoped event with no prompt origin. `event_sequence`
/// is connection-wide and strictly increases for every published update.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnharnessedSessionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub prompt_turn_id: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event_sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<UnharnessedEventPhase>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outcome: Option<UnharnessedOutcome>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub terminal_quiescence_expected: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cwd: Option<UnharnessedCwdMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quiescence: Option<UnharnessedQuiescenceMeta>,
    /// Rich kernel output reported by the ipython tool (attachments the cell
    /// loaded into context, plus the number of diffs it displayed).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ipython: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub goal: Option<UnharnessedGoalMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compaction: Option<UnharnessedCompactionMeta>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub autonomous: Option<UnharnessedAutonomousMeta>,
}

/// A goal's live state, surfaced after `/goal` commands and driver turns.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnharnessedGoalMeta {
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub objective: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<u64>,
}

/// One compaction that ran during the session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnharnessedCompactionMeta {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tokens_before: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
}

/// Autonomous-mode accounting surfaced with a turn's completion.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UnharnessedAutonomousMeta {
    pub enabled: bool,
    pub continuations_used: u64,
    pub turns_used: u64,
    pub tokens_used: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_attempt: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate_failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub limit_reason: Option<String>,
}

/// The `_meta.autonomous` accounting for a completion update: per-run usage
/// plus the latest gate attempt and failure (TS `autonomousMeta` in
/// acp-mode.ts). Shared by the in-process and daemon-attached settlements.
pub fn autonomous_meta(
    status: &uh_core::autonomous::AgentAutonomousStatus,
) -> UnharnessedAutonomousMeta {
    let gate_attempt = std::iter::once(
        status
            .last_gate_failure
            .as_ref()
            .map_or(0, |failure| failure.attempt),
    )
    .chain(status.gate_attempts.values().copied())
    .max()
    .unwrap_or(0);
    UnharnessedAutonomousMeta {
        enabled: status.enabled,
        continuations_used: status.continuations_used,
        turns_used: status.turns_used,
        tokens_used: status.tokens_used,
        gate_attempt: (gate_attempt > 0).then_some(gate_attempt),
        gate_failure: status
            .last_gate_failure
            .as_ref()
            .map(|failure| failure.exit_text.clone()),
        limit_reason: None,
    }
}

/// Wrap an unharnessed payload in its namespaced `_meta` envelope.
pub fn unharnessed_meta(payload: &UnharnessedSessionMeta) -> Value {
    json!({ PRIME_AGENT_META_NAMESPACE: payload })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn meta_wraps_under_the_namespace_key() {
        let wrapped = unharnessed_meta(&UnharnessedSessionMeta {
            prompt_turn_id: Some(1),
            event_sequence: Some(2),
            phase: Some(UnharnessedEventPhase::ResponseBoundary),
            outcome: Some(UnharnessedOutcome::Error),
            ..Default::default()
        });
        assert_eq!(
            wrapped,
            json!({ "unharnessed": {
                "promptTurnId": 1,
                "eventSequence": 2,
                "phase": "responseBoundary",
                "outcome": "error",
            }})
        );
    }

    #[test]
    fn quiescence_serializes_camel_case() {
        let wrapped = unharnessed_meta(&UnharnessedSessionMeta {
            quiescence: Some(UnharnessedQuiescenceMeta {
                remaining_autonomous_continuations: 0,
            }),
            ..Default::default()
        });
        assert_eq!(
            wrapped[PRIME_AGENT_META_NAMESPACE]["quiescence"],
            json!({ "remainingAutonomousContinuations": 0 })
        );
    }
}
