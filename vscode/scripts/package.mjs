// Builds the extension's package for one platform: a .vsix that carries the
// agent binary, so that installing it is enough.
//   node scripts/package.mjs [target]     (from vscode/; default: this machine)
// `target` is a VS Code platform, one of TARGETS. The agent is built for it
// with cargo (a platform other than this machine's needs its Rust target and
// linker), then staged with the panel in a throwaway folder, which is the
// extension exactly as shipped:
//   package.json README.md LICENSE media/
//   dist/                     the production build (dist/ here stays the watcher's)
//   bin/unharnessed           the agent, and beside it what it looks for there:
//   bin/unharnessed-runtime/  the kernel's Python package
//   bin/skills/               the built-in skills
// Of the runtime and the skills, the files git tracks are shipped.
// The .vsix lands in vscode/, named unharnessed-<target>-<version>.vsix.
import { execFileSync } from "node:child_process";
import { chmodSync, copyFileSync, cpSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const VSCE = "@vscode/vsce@4.1.0";
const TARGETS = {
  "darwin-arm64": "aarch64-apple-darwin",
  "darwin-x64": "x86_64-apple-darwin",
  "linux-x64": "x86_64-unknown-linux-musl",
  "linux-arm64": "aarch64-unknown-linux-musl",
  "win32-x64": "x86_64-pc-windows-msvc",
};

const extension = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repo = resolve(extension, "..");
const run = (command, args, cwd) => execFileSync(command, args, { cwd, stdio: "inherit", shell: process.platform === "win32" });

const target = process.argv[2] ?? `${process.platform}-${process.arch}`;
const triple = TARGETS[target];
if (!triple) {
  console.error(`unknown target ${target}; one of: ${Object.keys(TARGETS).join(", ")}`);
  process.exit(1);
}

// This machine's own platform builds into target/release, like `cargo build --release`.
const host = /^host: (.+)$/m.exec(execFileSync("rustc", ["-vV"], { cwd: repo, encoding: "utf8" }))[1];
const cross = triple === host ? [] : ["--target", triple];
run("cargo", ["build", "--release", "--locked", "-p", "uh-cli", "--bin", "unharnessed", ...cross], repo);
const exe = target.startsWith("win32") ? "unharnessed.exe" : "unharnessed";
const built = join(repo, "target", ...cross.slice(1), "release", exe);

const stage = mkdtempSync(join(tmpdir(), "uh-package-"));
try {
  for (const file of ["package.json", "README.md", "media/icon.svg", "media/icon-light.svg", "media/icon-dark.svg", "media/icon.png", "media/fonts/OFL.txt", "media/fonts/Seti-LICENSE.txt"]) {
    mkdirSync(dirname(join(stage, file)), { recursive: true });
    copyFileSync(join(extension, file), join(stage, file));
  }
  copyFileSync(join(repo, "LICENSE"), join(stage, "LICENSE"));
  run("node", ["esbuild.mjs", "--out", join(stage, "dist")], extension);

  const bin = join(stage, "bin");
  mkdirSync(bin);
  copyFileSync(built, join(bin, exe));
  chmodSync(join(bin, exe), 0o755);
  const tracked = execFileSync("git", ["ls-files", "-z", "--", "unharnessed-runtime/pyproject.toml", "unharnessed-runtime/src", "skills"], { cwd: repo, encoding: "utf8" });
  for (const file of tracked.split("\0").filter(Boolean)) {
    mkdirSync(dirname(join(bin, file)), { recursive: true });
    cpSync(join(repo, file), join(bin, file));
  }

  const { version } = JSON.parse(readFileSync(join(stage, "package.json"), "utf8"));
  const vsix = join(extension, `unharnessed-${target}-${version}.vsix`);
  run("npx", ["--yes", VSCE, "package", "--no-dependencies", "--target", target, "--out", vsix], stage);
  console.log(`\n${vsix}`);
} finally {
  rmSync(stage, { recursive: true, force: true });
}
