# uh-daemon

The ACP transport and the session-ownership guards. Despite the crate name,
Unharnessed has no background daemon: the supervisor, workers, RPC mode and
their wire types were removed (`UNHARNESSED_PLAN.md`, step 2). The crate keeps
its name so the ACP code stays where it was.

## Scope

- `acp` — the Agent Client Protocol stdio mode (`unharnessed --mode acp`): a thin
  JSON-RPC transport over the uh-core session engine. One connection hosts at
  most one session. `session/new` admits it and advertises the model and
  reasoning-effort pickers; `session/prompt` drives one engine turn with
  follow-up queueing; `session/set_config_option` applies a picker selection;
  `session/cancel` and `session/close` stop work (close flushes the kernel
  snapshot). Every frame leaves through one ordered write queue, so responses
  and `session/update` notifications interleave in publication order, with
  namespaced `_meta` (`unharnessed`). The transport hosts the
  automatic compaction arms at its turn boundaries (`acp/compaction_arms.rs`:
  overflow compact-and-retry, model-requested `compact.run`, threshold) and the
  goal/autonomous continuation loop (`acp/goal_continuation.rs`). A failed turn
  goes through the provider retry/failover chain first. When a background
  `bash()` finishes, a goal backoff passes or a quota resets while no prompt
  runs, the agent starts the turn itself (`acp/prompt.rs`, `start_wake_turn`):
  it streams like a prompt turn but answers no request, and a client prompt
  waits for it to settle. The `_unharnessed/providers/*` extension methods
  (`acp/providers.rs`) list the providers, save or remove an API key, and run
  a browser sign-in, relaying its steps (sign-in URL, paste prompts, Prime team
  picker) as `_unharnessed/providers/login_update` notifications.
- `provider_auth`, `auth_panel` — the provider-login contract the ACP methods
  drive: the `ProviderAuthCommands` trait (implemented by uh-cli, the
  composition root) and the flow's request channel (`AuthPanelHandle`).
- `lease` — the per-session-file runtime lease
  (`<agent-dir>/session-leases/<sha256(path)>.lock`, atomic acquisition, stale
  owner reclaim), so two processes never drive the same session file.
- `hold_refusal` — the message a `-c`/`-r` open prints when another live process
  holds the lease (whether the holder is another Unharnessed process, `kill <pid>`).
- `overflow_compaction` — the overflow-arm outcome shared by the print and ACP
  compaction arms.
- `paths`, `util` — small helpers for the above.

## Non-goals

Background sessions, reattach, multi-session routing, scheduling, sub-agents.

## Public API

`acp::run_acp_mode` with `acp::AcpOptions`; `provider_auth` and `auth_panel`
(the login contract uh-cli implements); `lease`
(`acquire_runtime_session_lease`, `SessionLease`, `SessionAlreadyActiveError`);
`hold_refusal` (`refusal_message`, `HoldIdentity`). The other modules are
crate-internal.
