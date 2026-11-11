// Bundles the extension host code into dist/extension.js and the panel's
// React app into dist/webview/ (main.js, main.css, the fonts; the provider
// logos are inlined in main.js).
// `--watch` rebuilds on change; the start/end lines feed the background
// problem matcher in .vscode/tasks.json, once per round of both bundles.
// `--out <dir>` builds there instead of dist/, without the source map: the
// package's build (scripts/package.mjs), which leaves dist/ to the watcher.
import * as esbuild from "esbuild";

const watch = process.argv.includes("--watch");
const outFlag = process.argv.indexOf("--out");
const out = outFlag < 0 ? "dist" : process.argv[outFlag + 1];

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
    outfile: `${out}/extension.js`,
    format: "cjs",
    platform: "node",
    target: "node20",
    external: ["vscode"],
    sourcemap: outFlag < 0,
  }),
  esbuild.context({
    ...common,
    entryPoints: { main: "webview/main.tsx" },
    outdir: `${out}/webview`,
    format: "iife",
    platform: "browser",
    target: "chrome120",
    jsx: "automatic",
    // The provider logos are inlined as SVG markup.
    loader: { ".woff2": "file", ".woff": "file", ".svg": "text" },
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
