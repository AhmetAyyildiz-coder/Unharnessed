// Runs inside the VS Code extension host. Opens the panel, then goes through
// UNHARNESSED_E2E_STEPS (a JSON list): a string waits for a line containing it
// in the extension's log channel; `{ wait, times, timeout }` waits for its
// `times`-th such line; `{ wait, capture: { name: regex } }` keeps what each
// regex's first group finds in that line; `{ post }` sends a message as the
// panel would, `${name}` in its strings standing for what was kept.
const vscode = require("vscode");
const { readLog } = require("./logs.cjs");

const TIMEOUT_MS = 20_000;

exports.run = async function run() {
  await vscode.commands.executeCommand("unharnessed.chat.focus");

  const kept = {};
  for (const step of JSON.parse(process.env.UNHARNESSED_E2E_STEPS)) {
    if (typeof step === "object" && step.post) {
      const post = JSON.parse(
        JSON.stringify(step.post).replace(/\$\{(\w+)\}/g, (_, name) => {
          if (!(name in kept)) throw new Error(`nothing kept as ${name}`);
          return kept[name];
        }),
      );
      await vscode.commands.executeCommand("unharnessed.e2e.post", post);
      console.log(`post: ${JSON.stringify(post)}`);
      continue;
    }
    const { wait, times = 1, timeout = TIMEOUT_MS, capture = {} } = typeof step === "string" ? { wait: step } : step;
    const line = await waitForLogLine(process.env.UNHARNESSED_E2E_LOGS, wait, times, timeout);
    console.log(`ok: ${line.trim()}`);
    for (const [name, regex] of Object.entries(capture)) {
      const found = new RegExp(regex).exec(line)?.[1];
      if (found === undefined) throw new Error(`/${regex}/ finds nothing in: ${line.trim()}`);
      kept[name] = found;
      console.log(`kept: ${name} = ${found}`);
    }
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
