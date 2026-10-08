// Copies VS Code's default file icons (the built-in Seti theme, MIT) into the
// panel: the font to media/fonts/, and to webview/seti.json the icons and
// what picks them. VS Code picks an icon by the file's name, then its
// extension, then its language; the languages' names and extensions come
// from VS Code's built-in language extensions, so the panel needs no
// language lookup of its own.
// Run once per VS Code update, from vscode/:
//   node scripts/seti.mjs "/Applications/Visual Studio Code.app/Contents/Resources/app"
import { copyFileSync, readdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";

const app = process.argv[2];
if (!app) {
  console.error("usage: node scripts/seti.mjs <VS Code's Resources/app folder>");
  process.exit(1);
}
const extensions = join(app, "extensions");
const seti = join(extensions, "theme-seti");
const theme = JSON.parse(readFileSync(join(seti, "icons", "vs-seti-icon-theme.json"), "utf8"));

// Each icon once: its glyph, its color on a dark theme, then on a light one.
const icons = [];
const index = new Map();
const iconOf = (id) => {
  if (!index.has(id)) {
    const glyph = (def) => String.fromCodePoint(parseInt(def.fontCharacter.slice(1), 16));
    const dark = theme.iconDefinitions[id];
    const light = theme.iconDefinitions[`${id}_light`] ?? dark;
    index.set(id, icons.length);
    icons.push([glyph(dark), dark.fontColor, light.fontColor]);
  }
  return index.get(id);
};
const table = (entries) =>
  Object.fromEntries(Object.entries(entries).map(([key, id]) => [key.toLowerCase(), iconOf(id)]));

// A language's names and extensions, the first language claiming one keeps it.
const langNames = {};
const langExts = {};
for (const folder of readdirSync(extensions).sort()) {
  let manifest;
  try {
    manifest = JSON.parse(readFileSync(join(extensions, folder, "package.json"), "utf8"));
  } catch {
    continue;
  }
  for (const language of manifest.contributes?.languages ?? []) {
    const id = theme.languageIds[language.id];
    if (!id) continue;
    for (const name of language.filenames ?? []) langNames[name.toLowerCase()] ??= iconOf(id);
    for (const ext of language.extensions ?? []) langExts[ext.toLowerCase().replace(/^\./, "")] ??= iconOf(id);
  }
}

const version = JSON.parse(readFileSync(join(app, "package.json"), "utf8")).version;
const out = {
  source: `VS Code ${version}, theme-seti`,
  file: iconOf(theme.file),
  names: table(theme.fileNames),
  exts: table(theme.fileExtensions),
  langNames,
  langExts,
  icons,
};
writeFileSync("webview/seti.json", `${JSON.stringify(out)}\n`);
copyFileSync(join(seti, "icons", "seti.woff"), "media/fonts/seti.woff");
copyFileSync(join(seti, "ThirdPartyNotices.txt"), "media/fonts/Seti-LICENSE.txt");
console.log(
  `seti: ${icons.length} icons, ${Object.keys(out.names).length} names, ${Object.keys(out.exts).length} extensions, ` +
    `${Object.keys(langNames).length} language names, ${Object.keys(langExts).length} language extensions (VS Code ${version})`,
);
