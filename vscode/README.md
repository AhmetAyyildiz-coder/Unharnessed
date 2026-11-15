<p align="center">
  <img src="https://raw.githubusercontent.com/unharnessed-agent/Unharnessed/main/.github/banner.png" alt="Unharnessed" width="100%">
</p>

<h3 align="center">A new generation of harness.</h3>

<p align="center">
  <b>One tool: a Python REPL. The model writes the rest.</b>
</p>

<p align="center">
  <a href="https://unharnessed.dev"><b>Website</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/unharnessed-agent/Unharnessed"><b>GitHub</b></a>
  &nbsp;·&nbsp;
  <a href="https://github.com/unharnessed-agent/Unharnessed/issues"><b>Issues</b></a>
</p>

---

Unharnessed is a coding agent built on one idea: **the model gets exactly one
tool**, a persistent Python REPL. There is no `read`, no `grep`, no
`edit_file`. Reading a file, searching a repository, starting a dev server or
calling an MCP service: all of it is Python the model writes, in a kernel that
stays alive for the whole session.

**The agent runs the Python and the shell commands the model writes, with your
user permissions and no sandbox.** It never asks before running a command. Use
it in a container, a VM or a disposable checkout.

## In the panel

The extension puts the agent in the secondary side bar:

- **Sign-in and model pickers.** Use the subscription you already pay for, an
  API key, or a local model.
- **A kernel panel** that shows the running `bash()` commands, with their live
  tail and a stop button, and the variables in the kernel.
- **A context ring** next to send: how full the window is, and when a
  compaction is coming.
- **Goals**: give the agent an objective and let it work until it is done.

## Requirements

- [uv](https://docs.astral.sh/uv/). The agent builds its Python kernel with it
  the first time, which needs the network once:
  `curl -LsSf https://astral.sh/uv/install.sh | sh`
- An account or an API key with a model provider; sign in from the panel.

The agent itself comes with the extension.

## Conversations in the background

Each open conversation has its own agent process and its own Python kernel.
Switching conversations stops nothing: a conversation keeps working while
another one is in the panel, and several can work at once. The history menu
marks the ones working and the ones that finished since you last looked.

A conversation in the background sleeps after 90 minutes without work: its
agent stops and saves its kernel's variables, and opening it again brings them
back. It never sleeps while a turn, a goal or a `bash()` command runs.

Two conversations work on the same project files with no lock between them,
like two terminals open on one folder.

## More

How the kernel works, the three ways to run, compaction, providers and
failover: [unharnessed.dev](https://unharnessed.dev) and the
[README on GitHub](https://github.com/unharnessed-agent/Unharnessed#readme).
