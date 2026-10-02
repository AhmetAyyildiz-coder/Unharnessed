// Bundles the extension host code into dist/extension.js and the panel's
// React app into dist/webview/ (main.js, main.css, the font; the provider
// logos are inlined in main.js).
// `--watch` rebuilds on change; the start/end lines feed the background
// problem matcher in .vscode/tasks.json, once per round of both bundles.
import * as esbuild from "esbuild";

const watch = process.argv.includes("--watch");

let running = 0;
const logPlugin = {
  name: "log",
  setup(build) {
    build.onStart(() => {
      if (running++ === 0) console.log("[watch] build started");
    });
    build.onEnd((result) => {
      for (const { text, location } of result.errors) {
        const where = location ? `${location.file}:${location.line}:${location.column + 1}` : "esbuild";
        console.error(`error: ${where}: ${text}`);
      }
      if (--running === 0) console.log("[watch] build finished");
    });
  },
};

const common = { bundle: true, logLevel: "silent", plugins: [logPlugin] };

const contexts = await Promise.all([
  esbuild.context({
    ...common,
    entryPoints: ["src/extension.ts"],
    outfile: "dist/extension.js",
    format: "cjs",
    platform: "node",
    target: "node20",
    external: ["vscode"],
    sourcemap: true,
  }),
  esbuild.context({
    ...common,
    entryPoints: { main: "webview/main.tsx" },
    outdir: "dist/webview",
    format: "iife",
    platform: "browser",
    target: "chrome120",
    jsx: "automatic",
    // The provider logos are inlined as SVG markup.
    loader: { ".woff2": "file", ".svg": "text" },
    assetNames: "[name]",
    define: { "process.env.NODE_ENV": JSON.stringify(watch ? "development" : "production") },
    minify: !watch,
    sourcemap: watch ? "inline" : false,
  }),
]);

if (watch) {
  await Promise.all(contexts.map((context) => context.watch()));
} else {
  // A failed build rejects; the plugin has already printed the errors.
  const results = await Promise.all(contexts.map((context) => context.rebuild().then(() => true, () => false)));
  await Promise.all(contexts.map((context) => context.dispose()));
  if (results.includes(false)) process.exit(1);
}
