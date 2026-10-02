//! Provider auth management (`/login`, `/logout`) as a command contract a
//! frontend drives (TS `ProviderAuthFlows`: `getLoginProviderOptions` /
//! `getLogoutProviderOptions` / the login flows / `runLogout`). Moved from
//! the removed pa-tui crate, whose provider selector rendered these rows.
//! The ACP `_unharnessed/providers/*` methods drive it (`acp::providers`);
//! the implementation lives in uh-cli (`provider_login`), the composition
//! root, which hands it to the ACP mode through `AcpOptions`.

use std::pin::Pin;

/// The credential type a provider row logs in with (TS `authType`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthType {
    Oauth,
    ApiKey,
}

impl AuthType {
    /// The row's auth label (TS `authLabel`).
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            AuthType::Oauth => "subscription",
            AuthType::ApiKey => "api key",
        }
    }
}

/// How the login runs: the frontend prompts for the key, or the
/// composition root runs the provider's flow against the inline auth
/// panel (TS splits the same way: `showApiKeyLoginDialog` vs the
/// OAuth/Prime/Bedrock login dialogs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthFlow {
    /// Prompt for the key in the panel (TS `showPrompt("Enter API key:")`).
    ApiKeyPrompt,
    /// Run the flow through the inline auth panel (browser OAuth, the
    /// Prime login, the MCP device flow).
    TerminalFlow,
}

/// The status indicator of one row (TS `formatStatusIndicator`: the label
/// plus its theme color).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthStatusIndicator {
    pub style: AuthStatusStyle,
    pub label: String,
}

/// The status label's color (TS `theme.fg` kinds).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AuthStatusStyle {
    Success,
    Warning,
    Muted,
}

/// One provider row (TS `AuthSelectorProvider` plus its rendered
/// status).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderRow {
    pub id: String,
    pub name: String,
    pub auth_type: AuthType,
    /// The row's status indicator; `None` hides the trailing meta (TS's
    /// unconfigured non-stale inline case).
    pub status: Option<AuthStatusIndicator>,
    /// The login flow the row runs.
    pub flow: AuthFlow,
    /// Whether a usable credential exists (TS
    /// `getProviderAuthStatus(id).configured`): the onboarding picker's
    /// connected check, distinct from the display indicator.
    pub configured: bool,
    /// Whether this build carries the row's login flow (the codex
    /// subscription row does; the not-yet-ported subscription providers
    /// do not). An unavailable row renders dimmed with the "not
    /// available" annotation and Enter is inert — the menu states the
    /// dead-end BEFORE selection instead of error-walling after it.
    pub available: bool,
}

/// The outcome of one login/logout flow: the status row to show, the
/// error row, or a silent cancel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProviderAuthOutcome {
    Status(String),
    Error(String),
    /// The flow was cancelled (TS `AuthenticationResult`'s `cancelled`
    /// state): silent — no status row, no error row.
    Cancelled,
}

/// The boxed-future shape of the hook's methods.
pub type ProviderRowsFuture = Pin<Box<dyn std::future::Future<Output = Vec<ProviderRow>> + Send>>;
pub type ProviderAuthFuture =
    Pin<Box<dyn std::future::Future<Output = ProviderAuthOutcome> + Send>>;

/// `/login` + `/logout` provider auth, implemented by the composition
/// root (credential storage, OAuth flows, and the provider catalog stay
/// above this crate).
pub trait ProviderAuthCommands: Send + Sync {
    /// TS `getLoginProviderOptions`: the provider rows sorted TS-style
    /// (configured first, prime-inference first among them, oauth before
    /// api key, then by name).
    fn login_options(&self) -> ProviderRowsFuture;
    /// TS `getLogoutProviderOptions`: one row per stored credential,
    /// sorted by name.
    fn logout_options(&self) -> ProviderRowsFuture;
    /// TS `loginProvider`: store the key for `ApiKeyPrompt` rows. The
    /// unavailable rows never reach this (the menu marks them before
    /// selection); an OAuth row arriving here answers the silent
    /// cancel, never an error wall.
    fn login(&self, provider: &ProviderRow, api_key: Option<&str>) -> ProviderAuthFuture;
    /// TS `loginProvider` for the panel-driven flows (the MCP OAuth
    /// login, the Prime Inference login): the frontend hosts the auth
    /// panel channel ([`crate::auth_panel`]) and services the flow's requests
    /// while it runs in the background; the future settles the flow's
    /// outcome (the session sends it back through the panel channel).
    fn login_on_panel(
        &self,
        provider: &ProviderRow,
        panel: crate::auth_panel::AuthPanelHandle,
    ) -> ProviderAuthFuture;
    /// TS `runLogout`: remove the stored credential.
    fn logout(&self, provider: &ProviderRow) -> ProviderAuthFuture;
    /// TS `getAnthropicSubscriptionAuthWarning`: the composition root
    /// reports the subscription-auth warning text when the stored
    /// Anthropic credential is an OAuth login or the resolved key is a
    /// subscription token (`sk-ant-oat...`); `None` when it is not.
    fn anthropic_subscription_warning(&self) -> ProviderWarningFuture;
}

/// The boxed-future shape of the subscription-auth warning lookup.
pub type ProviderWarningFuture =
    Pin<Box<dyn std::future::Future<Output = Option<&'static str>> + Send>>;
