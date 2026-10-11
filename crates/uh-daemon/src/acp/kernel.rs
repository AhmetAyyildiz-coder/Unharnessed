//! The kernel on the panel: the commands the agent left running in the
//! background, and the variables its cells made.
//!
//! - `_unharnessed/kernel/changed` (a notification) `{ sessionId, commands?,
//!   variables? }` carries a list again each time it changes. A command is a
//!   `bash()` handle the agent left running, or that ended since: `{ id,
//!   command, startedAt, status: "running" | "finished", exitCode, killed,
//!   ports, durationMs? }` (`ports`: the TCP ports its process group listens
//!   on; `durationMs` once finished). A variable is `{ name, type, rev,
//!   value? | signature? | len, unit? | shape, dtype? }`; `rev` changes when
//!   the name is bound to another object.
//! - `_unharnessed/kernel/restored` (a notification) `{ sessionId, restored }`:
//!   a new kernel brought back variables of the list above (`restored`), or
//!   none could be (`false`). Once per revive, never when none were saved.
//! - `_unharnessed/kernel/tail` `{ sessionId, id, lines }` answers `{ tail }`,
//!   a command's last lines.
//! - `_unharnessed/kernel/kill` `{ sessionId, id }` stops a command and
//!   answers `{ killed }`.
//! - `_unharnessed/kernel/preview` `{ sessionId, name }` answers `{ preview,
//!   style: "code" | "text", truncated }`, or is refused (`data.details`):
//!   `busy` while a cell runs, or the value could not be shown.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use tokio::sync::Mutex;

use uh_core::kernel::shared::KernelChange;
use uh_core::session_engine::engine::SessionEngine;

use super::types::session_id_params;
use super::{internal_error, jsonrpc, producer, AcpModeState, ConnectionState};

pub(super) const TAIL: &str = "_unharnessed/kernel/tail";
pub(super) const KILL: &str = "_unharnessed/kernel/kill";
pub(super) const PREVIEW: &str = "_unharnessed/kernel/preview";
const CHANGED: &str = "_unharnessed/kernel/changed";
const RESTORED: &str = "_unharnessed/kernel/restored";

/// A server binds its port a moment after it starts: a young command without
/// one is looked at again this often, for this long.
const PORT_PROBE_EVERY: Duration = Duration::from_secs(2);
const PORT_PROBE_FOR: Duration = Duration::from_secs(60);
/// While a command runs, the list is read again this often all the same.
const HEARTBEAT: Duration = Duration::from_secs(20);
const LSOF_TIMEOUT: Duration = Duration::from_millis(1500);

/// Send the panel the kernel's lists each time they change.
pub(super) async fn watch(
    state: Arc<Mutex<ConnectionState>>,
    mode: AcpModeState,
    tx: producer::FrameSink,
) {
    let (changes_tx, mut changes) = tokio::sync::mpsc::unbounded_channel();
    mode.engine.watch_kernel(Arc::new(move |change| {
        let _ = changes_tx.send(change);
    }));
    let mut sent_commands: Option<Value> = None;
    let mut sent_variables: Option<Value> = None;
    // When each running command was first seen, for the port probes.
    let mut seen: HashMap<String, Instant> = HashMap::new();
    let mut next: Option<Duration> = None;
    loop {
        let change = match next {
            Some(after) => tokio::time::timeout(after, changes.recv()).await.ok(),
            None => Some(changes.recv().await),
        };
        let mut cell = match change {
            Some(None) => return,
            Some(Some(change)) => change == KernelChange::Cell,
            None => false,
        };
        // A cell's end and its handles' events come in a burst: read once.
        tokio::time::sleep(Duration::from_millis(40)).await;
        while let Ok(change) = changes.try_recv() {
            cell |= change == KernelChange::Cell;
        }
        let session_id = {
            let state = state.lock().await;
            state.session.as_ref().map(|entry| entry.session.id.clone())
        };
        let Some(session_id) = session_id else {
            next = None;
            continue;
        };
        report_restore(&mode.engine, &session_id, &tx).await;
        if cell {
            // A stopped kernel keeps its last list: the session revives the
            // namespace on the next start.
            if let Ok(variables) = mode.engine.kernel_variables().await {
                if sent_variables.as_ref() != Some(&variables) {
                    let params = json!({ "sessionId": session_id, "variables": variables });
                    let _ = tx.send(jsonrpc::notification(CHANGED, &params));
                    sent_variables = Some(variables);
                }
            }
        }
        let commands = commands(&mode.engine).await;
        let now = Instant::now();
        seen.retain(|id, _| commands.iter().any(|row| row["id"] == json!(id)));
        let mut probing = false;
        let mut running = false;
        for row in &commands {
            if row["status"] != "running" {
                continue;
            }
            running = true;
            let id = row["id"].as_str().unwrap_or_default().to_string();
            let first = *seen.entry(id).or_insert(now);
            let portless = row["ports"].as_array().is_none_or(Vec::is_empty);
            probing |= portless && now.duration_since(first) < PORT_PROBE_FOR;
        }
        next = if probing {
            Some(PORT_PROBE_EVERY)
        } else if running {
            Some(HEARTBEAT)
        } else {
            None
        };
        let commands = Value::Array(commands);
        if sent_commands.as_ref() != Some(&commands) {
            let params = json!({ "sessionId": session_id, "commands": commands });
            let _ = tx.send(jsonrpc::notification(CHANGED, &params));
            sent_commands = Some(commands);
        }
    }
}

