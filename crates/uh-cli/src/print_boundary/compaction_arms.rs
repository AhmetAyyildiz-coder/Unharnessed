//! The divider logic: the TS `_checkCompaction` arms of the print
//! runtime's turn boundary — the overflow compact-and-retry machine
//! (Case 1) with its three state enums, and the threshold arm — the
//! child cut of the `print_boundary` facade.

use super::{
    compaction_end_success_event, compaction_start_event, is_context_overflow_failure,
    json_round_trip, CompactOutcome, CompactionOutcomeKind, CompactionOutcomeReason, Model,
    SessionAgentMessage, SessionEngine, TrailingAssistantFilter, TurnBoundary,
};

/// The TS failure text when one compact-and-retry attempt could not save
/// the turn (`_checkCompaction`'s reported state).
pub(super) const OVERFLOW_RECOVERY_FAILED_MESSAGE: &str = "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.";

/// One recovery attempt per overflow (TS `_overflowRecovery`): "attempted"
/// marks a compact-and-retry in flight; "reported" dedups the failure
/// notice when the retry overflows too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) enum OverflowRecovery {
    #[default]
    Idle,
    Attempted,
    Reported,
}

/// Which boundary the arm runs at. The re-issue differs: a settled-turn
/// overflow compaction re-issues the turn (TS `agent.continue()`); a
/// pre-turn one leaves the loop to the admitted prompt, which continues
/// on the compacted context (TS `_runPreTurnCompaction` never re-issues).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverflowBoundary {
    SettledTurn,
    PreTurn,
}

/// What the overflow arm decided for the settled turn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum OverflowOutcome {
    /// No arm fired (not an overflow, or a guard skipped it): the
    /// threshold arm still gets its turn.
    NotApplicable,
    /// The compact-and-retry ran: the turn re-issued on the compacted
    /// context, and the newly settled turn needs the same checks.
    RetryTurn,
    /// The turn is over (a skipped or failed compaction, or the reported
    /// second overflow).
    Finished,
}

impl TurnBoundary {
    /// Reset the overflow recovery state (TS: a message that starts an
    /// agent run — the admitted prompt — and every settled non-error
    /// assistant turn reset `_overflowRecovery`).
    pub(crate) fn reset(&mut self) {
        self.recovery = OverflowRecovery::Idle;
    }

    /// The threshold arm (TS `_checkCompaction` Case 3, after Case 1 stayed
    /// silent): the live context crossing the reserve headroom compacts —
    /// the settled turn's usage at `agent_end`, or the resumed context
    /// before an admitted prompt. Both boundaries share the body (TS
    /// `_runPreTurnCompaction` is the same `_checkCompaction` call; only
    /// the overflow arm's re-issue differs by boundary). The
    /// `compaction_start` / `compaction_end` pair streams in json mode (the
    /// outcome persists in the session entries the headless terminal
    /// result reads in text mode).
    pub(super) async fn threshold_arm(
        &mut self,
        engine: &SessionEngine,
        model: &Model,
        api_key: Option<String>,
    ) -> Result<(), String> {
        if !engine.session.auto_compaction_due(model).await {
            return Ok(());
        }
        self.emit_json(&compaction_start_event(
            CompactionOutcomeReason::Threshold.wire(),
        ));
        match engine.session.compact(None, model, api_key, None).await {
            Ok(CompactOutcome::Ran(run)) => {
                self.emit_ipython_state_row(&run);
                self.emit_json(&compaction_end_success_event(
                    CompactionOutcomeReason::Threshold.wire(),
                    &run,
                    false,
                ));
            }
            Ok(CompactOutcome::Skipped(message)) => {
                self.end_unsuccessfully(
                    engine,
                    CompactionOutcomeReason::Threshold,
                    CompactionOutcomeKind::Skipped,
                    &format!("Auto-compaction skipped: {message}"),
                )
                .await;
            }
            Err(error) => {
                self.end_unsuccessfully(
                    engine,
                    CompactionOutcomeReason::Threshold,
                    CompactionOutcomeKind::Failed,
                    &format!("Auto-compaction failed: {error:#}"),
                )
                .await;
            }
        }
        Ok(())
    }

