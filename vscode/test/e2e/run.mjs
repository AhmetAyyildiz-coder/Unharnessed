// Launches a real VS Code with the extension once per scenario, each in a
// throwaway profile, and runs suite.cjs. The agent's data (agent dir, kernel
// venv, cwd) lives in that profile too; a scenario's `auth` is written to the
// agent dir's auth.json first, `settings` to its settings.json, `catalog` seeds the agent's model catalog cache
// with the uh-models test fixture, `mcpCatalog` its MCP catalog cache (the Marketplace) with the uh-core one,
// both fresh so that nothing is fetched, and `env` adds to the agent's environment
// (`UNHARNESSED_OFFLINE` keeps the catalog off the network), `workspace` writes
// files into the opened folder, `git` makes it a repository. `steps` are the
// log lines to wait for, in order, and the panel messages to send between
// them (suite.cjs). Provider keys and tokens from the caller's environment
// are dropped. After VS Code exits, checks that no agent process
// (or anything else started in the profile) is left behind.
// UNHARNESSED_VSCODE_EXECUTABLE picks an installed VS Code; without it, VS Code
// stable is downloaded into .vscode-test/. UNHARNESSED_BIN picks the agent
// binary, default target/debug/unharnessed. UNHARNESSED_E2E_ONLY runs the
// scenario of that name alone.
import { execFileSync } from "node:child_process";
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from "node:fs";
import { createServer } from "node:http";
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

// The chat scenario's model: the faux provider, one response per model call.
const CHAT_SCRIPT = {
  tokensPerSecond: 40,
  responses: [
    {
      content: [
        { type: "thinking", thinking: "Multiplying." },
        { type: "text", text: "Let me compute it." },
        { type: "toolCall", name: "ipython", arguments: { code: "print(6 * 7)" } },
      ],
    },
    "It is **42**.",
    // Long enough to stop (2.5 s at the pace above); a stop before the model answers leaves it to the next turn.
    "word ".repeat(80),
    "first answer",
    "second answer",
    "a single pixel",
  ],
};

// The goal scenario's model: a long answer, which the pause stops; after the
// play, the goal is reached and said so.
const GOAL_SCRIPT = {
  tokensPerSecond: 40,
  responses: [
    "word ".repeat(80),
    { content: [{ type: "toolCall", name: "ipython", arguments: { code: "await goal.complete()" } }] },
    "Counted to three.",
    "You are welcome.",
    "word ".repeat(80),
  ],
};

// The local model scenario's server, on this machine: it answers as Ollama
// does, with one chat model, and streams one answer.
const ollama = createServer((request, response) => {
  request.resume();
  request.on("end", () => {
    const json = (code, value) => {
      response.writeHead(code, { "Content-Type": "application/json" });
      response.end(JSON.stringify(value));
    };
    const route = `${request.method} ${request.url}`;
    if (route === "GET /v1/models") {
      return json(200, { object: "list", data: [{ id: "tiny:1b", object: "model", created: 0, owned_by: "library" }] });
    }
    if (route === "GET /api/version") return json(200, { version: "0.0.0-e2e" });
    if (route === "POST /api/show") {
      return json(200, { capabilities: ["completion"], model_info: { "general.architecture": "tiny", "tiny.context_length": 8192 } });
    }
    if (route === "POST /v1/chat/completions") {
      response.writeHead(200, { "Content-Type": "text/event-stream" });
      const head = { id: "e2e", object: "chat.completion.chunk", created: 0, model: "tiny:1b" };
      for (const chunk of [
        { choices: [{ index: 0, delta: { role: "assistant", content: "Hello from the local model." } }] },
        { choices: [{ index: 0, delta: {}, finish_reason: "stop" }] },
      ]) {
        response.write(`data: ${JSON.stringify({ ...head, ...chunk })}\n\n`);
      }
      return response.end("data: [DONE]\n\n");
    }
    return json(404, { error: "not found" });
  });
});
await new Promise((resolve) => ollama.listen(0, "127.0.0.1", resolve));
const OLLAMA = `127.0.0.1:${ollama.address().port}`;

