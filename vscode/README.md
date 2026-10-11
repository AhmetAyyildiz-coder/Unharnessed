# Unharnessed for VS Code

A chat panel in the secondary side bar for the Unharnessed agent: sign-in,
settings, model pickers and the conversation, all in the panel.

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
