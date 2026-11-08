<p align="center"><img src=".github/banner.png" alt="Unharnessed"></p>

Unharnessed is a minimal agent harness. The model gets exactly one tool,
`ipython`: a persistent CPython REPL. Everything else the agent can do is a
Python function inside that REPL.

What it does:

- **The persistent Python kernel**, with `bash()` handles for shell commands and a
  namespace snapshot that survives session resume (`-c`/`-r`).
- **Three built-in skills**, the kernel's API to the harness: `goal` (persistent
  goals), `mcp` (generic MCP client) and `attach-image`.
- **Automatic compaction** (threshold, overflow recovery, `/compact`),
  **goals** (a goal ends `blocked` when the model says so or after 3 turns
  without a tool call: the hand comes back, and `-p` exits with code 0),
  **autonomous mode** with turn/token/time budgets, and **provider
  retry/failover** (backoff, the same model on another configured provider,
  waiting out a quota reset).
- **Wake-ups of an idle session**: a background `bash()` that outlives its
  cell, a goal's backoff and a quota reset start the next turn. `-p` waits for
  them before exiting, at most `--background-wait-ms` per wait (default 10
  minutes, `0` = no wait), then kills the commands still running; in ACP the
  agent starts the turn itself and streams it as `session/update`. A goal
  waiting on a running command has its own wait: it relaunches when the last
  one ends, or every 10 minutes for one that never does.
- **Sessions** saved as append-only JSONL, resumable.
- **Skills** from `~/.unharnessed/skills`, `<project>/.unharnessed/skills`,
  `.agents/skills` in parent directories, and the `skills` setting; `AGENTS.md`
  and `CLAUDE.md` are loaded into the system prompt.
- **All providers and logins** (Anthropic, OpenAI/Codex, Google/Vertex, Bedrock,
  Azure, Mistral, xAI, Prime Inference, ...).

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

Credentials come from the auth store (`~/.unharnessed/auth.json`) or the
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

MIT (see [LICENSE](LICENSE)).
