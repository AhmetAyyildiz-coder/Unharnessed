//! The client versions subscription requests claim: Claude Code's on an
//! Anthropic login, Antigravity's on a Google one. Each backend refuses
//! clients older than a minimum that moves with its releases, so a
//! compiled version goes stale. The version claimed is the highest of
//! the compiled floor, the client's latest published version (looked up
//! in the background at most once a day and kept in the models cache
//! dir), and, for Claude Code, the version an API refusal asks for.
//!
//! `UNHARNESSED_CLAUDE_CODE_VERSION` and `UNHARNESSED_ANTIGRAVITY_VERSION`
//! override the claim; `UNHARNESSED_OFFLINE` stops the lookups.

use std::cmp::Ordering;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard, PoisonError};

use serde_json::{json, Value};

use crate::utils_inner::diagnostics::now_ms;
use crate::utils_inner::http::{send, RequestOptions};

/// How long a lookup's answer is trusted.
const LOOKUP_INTERVAL_MS: u64 = 24 * 60 * 60 * 1000;
/// How long a failed lookup waits before the next one.
const FAILED_LOOKUP_WAIT_MS: u64 = 30 * 60 * 1000;
const LOOKUP_TIMEOUT_MS: u64 = 5_000;

/// A client whose version requests claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Client {
    ClaudeCode,
    Antigravity,
}

struct Tracked {
    /// The highest version known, `None` until the floor or the cache
    /// file was read.
    version: Option<String>,
    checked_at_ms: u64,
    looking_up: bool,
}

struct State {
    cache_dir: Option<PathBuf>,
    claude_code: Tracked,
    antigravity: Tracked,
}

static STATE: Mutex<State> = Mutex::new(State {
    cache_dir: None,
    claude_code: Tracked {
        version: None,
        checked_at_ms: 0,
        looking_up: false,
    },
    antigravity: Tracked {
        version: None,
        checked_at_ms: 0,
        looking_up: false,
    },
});

fn lock() -> MutexGuard<'static, State> {
    STATE.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Keep the looked-up versions in `dir` (the models cache dir), and read
/// what an earlier run left there.
pub fn set_cache_dir(dir: &Path) {
    let mut state = lock();
    if state.cache_dir.as_deref() != Some(dir) {
        state.cache_dir = Some(dir.to_path_buf());
        for client in [Client::ClaudeCode, Client::Antigravity] {
            let cached = std::fs::read_to_string(dir.join(client.cache_file()))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok());
            let Some(cached) = cached else {
                continue;
            };
            let tracked = client.tracked(&mut state);
            tracked.checked_at_ms = cached["checkedAtMs"].as_u64().unwrap_or(0);
            if let Some(version) = cached["version"].as_str() {
                client.raise(tracked, version);
            }
        }
    }
}