    /// The shared Case-1 body (TS `_checkCompaction` Case 1). Guard order is
    /// the TS one: a settled non-error turn resets the recovery state, the
    /// message must come from the session's current model, may not predate
    /// the latest compaction boundary, compaction must be enabled, and the
    /// shared overflow classifier must recognize it. On a retry the
    /// settled-turn arm re-issues the turn without a new user message (TS
    /// `agent.continue()`); the pre-turn arm leaves the loop to the
    /// admitted prompt (the boundary the caller passes decides).
    pub(super) async fn overflow_recovery_attempt(
        &mut self,
        engine: &SessionEngine,
        model: &Model,
        api_key: Option<String>,
        boundary: OverflowBoundary,
    ) -> Result<OverflowOutcome, String> {
        let Some(SessionAgentMessage::Assistant(wire)) =
            engine.session.last_assistant_message().await
        else {
            return Ok(OverflowOutcome::NotApplicable);
        };
        // A settled non-error turn resets the recovery state (TS resets at
        // every non-error assistant message end).
        if wire.stop_reason != uh_types::ai::StopReason::Error {
            self.reset();
            return Ok(OverflowOutcome::NotApplicable);
        }
        // Skip the overflow check when the message came from a different
        // model (TS `sameModel`: a model switch must not compact for the
        // old model's overflow).
        if wire.provider != model.provider || wire.model != model.id {
            return Ok(OverflowOutcome::NotApplicable);
        }
        // Skip the check when the message predates the latest compaction
        // boundary (TS `assistantIsFromBeforeCompaction`): a stale
        // pre-compaction overflow must not retrigger.
        if engine
            .session
            .latest_compaction_timestamp()
            .await
            .is_some_and(|timestamp| wire.timestamp <= timestamp)
        {
            return Ok(OverflowOutcome::NotApplicable);
        }
        // Enablement: the compaction settings gate.
        if !engine.session.auto_compaction_enabled() {
            return Ok(OverflowOutcome::NotApplicable);
        }
        // The shared overflow classifier (TS `isContextOverflow`).
        let Some(assistant) = json_round_trip::<_, uh_agent::types::AssistantMessage>(&wire) else {
            return Ok(OverflowOutcome::NotApplicable);
        };
        if !is_context_overflow_failure(&assistant, model.context_window) {
            return Ok(OverflowOutcome::NotApplicable);
        }
        // One recovery attempt per overflow (TS `_overflowRecovery`).
        match self.recovery {
            OverflowRecovery::Attempted => {
                self.recovery = OverflowRecovery::Reported;
                // The retry still overflows: report once — the durable
                // outcome row plus the `compaction_end` failure (no error
                // severity on the wire — TS passes none for the auto arms).
                self.end_unsuccessfully(
                    engine,
                    CompactionOutcomeReason::Overflow,
                    CompactionOutcomeKind::Failed,
                    OVERFLOW_RECOVERY_FAILED_MESSAGE,
                )
                .await;
                return Ok(OverflowOutcome::Finished);
            }
            OverflowRecovery::Reported => return Ok(OverflowOutcome::NotApplicable),
            OverflowRecovery::Idle => self.recovery = OverflowRecovery::Attempted,
        }
        // Remove the error turn from the loop context first (TS: it stays
        // in the session history, but the retry must not re-send it).
        engine
            .session
            .drop_trailing_assistant(TrailingAssistantFilter::Any)
            .await;
        self.emit_json(&compaction_start_event(
            CompactionOutcomeReason::Overflow.wire(),
        ));
        // Headless compactions run unsignaled (TS print-mode compactions
        // have no abort trigger), so no abort race wraps the run.
        let outcome = engine.session.compact(None, model, api_key, None).await;
        match outcome {
            Ok(CompactOutcome::Ran(run)) => {
                self.emit_ipython_state_row(&run);
                // The wire result is the TS `CompactionResult` shape; the
                // end event carries `willRetry: true` (the turn re-issues).
                self.emit_json(&compaction_end_success_event(
                    CompactionOutcomeReason::Overflow.wire(),
                    &run,
                    true,
                ));
                // The compaction rebuild re-adds the error turn from the
                // kept tail: drop it again so the retried request is free
                // of it (TS will-retry branch).
                engine
                    .session
                    .drop_trailing_assistant(TrailingAssistantFilter::ErrorOnly)
                    .await;
                if boundary == OverflowBoundary::PreTurn {
                    // The admitted prompt continues the loop on the
                    // compacted context (TS `_runPreTurnCompaction` never
                    // re-issues; the prompt's own commit is the
                    // continuation).
                    return Ok(OverflowOutcome::Finished);
                }
                // Re-issue the turn without a new user message (TS
                // `agent.continue()`), then hand the newly settled turn
                // back to the boundary checks.
                engine
                    .session
                    .agent()
                    .continue_run()
                    .await
                    .map_err(|error| format!("{error:#}"))?;
                engine.session.agent().wait_for_idle().await;
                Ok(OverflowOutcome::RetryTurn)
            }
            // A skipped overflow recovery does not re-issue (TS excludes
            // overflow from `resumeAfterFailure`).
            Ok(CompactOutcome::Skipped(message)) => {
                self.end_unsuccessfully(
                    engine,
                    CompactionOutcomeReason::Overflow,
                    CompactionOutcomeKind::Skipped,
                    &format!("Auto-compaction skipped: {message}"),
                )
                .await;
                Ok(OverflowOutcome::Finished)
            }
            Err(error) => {
                self.end_unsuccessfully(
                    engine,
                    CompactionOutcomeReason::Overflow,
                    CompactionOutcomeKind::Failed,
                    &format!("Context overflow recovery failed: {error:#}"),
                )
                .await;
                Ok(OverflowOutcome::Finished)
            }
        }
    }
}
