// Reads the extension's "Unharnessed" log channel from a VS Code logs dir.
const fs = require("node:fs");
const path = require("node:path");

function findFiles(dir, name) {
  if (!dir || !fs.existsSync(dir)) return [];
  return fs.readdirSync(dir, { withFileTypes: true }).flatMap((entry) => {
    const full = path.join(dir, entry.name);
    if (entry.isDirectory()) return findFiles(full, name);
    return entry.name === name ? [full] : [];
  });
}

/** Every line of every Unharnessed.log under `logsDir`. */
function readLog(logsDir) {
  return findFiles(logsDir, "Unharnessed.log").flatMap((file) => fs.readFileSync(file, "utf8").split("\n"));
}

module.exports = { readLog };
