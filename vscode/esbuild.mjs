// Bundles the extension host code into dist/extension.js.
// `--watch` rebuilds on change; the start/end lines feed the
// background problem matcher in .vscode/tasks.json.
import * as esbuild from "esbuild";

const watch = process.argv.includes("--watch");

const logPlugin = {
  name: "log",
  setup(build) {
    build.onStart(() => console.log("[watch] build started"));
    build.onEnd((result) => {
      for (const { text, location } of result.errors) {
        const where = location ? `${location.file}:${location.line}:${location.column + 1}` : "esbuild";
        console.error(`error: ${where}: ${text}`);
      }
      console.log("[watch] build finished");
    });
  },
};

const context = await esbuild.context({
  entryPoints: ["src/extension.ts"],
  bundle: true,
  outfile: "dist/extension.js",
  format: "cjs",
  platform: "node",
  target: "node20",
  external: ["vscode"],
  sourcemap: true,
  logLevel: "silent",
  plugins: [logPlugin],
});

if (watch) {
  await context.watch();
} else {
  // A failed build rejects; the plugin has already printed the errors.
  const ok = await context.rebuild().then(() => true, () => false);
  await context.dispose();
  if (!ok) process.exit(1);
}
