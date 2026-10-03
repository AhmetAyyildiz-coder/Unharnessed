// Launches a real VS Code with the extension once per scenario, each in a
// throwaway profile, and runs suite.cjs. The agent's data (agent dir, kernel
// venv, cwd) lives in that profile too; a scenario's `auth` is written to the
// agent dir's auth.json first, `settings` to its settings.json, `catalog` seeds the agent's model catalog cache
// with the uh-models test fixture, and `env` adds to the agent's environment
// (`UNHARNESSED_OFFLINE` keeps the catalog off the network). Provider keys and tokens from the caller's
// environment are dropped. After VS Code exits, checks that no agent process
// (or anything else started in the profile) is left behind.
// UNHARNESSED_VSCODE_EXECUTABLE picks an installed VS Code; without it, VS Code
// stable is downloaded into .vscode-test/. UNHARNESSED_BIN picks the agent
// binary, default target/debug/unharnessed.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createRequire } from "node:module";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const { readLog } = createRequire(import.meta.url)("./logs.cjs");
const here = dirname(fileURLToPath(import.meta.url));
const binary = process.env.UNHARNESSED_BIN || resolve(here, "../../../target/debug/unharnessed");
// VS Code, and the agent it starts, inherit this environment.
for (const name of Object.keys(process.env)) {
  if (/_(API_KEY|TOKEN)$/.test(name)) delete process.env[name];
}

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
      '→ {"jsonrpc":"2.0","id":1,"method":"_unharnessed/providers/list"',
      "providers: ",
      "connected: none",
      // Nothing connected: the providers screen opens first.
      "panel: providers",
    ],
  },
  {
    name: "provider connected",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    expect: ["agent ready: unharnessed", "connected: openai (apiKey)", "panel: status"],
  },
  {
    name: "model pickers",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    catalog: true,
    env: { UNHARNESSED_OFFLINE: "1" },
    expect: [
      "connected: openai (apiKey)",
      '"method":"session/new"',
      // The fixture's 44 OpenAI models; gpt-5.4 is OpenAI's default, with an effort and a fast mode.
      "session: opened, model openai/gpt-5.4 (44 choices), effort medium, fast off",
      "panel: status",
    ],
  },
  {
    name: "settings",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    settings: {
      compaction: { reserveTokens: 32000 },
      theme: "dark",
      mcpServers: { docs: { type: "stdio", command: "echo" } },
    },
    expect: [
      '"method":"_unharnessed/settings/get"',
      // The removed `theme` key is not a setting: `compaction` and `mcpServers` are.
      "settings: read, 2 set",
      // A server reached by a command needs no sign-in; the built-in services wait for one.
      "mcp: docs (connected), linear (not_connected), notion (not_connected)",
      // The built-in skills at least (the caller's ~/.agents/skills may add some).
      "skills: read, ",
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
  const agentDir = join(profile, "a");
  mkdirSync(workspace);
  mkdirSync(agentDir);
  if (scenario.auth) writeFileSync(join(agentDir, "auth.json"), JSON.stringify(scenario.auth));
  if (scenario.settings) writeFileSync(join(agentDir, "settings.json"), JSON.stringify(scenario.settings));
  if (scenario.catalog) seedCatalog(agentDir);
  try {
    await runTests({
      vscodeExecutablePath: process.env.UNHARNESSED_VSCODE_EXECUTABLE,
      extensionDevelopmentPath: resolve(here, "../.."),
      extensionTestsPath: join(here, "suite.cjs"),
      extensionTestsEnv: {
        UNHARNESSED_E2E_LOGS: logsDir,
        UNHARNESSED_E2E_EXPECT: JSON.stringify(scenario.expect),
        UNHARNESSED_BIN: scenario.binary,
        UNHARNESSED_CODING_AGENT_DIR: agentDir,
        UNHARNESSED_KERNEL_VENV: join(profile, "v"),
        DO_NOT_TRACK: "1",
        ...scenario.env,
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

/** The agent's catalog cache, as a fetch of the catalog would have left it. */
function seedCatalog(agentDir) {
  const payload = JSON.parse(readFileSync(resolve(here, "../../../crates/uh-models/tests/fixtures/catalog.v1.json"), "utf8"));
  const url = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json";
  mkdirSync(join(agentDir, "models"));
  const cache = { url, scope: "public", fetchedAt: Date.now(), payload };
  writeFileSync(join(agentDir, "models", "provider-model-catalog.v1.json"), JSON.stringify(cache));
}

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
