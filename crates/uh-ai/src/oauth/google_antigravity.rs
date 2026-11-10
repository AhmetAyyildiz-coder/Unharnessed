//! The Google Antigravity subscription login (Google AI Pro/Ultra):
//! the PKCE authorization request against Antigravity's client
//! registration, the localhost callback raced against the manual
//! paste, the token exchange and refresh, and the Cloud Code Assist
//! project the account's requests bill to (found with
//! `loadCodeAssist`, created with `onboardUser` for an account that
//! has none yet). The credentials persist under the provider id
//! `google-antigravity`; the project rides them, since every request
//! names it.

use std::time::Duration;

use serde_json::{json, Map, Value};

use super::anthropic::wait_for_code;
use super::anthropic_callback::{AnthropicCallbackServer, CallbackRoute};
use super::pkce::generate_pkce;
use super::provider_http::{
    ProviderHttp, ProviderHttpMethod, ProviderHttpRequest, ProviderHttpResponse,
};
use super::types::OAuthLoginUi;
use crate::client_versions::Client;

/// Antigravity's installed-app registration.
const CLIENT_ID: &str = "1071006060591-tmhssin2h21lcre235vtolojh4g403ep.apps.googleusercontent.com";
const CLIENT_SECRET: &str = "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";
const AUTHORIZE_URL: &str = "https://accounts.google.com/o/oauth2/v2/auth";
const TOKEN_URL: &str = "https://oauth2.googleapis.com/token";
const SCOPES: &str = "https://www.googleapis.com/auth/cloud-platform https://www.googleapis.com/auth/userinfo.email https://www.googleapis.com/auth/userinfo.profile https://www.googleapis.com/auth/cclog https://www.googleapis.com/auth/experimentsandconfigs";
/// The registration's redirect: the port is fixed.
const ROUTE: CallbackRoute = CallbackRoute {
    port: 51_121,
    path: "/oauth-callback",
    provider: "Google",
};
const REDIRECT_URI: &str = "http://localhost:51121/oauth-callback";

pub(crate) const PROD_ENDPOINT: &str = "https://cloudcode-pa.googleapis.com";
pub(crate) const DAILY_ENDPOINT: &str = "https://daily-cloudcode-pa.googleapis.com";
const DAILY_SANDBOX_ENDPOINT: &str = "https://daily-cloudcode-pa.sandbox.googleapis.com";
const AUTOPUSH_SANDBOX_ENDPOINT: &str = "https://autopush-cloudcode-pa.sandbox.googleapis.com";
/// The project requests name when the account's own cannot be found.
const FALLBACK_PROJECT_ID: &str = "rising-fact-p41fc";
const TOKEN_TIMEOUT_MS: u64 = 30_000;
/// The other agents on the same auth store wait for a refresh.
const REFRESH_TIMEOUT_MS: u64 = 8_000;
const EXPIRY_SKEW_MS: i64 = 5 * 60 * 1000;
const ONBOARD_POLL_INTERVAL: Duration = Duration::from_secs(2);
const ONBOARD_POLLS: usize = 30;
const AUTH_INSTRUCTIONS: &str =
    "Complete login in your browser. If the browser is on another machine, paste the final redirect URL here.";
/// The cancel error the driving surface maps to the silent cancelled
/// outcome.
pub const LOGIN_CANCELLED: &str = "Login cancelled";

/// The credentials the flow returns and persists.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GoogleAntigravityCredentials {
    pub access: String,
    pub refresh: String,
    /// Wall-clock epoch milliseconds, five minutes early.
    pub expires: i64,
    /// The Cloud Code Assist project the requests name.
    pub project_id: String,
}

/// The request key of a stored login: the provider needs the project
/// beside the token.
#[must_use]
pub fn google_antigravity_api_key(access: &str, project_id: &str) -> String {
    json!({ "token": access, "projectId": project_id }).to_string()
}

