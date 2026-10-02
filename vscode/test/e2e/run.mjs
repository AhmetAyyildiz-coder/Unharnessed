// Launches a real VS Code with the extension in an isolated profile and runs suite.cjs.
// UNHARNESSED_VSCODE_EXECUTABLE picks an installed VS Code; without it, VS Code stable is
// downloaded into .vscode-test/.
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import { runTests } from "@vscode/test-electron";

const here = dirname(fileURLToPath(import.meta.url));
// Short names: VS Code puts its IPC socket in the user data dir (103-char limit on macOS).
const profile = mkdtempSync(join(tmpdir(), "uh-e2e-"));
const userDataDir = join(profile, "u");

try {
  await runTests({
    vscodeExecutablePath: process.env.UNHARNESSED_VSCODE_EXECUTABLE,
    extensionDevelopmentPath: resolve(here, "../.."),
    extensionTestsPath: join(here, "suite.cjs"),
    extensionTestsEnv: { UNHARNESSED_E2E_LOGS: join(userDataDir, "logs") },
    launchArgs: [
      `--user-data-dir=${userDataDir}`,
      `--extensions-dir=${join(profile, "x")}`,
      "--disable-extensions",
      "--disable-workspace-trust",
      "--skip-welcome",
      "--skip-release-notes",
    ],
  });
} catch (error) {
  console.error(`e2e failed: ${error instanceof Error ? error.message : error}`);
  console.error(`profile kept for inspection: ${profile}`);
  process.exit(1);
}
