// Runs inside the VS Code extension host. Opens the chat view and waits for the
// webview script to report back, which the extension logs to its output channel.
const fs = require("node:fs");
const path = require("node:path");
const vscode = require("vscode");

const TIMEOUT_MS = 20_000;

exports.run = async function run() {
  await vscode.commands.executeCommand("unharnessed.chat.focus");

  const line = await waitForLogLine(process.env.UNHARNESSED_E2E_LOGS, "webview ready");
  console.log(`ok: ${line.trim()}`);
};

async function waitForLogLine(logsDir, needle) {
  const deadline = Date.now() + TIMEOUT_MS;
  while (Date.now() < deadline) {
    for (const file of findFiles(logsDir, "Unharnessed.log")) {
      const line = fs.readFileSync(file, "utf8").split("\n").find((l) => l.includes(needle));
      if (line) return line;
    }
    await sleep(250);
  }
  throw new Error(`no "${needle}" in Unharnessed.log under ${logsDir} after ${TIMEOUT_MS} ms`);
}

function findFiles(dir, name) {
  if (!dir || !fs.existsSync(dir)) return [];
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) return findFiles(full, name);
    return entry.name === name ? [full] : [];
  });
}

function sleep(ms) {
  return new Promise((resolve) => setTimeout(resolve, ms));
}