/// The token and project of a [`google_antigravity_api_key`].
pub(crate) fn parse_google_antigravity_api_key(api_key: &str) -> Option<(String, String)> {
    let key: Value = serde_json::from_str(api_key).ok()?;
    let field = |name: &str| {
        key.get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    Some((field("token")?, field("projectId")?))
}

/// The `User-Agent` the backend expects from Antigravity.
pub(crate) fn antigravity_user_agent() -> String {
    format!("antigravity/{} darwin/arm64", Client::Antigravity.version())
}

/// Run the login: present the sign-in URL, wait for the browser
/// redirect or the pasted one, exchange the code, and find the
/// account's project.
///
/// # Errors
///
/// Returns an error when the callback port cannot be bound, the
/// surface cancelled the login ([`LOGIN_CANCELLED`]), the redirect's
/// state mismatches, or the token exchange fails.
pub async fn login_google_antigravity(
    http: &dyn ProviderHttp,
    ui: &dyn OAuthLoginUi,
) -> Result<GoogleAntigravityCredentials, String> {
    if ui.is_cancelled() {
        return Err(LOGIN_CANCELLED.to_string());
    }
    let (verifier, challenge) = generate_pkce();
    let (state, _) = generate_pkce();
    let server = AnthropicCallbackServer::start_on(ROUTE, &state)?;
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("response_type", "code"),
            ("client_id", CLIENT_ID),
            ("redirect_uri", REDIRECT_URI),
            ("scope", SCOPES),
            ("code_challenge", challenge.as_str()),
            ("code_challenge_method", "S256"),
            ("access_type", "offline"),
            ("prompt", "consent"),
            ("state", state.as_str()),
        ])
        .finish();
    ui.on_auth(&format!("{AUTHORIZE_URL}?{query}"), Some(AUTH_INSTRUCTIONS));

    let code = wait_for_code(&server, ui, &state, REDIRECT_URI).await?;
    if ui.is_cancelled() {
        return Err(LOGIN_CANCELLED.to_string());
    }
    ui.on_progress("Exchanging authorization code for tokens...");
    let token = token_request(
        http,
        &[
            ("grant_type", "authorization_code"),
            ("code", &code.code),
            ("redirect_uri", REDIRECT_URI),
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("code_verifier", &verifier),
        ],
        "Google token exchange",
        TOKEN_TIMEOUT_MS,
    )
    .await?;
    let access = token
        .get("access_token")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let project_id = discover_project(http, ui, access).await?;
    credentials_from(&token, None, project_id)
}

/// Refresh an expired credential; Google keeps the refresh token, so
/// the stored one stays when the answer carries none.
///
/// # Errors
///
/// Returns an error when the token refresh fails.
pub async fn refresh_google_antigravity_token(
    http: &dyn ProviderHttp,
    refresh_token: &str,
    project_id: &str,
) -> Result<GoogleAntigravityCredentials, String> {
    let token = token_request(
        http,
        &[
            ("grant_type", "refresh_token"),
            ("client_id", CLIENT_ID),
            ("client_secret", CLIENT_SECRET),
            ("refresh_token", refresh_token),
        ],
        "Google token refresh",
        REFRESH_TIMEOUT_MS,
    )
    .await?;
    credentials_from(&token, Some(refresh_token), project_id.to_string())
}

async fn token_request(
    http: &dyn ProviderHttp,
    fields: &[(&str, &str)],
    label: &str,
    timeout_ms: u64,
) -> Result<Value, String> {
    let body = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields)
        .finish();
    let response = http
        .request(
            ProviderHttpRequest {
                method: ProviderHttpMethod::Post,
                url: TOKEN_URL.to_string(),
                headers: vec![
                    (
                        "Content-Type".to_string(),
                        "application/x-www-form-urlencoded".to_string(),
                    ),
                    ("Accept".to_string(), "application/json".to_string()),
                ],
                body: Some(body),
                follow_redirects: true,
            },
            timeout_ms,
        )
        .await
        .map_err(|error| format!("{label} failed: {error}"))?;
    if !response.ok() {
        return Err(format!(
            "{label} failed ({}): {}",
            response.status, response.body
        ));
    }
    serde_json::from_str(&response.body)
        .map_err(|error| format!("{label} returned invalid JSON: {error}"))
}