// The edit scenario's model: one answer per prompt.
const EDIT_SCRIPT = { responses: ["first answer", "second answer", "the changed answer"] };

// The compaction scenario's model: two answers, then the summary of the
// compaction that follows the second, slow enough to show while it runs.
const COMPACTION_SCRIPT = {
  tokensPerSecond: 40,
  responses: ["It is **42**.", "It is **48**.", "## Goal\nMultiply.\n\n## Done\n- 6 times 7 is 42."],
};

// The kernel panel scenario's model: a cell that leaves a server running in
// the background and makes some variables, then an answer per wake.
const KERNEL_CELL = [
  "n = 42",
  'cfg = {"model": "claude", "retries": 3}',
  'server = bash("exec python3 -u -m http.server 0 --bind 127.0.0.1")',
  "server.pid",
  'fg = await bash("echo foreground-only")',
].join("\n");
const KERNEL_SCRIPT = {
  responses: [
    { content: [{ type: "toolCall", name: "ipython", arguments: { code: KERNEL_CELL } }] },
    "The server runs.",
    "It stopped.",
    "Noted.",
  ],
};

// The kernel restored scenario's model: a cell that makes a variable, then
// an answer.
const RESTORE_SCRIPT = {
  responses: [{ content: [{ type: "toolCall", name: "ipython", arguments: { code: "n = 42" } }] }, "Kept."],
};

// The cells scenario's model: one cell of each kind, run for real, then an
// answer. The written file's text holds code a cell would run: it is not
// this cell's.
const CELLS = [
  'path = "src/app.ts"\ns = open(path).read()\nassert "a = 1" in s\ns = s.replace("a = 1", "a = 2")\nopen(path, "w").write(s)',
  "code = r'''import { x } from \"y\";\nbash(\"npm publish\")  # not run\nconst s = open(\"z\").read();\n'''\nwith open(\"src/new.ts\", \"w\") as f:\n    f.write(code)",
  'from pathlib import Path\nprint(Path("src/app.ts").read_text())',
  "r = await bash(\"grep -rn 'a = 2' src\")\nprint(r.output)",
  'r = await bash("echo built")\nprint(r.output)',
  "print(6 * 7)",
];
const CELLS_SCRIPT = {
  responses: [...CELLS.map((code) => ({ content: [{ type: "toolCall", name: "ipython", arguments: { code } }] })), "Done."],
};

// The cell results scenario's model: an edit whose text is not there,
// then one that replaces two texts (one of them dedented, one with escapes),
// then a read out of the agent's folder.
const EDITS = [
  'path = "src/app.ts"\ns = open(path).read()\nold = "export const b = 9;"\nassert old in s\nopen(path, "w").write(s.replace(old, "export const b = 2;"))',
  [
    "from textwrap import dedent",
    'path = "src/app.ts"',
    "s = open(path).read()",
    'old = dedent("""\\',
    "    export const b = 1;",
    "    export const c = 2;",
    '""")',
    'new = dedent("""\\',
    "    export const b = 2;",
    "    export const c = 2;",
    '""")',
    "assert old in s",
    "s = s.replace(old, new)",
    "s = s.replace('from \"./a\";\\n', 'from \"./a\";\\nimport { d } from \"./d\";\\n')",
    'open(path, "w").write(s)',
  ].join("\n"),
  'print(open("/etc/hosts").read()[:40])',
];
const EDITS_SCRIPT = {
  responses: [...EDITS.map((code) => ({ content: [{ type: "toolCall", name: "ipython", arguments: { code } }] })), "Done."],
};

// A 1x1 PNG, as the panel attaches it.
const PIXEL_DATA =
  "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==";
const PIXEL = { data: PIXEL_DATA, mimeType: "image/png", thumb: `data:image/png;base64,${PIXEL_DATA}`, name: "pixel.png" };

