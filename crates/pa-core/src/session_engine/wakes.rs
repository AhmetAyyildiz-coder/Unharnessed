//! Deferred wake-ups of an idle in-process session (`-p` and ACP): a
//! detached `bash()` completion, a quota park's reset, and the goal's
//! no-progress backoff. The daemon worker owned them before
//! (`bash_notices`, the quota-resume and goal-backoff cron jobs); the
//! in-process hosts now drive them through [`SessionEngine::next_wake`].
//!
//! The kernel sends `bash.completed` when a background `bash()` outlives
//! the cell that started it and nobody read its result (`bash.py`). The
//! notice waits here until the host reaches a settled boundary, and a read
//! that reaches the model first (`bash.consumed`) withdraws it. Unlike the
//! daemon, a notice is never steered into a running turn: the host
//! delivers it once the run settles.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use serde_json::Value;
use tokio::sync::Notify;
use tokio::time::Instant;

use super::engine::SessionEngine;
use super::goal_driver::GOAL_BACKOFF_WAKE_MARKER_TEXT;
use super::messages::create_async_bash_completion_message;
use super::provider_park::QUOTA_RESUME_MARKER_TEXT;
use crate::kernel::shared::{host_handler, HostRequestHandlers};

/// What wakes an idle session.
#[derive(Debug, Clone)]
pub enum Wake {
    /// A detached `bash()` finished: the turn runs on this injected
    /// `[bash-done ...]` row (`AgentSession::prompt_injected_message`).
    BashDone(Box<pa_types::session::CustomMessage>),
    /// A timed wake (quota reset, goal backoff): the turn runs on this
    /// marker prompt.
    Marker(&'static str),
}

/// The session's pending wakes; the `bash.*` host handlers and the retry
/// chain's quota park feed it, the host drains it.
#[derive(Default)]
pub struct SessionWakes {
    state: Mutex<WakeState>,
    changed: Notify,
}

#[derive(Default)]
struct WakeState {
    /// Wakes a host took but could not run yet (ACP: a prompt turn held
    /// the session); they come first.
    requeued: std::collections::VecDeque<Wake>,
    /// Undelivered completions, oldest first, with the pid and command
    /// `bash.consumed` matches on (pids are reused across handles).
    notices: Vec<(u32, String, pa_types::session::CustomMessage)>,
    /// The quota park's resume instant (epoch ms).
    quota_resume_at_ms: Option<u64>,
    /// Parks of the current quota episode; a turn that succeeds ends it.
    quota_parks_used: u32,
    /// The goal backoff deadline last seen open; due once it passes.
    goal_wake_at_ms: Option<u64>,
}

impl SessionWakes {
    /// Wake a waiting [`SessionEngine::next_wake`]: something it waits on
    /// may have changed (a stored permit covers a waiter that has not
    /// started waiting yet).
    pub fn poke(&self) {
        self.changed.notify_one();
    }

    /// Wait until [`SessionWakes::poke`] signals a change.
    pub async fn changed(&self) {
        self.changed.notified().await;
    }

    /// Hand back a wake the host took but cannot run now: the next
    /// [`SessionEngine::take_due_wake`] returns it first.
    pub fn requeue(&self, wake: Wake) {
        self.state().requeued.push_front(wake);
    }

    fn state(&self) -> MutexGuard<'_, WakeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// The `bash.completed`/`bash.consumed` kernel host handlers, with the
    /// TS validation: a positive integer pid, a non-empty command, and an
    /// integer exit code.
    pub(crate) fn register_host_handlers(wakes: &Arc<Self>, handlers: &mut HostRequestHandlers) {
        let completed = Arc::clone(wakes);
        handlers.register(
            "bash.completed",
            host_handler(move |payload| {
                let wakes = Arc::clone(&completed);
                async move {
                    let (pid, command) = bash_notice_identity(&payload.data)?;
                    let exit_code = payload
                        .data
                        .get("exitCode")
                        .and_then(Value::as_i64)
                        .ok_or_else(|| {
                            anyhow::anyhow!("bash.completed exitCode must be an integer")
                        })?;
                    let message =
                        create_async_bash_completion_message(pid, &command, exit_code, now_ms());
                    wakes.state().notices.push((pid, command, message));
                    wakes.poke();
                    Ok(serde_json::json!({}))
                }
            }),
        );
        let consumed = Arc::clone(wakes);
        handlers.register(
            "bash.consumed",
            host_handler(move |payload| {
                let wakes = Arc::clone(&consumed);
                async move {
                    let (pid, command) = bash_notice_identity(&payload.data)?;
                    wakes
                        .state()
                        .notices
                        .retain(|(notice_pid, notice_command, _)| {
                            *notice_pid != pid || *notice_command != command
                        });
                    Ok(serde_json::json!({}))
                }
            }),
        );
    }

