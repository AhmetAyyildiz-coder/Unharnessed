//! The login flows' request channel to whatever surface hosts them (moved
//! from the removed pa-tui crate's inline auth panel; its rendering went
//! with the TUI). The host is now the ACP mode (`acp::providers`), which
//! relays each request to the client as a `_unharnessed/providers/login_update`
//! notification. A flow runs in the background and drives the host
//! through [`AuthPanelHandle`] — progress lines, the browser URL block, a
//! paste prompt, or the Prime team picker. Every prompt carries its own
//! oneshot reply, and a flow completion is a request too (the `Settled`
//! variants).
//!
//! Cancellation is cooperative: every handle shares one cancel flag. The
//! host marks it when its surface exits, and a running flow checks it
//! between its poll steps and before its credential writes (a
//! `JoinHandle::abort` cannot reach a started blocking login body).

use tokio::sync::{mpsc, oneshot};

use crate::provider_auth::ProviderAuthOutcome;

/// One Prime team option (TS `PrimeTeam` as the selector renders it).
/// `created_at` is carry-through metadata the flow stores with the
/// selection (the picker never renders it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrimeTeamOption {
    pub team_id: String,
    pub name: String,
    pub slug: Option<String>,
    pub role: Option<String>,
    pub created_at: Option<String>,
}

/// The team picker's answer (TS `PrimeTeamSelectorComponent`'s
/// `onSelect`/`onCancel`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrimeTeamPick {
    /// TS `onSelect(team)`.
    Team(PrimeTeamOption),
    /// TS `onSelect(null)`: the personal account.
    PersonalAccount,
    /// TS `onCancel`: the stored selection stays untouched.
    Cancelled,
}

/// How the paste field renders its value (TS `MenuSearchInput`'s masked
/// mode: the token paste panel renders bullets, the login dialog
/// renders the typed key).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteStyle {
    /// The typed value renders as typed (TS `LoginDialogComponent`).
    Visible,
    /// The typed value renders as bullets (TS
    /// `McpTokenPastePanelComponent`: a rendered line never contains the
    /// secret).
    Masked,
}

/// The paste prompt's rendering tone: TS `showManualInput` renders its
/// prompt muted (the browser-step hint under the URL block), while
/// `showPrompt` renders it as a section title in text colour (the API-key
/// prompt "Enter API key:").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PastePromptTone {
    /// TS `addMutedText(prompt)` — `showManualInput`'s arm-prompt.
    Muted,
    /// TS `addSectionTitle(message)` — `showPrompt`'s "Enter API key:".
    Text,
}

/// One request a login flow (or its session wrapper) sends to the inline
/// auth panel. Fire-and-forget requests render; the prompt and picker
/// requests await their oneshot replies; the settled requests close the
/// panel and apply the outcome.
pub enum AuthPanelRequest {
    /// TS `dialog.showProgress`: a muted progress line joins the panel.
    /// `chatter` marks the line as the `onProgress` callback's step
    /// chatter (TS `runPrimeInferenceLogin`'s guarded arm): the
    /// onboarding surface drops it — "onboarding narrates itself; step
    /// chatter stays in the chat flows" (TS `if (!this.isOnboarding())`)
    /// — while a direct `showProgress` line (the browser-fallback arm,
    /// the OAuth dialogs' chatter) renders on every surface.
    Progress { message: String, chatter: bool },
    /// TS `dialog.showWaiting`: the polling device flow's waiting line
    /// (the Copilot browser authentication pend) — the accent row that
    /// joins above the actions row. It renders on every surface: the
    /// dialog's own method carries no onboarding guard, unlike the
    /// `onProgress` chatter arm.
    Waiting { message: String },
    /// TS `dialog.showAuth`: the browser URL block. The flow does not open
    /// the browser: the host does when the user asks (the ACP
    /// `_unharnessed/providers/login_open`).
    AuthUrl {
        url: String,
        /// The provider instructions; `None` renders TS's default
        /// "Complete the sign-in in your browser." line.
        instructions: Option<String>,
    },
    /// TS `dialog.showManualInput` / `armManualInput` (the muted prompt)
    /// and `dialog.showPrompt` (the section-title prompt, TS text
    /// colour): the prompt above the panel's paste field. Enter submits
    /// the trimmed value (a blank submit resolves when the prompt allows
    /// it — TS `OAuthPrompt.allowEmpty` — else the field stays mounted:
    /// the token panel shows its notice, the login dialog waits
    /// silently); Esc cancels the flow (`None`).
    PastePrompt {
        prompt: String,
        tone: PastePromptTone,
        style: PasteStyle,
        allow_empty: bool,
        reply: oneshot::Sender<Option<String>>,
    },
    /// TS `PrimeTeamSelectorComponent` mounts over the panel: Esc
    /// cancels the selection (the stored selection stays).
    SelectTeam {
        teams: Vec<PrimeTeamOption>,
        /// TS `currentTeamId`: the stored selection's team id; `None`
        /// marks the personal account current.
        current: Option<String>,
        reply: oneshot::Sender<PrimeTeamPick>,
    },
    /// A provider login settled (`/login`'s rows): the outcome row
    /// applies and the panel unmounts. `provider` is the row's provider
    /// id (the model-picker sign-in route keys its parked retry on it).
    ProviderSettled {
        provider: String,
        outcome: ProviderAuthOutcome,
    },
    /// A `/mcp` view auth command settled: its status line applies.
    McpSettled { note: String },
}

