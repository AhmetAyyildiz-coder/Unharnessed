# Unharnessed for VS Code

A chat panel in the secondary side bar for the Unharnessed agent: sign-in,
settings, model pickers and the conversation, all in the panel.

Unharnessed gives the model one tool, a persistent Python kernel; everything
else the agent does is Python in that kernel.

**The agent runs the Python and the shell commands the model writes, with your
user permissions and no sandbox.** Use it on projects where that is acceptable.

## Requirements

- [uv](https://docs.astral.sh/uv/). The agent builds its Python kernel with it
  the first time, which needs the network once:
  `curl -LsSf https://astral.sh/uv/install.sh | sh`
- An account or an API key with a model provider; sign in from the panel.

The agent itself comes with the extension.

## Conversations in the background

Each open conversation has its own agent process and its own Python kernel.
Switching conversations from the history menu is instant and stops nothing: a
conversation keeps working while another one is in the panel, and several can
work at once. The history menu marks the ones working in the background and the
ones that finished there since you last looked.

A conversation in the background sleeps after 90 minutes without work: its
agent stops and saves its kernel's variables, and opening it again brings them
back. It never sleeps while a turn runs, a goal goes on, a background `bash()`
command runs or messages wait. Closing VS Code stops every agent the same way.

Two conversations work on the same project files with no lock between them,
like two terminals open on one folder.

## Development

- `npm run build`, `npm run typecheck`; F5 opens an Extension Development Host
  with `UNHARNESSED_BIN` pointing at the debug binary.
- `npm run test:e2e` drives a real VS Code through each scenario of
  `test/e2e/run.mjs` with the faux provider (`UNHARNESSED_E2E_ONLY=<name>` runs
  one).
- `npm run package [target]` builds the `.vsix` of one platform, agent
  included (`scripts/package.mjs`);
  `UNHARNESSED_E2E_VSIX=<the .vsix> node test/e2e/run.mjs` runs the scenarios
  on it, installed by VS Code.