fn credentials_from(
    token: &Value,
    stored_refresh: Option<&str>,
    project_id: String,
) -> Result<GoogleAntigravityCredentials, String> {
    let text = |name: &str| {
        token
            .get(name)
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
    };
    let access = text("access_token").ok_or("Google token response has no access token")?;
    let refresh = text("refresh_token")
        .or(stored_refresh)
        .ok_or("No refresh token received. Please try again.")?;
    let expires_in = token
        .get("expires_in")
        .and_then(Value::as_i64)
        .unwrap_or(3600);
    // Epoch millis fit i64 for ~292 million years.
    #[allow(clippy::cast_possible_truncation)]
    let now_ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(i64::MAX, |elapsed| elapsed.as_millis() as i64);
    Ok(GoogleAntigravityCredentials {
        access: access.to_string(),
        refresh: refresh.to_string(),
        expires: now_ms
            .saturating_add(expires_in.saturating_mul(1000))
            .saturating_sub(EXPIRY_SKEW_MS),
        project_id,
    })
}

/// The account's Cloud Code Assist project: the one `loadCodeAssist`
/// names (`GOOGLE_CLOUD_PROJECT` asks for a given one), or a new one
/// on the account's default tier. A failed lookup answers the fallback
/// project instead of failing the login; only a cancel is an error.
async fn discover_project(
    http: &dyn ProviderHttp,
    ui: &dyn OAuthLoginUi,
    access: &str,
) -> Result<String, String> {
    let env_project = ["GOOGLE_CLOUD_PROJECT", "GOOGLE_CLOUD_PROJECT_ID"]
        .into_iter()
        .find_map(|name| std::env::var(name).ok())
        .map(|project| project.trim().to_string())
        .filter(|project| !project.is_empty());
    let fallback = || {
        env_project
            .clone()
            .unwrap_or_else(|| FALLBACK_PROJECT_ID.to_string())
    };
    let mut metadata = Map::new();
    metadata.insert("ideType".into(), json!("ANTIGRAVITY"));
    metadata.insert("ideVersion".into(), json!(Client::Antigravity.version()));
    metadata.insert("ideName".into(), json!("antigravity"));
    metadata.insert(
        "platform".into(),
        json!(if cfg!(target_os = "windows") {
            "WINDOWS_AMD64"
        } else if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
            "DARWIN_ARM64"
        } else if cfg!(target_os = "macos") {
            "DARWIN_AMD64"
        } else if cfg!(target_arch = "aarch64") {
            "LINUX_ARM64"
        } else {
            "LINUX_AMD64"
        }),
    );
    metadata.insert("pluginType".into(), json!("GEMINI"));
    let mut body = Map::new();
    if let Some(project) = &env_project {
        metadata.insert("duetProject".into(), json!(project));
        body.insert("cloudaicompanionProject".into(), json!(project));
    }
    body.insert("metadata".into(), Value::Object(metadata));

    ui.on_progress("Looking up your Antigravity project...");
    let load_agent = format!(
        "{} google-api-nodejs-client/10.3.0",
        antigravity_user_agent()
    );
    let mut load = None;
    for endpoint in [
        PROD_ENDPOINT,
        DAILY_ENDPOINT,
        DAILY_SANDBOX_ENDPOINT,
        AUTOPUSH_SANDBOX_ENDPOINT,
    ] {
        if ui.is_cancelled() {
            return Err(LOGIN_CANCELLED.to_string());
        }
        let request = code_assist_request(
            ProviderHttpMethod::Post,
            format!("{endpoint}/v1internal:loadCodeAssist"),
            access,
            &load_agent,
            Some(Value::Object(body.clone()).to_string()),
        );
        load = json_answer(http.request(request, TOKEN_TIMEOUT_MS).await);
        if load.is_some() {
            break;
        }
    }
    let Some(load) = load else {
        return Ok(fallback());
    };
    // The project is a bare id or an object carrying it.
    let loaded = &load["cloudaicompanionProject"];
    if let Some(project) = loaded
        .as_str()
        .or_else(|| loaded["id"].as_str())
        .filter(|project| !project.trim().is_empty())
    {
        return Ok(project.to_string());
    }

    // No project yet: onboard the account on its default tier.
    let tiers = load["allowedTiers"].as_array();
    let Some(tier) = tiers.and_then(|tiers| {
        tiers
            .iter()
            .find(|tier| tier["isDefault"] == json!(true))
            .or_else(|| tiers.first())
    }) else {
        return Ok(fallback());
    };
    body.insert(
        "tierId".into(),
        json!(tier["id"].as_str().unwrap_or("FREE")),
    );
    ui.on_progress("Setting up your Antigravity project...");
    Ok(onboard_project(http, ui, access, body)
        .await?
        .unwrap_or_else(fallback))
}

