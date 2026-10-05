// Runs inside the VS Code extension host. Opens the panel, then goes through
// UNHARNESSED_E2E_STEPS (a JSON list): a string waits for a line containing it
// in the extension's log channel; `{ wait, times, timeout }` waits for its
// `times`-th such line; `{ post }` sends a message as the panel would.
const vscode = require("vscode");
const { readLog } = require("./logs.cjs");

const TIMEOUT_MS = 20_000;

exports.run = async function run() {
  await vscode.commands.executeCommand("unharnessed.chat.focus");

  for (const step of JSON.parse(process.env.UNHARNESSED_E2E_STEPS)) {
    if (typeof step === "object" && step.post) {
      await vscode.commands.executeCommand("unharnessed.e2e.post", step.post);
      console.log(`post: ${JSON.stringify(step.post)}`);
      continue;
    }
    const { wait, times = 1, timeout = TIMEOUT_MS } = typeof step === "string" ? { wait: step } : step;
    const line = await waitForLogLine(process.env.UNHARNESSED_E2E_LOGS, wait, times, timeout);
    console.log(`ok: ${line.trim()}`);
  }
};

async function waitForLogLine(logsDir, needle, times, timeout) {
  const deadline = Date.now() + timeout;
  while (Date.now() < deadline) {
    const line = readLog(logsDir).filter((l) => l.includes(needle))[times - 1];
    if (line) return line;
    await sleep(250);
  }
  const which = times > 1 ? ` ${times} times` : "";
  throw new Error(`no "${needle}"${which} in Unharnessed.log under ${logsDir} after ${timeout} ms`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
