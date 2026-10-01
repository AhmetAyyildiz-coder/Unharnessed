//! Session supervisor, worker processes, and wire protocol for Prime Agent.
//!
//! Ported from the TypeScript daemon: `modes/daemon/*`, `modes/session-worker/*`,
//! `core/session-manager.ts`, and `core/session-lease.ts`. The supervisor hosts
//! no sessions: it spawns one worker process per active session, supervises
//! restarts with backoff, and routes clients. Sessions persist as append-only
//! JSONL under `<agent-dir>/sessions/` using the same layout as the TS product.
// Pedantic-gate dispositions (fleet-uniform ruling; see this lane's PR for
// the full rationale).
// Stack-resident futures by design on the daemon's hot paths; boxing the
// call sites for a lint tick is a perf regression with zero correctness gain.
#![allow(clippy::large_futures)]
// 64-bit-only targets; the narrowing casts sit at OS boundaries
// (pid/fd/time/size) where the values are bounded by the kernel - the
// dead-guard expect()s would add panic paths where silent wrap was
// deliberate.
#![allow(
    clippy::cast_possible_truncation,
    clippy::cast_possible_wrap,
    clippy::cast_sign_loss,
    clippy::cast_precision_loss
)]
// The fn-length threshold is a style gate, not correctness; the structure
// campaign owns the god-fn splits as a follow-up.
#![allow(clippy::too_many_lines)]
// API-shape opinions, not defects; the surfaces are deliberate.
#![allow(
    clippy::unnecessary_wraps,
    clippy::zero_sized_map_values,
    clippy::struct_excessive_bools,
    clippy::struct_field_names
)]
pub mod acp;
pub mod hold_refusal;
pub mod lease;
pub(crate) mod overflow_compaction;
pub(crate) mod paths;
pub(crate) mod util;

/// The faux provider registry is process-global: every unit test that
/// drives it holds this lock.
#[cfg(test)]
pub(crate) static FAUX_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