/// Create the account's project with `onboardUser` and wait for the
/// operation: the new project, or `None` when the backend did not
/// answer one. Only a cancel is an error.
async fn onboard_project(
    http: &dyn ProviderHttp,
    ui: &dyn OAuthLoginUi,
    access: &str,
    body: Map<String, Value>,
) -> Result<Option<String>, String> {
    let agent = antigravity_user_agent();
    let request = code_assist_request(
        ProviderHttpMethod::Post,
        format!("{DAILY_ENDPOINT}/v1internal:onboardUser"),
        access,
        &agent,
        Some(Value::Object(body).to_string()),
    );
    let Some(mut operation) = json_answer(http.request(request, TOKEN_TIMEOUT_MS).await) else {
        return Ok(None);
    };
    for _ in 0..ONBOARD_POLLS {
        let Some(name) = operation["name"].as_str() else {
            break;
        };
        if operation["done"] == json!(true) {
            break;
        }
        tokio::time::sleep(ONBOARD_POLL_INTERVAL).await;
        if ui.is_cancelled() {
            return Err(LOGIN_CANCELLED.to_string());
        }
        let request = code_assist_request(
            ProviderHttpMethod::Get,
            format!(
                "{DAILY_ENDPOINT}/v1internal/{}",
                name.trim_start_matches('/')
            ),
            access,
            &agent,
            None,
        );
        let Some(polled) = json_answer(http.request(request, TOKEN_TIMEOUT_MS).await) else {
            return Ok(None);
        };
        operation = polled;
    }
    Ok(operation["response"]["cloudaicompanionProject"]["id"]
        .as_str()
        .filter(|project| !project.trim().is_empty())
        .map(str::to_string))
}

fn code_assist_request(
    method: ProviderHttpMethod,
    url: String,
    access: &str,
    user_agent: &str,
    body: Option<String>,
) -> ProviderHttpRequest {
    ProviderHttpRequest {
        method,
        url,
        headers: vec![
            ("Authorization".to_string(), format!("Bearer {access}")),
            ("User-Agent".to_string(), user_agent.to_string()),
            (
                "X-Goog-Api-Client".to_string(),
                "gl-node/22.21.1".to_string(),
            ),
            ("Content-Type".to_string(), "application/json".to_string()),
            ("Accept".to_string(), "application/json".to_string()),
        ],
        body,
        follow_redirects: true,
    }
}

/// The JSON body of a successful answer.
fn json_answer(answer: Result<ProviderHttpResponse, String>) -> Option<Value> {
    let response = answer.ok().filter(ProviderHttpResponse::ok)?;
    serde_json::from_str(&response.body).ok()
}