/// Tell the panel the kernel revived the session's saved variables, or could
/// not. A revive before the session opened waits for `session/load` to call
/// this once its conversation is replayed.
pub(super) async fn report_restore(
    engine: &SessionEngine,
    session_id: &str,
    tx: &producer::FrameSink,
) {
    let Some(restore) = engine.take_kernel_restore() else {
        return;
    };
    // The runtime's own handles (`goal`) are saved and revived too: only
    // the variables the panel lists count.
    let Ok(variables) = engine.kernel_variables().await else {
        return;
    };
    let restored = variables
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["name"].as_str())
        .any(|name| restore.restored.iter().any(|revived| revived == name));
    if !restored && restore.failed.is_empty() {
        return;
    }
    let params = json!({ "sessionId": session_id, "restored": restored });
    let _ = tx.send(jsonrpc::notification(RESTORED, &params));
}

/// The background commands, as the panel lists them; none while the kernel
/// is not running.
async fn commands(engine: &SessionEngine) -> Vec<Value> {
    let Ok(listed) = engine.bash_activity("list", None, 50).await else {
        return Vec::new();
    };
    let background: Vec<&Value> = listed["activities"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|row| row["background"] == true)
        .collect();
    let groups: Vec<i64> = background
        .iter()
        .filter(|row| row["status"] == "running")
        .filter_map(|row| row["pid"].as_i64())
        .collect();
    let ports = listening_ports(&groups).await;
    background
        .into_iter()
        .map(|row| {
            let mut out = json!({
                "id": row["id"],
                "command": row["command"],
                "startedAt": row["startedAt"],
                "status": row["status"],
                "exitCode": row["exitCode"],
                "killed": row["killed"] == true,
                "ports": row["pid"].as_i64().and_then(|pid| ports.get(&pid)).cloned().unwrap_or_default(),
            });
            // A running command's duration grows on every read: the panel
            // counts it from `startedAt`, and the list stays comparable.
            if row["status"] == "finished" {
                out["durationMs"] = row["durationMs"].clone();
            }
            out
        })
        .collect()
}

/// The TCP ports each process group listens on (a `bash()` command leads
/// its own group, so its pid names it). Empty where `lsof` is missing or
/// slow.
async fn listening_ports(groups: &[i64]) -> HashMap<i64, Vec<u16>> {
    let mut ports: HashMap<i64, Vec<u16>> = HashMap::new();
    if groups.is_empty() || cfg!(windows) {
        return ports;
    }
    let set = groups
        .iter()
        .map(i64::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let mut command = tokio::process::Command::new("lsof");
    command
        .args(["-nP", "-a", "-iTCP", "-sTCP:LISTEN", "-g", &set, "-Fgn"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    let Ok(Ok(output)) = tokio::time::timeout(LSOF_TIMEOUT, command.output()).await else {
        return ports;
    };
    // `p<pid>` opens a process, `g<pgid>` names its group, `n<addr>:<port>`
    // is one listening socket.
    let mut group = None;
    for line in String::from_utf8_lossy(&output.stdout).lines() {
        if let Some(pgid) = line.strip_prefix('g') {
            group = pgid.parse::<i64>().ok();
        } else if let Some(name) = line.strip_prefix('n') {
            let port = name
                .rsplit(':')
                .next()
                .and_then(|port| port.parse::<u16>().ok());
            if let (Some(group), Some(port)) = (group, port) {
                let list = ports.entry(group).or_default();
                if !list.contains(&port) {
                    list.push(port);
                }
            }
        }
    }
    for list in ports.values_mut() {
        list.sort_unstable();
    }
    ports
}

/// Serve a `_unharnessed/kernel/*` request; `false` when `method` is not one.
pub(super) async fn handle_request(
    id: &Value,
    method: &str,
    params: &Value,
    state: &Arc<Mutex<ConnectionState>>,
    mode: &AcpModeState,
    tx: &producer::FrameSink,
) -> bool {
    if ![TAIL, KILL, PREVIEW].contains(&method) {
        return false;
    }
    let session_id = session_id_params(params);
    let open = {
        let state = state.lock().await;
        state
            .session
            .as_ref()
            .is_some_and(|entry| entry.session.id == session_id)
    };
    if !open {
        let _ = tx.send(internal_error(
            id,
            &format!("Unknown ACP session: {session_id}"),
        ));
        return true;
    }
    // A preview may take the kernel a moment: the requests after it do not
    // wait for it.
    let (id, method, params, engine, tx) = (
        id.clone(),
        method.to_string(),
        params.clone(),
        mode.engine.clone(),
        tx.clone(),
    );
    tokio::spawn(async move {
        let text = |key: &str| params.get(key).and_then(Value::as_str).unwrap_or_default();
        let answer = match method.as_str() {
            TAIL => {
                let lines = params
                    .get("lines")
                    .and_then(Value::as_u64)
                    .and_then(|lines| usize::try_from(lines).ok())
                    .unwrap_or(50);
                engine
                    .bash_activity("tail", Some(text("id")), lines)
                    .await
                    .map(|fields| json!({ "tail": fields["tail"] }))
            }
            KILL => engine
                .bash_activity("kill", Some(text("id")), 50)
                .await
                .map(|fields| json!({ "killed": fields["killed"] == true })),
            _ => engine
                .kernel_variable_preview(text("name"))
                .await
                .map(|fields| {
                    json!({
                        "preview": fields["preview"],
                        "style": fields["style"],
                        "truncated": fields["truncated"] == true,
                    })
                }),
        };
        let _ = tx.send(match answer {
            Ok(result) => jsonrpc::response(&id, &result),
            Err(error) => internal_error(&id, &format!("{error:#}")),
        });
    });
    true
}