const scenarios = [
  {
    name: "agent ready",
    binary,
    steps: [
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
    steps: ["agent ready: unharnessed", "connected: openai (apiKey)", "panel: status"],
  },
  {
    name: "model pickers",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    catalog: true,
    env: { UNHARNESSED_OFFLINE: "1" },
    steps: [
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
    mcpCatalog: true,
    steps: [
      '"method":"_unharnessed/settings/get"',
      // The removed `theme` key is not a setting: `compaction` and `mcpServers` are.
      "settings: read, 2 set",
      // A server reached by a command needs no sign-in; the catalog's services wait in the Marketplace.
      "mcp: docs (connected); 68 in the Marketplace",
      // The built-in skills at least (the caller's ~/.agents/skills may add some).
      "skills: read, ",
      "panel: status",
    ],
  },
  {
    name: "instructions",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    catalog: true,
    env: { UNHARNESSED_OFFLINE: "1" },
    settings: {
      modelInstructions: [
        // Anthropic is not connected: Claude is hidden, and stays.
        { models: ["anthropic/claude-opus-4-7", "openai/gpt-5.4", "openai/gpt-4.1"], text: "Be terse." },
        // No model connected: the whole group is hidden, and stays.
        { models: ["anthropic/claude-sonnet-4-6"], text: "Hidden group." },
      ],
    },
    steps: [
      "connected: openai (apiKey)",
      "session: opened, model openai/gpt-5.4",
      "settings: read, 1 set",
      // gpt-5.4 unchecked, gpt-5.5 checked: Claude keeps its place.
      {
        post: {
          type: "setInstructions",
          index: 0,
          group: { models: ["openai/gpt-4.1", "openai/gpt-5.5"], text: "  Be brief.\n" },
        },
      },
      'settings: modelInstructions set to [{"models":["anthropic/claude-opus-4-7","openai/gpt-4.1","openai/gpt-5.5"],"text":"Be brief."},{"models":["anthropic/claude-sonnet-4-6"],"text":"Hidden group."}]',
      // A new group takes only models the panel can show.
      {
        post: {
          type: "setInstructions",
          index: null,
          group: { models: ["openai/gpt-5.4", "anthropic/claude-opus-4-7"], text: "Read first." },
        },
      },
      'settings: modelInstructions set to [{"models":["anthropic/claude-opus-4-7","openai/gpt-4.1","openai/gpt-5.5"],"text":"Be brief."},{"models":["anthropic/claude-sonnet-4-6"],"text":"Hidden group."},{"models":["openai/gpt-5.4"],"text":"Read first."}]',
      { post: { type: "setInstructions", index: null, group: { models: ["anthropic/claude-opus-4-7"], text: "x" } } },
      "settings: modelInstructions refused: Check at least one model.",
      // Deleting a group leaves the hidden one.
      { post: { type: "setInstructions", index: 0, group: null } },
      'settings: modelInstructions set to [{"models":["anthropic/claude-sonnet-4-6"],"text":"Hidden group."},{"models":["openai/gpt-5.4"],"text":"Read first."}]',
    ],
  },
  {
    name: "chat",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(CHAT_SCRIPT) },
    steps: [
      "session: opened",
      "commands: compact, goal, autonomous",
      { post: { type: "send", text: "what is 6 times 7?", images: [] } },
      "chat: turn 1 sent (18 chars, 0 images)",
      // The reasoning opens, streams its words ("Multiplying."), closes, timed.
      { wait: "chat: turn 1 thought for", timeout: 90_000 },
      " s (12 chars)",
      // The reasoning, the text before the cell, the cell, the answer.
      { wait: "chat: turn 1 done (end_turn); 1 thought, 2 texts, 1 tool", timeout: 90_000 },
      // What the conversation has used: nothing measured once open, then each reply measures the context.
      "chat: usage not measured of 100,000 tokens (compacts at 79,520), $0.0000",
      { wait: "chat: usage ", times: 2, capture: { measured: "chat: usage ([0-9,]+) of 100,000 tokens" } },
      // A stop: the running turn ends, the message waiting behind it comes back to the box.
      { post: { type: "send", text: "talk", images: [] } },
      "chat: turn 2 sent",
      { post: { type: "send", text: "after the stop", images: [] } },
      "chat: queued (1 waiting)",
      { post: { type: "cancel" } },
      "chat: 1 waiting back in the message box",
      "chat: stop requested",
      "chat: turn 2 cancelled (cancelled)",
      // A message sent while a turn runs goes once it ends.
      { post: { type: "send", text: "one", images: [] } },
      "chat: turn 4 sent",
      { post: { type: "send", text: "two", images: [] } },
      { wait: "chat: queued (1 waiting)", times: 2 },
      "chat: turn 4 done (end_turn); 1 text",
      "chat: turn 5 sent",
      "chat: turn 5 done (end_turn); 1 text",
      // An image goes with its message; with images blocked, the agent refuses it.
      { post: { type: "send", text: "what is this?", images: [PIXEL] } },
      "chat: turn 6 sent (13 chars, 1 image)",
      "chat: turn 6 done (end_turn); 1 text",
      { post: { type: "setSetting", key: "images.blockImages", value: true } },
      "settings: images.blockImages set to true",
      { post: { type: "send", text: "and this?", images: [PIXEL] } },
      'chat: turn 7 failed: Images are blocked: turn off "Block images" in the settings to send them.',
      // A new conversation is a new agent process.
      { post: { type: "send", text: "/new", images: [] } },
      "chat: new conversation",
      { wait: "agent started (pid", times: 2 },
      { wait: "session: opened", times: 2 },
      // The one before is saved, titled by its first prompt; nothing was asked in the new one yet.
      { post: { type: "listChats" } },
      { wait: "history: 1 conversation: ", capture: { chat: ': ([0-9a-f-]+) "what is 6 times 7\\?"' } },
      // It opens: the agent restarts on it and replays it, its five turns (the refused one never reached it).
      { post: { type: "openChat", id: "${chat}" } },
      "chat: opening conversation ",
      { wait: "agent started (pid", times: 3 },
      "session: loaded ",
      "chat: loaded 5 turns; ",
      // It goes on: the faux script starts over in the new process.
      { post: { type: "send", text: "and 6 times 8?", images: [] } },
      { wait: "done (end_turn); 1 thought, 2 texts, 1 tool", times: 2, timeout: 90_000 },
      // The one in the panel is not deleted.
      { post: { type: "listChats" } },
      '"what is 6 times 7?" (current)',
      { post: { type: "deleteChat", id: "${chat}" } },
      "history: delete refused: This is the conversation in progress",
      // After a new conversation, it is.
      { post: { type: "newChat" } },
      { wait: "session: opened", times: 3 },
      { post: { type: "deleteChat", id: "${chat}" } },
      " deleted",
      "history: 0 conversations",
    ],
  },
  {
    name: "goal",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(GOAL_SCRIPT) },
    steps: [
      "session: opened",
      { post: { type: "send", text: "/goal count to three", images: [] } },
      "chat: turn 1 sent",
      "chat: goal active: count to three",
      // Pause: the running turn stops, then the goal waits; its time stands at the pause.
      { post: { type: "goal", action: "pause" } },
      "chat: goal paused from the panel",
      "chat: turn 1 cancelled (cancelled)",
      "chat: turn 2 sent (11 chars, 0 images)",
      "chat: goal paused, ",
      "chat: turn 2 done",
      // Opened again, the conversation brings its paused goal back.
      { post: { type: "send", text: "/new", images: [] } },
      { wait: "session: opened", times: 2 },
      "chat: goal gone",
      { post: { type: "listChats" } },
      { wait: "history: 1 conversation: ", capture: { chat: ': ([0-9a-f-]+) "/goal count to three"' } },
      { post: { type: "openChat", id: "${chat}" } },
      "chat: loaded ",
      { wait: "chat: goal paused, ", times: 2 },
      // Play: the goal goes on, and is reached (the faux script starts over in the new process).
      { post: { type: "goal", action: "resume" } },
      "chat: goal resumed from the panel",
      "sent (12 chars, 0 images)",
      { wait: "chat: goal active: count to three", times: 2 },
      { wait: "chat: goal complete, ", timeout: 90_000 },
      { wait: "done (end_turn); 2 notices", timeout: 90_000 },
      // A goal reached stays until the next message.
      { post: { type: "send", text: "thanks", images: [] } },
      { wait: "chat: goal gone", times: 2 },
      "done (end_turn); 1 text",
      // The cross ends a goal under way: the running turn stops, the goal goes.
      { post: { type: "send", text: "/goal count to four", images: [] } },
      "chat: goal active: count to four",
      { post: { type: "goal", action: "end" } },
      "chat: goal ended from the panel",
      { wait: "chat: turn 7 cancelled (cancelled)" },
      "chat: turn 8 sent (11 chars, 0 images)",
      { wait: "chat: goal gone", times: 3 },
      "chat: turn 8 done (end_turn); 1 notice",
    ],
  },
  {
    name: "edit",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(EDIT_SCRIPT) },
    steps: [
      "session: opened",
      { post: { type: "send", text: "first", images: [] } },
      "chat: turn 1 done (end_turn); 1 text",
      { post: { type: "send", text: "second", images: [PIXEL] } },
      "chat: turn 2 done (end_turn); 1 text",
      // The second message comes back to the box with its image; sent changed, it takes the first one's place.
      { post: { type: "edit", id: 2 } },
      "chat: turn 2 back in the message box to edit (6 chars, 1 image)",
      { post: { type: "send", text: "second, changed", images: [], edit: 2 } },
      "chat: back to before turn 2",
      "chat: turn 3 sent (15 chars, 0 images)",
      "chat: turn 3 done (end_turn); 1 text",
      // Opened again, the conversation is the one it went on with, and its turns go back too.
      { post: { type: "send", text: "/new", images: [] } },
      { wait: "session: opened", times: 2 },
      { post: { type: "listChats" } },
      { wait: "history: 1 conversation: ", capture: { chat: ': ([0-9a-f-]+) "first"' } },
      { post: { type: "openChat", id: "${chat}" } },
      "chat: loaded 2 turns; ",
      { post: { type: "edit", id: 5 } },
      "chat: turn 5 back in the message box to edit (15 chars, 0 images)",
      { post: { type: "send", text: "second, again", images: [], edit: 5 } },
      "chat: back to before turn 5",
      "chat: turn 6 sent (13 chars, 0 images)",
      "chat: turn 6 done (end_turn); 1 text",
    ],
  },
  {
    name: "compaction",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    // A reserve that leaves the context 904 tokens: each answer goes past it.
    settings: { compaction: { enabled: true, reserveTokens: 95_000, keepRecentTokens: 10 } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(COMPACTION_SCRIPT) },
    steps: [
      "session: opened",
      "chat: usage not measured of 100,000 tokens (compacts at 904), $0.0000",
      // Past the threshold, the turn compacts on its own before it ends; it shows in the turn while it runs.
      // With one exchange there is nothing to summarize: it does not run, and its mark leaves.
      { post: { type: "send", text: "what is 6 times 7?", images: [] } },
      "chat: turn 1 compacting automatically",
      "chat: turn 1 compaction did not run",
      "chat: turn 1 done (end_turn); 1 text",
      // With two, it runs: how much it summarized and the summary; the context waits for the next reply to measure it.
      { post: { type: "send", text: "and 6 times 8?", images: [] } },
      "chat: turn 2 compacting automatically",
      { wait: ", compacting", times: 2 },
      { wait: "chat: turn 2 compacted (", timeout: 90_000 },
      " tokens summarized, a summary of ",
      { wait: "chat: usage not measured of 100,000 tokens (compacts at 904)", times: 2 },
      "chat: turn 2 done (end_turn); 1 text, 1 compaction",
      // `/compact` with nothing new: it does not run either.
      { post: { type: "send", text: "/compact", images: [] } },
      "chat: turn 3 compacting",
      "chat: turn 3 compaction did not run",
      "chat: turn 3 done (end_turn)",
      // Opened again, the conversation brings its compaction back, done.
      { post: { type: "send", text: "/new", images: [] } },
      { wait: "session: opened", times: 2 },
      { post: { type: "listChats" } },
      { wait: "history: 1 conversation: ", capture: { chat: ': ([0-9a-f-]+) "what is 6 times 7\\?"' } },
      { post: { type: "openChat", id: "${chat}" } },
      "chat: loaded 3 turns; 2 texts, 1 compaction, 1 notice",
      // With its summary, as it came when it ran.
      { wait: "## Goal\\nMultiply.", times: 2 },
    ],
  },
  {
    name: "slash menu",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify({ tokensPerSecond: 40, responses: ["word ".repeat(80), "word ".repeat(80)] }) },
    steps: [
      "session: opened",
      // A "/" back in the message box opens the commands menu; more text after it closes it, and the panel stays up.
      { post: { type: "send", text: "talk", images: [] } },
      "chat: turn 1 sent",
      { post: { type: "send", text: "/", images: [] } },
      "chat: queued (1 waiting)",
      { post: { type: "cancel" } },
      "chat: 1 waiting back in the message box",
      { post: { type: "send", text: "talk again", images: [] } },
      "chat: turn 3 sent",
      { post: { type: "send", text: "x", images: [] } },
      { wait: "chat: queued (1 waiting)", times: 2 },
      { post: { type: "cancel" } },
      { wait: "chat: 1 waiting back in the message box", times: 2 },
      { post: { type: "listFiles" } },
      { wait: "files: ", times: 2 },
    ],
  },
  {
    name: "restart",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify({ responses: ["first answer"] }) },
    steps: [
      "session: opened",
      // Before any message: a new conversation.
      { post: { type: "restartAgent" } },
      { wait: "session: opened", times: 2 },
      { post: { type: "send", text: "hello", images: [] } },
      { wait: "chat: turn 1 done (end_turn); 1 text", timeout: 90_000 },
      // After one: the agent restarts on the conversation in the panel, which comes back.
      { post: { type: "restartAgent" } },
      "chat: opening conversation ",
      { wait: "agent started (pid", times: 3 },
      "session: loaded ",
      "chat: loaded 1 turn; 1 text",
    ],
  },
  {
    name: "cells",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(CELLS_SCRIPT) },
    workspace: { "src/app.ts": "export const a = 1;\n" },
    steps: [
      "session: opened",
      { post: { type: "send", text: "set a to 2", images: [] } },
      // What each cell does, from its code: the panel's icon and words.
      { wait: "chat: turn 1 cell: edit src/app.ts", timeout: 90_000 },
      "chat: turn 1 cell: write src/new.ts",
      "chat: turn 1 cell: read src/app.ts",
      "chat: turn 1 cell: search `grep -rn 'a = 2' src`",
      "chat: turn 1 cell: shell `echo built`",
      "chat: turn 1 cell: python",
      { wait: "chat: turn 1 done (end_turn); 6 tools, 1 text", timeout: 90_000 },
    ],
  },
  {
    name: "cell results",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(EDITS_SCRIPT) },
    workspace: { "src/app.ts": 'import { a } from "./a";\nexport const b = 1;\nexport const c = 2;\n' },
    steps: [
      "session: opened",
      { post: { type: "send", text: "set b to 2", images: [] } },
      // The lines an edit changes, from the texts its code replaces; the exception a failed one raised.
      { wait: "chat: turn 1 cell: edit src/app.ts +1 -1", timeout: 90_000 },
      { wait: "chat: turn 1 cell failed: AssertionError", timeout: 90_000 },
      "chat: turn 1 cell: edit src/app.ts +2 -1",
      { wait: "chat: turn 1 cell done", timeout: 90_000 },
      // A file out of the agent's folder shows the folder it lies in.
      "chat: turn 1 cell: read /etc/hosts (in /etc/)",
      { wait: "chat: turn 1 done (end_turn); 3 tools, 1 text", timeout: 90_000 },
    ],
  },
  {
    name: "mentions",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify({ responses: ["read"] }) },
    workspace: {
      "notes.txt": "six times seven\n",
      "docs/read me.md": "# Read me\n",
      ".gitignore": "build/\n",
      "build/out.txt": "built\n",
    },
    git: true,
    steps: [
      "session: opened",
      // The `@` menu's files: git's, less the ignored ones.
      { post: { type: "listFiles" } },
      "files: 3 listed by git, 0 open",
      // Each mention that names something goes as a link; the missing file does not.
      { post: { type: "send", text: 'read @notes.txt and @"docs/read me.md", not @build/gone.txt', images: [] } },
      "chat: turn 1 links notes.txt, docs/read me.md",
      { wait: "chat: turn 1 done (end_turn); 1 text", timeout: 90_000 },
      // The agent keeps the links in the prompt; reopened, it reads as it was written, title too.
      { post: { type: "restartAgent" } },
      "chat: opening conversation ",
      "session: loaded ",
      "replayed without its 2 links",
      "chat: loaded 1 turn; 1 text",
      { post: { type: "listChats" } },
      '"read @notes.txt and @"docs/read me.md", not @build/gone.txt" (current)',
    ],
  },
  {
    name: "skill commands",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify({ responses: ["tidied"] }) },
    workspace: {
      ".unharnessed/skills/tidy/SKILL.md": "---\nname: tidy\ndescription: Tidy the desk.\n---\nPut every file back.\n",
    },
    steps: [
      "session: opened",
      // The `/` menu has the folder's skill; the agent's built-in ones stay out of it.
      { wait: "commands: compact, goal, autonomous", capture: { skill: "(skill:tidy)", left: "; (\\d+) built-in skills? left out" } },
      // Typed by its name, the skill goes as the agent takes it.
      { post: { type: "send", text: "/tidy the desk", images: [] } },
      "chat: turn 1 runs /skill:tidy",
      { wait: "chat: turn 1 done (end_turn); 1 text", timeout: 90_000 },
      // The agent expanded it: the conversation's title is its skill.
      { post: { type: "listChats" } },
      '"<skill name="tidy" ',
    ],
  },
  {
    name: "local model",
    binary,
    catalog: true,
    env: { UNHARNESSED_OFFLINE: "1" },
    steps: [
      "connected: none",
      "panel: providers",
      { post: { type: "scanLocal" } },
      "local: scan found",
      { post: { type: "probeLocal", baseUrl: OLLAMA, apiKey: "" } },
      `local: ${OLLAMA} is Ollama at http://${OLLAMA}/v1, 1 model: tiny:1b`,
      {
        post: {
          type: "saveLocal",
          rowId: "local:new",
          server: {
            id: "ollama",
            name: "Ollama",
            baseUrl: `http://${OLLAMA}/v1`,
            models: [{ id: "tiny:1b", contextWindow: 8192, image: false, reasoning: false }],
          },
        },
      },
      "local server Ollama: done (1 model of ollama in the model menu)",
      // The session opened on nothing usable moves onto the local model.
      "session: options updated, model ollama/tiny:1b (1 choices)",
      { post: { type: "send", text: "Say hello.", images: [] } },
      { wait: "chat: turn 1 done (end_turn); 1 text", timeout: 90_000 },
      { post: { type: "removeLocal", providerId: "ollama" } },
      "removal of local server ollama: done (ollama removed)",
      // Nothing usable again: the providers screen is the way in.
      { wait: "panel: providers", times: 2 },
    ],
  },
  {
    name: "kernel panel",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(KERNEL_SCRIPT) },
    steps: [
      "session: opened",
      { post: { type: "send", text: "start a server", images: [] } },
      // The awaited one-shot is a variable's value, never a command of the list.
      "kernel: 4 variables: n, cfg, server, fg",
      { wait: "running: exec python3", capture: { server: "kernel: command ([0-9a-f]+) running: " } },
      "chat: turn 1 done (end_turn)",
      { wait: "listens on :", timeout: 30_000 },
      { post: { type: "kernelPreview", name: "cfg" } },
      "kernel: preview of cfg (code, 33 chars)",
      { post: { type: "kernelPreview", name: "asyncio" } },
      "kernel: no preview of asyncio: Unknown variable",
      // Stopped from the panel: it ends, and the agent hears of it.
      { post: { type: "kernelKill", id: "${server}" } },
      "stopped",
      "ended (stopped)",
      "chat: turn 2 started by the agent (a background command ended)",
      "chat: turn 2 done; 1 text",
    ],
  },
  {
    name: "kernel restored",
    binary,
    auth: { openai: { type: "api_key", key: "sk-e2e" } },
    env: { UNHARNESSED_OFFLINE: "1", UNHARNESSED_FAUX_SCRIPT: JSON.stringify(RESTORE_SCRIPT) },
    steps: [
      "session: opened",
      { post: { type: "send", text: "keep n", images: [] } },
      "kernel: 1 variables: n",
      { wait: "chat: turn 1 done (end_turn)", timeout: 90_000 },
      // A new conversation's agent closes this one, saving its kernel.
      { post: { type: "send", text: "/new", images: [] } },
      { wait: "session: opened", times: 2 },
      { post: { type: "listChats" } },
      { wait: "history: 1 conversation: ", capture: { chat: ': ([0-9a-f-]+) "keep n"' } },
      // It opens: its kernel comes back, and a line says so under the conversation.
      { post: { type: "openChat", id: "${chat}" } },
      "chat: loaded 1 turn; ",
      { wait: "chat: kernel restored", timeout: 90_000 },
    ],
  },
  {
    name: "binary missing",
    binary: "/nonexistent/unharnessed",
    steps: ["webview ready", "agent failed: spawn /nonexistent/unharnessed ENOENT", "panel: status"],
  },
  {
    name: "agent exits at startup",
    binary: "/usr/bin/false",
    steps: ["webview ready", "agent started (pid", "agent failed: agent exited (code 1)", "panel: status"],
  },
];