    /// Park the session until `resume_at_ms`: the quota resume marker
    /// becomes due then.
    pub(crate) fn park_quota(&self, resume_at_ms: u64) {
        {
            let mut state = self.state();
            state.quota_resume_at_ms = Some(resume_at_ms);
            state.quota_parks_used += 1;
        }
        self.poke();
    }

    pub(crate) fn quota_parks_used(&self) -> u32 {
        self.state().quota_parks_used
    }

    /// A turn succeeded: the quota episode (and its park budget) ends.
    pub(crate) fn end_quota_episode(&self) {
        self.state().quota_parks_used = 0;
    }
}

fn bash_notice_identity(data: &Value) -> anyhow::Result<(u32, String)> {
    let pid = data
        .get("pid")
        .and_then(Value::as_u64)
        .filter(|pid| *pid > 0)
        .and_then(|pid| u32::try_from(pid).ok())
        .ok_or_else(|| anyhow::anyhow!("bash.completed pid must be a positive integer"))?;
    let command = data
        .get("command")
        .and_then(Value::as_str)
        .filter(|command| !command.is_empty())
        .ok_or_else(|| anyhow::anyhow!("bash.completed command must be a non-empty string"))?
        .to_string();
    Ok((pid, command))
}

fn now_ms() -> u64 {
    crate::autonomous::now_millis()
}

impl SessionEngine {
    /// The session's pending wakes.
    #[must_use]
    pub fn wakes(&self) -> &Arc<SessionWakes> {
        &self.wakes
    }

    /// The next wake due now, if any: a requeued wake first, then a queued
    /// `bash()` completion, a quota resume, and the goal backoff (dropped
    /// when the goal is no longer active).
    pub async fn take_due_wake(&self) -> Option<Wake> {
        let (goal_window, goal_active) = {
            let driver = self.goal_driver.lock().await;
            (driver.backoff_wake_at(), driver.owns_continuation_wakeup())
        };
        let now = now_ms();
        let mut state = self.wakes.state();
        if let Some(wake) = state.requeued.pop_front() {
            return Some(wake);
        }
        match goal_window {
            Some(at) => state.goal_wake_at_ms = Some(at),
            // The window closed early (a turn made progress): no wake.
            None if state.goal_wake_at_ms.is_some_and(|at| at > now) => {
                state.goal_wake_at_ms = None;
            }
            None => {}
        }
        if !state.notices.is_empty() {
            return Some(Wake::BashDone(Box::new(state.notices.remove(0).2)));
        }
        if state.quota_resume_at_ms.is_some_and(|at| at <= now) {
            state.quota_resume_at_ms = None;
            return Some(Wake::Marker(QUOTA_RESUME_MARKER_TEXT));
        }
        if state.goal_wake_at_ms.is_some_and(|at| at <= now) {
            state.goal_wake_at_ms = None;
            if goal_active {
                return Some(Wake::Marker(GOAL_BACKOFF_WAKE_MARKER_TEXT));
            }
        }
        None
    }

    /// Wait for the next wake.
    ///
    /// With a `limit` (`-p`), the wait returns `None` once the limit passes,
    /// and at once when nothing can wake the session any more: no queued
    /// notice, no live background `bash()`, no quota park, no open goal
    /// backoff. Without one (ACP) it never gives up and sleeps until
    /// something changes.
    pub async fn next_wake(&self, limit: Option<Duration>) -> Option<Wake> {
        let give_up_at = limit.map(|limit| Instant::now() + limit);
        loop {
            if let Some(wake) = self.take_due_wake().await {
                return Some(wake);
            }
            let deadline_ms = {
                let state = self.wakes.state();
                match (state.quota_resume_at_ms, state.goal_wake_at_ms) {
                    (Some(quota), Some(goal)) => Some(quota.min(goal)),
                    (quota, goal) => quota.or(goal),
                }
            };
            let background = self
                .provisioner
                .manager()
                .is_some_and(|manager| manager.has_background_work());
            if give_up_at.is_some() && deadline_ms.is_none() && !background {
                return None;
            }
            let deadline = deadline_ms
                .map(|at| Instant::now() + Duration::from_millis(at.saturating_sub(now_ms())));
            let sleep_until = match (give_up_at, deadline) {
                (Some(give_up), Some(deadline)) => Some(give_up.min(deadline)),
                (give_up, deadline) => give_up.or(deadline),
            };
            match sleep_until {
                Some(at) => {
                    tokio::select! {
                        () = self.wakes.changed.notified() => {}
                        () = tokio::time::sleep_until(at) => {}
                    }
                }
                None => self.wakes.changed.notified().await,
            }
            if give_up_at.is_some_and(|at| Instant::now() >= at) {
                return self.take_due_wake().await;
            }
        }
    }
}
