// Launches a real VS Code with the extension once per scenario, each in a
// throwaway profile, and runs suite.cjs. The agent's data (agent dir, kernel
// venv, cwd) lives in that profile too. After VS Code exits, checks that no
// agent process (or anything else started in the profile) is left behind.
// UNHARNESSED_VSCODE_EXECUTABLE picks an installed VS Code; without it, VS Code
// stable is downloaded into .vscode-test/. UNHARNESSED_BIN picks the agent
// binary, default target/debug/unharnessed.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, rmSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const { readLog } = createRequire(import.meta.url)("./logs.cjs");
const here = dirname(fileURLToPath(import.meta.url));
const binary = process.env.UNHARNESSED_BIN || resolve(here, "../../../target/debug/unharnessed");

const scenarios = [
  {
    name: "agent ready",
    binary,
    expect: [
      "webview ready",
      "panel: splash",
      "agent started (pid",
      '→ {"jsonrpc":"2.0","id":0,"method":"initialize"',
      "agent ready: unharnessed",
      "panel: status",
    ],
  },
  {
    name: "binary missing",
    binary: "/nonexistent/unharnessed",
    expect: ["webview ready", "agent failed: spawn /nonexistent/unharnessed ENOENT", "panel: status"],
  },
  {
    name: "agent exits at startup",
    binary: "/usr/bin/false",
    expect: ["webview ready", "agent started (pid", "agent failed: agent exited (code 1)", "panel: status"],
  },
];

let failed = 0;
for (const scenario of scenarios) {
  console.log(`\n# ${scenario.name} (${scenario.binary})`);
  // Short names: VS Code puts its IPC socket in the user data dir (103-char limit on macOS).
  const profile = mkdtempSync(join(tmpdir(), "uh-e2e-"));
  const userDataDir = join(profile, "u");
  const logsDir = join(userDataDir, "logs");
  const workspace = join(profile, "w");
  mkdirSync(workspace);
  try {
    await runTests({
      vscodeExecutablePath: process.env.UNHARNESSED_VSCODE_EXECUTABLE,
      extensionDevelopmentPath: resolve(here, "../.."),
      extensionTestsPath: join(here, "suite.cjs"),
      extensionTestsEnv: {
        UNHARNESSED_E2E_LOGS: logsDir,
        UNHARNESSED_E2E_EXPECT: JSON.stringify(scenario.expect),
        UNHARNESSED_BIN: scenario.binary,
        UNHARNESSED_CODING_AGENT_DIR: join(profile, "a"),
        UNHARNESSED_KERNEL_VENV: join(profile, "v"),
        DO_NOT_TRACK: "1",
      },
      launchArgs: [
        workspace,
        `--user-data-dir=${userDataDir}`,
        `--extensions-dir=${join(profile, "x")}`,
        "--disable-extensions",
        "--disable-workspace-trust",
        "--skip-welcome",
        "--skip-release-notes",
      ],
    });
    await checkNothingLeft(profile, logsDir);
    console.log(`pass: ${scenario.name}`);
    rmSync(profile, { recursive: true, force: true });
  } catch (error) {
    failed++;
    console.error(`FAIL: ${scenario.name}: ${error instanceof Error ? error.message : error}`);
    console.error(`profile kept for inspection: ${profile}`);
  }
}
process.exit(failed ? 1 : 0);

async function checkNothingLeft(profile, logsDir) {
  const pids = readLog(logsDir).flatMap((line) => [...line.matchAll(/agent started \(pid (\d+)\)/g)].map((m) => +m[1]));
  for (const pid of pids) {
    if (!(await exitsWithin(pid, 5_000))) throw new Error(`agent pid ${pid} still running after VS Code exited`);
    console.log(`ok: agent pid ${pid} gone after VS Code exited`);
  }
  const left = processesMentioning(profile);
  if (left) throw new Error(`processes still running from ${profile}:\n${left}`);
  console.log("ok: no process left running from the profile");
}

async function exitsWithin(pid, ms) {
  const deadline = Date.now() + ms;
  while (Date.now() < deadline) {
    try {
      process.kill(pid, 0);
    } catch {
      return true;
    }
    await new Promise((resolve) => setTimeout(resolve, 100));
  }
  return false;
}

function processesMentioning(text) {
  try {
    return execFileSync("pgrep", ["-fl", text], { encoding: "utf8" }).trim();
  } catch {
    return ""; // pgrep exits 1 when nothing matches
  }
}
