# uh-cli

The `prime-agent` binary and composition root.

## Scope

- Argument parsing, mode selection and exit codes. Modes: `-p` (print, `text`
  or `json` via `--mode`) and `--mode acp`. Public commands: `help`,
  `mcp add|list|get|remove`, `model list`, `session export`, `prompt`. The TUI,
  the daemon modes and their commands (`agents`, `attach`, `status`, `doctor`,
  `shutdown`, `update`, `schedule`, ...) were removed; the old names print
  removal guidance.
- **Print runtime** (`print_runtime.rs`): drives the in-process session engine
  for `-p`. It admits the prompts, runs the turn-boundary compaction checks
  (`print_boundary.rs`: provider retry/failover, overflow compact-and-retry,
  model-requested `compact.run`, threshold), emits the json event stream (session header,
  `message_update` deltas, tool events, `compaction_start`/`compaction_end`,
  `goal_update`, `session_action_update`), and picks the terminal result
  (stdout, stderr, exit code). Once the prompts settle, it runs the session's
  wakes (a background `bash()` completion, a goal backoff, a quota reset) as
  further turns, waiting at most `--background-wait-ms` (default 10 minutes)
  each time. The kernel shuts down before the process exits: a persisted
  session flushes its snapshot, so `-c`/`-r` revive the Python namespace, and
  the background commands still running die with it.
- **Goals and autonomous mode** (`print_goal.rs`, `print_autonomous.rs`): the
  `--goal` seed rides the first turn; the in-loop continuation hook runs an
  active goal's or the autonomous run's natural continuations inside one agent
  run, with the budget-limit steer and the threshold-held continuation drained
  as follow-up runs.
- **Session slash commands** (`print_session_command.rs`): prompts that are
  `/compact`, `/goal` or `/autonomous` run through the uh-core session-command
  executor instead of reaching the model.
- **ACP** (`--mode acp`): wires uh-daemon's ACP transport to the same headless
  engine builder.
- **Login logic** kept for a future front-end (`provider_login.rs`,
  `prime_inference_login.rs`, `mcp_login.rs`, `client_auth.rs`): the provider
  and MCP auth flows, driven through an auth-panel channel.
- **Packaged layout**: the binary plus `package.json`, `prime-agent-runtime/`
  and `skills/`, resolved from the executable's directory; the hidden
  `--prime-agent-bootstrap` flag pre-builds the kernel venv. Verifier:
  `tests/packaged_layout_e2e.rs`.

## Non-goals

No business logic: it composes uh-core, uh-daemon, uh-ai and uh-agent through
their public APIs.

## Public API

The `prime-agent` executable surface (flags, subcommands, exit codes);
`print_runtime` is public for the integration verifiers. Everything else is
`pub(crate)`.

## Depends on

uh-types, uh-ai, uh-agent, uh-core, uh-daemon (one-way, composition root).
Verifiers: `tests/print_runtime_e2e.rs`, `tests/acp_mode_e2e.rs`,
`tests/acp_config_option_e2e.rs`, `tests/continue_guard_e2e.rs`.