/// A login flow's cooperative cancel signal: the flag the blocking body
/// polls before its auth-store writes, and the watch that wakes every
/// pending panel prompt (a prompt's answer can only come from the pane
/// that is exiting, so a pending prompt waits on the watch instead of
/// hanging the exit — the watch keeps the marked value, so a mark that
/// races a wait is never lost).
#[derive(Clone)]
pub struct FlowCancel {
    flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
    wake: std::sync::Arc<tokio::sync::watch::Sender<bool>>,
}

impl std::fmt::Debug for FlowCancel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FlowCancel").finish()
    }
}

impl FlowCancel {
    /// The signal starts live (nothing cancelled it yet).
    fn new() -> Self {
        FlowCancel {
            flag: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
            wake: std::sync::Arc::new(tokio::sync::watch::channel(false).0),
        }
    }

    /// `true` once the driving pane exited.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.flag.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// The bare flag's storage (the #2790 panel consumers load it
    /// directly; every `mark` is visible through it).
    pub(crate) fn flag_arc(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        std::sync::Arc::clone(&self.flag)
    }

    /// The pane exits: mark the flow cancelled and wake every prompt
    /// that is waiting for an answer the exited pane can no longer give.
    pub(crate) fn mark(&self) {
        self.flag.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.wake.send(true);
    }

    /// Wait until the flow's cancel signal fires.
    pub async fn wait(&self) {
        let mut marked = self.wake.subscribe();
        // A mark that landed before the subscribe is already visible in
        // `cancelled`; a mark after it fires `changed`.
        while !self.cancelled() {
            if marked.changed().await.is_err() {
                return;
            }
        }
    }
}

/// The flow-side handle to the inline auth panel: one login run's
/// request channel. The composition root drives its flow against this
/// handle; the host (the ACP mode) owns the receiving side and services every
/// request. Cheap to clone; a clone shares the run's channel and the
/// run's cancel signal.
#[derive(Clone)]
pub struct AuthPanelHandle {
    tx: mpsc::UnboundedSender<AuthPanelRequest>,
    /// The flow's cooperative cancel signal: the driving surface marks
    /// it when the panel or pane exits, and a blocking login body
    /// checks it before its auth-store writes — a `JoinHandle::abort`
    /// cannot reach a started `spawn_blocking` closure (#2770).
    cancel: FlowCancel,
}

impl AuthPanelHandle {
    /// Build the handle over one run's request channel (the session
    /// creates the pair; the loop owns the receiver).
    #[must_use]
    pub fn new(tx: mpsc::UnboundedSender<AuthPanelRequest>) -> Self {
        AuthPanelHandle {
            tx,
            cancel: FlowCancel::new(),
        }
    }

    /// The flow's cancel state: `true` once the driving surface exited.
    #[must_use]
    pub fn cancelled(&self) -> bool {
        self.cancel.cancelled()
    }

    /// The flow's cancel signal for the driving side (the pane marks it
    /// on exit; every handle clone shares it).
    #[must_use]
    pub fn cancel_signal(&self) -> FlowCancel {
        self.cancel.clone()
    }

    /// The bare cancel flag (the #2790 codex login's shape): the
    /// same storage the [`FlowCancel`] arms — loads observe every mark.
    #[must_use]
    pub fn cancel_flag(&self) -> std::sync::Arc<std::sync::atomic::AtomicBool> {
        self.cancel.flag_arc()
    }

