//! The settings screen over ACP: the `_unharnessed/settings/*` extension
//! methods the VS Code extension's settings screen drives. They read and
//! write the global settings file (`<agent dir>/settings.json`) only.
//!
//! - `_unharnessed/settings/get` answers `{ values, defaults, restart }`:
//!   each key's value in the global file (`null` when unset or unusable),
//!   the value the agent uses when it is unset (`null`: nothing), and the
//!   keys whose change applies when the agent next starts. The other keys
//!   are read again each time they are used.
//! - `_unharnessed/settings/set` `{ key, value }` writes one key, or
//!   removes it with `null` (its default applies again), and answers like
//!   `get`. An unknown key or a value of the wrong kind is refused with
//!   `-32602` and `data.details`; nothing is written then.
//!
//! Keys are the file's dotted paths (`compaction.enabled`).

use std::path::PathBuf;
use std::sync::Arc;

use serde_json::{json, Map, Value};
use uh_core::session_engine::{compaction, provider_failover, provider_park, provider_retry};
use uh_core::settings::SettingsManager;

use super::jsonrpc;
use super::producer::FrameSink;

pub(super) const GET: &str = "_unharnessed/settings/get";
pub(super) const SET: &str = "_unharnessed/settings/set";

/// What a key holds.
#[derive(Clone, Copy)]
enum Kind {
    Bool,
    /// A whole number in `0..=max`.
    Count {
        max: u64,
    },
    /// A `provider/model-id` reference.
    Model,
    /// The `mcpServers` map: server name to its config.
    McpServers,
}

struct Key {
    name: &'static str,
    kind: Kind,
    /// Read when the agent starts (the engine keeps it), not on each use.
    restart: bool,
}

const HOUR_MS: u64 = 3_600_000;

const KEYS: &[Key] = &[
    Key {
        name: "compaction.enabled",
        kind: Kind::Bool,
        restart: true,
    },
    Key {
        name: "compaction.reserveTokens",
        kind: Kind::Count { max: 2_000_000 },
        restart: true,
    },
    Key {
        name: "compaction.keepRecentTokens",
        kind: Kind::Count { max: 2_000_000 },
        restart: true,
    },
    Key {
        name: "compaction.agentCallable",
        kind: Kind::Bool,
        restart: true,
    },
    Key {
        name: "auxiliaryModel",
        kind: Kind::Model,
        restart: false,
    },
    Key {
        name: "retry.enabled",
        kind: Kind::Bool,
        restart: false,
    },
    Key {
        name: "retry.maxRetries",
        kind: Kind::Count { max: 100 },
        restart: false,
    },
    Key {
        name: "retry.baseDelayMs",
        kind: Kind::Count { max: HOUR_MS },
        restart: false,
    },
    Key {
        name: "retry.provider.maxRetryDelayMs",
        kind: Kind::Count { max: 24 * HOUR_MS },
        restart: false,
    },
    Key {
        name: "retry.failover.enabled",
        kind: Kind::Bool,
        restart: false,
    },
    Key {
        name: "retry.failover.maxRetries",
        kind: Kind::Count { max: 100 },
        restart: false,
    },
    Key {
        name: "retry.failover.baseDelayMs",
        kind: Kind::Count { max: HOUR_MS },
        restart: false,
    },
    Key {
        name: "retry.failover.maxDelayMs",
        kind: Kind::Count { max: HOUR_MS },
        restart: false,
    },
    Key {
        name: "retry.provider.waitForUsage.pauseUntilReset",
        kind: Kind::Bool,
        restart: false,
    },
    Key {
        name: "retry.provider.waitForUsage.maxPauseMs",
        kind: Kind::Count {
            max: 7 * 24 * HOUR_MS,
        },
        restart: false,
    },
    Key {
        name: "retry.provider.waitForUsage.maxParks",
        kind: Kind::Count { max: 100 },
        restart: false,
    },
    Key {
        name: "imageModel",
        kind: Kind::Model,
        restart: false,
    },
    Key {
        name: "images.autoResize",
        kind: Kind::Bool,
        restart: false,
    },
    Key {
        name: "images.blockImages",
        kind: Kind::Bool,
        restart: false,
    },
    Key {
        name: "mcpServers",
        kind: Kind::McpServers,
        restart: true,
    },
];

/// Serve a `_unharnessed/settings/*` request; `false` when `method` is
/// not one.
pub(super) async fn handle_request(
    id: &Value,
    method: &str,
    params: &Value,
    cwd: &Arc<PathBuf>,
    agent_dir: &Arc<PathBuf>,
    tx: &FrameSink,
) -> bool {
    let write = match method {
        GET => None,
        SET => match parse_set(params) {
            Ok(write) => Some(write),
            Err(details) => {
                let _ = tx.send(invalid_params(id, &details));
                return true;
            }
        },
        _ => return false,
    };
    // File reads and writes run off the async loop.
    let task = tokio::task::spawn_blocking({
        let cwd = Arc::clone(cwd);
        let agent_dir = Arc::clone(agent_dir);
        move || {
            let mut settings = SettingsManager::create(cwd.as_path(), agent_dir.as_path());
            if let Some((key, value)) = write {
                let path: Vec<&str> = key.split('.').collect();
                settings
                    .set_global_value(&path, value.as_ref())
                    .map_err(|error| error.to_string())?;
            }
            Ok::<Value, String>(view(&settings))
        }
    });
    let answer = match task.await {
        Ok(Ok(view)) => jsonrpc::response(id, &view),
        Ok(Err(details)) => invalid_params(id, &details),
        Err(_) => {
            jsonrpc::error_response(id, jsonrpc::INTERNAL_ERROR, "settings task failed", None)
        }
    };
    let _ = tx.send(answer);
    true
}

