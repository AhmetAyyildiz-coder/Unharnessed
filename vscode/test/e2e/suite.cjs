// Runs inside the VS Code extension host. Opens the panel, then waits for each
// line of UNHARNESSED_E2E_EXPECT (a JSON list) in the extension's log channel.
const vscode = require("vscode");
const { readLog } = require("./logs.cjs");

const TIMEOUT_MS = 20_000;

exports.run = async function run() {
  await vscode.commands.executeCommand("unharnessed.chat.focus");

  for (const needle of JSON.parse(process.env.UNHARNESSED_E2E_EXPECT)) {
    const line = await waitForLogLine(process.env.UNHARNESSED_E2E_LOGS, needle);
    console.log(`ok: ${line.trim()}`);
  }
};

async function waitForLogLine(logsDir, needle) {
  const deadline = Date.now() + TIMEOUT_MS;
  while (Date.now() < deadline) {
    const line = readLog(logsDir).find((l) => l.includes(needle));
    if (line) return line;
    await sleep(250);
  }
  throw new Error(`no "${needle}" in Unharnessed.log under ${logsDir} after ${TIMEOUT_MS} ms`);
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
