//! The MCP servers on the settings screen: the `_unharnessed/mcp/*`
//! extension methods. They read the agent dir's files afresh on each call,
//! so they show what the agent will use when it next starts.
//!
//! - `_unharnessed/mcp/list` answers `{ servers }`: the `mcpServers`
//!   servers and the services of Prime's catalog (the Marketplace,
//!   refreshed first when the last try is an hour old), connected first,
//!   each `{ id, label, source, status, signIn, signedIn, description?,
//!   hint?, category?, publisher?, aliases?, token? }`. `source` is
//!   `catalog` or `user`; `status` is `connected`, `pending` (signed in,
//!   not verified yet), `not_connected`, `setup_required`, `disabled` or
//!   `error`, and `hint` says why. `signIn`: an OAuth sign-in can run now,
//!   as the provider login `mcp:<id>` (`providers.rs`). `signedIn`: a
//!   sign-in or token is stored. `token`: a catalog service connected by a
//!   pasted token, what to paste.
//! - `_unharnessed/mcp/set_token` `{ id, token }` stores a catalog
//!   service's token once a handshake with it does not refuse it, and
//!   answers like `list`.
//! - `_unharnessed/mcp/logout` `{ id }` removes the server's stored
//!   sign-in or token and connection record, and answers like `list`.
//!
//! Servers are added, changed and removed through the `mcpServers` setting
//! (`settings.rs`). The running agent keeps the servers and sign-ins it
//! started with: a change applies when it restarts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use uh_core::auth::AuthStorage;
use uh_core::mcp::{McpManager, ViewSource, PROBE_ERROR_UNAUTHORIZED};

use super::jsonrpc;
use super::producer::FrameSink;

pub(super) const LIST: &str = "_unharnessed/mcp/list";
pub(super) const SET_TOKEN: &str = "_unharnessed/mcp/set_token";
pub(super) const LOGOUT: &str = "_unharnessed/mcp/logout";

/// What a request changes before the list answers it.
enum Change {
    SetToken { server: String, token: String },
    Logout(String),
}

/// Serve a `_unharnessed/mcp/*` request; `false` when `method` is not one.
pub(super) async fn handle_request(
    id: &Value,
    method: &str,
    params: &Value,
    cwd: &Arc<PathBuf>,
    agent_dir: &Arc<PathBuf>,
    tx: &FrameSink,
) -> bool {
    let change = match method {
        LIST => None,
        SET_TOKEN | LOGOUT => {
            let Some(server) = string_param(params, "id") else {
                let _ = tx.send(invalid_params(id, "id must be a non-empty string"));
                return true;
            };
            if method == LOGOUT {
                Some(Change::Logout(server))
            } else if let Some(token) = string_param(params, "token") {
                Some(Change::SetToken { server, token })
            } else {
                let _ = tx.send(invalid_params(id, "token must be a non-empty string"));
                return true;
            }
        }
        _ => return false,
    };
    let answer = match serve(change, Arc::clone(cwd), Arc::clone(agent_dir)).await {
        Ok(servers) => jsonrpc::response(id, &json!({ "servers": servers })),
        Err(details) => invalid_params(id, &details),
    };
    let _ = tx.send(answer);
    true
}

async fn serve(
    change: Option<Change>,
    cwd: Arc<PathBuf>,
    agent_dir: Arc<PathBuf>,
) -> Result<Vec<Value>, String> {
    match change {
        None => uh_core::mcp::refresh_plugins_catalog_for(&agent_dir).await,
        Some(Change::SetToken { server, token }) => {
            let (inputs, handles) = off_loop({
                let (cwd, agent_dir) = (Arc::clone(&cwd), Arc::clone(&agent_dir));
                move || {
                    let manager = McpManager::from_settings(&cwd, &agent_dir);
                    manager
                        .paste_install_inputs(&server)
                        .map(|inputs| (inputs, manager.connection_handles()))
                }
            })
            .await??;
            let (server, label) = (inputs.server.clone(), inputs.label.clone());
            let installed = uh_core::mcp::install_static_token(inputs, &token).await?;
            // A refused token is not kept: the service stays to add.
            if installed.error.as_deref() == Some(PROBE_ERROR_UNAUTHORIZED) {
                uh_core::mcp::remove_mcp_connection(&handles, &server).await?;
                return Err(format!("{label} refused this token."));
            }
        }
        Some(Change::Logout(server)) => {
            let handles = off_loop({
                let (cwd, agent_dir) = (Arc::clone(&cwd), Arc::clone(&agent_dir));
                move || McpManager::from_settings(&cwd, &agent_dir).connection_handles()
            })
            .await?;
            uh_core::mcp::remove_mcp_connection(&handles, &server).await?;
        }
    }
    off_loop(move || list(&cwd, &agent_dir)).await
}

/// The manager reads the files and locks its stores: off the async loop.
async fn off_loop<T: Send + 'static>(
    task: impl FnOnce() -> T + Send + 'static,
) -> Result<T, String> {
    tokio::task::spawn_blocking(task)
        .await
        .map_err(|_| "MCP task failed".to_string())
}

/// The servers, as `list` answers them.
fn list(cwd: &Path, agent_dir: &Path) -> Vec<Value> {
    let manager = McpManager::from_settings(cwd, agent_dir);
    let stored = AuthStorage::create(agent_dir).get_all();
    manager
        .service_catalog_views()
        .into_iter()
        .map(|view| {
            let signed_in = stored
                .credential(&format!("mcp:{}", view.service_id))
                .is_some();
            let mut server = json!({
                "id": view.service_id,
                "label": view.label,
                "source": if view.source == ViewSource::User { "user" } else { "catalog" },
                "status": view.connection_status.as_str(),
                "signIn": view.connectable && view.uses_oauth,
                "signedIn": signed_in,
            });
            if let Some(description) = view.description {
                server["description"] = json!(description);
            }
            if let Some(hint) = view.setup_hint {
                server["hint"] = json!(hint);
            }
            if let Some(category) = view.category {
                server["category"] = json!(category);
            }
            if let Some(publisher) = view.publisher {
                server["publisher"] = json!(publisher);
            }
            if let Some(aliases) = view.aliases {
                server["aliases"] = json!(aliases);
            }
            let catalog = view.source != ViewSource::User;
            if let Some(token) = catalog
                .then(|| manager.paste_token_label(&view.service_id))
                .flatten()
            {
                server["token"] = json!(token);
            }
            server
        })
        .collect()
}

fn string_param(params: &Value, name: &str) -> Option<String> {
    params
        .get(name)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_string)
}

fn invalid_params(id: &Value, details: &str) -> Value {
    jsonrpc::error_response(
        id,
        jsonrpc::INVALID_PARAMS,
        "Invalid params",
        Some(&json!({ "details": details })),
    )
}