let failed = 0;
const only = process.env.UNHARNESSED_E2E_ONLY;
for (const scenario of scenarios.filter(({ name }) => !only || name === only)) {
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
  if (scenario.mcpCatalog) seedMcpCatalog(agentDir);
  for (const [path, text] of Object.entries(scenario.workspace ?? {})) {
    mkdirSync(dirname(join(workspace, path)), { recursive: true });
    writeFileSync(join(workspace, path), text);
  }
  if (scenario.git) execFileSync("git", ["init", "--quiet", workspace]);
  try {
    await runTests({
      vscodeExecutablePath: process.env.UNHARNESSED_VSCODE_EXECUTABLE,
      extensionDevelopmentPath: resolve(here, "../.."),
      extensionTestsPath: join(here, "suite.cjs"),
      extensionTestsEnv: {
        UNHARNESSED_E2E_LOGS: logsDir,
        UNHARNESSED_E2E_STEPS: JSON.stringify(scenario.steps),
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
ollama.close();
process.exit(failed ? 1 : 0);

/** The agent's catalog cache, as a fetch of the catalog would have left it. */
function seedCatalog(agentDir) {
  const payload = JSON.parse(readFileSync(resolve(here, "../../../crates/uh-models/tests/fixtures/catalog.v1.json"), "utf8"));
  const url = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json";
  mkdirSync(join(agentDir, "models"));
  const cache = { url, scope: "public", fetchedAt: Date.now(), payload };
  writeFileSync(join(agentDir, "models", "provider-model-catalog.v1.json"), JSON.stringify(cache));
}

/** The agent's MCP catalog cache, the Marketplace, as a fetch of Prime's catalog would have left it. */
function seedMcpCatalog(agentDir) {
  const payload = JSON.parse(readFileSync(resolve(here, "../../../crates/uh-core/tests/fixtures/mcp/plugins-catalog.v2.json"), "utf8"));
  const url = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/plugins/catalog.v2.json";
  const cache = { url, scope: "public", fetchedAt: Date.now(), payload };
  writeFileSync(join(agentDir, "mcp-service-catalog.v2.json"), JSON.stringify(cache));
}

async function checkNothingLeft(profile, logsDir) {
  const crash = readLog(logsDir).find((line) => line.includes("panel: crashed"));
  if (crash) throw new Error(crash);
  console.log("ok: the panel never crashed");
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