impl Client {
    /// The version compiled in: the one claimed until a newer one is known.
    const fn floor(self) -> &'static str {
        match self {
            Client::ClaudeCode => "2.1.319",
            Client::Antigravity => "2.31.0",
        }
    }

    const fn env(self) -> &'static str {
        match self {
            Client::ClaudeCode => "UNHARNESSED_CLAUDE_CODE_VERSION",
            Client::Antigravity => "UNHARNESSED_ANTIGRAVITY_VERSION",
        }
    }

    const fn cache_file(self) -> &'static str {
        match self {
            Client::ClaudeCode => "claude-code-version.json",
            Client::Antigravity => "antigravity-version.json",
        }
    }

    /// Where the client's latest version is published: the npm
    /// registry's `latest` tag, and the manifest Antigravity's own
    /// updater reads.
    const fn published_at(self) -> &'static str {
        match self {
            Client::ClaudeCode => "https://registry.npmjs.org/@anthropic-ai%2Fclaude-code/latest",
            Client::Antigravity => "https://antigravity-hub-auto-updater-974169037036.us-central1.run.app/manifest/latest-arm64-mac.yml",
        }
    }

    fn tracked(self, state: &mut State) -> &mut Tracked {
        match self {
            Client::ClaudeCode => &mut state.claude_code,
            Client::Antigravity => &mut state.antigravity,
        }
    }

    fn overridden(self) -> Option<String> {
        std::env::var(self.env())
            .ok()
            .map(|version| version.trim().to_string())
            .filter(|version| !version.is_empty())
    }

    /// Raise the tracked version to `version` when it is a newer one.
    fn raise(self, tracked: &mut Tracked, version: &str) -> bool {
        let current = tracked.version.as_deref().unwrap_or(self.floor());
        let newer = is_version(version) && compare(version, current) == Ordering::Greater;
        if newer {
            tracked.version = Some(version.to_string());
        }
        newer
    }

    /// The version to claim.
    #[must_use]
    pub fn version(self) -> String {
        self.overridden().unwrap_or_else(|| {
            let mut state = lock();
            let tracked = self.tracked(&mut state);
            tracked
                .version
                .clone()
                .unwrap_or_else(|| self.floor().to_string())
        })
    }

    /// Look the latest published version up in the background when the
    /// last answer is older than a day. The request being sent goes with
    /// the version known now; later ones get the newer one.
    pub(crate) fn refresh_if_stale(self) {
        let offline = std::env::var("UNHARNESSED_OFFLINE").is_ok_and(|value| {
            matches!(
                value.trim().to_ascii_lowercase().as_str(),
                "1" | "true" | "yes"
            )
        });
        if offline || self.overridden().is_some() {
            return;
        }
        let mut state = lock();
        // Without a cache dir the process is not a session's (a test, a
        // library use): nothing is looked up.
        if state.cache_dir.is_none() {
            return;
        }
        let tracked = self.tracked(&mut state);
        if tracked.looking_up || now_ms().saturating_sub(tracked.checked_at_ms) < LOOKUP_INTERVAL_MS
        {
            return;
        }
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        tracked.looking_up = true;
        drop(state);
        runtime.spawn(async move {
            let published = self.published().await;
            let mut state = lock();
            let tracked = self.tracked(&mut state);
            tracked.looking_up = false;
            match published {
                Some(version) => {
                    self.raise(tracked, &version);
                    tracked.checked_at_ms = now_ms();
                    self.store(&state);
                }
                None => {
                    tracked.checked_at_ms =
                        now_ms().saturating_sub(LOOKUP_INTERVAL_MS - FAILED_LOOKUP_WAIT_MS);
                }
            }
        });
    }

    async fn published(self) -> Option<String> {
        let mut response = send(RequestOptions {
            timeout_ms: Some(LOOKUP_TIMEOUT_MS),
            ..RequestOptions::new(reqwest::Method::GET, self.published_at().to_string())
        })
        .await
        .ok()?;
        if response.status != 200 {
            return None;
        }
        let body = response.read_all_text().await.ok()?;
        match self {
            Client::ClaudeCode => registry_version(&body),
            Client::Antigravity => manifest_version(&body),
        }
    }

    /// Write the tracked version to the cache dir. The dir is the
    /// catalog caches': a missing one is not created for this.
    fn store(self, state: &State) {
        let tracked = match self {
            Client::ClaudeCode => &state.claude_code,
            Client::Antigravity => &state.antigravity,
        };
        if let Some(dir) = &state.cache_dir {
            let version = tracked.version.as_deref().unwrap_or(self.floor());
            let cached = json!({ "version": version, "checkedAtMs": tracked.checked_at_ms });
            let _ = std::fs::write(dir.join(self.cache_file()), cached.to_string());
        }
    }
}

/// Claim the Claude Code version an API refusal asks for. `true` when
/// the claim changed: the request is worth sending again.
pub(crate) fn adopt_required_claude_code_version(refusal: &str) -> bool {
    let client = Client::ClaudeCode;
    let Some(required) = required_version(refusal) else {
        return false;
    };
    if client.overridden().is_some() {
        return false;
    }
    let mut state = lock();
    let tracked = client.tracked(&mut state);
    let raised = client.raise(tracked, &required);
    if raised {
        client.store(&state);
    }
    raised
}

/// The version in a refusal such as `Claude Code 2.1.251 does not
/// support this model; version 2.1.280 or newer is required`.
fn required_version(refusal: &str) -> Option<String> {
    let wording = ["or newer", "or later", "or higher"]
        .into_iter()
        .find_map(|wording| refusal.find(wording))?;
    let version = refusal[..wording]
        .split_whitespace()
        .next_back()?
        .trim_matches(|character: char| !character.is_ascii_digit());
    is_version(version).then(|| version.to_string())
}

