# uh-core

The session engine.

## Scope

- `session_engine` — `create_session` assembles a `SessionEngine` (message in,
  events out): the agent loop wiring, the `ipython` tool, prompt admission and
  session slash commands (`/compact`, `/goal`, `/autonomous`), compaction
  (threshold, overflow recovery, model-requested `compact.*`, manual), the goal
  driver, provider retry/failover/park (`turn_retry`: the chain a host runs on a
  failed turn), the idle session's wakes (`wakes`: detached `bash()`
  completions, the goal backoff, a quota reset), auxiliary and image model routing,
  branch summaries, side questions, and the kernel host-request handlers
  (`goal.*`, `compact.*`, `model.info`).
- `kernel` — the persistent IPython kernel: venv bootstrap (`uv`, the
  `unharnessed-runtime` sidecar, the runtime-ready probe), spawn/execute/
  interrupt/teardown, the host-request bridge, the bootstrap code injected at
  start (binds `bash`, `mcp`, pre-imports the Python skills), and the namespace
  snapshot (`state_snapshot`) that revives a resumed session.
- `session` — append-only JSONL sessions: manager, windowed loading, leases'
  consumers, forks, git context, discovery.
- `skills`, `resources`, `resource_paths` — skill and prompt-template loading
  from the settings arrays, auto-discovery and the bundled `skills/`;
  `AGENTS.md`/`CLAUDE.md` discovery.
- `prompts` — the system prompt: the `core.md` layer plus the per-model map,
  then the dynamic tail (packages, project context, skills, MCP servers,
  environment). `prompt_guards` keeps `core.md` equal to the real surface.
- `mcp` — the host side of MCP: auth gating of built-in integrations, the
  `mcpServers` setting, the service catalog and connection views, the OAuth
  flow, and the `mcp.*` host requests the kernel's MCP client calls. The MCP
  protocol itself runs in the kernel.
- `autonomous` — autonomous-mode state, limits, quality gates (with
  `workspace_snapshot` for git worktree capture).
- `auth`, `models`, `settings`, `goals`, `export_html`, `platform` (process
  control, locks, permissions, shell selection, atomic rename), `tools`
  (`ipython` and the code previews).

## Non-goals

No provider HTTP (uh-ai), no loop policy (uh-agent), no transport (uh-cli for
`-p`, uh-daemon for ACP). No TUI, daemon, sub-agents, refinement, cron,
telemetry, updates or package installs (removed, see `UNHARNESSED_PLAN.md`).

## Public API

`session_engine::engine::{create_session, SessionEngineConfig, SessionEngine}`
and the session-engine types the transports drive; `kernel` (provisioner and
manager), `session::manager::SessionManager`, `settings::SettingsManager`,
`resources::load_resources`, `prompts`, `mcp::McpManager`, `auth`, `models`,
`export_html`. Subsystem internals are `pub(crate)`.

## Depends on

uh-types, uh-ai, uh-models, uh-agent (one-way).