    /// Submit one request directly (the helpers below and the session's
    /// settled notes all funnel here).
    pub fn send(&self, request: AuthPanelRequest) {
        let _ = self.tx.send(request);
    }

    /// TS the `onProgress` callback's step chatter (TS `dialog.showProgress`
    /// behind the `if (!this.isOnboarding())` guard): the onboarding
    /// surface drops the line — the flow narrates itself there.
    pub fn progress(&self, message: impl Into<String>) {
        self.send(AuthPanelRequest::Progress {
            message: message.into(),
            chatter: true,
        });
    }

    /// TS a direct `dialog.showProgress` line (the browser-sign-in
    /// fallback arm, the OAuth dialogs' chatter): renders on every
    /// surface, onboarding included.
    pub fn progress_line(&self, message: impl Into<String>) {
        self.send(AuthPanelRequest::Progress {
            message: message.into(),
            chatter: false,
        });
    }

    /// TS `dialog.showWaiting`: the polling device flow's waiting line
    /// — the accent row above the actions row, rendered on every
    /// surface (onboarding included; the dialog's own method, never the
    /// `onProgress` chatter arm the onboarding block drops).
    pub fn waiting(&self, message: impl Into<String>) {
        self.send(AuthPanelRequest::Waiting {
            message: message.into(),
        });
    }

    /// TS `dialog.showAuth`.
    pub fn auth_url(&self, url: &str, instructions: Option<&str>) {
        self.send(AuthPanelRequest::AuthUrl {
            url: url.to_string(),
            instructions: instructions.map(str::to_string),
        });
    }

    /// TS `dialog.showManualInput` / `armManualInput` (the muted
    /// browser-step prompt) and `dialog.showPrompt` (the text-coloured
    /// section-title prompt): prompt above the paste field; the
    /// submitted value resolves the future, a cancel answers `None`.
    pub async fn paste_prompt(
        &self,
        prompt: &str,
        tone: PastePromptTone,
        style: PasteStyle,
    ) -> Option<String> {
        self.paste_prompt_with(prompt, tone, style, false).await
    }

    /// The `allow_empty` variant (TS `OAuthPrompt.allowEmpty`): a blank
    /// submit resolves as an empty answer instead of the notice (the
    /// Copilot domain prompt's "blank for github.com").
    pub async fn paste_prompt_allow_empty(
        &self,
        prompt: &str,
        tone: PastePromptTone,
        style: PasteStyle,
    ) -> Option<String> {
        self.paste_prompt_with(prompt, tone, style, true).await
    }

    /// One paste prompt over the request channel (the two surfaces
    /// above funnel here).
    async fn paste_prompt_with(
        &self,
        prompt: &str,
        tone: PastePromptTone,
        style: PasteStyle,
        allow_empty: bool,
    ) -> Option<String> {
        // An exited pane can never answer the prompt: a cancelled flow
        // returns without sending (the blocking body's next
        // `cancelled` check reports the cancellation).
        if self.cancelled() {
            return None;
        }
        let (reply, answer) = oneshot::channel();
        self.send(AuthPanelRequest::PastePrompt {
            prompt: prompt.to_string(),
            tone,
            style,
            allow_empty,
            reply,
        });
        tokio::select! {
            answered = answer => answered.unwrap_or(None),
            () = self.cancelled_wait() => None,
        }
    }

    /// TS `PrimeTeamSelectorComponent`: the team picker; a cancel
    /// answers [`PrimeTeamPick::Cancelled`] (the stored selection
    /// stays).
    pub async fn select_team(
        &self,
        teams: Vec<PrimeTeamOption>,
        current: Option<&str>,
    ) -> PrimeTeamPick {
        // An exited pane can never answer the picker: a cancelled flow
        // returns the cancelled pick (the stored selection stays).
        if self.cancelled() {
            return PrimeTeamPick::Cancelled;
        }
        let (reply, answer) = oneshot::channel();
        self.send(AuthPanelRequest::SelectTeam {
            teams,
            current: current.map(str::to_string),
            reply,
        });
        tokio::select! {
            picked = answer => picked.unwrap_or(PrimeTeamPick::Cancelled),
            () = self.cancelled_wait() => PrimeTeamPick::Cancelled,
        }
    }

    /// Wait until the flow's cancel signal fires (the pane exited).
    async fn cancelled_wait(&self) {
        self.cancel.wait().await;
    }
}

impl std::fmt::Debug for AuthPanelHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AuthPanelHandle").finish()
    }
}
