//! The overflow arm of the automatic compaction check: the TS
//! `_checkCompaction` Case 1, the compact-and-retry recovery for a request
//! that exceeds the context window. When a settled turn errors with a
//! provider context-overflow (or a silent/length overflow against the
//! model window), the session drops the error turn from the loop context,
//! runs one compaction, and re-issues the turn on the compacted context
//! without re-adding the user message (TS `agent.continue()`).
//!
//! One recovery attempt per overflow: the retry that still overflows ends
//! the run with the TS failure surface — the durable `compaction_outcome`
//! row plus the `compaction_end` event carrying
//! `Context overflow recovery failed after one compact-and-retry attempt.
//! Try reducing context or switching to a larger-context model.`
//! The state machine also fires before the next admitted prompt: a stale
//! overflow error from the previous run gets its recovery attempt on the
//! freshly admitted prompt (TS `_runPreTurnCompaction` runs the same arm).
//! A new prompt admission or a settled non-error turn resets the state
//! (TS resets `_overflowRecovery` at agent-run message starts and at
//! non-error assistant message ends).

/// The TS failure text when one compact-and-retry attempt could not save
/// the turn (`_checkCompaction`'s reported state).
pub(crate) const OVERFLOW_RECOVERY_FAILED_MESSAGE: &str = "Context overflow recovery failed after one compact-and-retry attempt. Try reducing context or switching to a larger-context model.";

/// One recovery attempt per overflow (TS `_overflowRecovery`): "attempted"
/// marks a compact-and-retry in flight; "reported" dedups the failure
/// notice when the retry overflows too.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum OverflowRecovery {
    #[default]
    Idle,
    Attempted,
    Reported,
}