/// The `set` params, checked: the key and the value to write (`None`
/// removes it).
fn parse_set(params: &Value) -> Result<(&'static str, Option<Value>), String> {
    let name = params
        .get("key")
        .and_then(Value::as_str)
        .ok_or_else(|| "key must be a string".to_string())?;
    let key = KEYS
        .iter()
        .find(|key| key.name == name)
        .ok_or_else(|| format!("unknown setting: {name}"))?;
    let value = match params.get("value") {
        None | Some(Value::Null) => return Ok((key.name, None)),
        Some(value) => value,
    };
    let checked = match key.kind {
        Kind::Bool => value
            .as_bool()
            .map(Value::Bool)
            .ok_or_else(|| format!("{name} takes true or false"))?,
        Kind::Count { max } => match value.as_u64() {
            Some(count) if count <= max => json!(count),
            _ => return Err(format!("{name} takes a whole number from 0 to {max}")),
        },
        Kind::Model => match value.as_str().map(str::trim) {
            Some(model) if !model.is_empty() => json!(model),
            _ => return Err(format!("{name} takes a provider/model reference")),
        },
        Kind::McpServers => check_mcp_servers(value)?,
    };
    Ok((key.name, Some(checked)))
}

/// Each entry must be a server the agent can read: an entry it cannot
/// parse would be dropped without a word.
fn check_mcp_servers(value: &Value) -> Result<Value, String> {
    let servers = value
        .as_object()
        .ok_or_else(|| "mcpServers takes an object of servers".to_string())?;
    for (name, config) in servers {
        if !is_server_name(name) {
            return Err(format!(
                "MCP server {name:?}: a name is 1 to 64 letters, digits, - or _, starting with a letter or digit"
            ));
        }
        if uh_core::mcp::get_catalog_entry(name).is_some() {
            return Err(format!(
                "MCP server {name}: the name is taken by the built-in {name} server"
            ));
        }
        match serde_json::from_value::<uh_core::mcp::McpServerConfig>(config.clone()) {
            Ok(uh_core::mcp::McpServerConfig::Http { url, .. }) if url.trim().is_empty() => {
                return Err(format!("MCP server {name}: the URL is empty"));
            }
            Ok(uh_core::mcp::McpServerConfig::Stdio { command, .. })
                if command.trim().is_empty() =>
            {
                return Err(format!("MCP server {name}: the command is empty"));
            }
            Ok(_) => {}
            Err(error) => return Err(format!("MCP server {name}: {error}")),
        }
    }
    Ok(value.clone())
}

/// The names the agent can address a server by: `[A-Za-z0-9][A-Za-z0-9_-]{0,63}`.
fn is_server_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|first| first.is_ascii_alphanumeric())
        && name.len() <= 64
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// The answer to `get` and `set`.
fn view(settings: &SettingsManager) -> Value {
    // The global scope as the agent reads it: a value it cannot use reads
    // as unset, like the agent does.
    let global = serde_json::to_value(settings.global_settings()).unwrap_or_default();
    let mut values = Map::new();
    for key in KEYS {
        let value = key
            .name
            .split('.')
            .try_fold(&global, |value, part| value.get(part))
            .cloned()
            .unwrap_or(Value::Null);
        values.insert(key.name.to_string(), value);
    }
    let restart: Vec<&str> = KEYS
        .iter()
        .filter(|key| key.restart)
        .map(|key| key.name)
        .collect();
    json!({ "values": values, "defaults": defaults(), "restart": restart })
}

/// What the agent uses for an unset key.
fn defaults() -> Value {
    let retry = provider_retry::DEFAULT_PROVIDER_RETRY_POLICY;
    let failover = provider_failover::DEFAULT_PROVIDER_FAILOVER_POLICY;
    let park = provider_park::DEFAULT_PROVIDER_PARK_POLICY;
    json!({
        "compaction.enabled": true,
        "compaction.reserveTokens": compaction::DEFAULT_RESERVE_TOKENS,
        "compaction.keepRecentTokens": compaction::DEFAULT_KEEP_RECENT_TOKENS,
        "compaction.agentCallable": true,
        "auxiliaryModel": null,
        "retry.enabled": retry.enabled,
        "retry.maxRetries": retry.max_retries,
        "retry.baseDelayMs": retry.base_delay_ms,
        "retry.provider.maxRetryDelayMs": retry.max_retry_delay_ms,
        "retry.failover.enabled": failover.enabled,
        "retry.failover.maxRetries": failover.max_retries,
        "retry.failover.baseDelayMs": failover.base_delay_ms,
        "retry.failover.maxDelayMs": failover.max_delay_ms,
        "retry.provider.waitForUsage.pauseUntilReset": park.pause_until_reset,
        "retry.provider.waitForUsage.maxPauseMs": park.max_pause_ms,
        "retry.provider.waitForUsage.maxParks": park.max_parks,
        "imageModel": null,
        "images.autoResize": true,
        "images.blockImages": false,
        "mcpServers": {},
    })
}

fn invalid_params(id: &Value, details: &str) -> Value {
    jsonrpc::error_response(
        id,
        jsonrpc::INVALID_PARAMS,
        "Invalid params",
        Some(&json!({ "details": details })),
    )
}
