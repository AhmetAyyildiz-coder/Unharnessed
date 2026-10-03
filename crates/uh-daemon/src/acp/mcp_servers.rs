//! The MCP servers on the settings screen: the `_unharnessed/mcp/*`
//! extension methods. They read the agent dir's files afresh on each call,
//! so they show what the agent will use when it next starts.
//!
//! - `_unharnessed/mcp/list` answers `{ servers }`: the built-in and local
//!   catalog services and the `mcpServers` servers, connected first, each
//!   `{ id, label, source, status, signIn, signedIn, description?, hint? }`.
//!   `source` is `catalog` or `user`; `status` is `connected`, `pending`
//!   (signed in, not verified yet), `not_connected`, `setup_required`,
//!   `disabled` or `error`, and `hint` says why. `signIn`: an OAuth
//!   sign-in can run now, as the provider login `mcp:<id>`
//!   (`providers.rs`). `signedIn`: a sign-in or token is stored.
//! - `_unharnessed/mcp/logout` `{ id }` removes the server's stored
//!   sign-in and connection record, and answers like `list`.
//!
//! Servers are added, changed and removed through the `mcpServers` setting
//! (`settings.rs`). The running agent keeps the servers and sign-ins it
//! started with: a change applies when it restarts.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{json, Value};
use uh_core::auth::AuthStorage;
use uh_core::mcp::{McpManager, ViewSource};

use super::jsonrpc;
use super::producer::FrameSink;

pub(super) const LIST: &str = "_unharnessed/mcp/list";
pub(super) const LOGOUT: &str = "_unharnessed/mcp/logout";

/// Serve a `_unharnessed/mcp/*` request; `false` when `method` is not one.
pub(super) async fn handle_request(
    id: &Value,
    method: &str,
    params: &Value,
    cwd: &Arc<PathBuf>,
    agent_dir: &Arc<PathBuf>,
    tx: &FrameSink,
) -> bool {
    let logout = match method {
        LIST => None,
        LOGOUT => match params.get("id").and_then(Value::as_str) {
            Some(server) if !server.is_empty() => Some(server.to_string()),
            _ => {
                let _ = tx.send(invalid_params(id, "id must be a non-empty string"));
                return true;
            }
        },
        _ => return false,
    };
    let answer = match serve(logout, Arc::clone(cwd), Arc::clone(agent_dir)).await {
        Ok(servers) => jsonrpc::response(id, &json!({ "servers": servers })),
        Err(details) => invalid_params(id, &details),
    };
    let _ = tx.send(answer);
    true
}

async fn serve(
    logout: Option<String>,
    cwd: Arc<PathBuf>,
    agent_dir: Arc<PathBuf>,
) -> Result<Vec<Value>, String> {
    if let Some(server) = logout {
        let handles = off_loop({
            let (cwd, agent_dir) = (Arc::clone(&cwd), Arc::clone(&agent_dir));
            move || McpManager::from_settings(&cwd, &agent_dir).connection_handles()
        })
        .await?;
        uh_core::mcp::remove_mcp_connection(&handles, &server).await?;
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
            server
        })
        .collect()
}

fn invalid_params(id: &Value, details: &str) -> Value {
    jsonrpc::error_response(
        id,
        jsonrpc::INVALID_PARAMS,
        "Invalid params",
        Some(&json!({ "details": details })),
    )
}
