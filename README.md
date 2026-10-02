# Unharnessed

Unharnessed is a minimal agent harness stripped down from
[Prime Agent](https://github.com/PrimeIntellect-ai/prime-agent) (MIT). The model
gets exactly one tool, `ipython`: a persistent CPython REPL. Everything else the
agent can do is a Python function inside that REPL.

What it keeps:

- **The persistent Python kernel**, with `bash()` handles for shell commands and a
  namespace snapshot that survives session resume (`-c`/`-r`).
- **Four built-in skills**, the kernel's API to the harness: `goal` (persistent
  goals), `compact` (context compaction from the REPL), `mcp` (generic MCP client)
  and `attach-image`.
- **Automatic compaction** (threshold, overflow recovery, model-requested,
  `/compact`), **goals**, **autonomous mode** with turn/token/time budgets, and
  **provider retry/failover** (backoff, the same model on another configured
  provider, waiting out a quota reset).
- **Wake-ups of an idle session**: a background `bash()` that outlives its
  cell, a goal's backoff and a quota reset start the next turn. `-p` waits for
  them before exiting, at most `--background-wait-ms` per wait (default 10
  minutes, `0` = no wait), then kills the commands still running; in ACP the
  agent starts the turn itself and streams it as `session/update`.
- **Sessions** saved as append-only JSONL, resumable.
- **Skills** from `~/.prime/agent/skills`, `<project>/.prime/agent/skills`,
  `.agents/skills` in parent directories, and the `skills` setting; `AGENTS.md`
  and `CLAUDE.md` are loaded into the system prompt.
- **All providers and logins** (Anthropic, OpenAI/Codex, Google/Vertex, Bedrock,
  Azure, Mistral, xAI, Prime Inference, ...).

What was removed: the TUI, the background daemon, sub-agents and agent
messaging, the self-modifying "continual harness" (`/refine`), cron and
heartbeats, telemetry and trace upload, auto-update and the package manager,
and the extra skills (`edit`, `websearch`, `skill-creator`, `prime-intellect`).
`UNHARNESSED_PLAN.md` records every step.

## Usage

Build the binary:

```bash
cargo build --release -p uh-cli --bin unharnessed
./target/release/unharnessed --unharnessed-bootstrap   # build the kernel venv
```

Run it headless:

```bash
unharnessed -p "Compute 2**100 in the kernel"           # text output
unharnessed --mode json -p "..."                        # JSON event stream
unharnessed -c -p "..."                                 # continue the last session
unharnessed --mode acp                                  # Agent Client Protocol over stdio
unharnessed prompt [--json]                             # dump the assembled system prompt
```

Credentials come from the shared auth store (`~/.prime/agent/auth.json`) or the
providers' usual environment variables.

> [!WARNING]
> The agent executes model-generated Python and shell commands with your user
> permissions. The kernel process is **not** a security sandbox. Run it in a
> disposable checkout or an external sandbox.

## Development

`make check` runs fmt, clippy (`-D warnings`), the tests and the release build.
`scripts/unharnessed-smoke.sh <dir>` drives the real binary end to end against a
real model; `scripts/unharnessed-skill-loading.sh <dir>` checks skill loading
without a model; `scripts/unharnessed-wakes-e2e.py <dir>` checks the provider
retry and the wake-ups in `-p` and ACP with the faux provider.

## License

MIT, like the upstream project (see [LICENSE](LICENSE)).