/// The version of the npm registry's answer for a package tag.
fn registry_version(body: &str) -> Option<String> {
    let package: Value = serde_json::from_str(body).ok()?;
    let version = package["version"].as_str()?.trim();
    is_version(version).then(|| version.to_string())
}

/// The version of an updater manifest: its top-level `version:` line.
fn manifest_version(body: &str) -> Option<String> {
    let version = body
        .lines()
        .find_map(|line| line.strip_prefix("version:"))?
        .trim()
        .trim_matches(['"', '\'']);
    is_version(version).then(|| version.to_string())
}

/// Dotted numbers only: a pre-release is not a version to claim.
fn is_version(value: &str) -> bool {
    value.contains('.')
        && value
            .split('.')
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
}

fn compare(left: &str, right: &str) -> Ordering {
    let numbers = |version: &str| -> Vec<u64> {
        version
            .split('.')
            .map(|part| part.parse().unwrap_or(0))
            .collect()
    };
    let (left, right) = (numbers(left), numbers(right));
    let parts = left.len().max(right.len());
    let padded = |numbers: Vec<u64>| {
        numbers
            .into_iter()
            .chain(std::iter::repeat(0))
            .take(parts)
            .collect::<Vec<_>>()
    };
    padded(left).cmp(&padded(right))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_required_version_is_the_one_before_the_wording() {
        let cases = [
            // The refusal names the claimed version first.
            (
                "Claude Code 2.1.251 does not support this model; version 2.1.280 or newer is required. Run 'claude update'.",
                Some("2.1.280"),
            ),
            ("version 3.0.1 or later is required", Some("3.0.1")),
            ("needs (v2.2.0) or higher", Some("2.2.0")),
            // Inside the API's JSON body.
            (
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"version 2.1.330 or newer is required"}}"#,
                Some("2.1.330"),
            ),
            ("prompt is too long: 250000 tokens", None),
            ("version or newer is required", None),
            ("or newer", None),
            // A pre-release is not claimed.
            ("version 2.2.0-beta.1 or newer is required", None),
        ];
        for (refusal, required) in cases {
            assert_eq!(required_version(refusal).as_deref(), required, "{refusal}");
        }
    }

    #[test]
    fn published_versions_are_read_from_both_sources() {
        assert_eq!(
            registry_version(r#"{"name":"@anthropic-ai/claude-code","version":"2.1.322"}"#)
                .as_deref(),
            Some("2.1.322")
        );
        assert_eq!(registry_version(r#"{"version":"2.2.0-beta.1"}"#), None);
        assert_eq!(registry_version(r#"{"error":"Not found"}"#), None);
        assert_eq!(registry_version("<html>502</html>"), None);

        let manifest = "version: 2.31.0\nfiles:\n  - url: https://example/Antigravity.zip\n    version: 9.9.9\npath: Antigravity.zip\n";
        assert_eq!(manifest_version(manifest).as_deref(), Some("2.31.0"));
        assert_eq!(
            manifest_version("path: a.zip\nversion: '2.32.1'\n").as_deref(),
            Some("2.32.1")
        );
        assert_eq!(manifest_version("releaseVersion: 2.31.0\n"), None);
        assert_eq!(manifest_version("version: latest\n"), None);
        assert_eq!(manifest_version("<html>404</html>"), None);
    }

    #[test]
    fn versions_compare_by_number() {
        let cases = [
            ("2.1.280", "2.1.99", Ordering::Greater),
            ("2.2.0", "2.1.999", Ordering::Greater),
            ("3.0", "2.99.99", Ordering::Greater),
            ("2.31", "2.31.0", Ordering::Equal),
            ("2.31.0", "2.31.0.1", Ordering::Less),
            ("2.1.251", "2.1.280", Ordering::Less),
        ];
        for (left, right, expected) in cases {
            assert_eq!(compare(left, right), expected, "{left} vs {right}");
        }
        for not_a_version in ["", "2", "2.", ".1", "2..1", "v2.1", "2.1-rc", "latest"] {
            assert!(!is_version(not_a_version), "{not_a_version}");
        }
    }
}
