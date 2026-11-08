#!/usr/bin/env python3
"""End-to-end checks of what the panel's chat reads over ACP on the real
binary (VSCODE_PLAN.md step 6): a streamed turn (reasoning, text, a Python
cell and its output), each reasoning block's start and end, what started
each turn (the prompt, or a wake the agent runs on its own), a cancelled
turn, the slash commands the session advertises (session commands, prompt
templates, `/skill:<name>` and `enableSkillCommands`), what `/goal` and
`/compact` answer, the image settings (`images.blockImages`,
`images.autoResize`) on the images a prompt carries, the saved
conversations (`session/list`, `session/load` after `--resume`, and
`_unharnessed/sessions/delete`), what the conversation has used
(`_meta.unharnessed.usage`: the context, the compaction's threshold, the
cost, a compaction at work), an OpenRouter reasoning that streams as
one block, and an answer the provider cut at the output limit (the turn goes
on, live and on reload).

usage: scripts/unharnessed-chat-e2e.py <out-dir>

The model is the faux provider (UNHARNESSED_FAUX_SCRIPT), or for the
OpenRouter scenario a stand-in on 127.0.0.1 that streams as OpenRouter
does: nothing reaches the network. Each scenario runs in its own agent dir and HOME
(<out-dir>/<name>/) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start. The
kernel venv lives in <out-dir>. Provider keys and tokens from the caller's
environment are dropped. UNHARNESSED_BIN picks the binary (default
target/debug/unharnessed); UNHARNESSED_E2E_ONLY runs the scenario of that
name alone. Exits non-zero when a check fails.
"""
import base64
import itertools
import json
import os
import shutil
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/debug/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
ENV = {
    name: value
    for name, value in os.environ.items()
    if not name.endswith(("_API_KEY", "_TOKEN")) and not name.startswith("UNHARNESSED_")
}
ENV.update(UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), UNHARNESSED_OFFLINE="1", DO_NOT_TRACK="1")
FAILED = []
SET = "_unharnessed/settings/set"
# The usage scenario's model price, dollars per million tokens.
PRICE = {"input": 3, "output": 15, "cacheRead": 0.3, "cacheWrite": 3.75}
# A 1x1 PNG.
PIXEL = ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or detail == "" else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def call(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


class Agent:
    """One `unharnessed --mode acp` process on a faux script (none when
    `responses` is None), in its own agent dir and HOME, with one session open: a new one, the one it
    `load`s, or none when `session` is false. `home` names an earlier
    process's dir to share its agent dir and HOME; `work` is the folder it
    runs in there; `args` go after `--mode acp`."""

    def __init__(self, name, responses, settings=None, files=None, script=None, home=None, work="work", args=(),
                 load=None, session=True):
        base = OUT / (home or name)
        # A fresh agent dir: nothing from an earlier run.
        if home is None:
            shutil.rmtree(base, ignore_errors=True)
        self.agent_dir = base / "agent"
        home = base / "home"
        self.work = base / work
        for folder in (self.agent_dir, home, self.work):
            folder.mkdir(parents=True, exist_ok=True)
        if settings is not None:
            (self.agent_dir / "settings.json").write_text(json.dumps(settings, indent=2))
        for path, text in (files or {}).items():
            target = base / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir), "HOME": str(home)}
        if responses is not None:
            extra["UNHARNESSED_FAUX_SCRIPT"] = json.dumps(dict(script or {}, responses=responses))
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}='{value}'" for key, value in extra.items())
        self.out.write(f"$ cd {self.work} && {command} unharnessed --mode acp {' '.join(args)}\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp", *args], cwd=self.work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(base / f"stderr-{name}.log", "w"))
        self.start = time.time()
        self.lock = threading.Lock()
        self.frames = []
        self.times = []
        self.arrived = threading.Condition(self.lock)
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.initialized = self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        self.session_id = None
        if load:
            self.loaded = self.request("session/load", {"sessionId": load, "cwd": str(self.work), "mcpServers": []})
            self.session_id = load
        elif session:
            answer = self.request("session/new", {"cwd": str(self.work), "mcpServers": []})
            self.session_id = answer["result"]["sessionId"]

    def _log(self, arrow, frame):
        self.out.write(f"{time.time() - self.start:6.2f} {arrow} {json.dumps(frame)}\n")
        self.out.flush()

    def _read(self):
        for line in self.child.stdout:
            if line.strip():
                frame = json.loads(line)
                self._log("<", frame)
                with self.lock:
                    self.frames.append(frame)
                    self.times.append(time.time())
                    self.arrived.notify_all()

    def send(self, method, params=None):
        request_id = next(self.ids)
        frame = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        self._log(">", frame)
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()
        return request_id

    def notify(self, method, params):
        frame = {"jsonrpc": "2.0", "method": method, "params": params}
        self._log(">", frame)
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()

    def wait_for(self, test, timeout=120):
        """The first frame that passes `test`, waiting for it."""
        deadline = time.time() + timeout
        with self.lock:
            while True:
                found = next((frame for frame in self.frames if test(frame)), None)
                if found is not None:
                    return found
                if time.time() > deadline:
                    raise TimeoutError("no such frame")
                self.arrived.wait(timeout=0.2)

    def answer(self, request_id, timeout=120):
        return self.wait_for(lambda frame: frame.get("id") == request_id and "method" not in frame, timeout)

    def request(self, method, params=None, timeout=120):
        return self.answer(self.send(method, params), timeout)

    def prompt(self, text, images=()):
        blocks = [{"type": "text", "text": text}]
        blocks += [{"type": "image", "mimeType": "image/png", "data": data} for data in images]
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": blocks})

    def updates(self, turn=None, timed=False):
        """Every `session/update` so far, in order, as (turn, update), or
        (turn, update, arrival) when `timed`."""
        with self.lock:
            frames = list(zip(self.frames, self.times))
        found = []
        for frame, arrival in frames:
            if frame.get("method") != "session/update":
                continue
            update = frame["params"]["update"]
            at = update.get("_meta", {}).get("unharnessed", {}).get("promptTurnId")
            if turn is None or at == turn:
                found.append((at, update, arrival) if timed else (at, update))
        return found

    def sessions(self):
        """The text of every session file the agent wrote."""
        return "".join(path.read_text() for path in self.agent_dir.rglob("*.jsonl"))

    def close(self):
        if self.session_id:
            self.request("session/close", {"sessionId": self.session_id})
        self.child.stdin.close()
        code = self.child.wait(timeout=60)
        self.out.close()
        return code


def unharnessed(update):
    return update.get("_meta", {}).get("unharnessed", {})


def turn_starts(agent):
    return [(turn, unharnessed(update)["turnStart"]) for turn, update in agent.updates()
            if "turnStart" in unharnessed(update)]


def scenario_turn():
    agent = Agent("turn", [
        {"content": [{"type": "thinking", "thinking": "Adding them up."},
                     {"type": "text", "text": "Let me count."},
                     {"type": "toolCall", "name": "ipython", "arguments": {"code": "print(1 + 1)"}}]},
        "It is 2.",
    ])
    answer = agent.prompt("what is 1 + 1?")
    check("turn: the prompt ends the turn", answer.get("result") == {"stopReason": "end_turn"}, answer)
    starts = turn_starts(agent)
    check("turn: it starts with its origin, the prompt", [s for _, s in starts] == [{"origin": "prompt"}], starts)
    turn = starts[0][0] if starts else None
    updates = agent.updates(turn)
    kinds = [update["sessionUpdate"] for _, update in updates]
    check("turn: the origin comes before anything else of the turn",
          bool(updates) and "turnStart" in unharnessed(updates[0][1]), kinds[:3])
    thought = "".join(u["content"]["text"] for _, u in updates if u["sessionUpdate"] == "agent_thought_chunk")
    check("turn: the reasoning streams as thought chunks", thought == "Adding them up.", thought)
    messages = {}
    for _, update in updates:
        if update["sessionUpdate"] == "agent_message_chunk":
            messages[update["messageId"]] = messages.get(update["messageId"], "") + update["content"]["text"]
    check("turn: each assistant message has its own id",
          list(messages.values()) == ["Let me count.", "It is 2."], messages)
    calls = [u for _, u in updates if u["sessionUpdate"] == "tool_call"]
    check("turn: the Python cell comes with its code",
          len(calls) == 1 and calls[0]["title"] == "Python cell" and calls[0]["rawInput"] == {"code": "print(1 + 1)"},
          calls)
    done = [u for _, u in updates if u["sessionUpdate"] == "tool_call_update"]
    output = done[0].get("content", [{}])[0].get("content", {}).get("text", "") if done else ""
    check("turn: the cell's output comes with its end",
          len(done) == 1 and done[0]["status"] == "completed" and output.strip() == "2", done)
    boundary = [unharnessed(u) for _, u in updates if unharnessed(u).get("phase") == "responseBoundary"]
    check("turn: the response boundary closes it with its outcome",
          [b.get("outcome") for b in boundary] == ["result"], boundary)
    check("turn: the agent exits cleanly", agent.close() == 0)


def scenario_thinking():
    # Slow enough that a reasoning block's start and its first words are
    # apart; the second block reasons without sending any text.
    agent = Agent("thinking", [
        {"content": [{"type": "thinking", "thinking": "Reading the question.\n\nTwo ones make two."},
                     {"type": "text", "text": "Let me check."},
                     {"type": "toolCall", "name": "ipython", "arguments": {"code": "print(1 + 1)"}}]},
        {"content": [{"type": "thinking", "thinking": ""}, {"type": "text", "text": "It is 2."}]},
    ], script={"tokensPerSecond": 10})
    answer = agent.prompt("what is 1 + 1?")
    check("thinking: the prompt ends the turn", answer.get("result") == {"stopReason": "end_turn"}, answer)
    turn = turn_starts(agent)[0][0]
    steps = []
    for _, update, arrival in agent.updates(turn, timed=True):
        kind = update["sessionUpdate"]
        thinking = unharnessed(update).get("thinking")
        if thinking:
            steps.append((thinking["state"], thinking["messageId"], arrival))
        elif kind in ("agent_thought_chunk", "agent_message_chunk"):
            label = "thought" if kind == "agent_thought_chunk" else "text"
            steps.append((label, update["messageId"], arrival, update["content"]["text"]))
        elif kind in ("tool_call", "tool_call_update"):
            steps.append((kind, None, arrival))
    labels = [step[0] for step in steps]
    order = [label for at, label in enumerate(labels) if at == 0 or labels[at - 1] != label]
    check("thinking: each reasoning block opens and closes around its text, the block with none too",
          order == ["start", "thought", "end", "text", "tool_call", "tool_call_update", "start", "end", "text"],
          order)
    thought = "".join(step[3] for step in steps if step[0] == "thought")
    check("thinking: the reasoning text streams between them",
          thought == "Reading the question.\n\nTwo ones make two.", thought)
    start = next((step for step in steps if step[0] == "start"), None)
    first = next((step for step in steps if step[0] == "thought"), None)
    gap = first[2] - start[2] if start and first else None
    check("thinking: the start comes when the block opens, before its first words",
          gap is not None and gap >= 0.15, gap)
    ids = {}
    for step in steps:
        if step[0] in ("start", "end", "thought", "text"):
            ids.setdefault(step[1], []).append(step[0])
    check("thinking: start and end carry the id of the message their text streams in",
          [sorted(set(kinds)) for kinds in ids.values()] == [["end", "start", "text", "thought"],
                                                             ["end", "start", "text"]], ids)
    check("thinking: the agent exits cleanly", agent.close() == 0)


def scenario_openrouter_thinking():
    # OpenRouter sends each piece of reasoning twice in one delta: as text
    # (`reasoning`) and structured (`reasoning_details`), which the agent
    # keeps as a second block. The panel opens a reasoning on each start:
    # the first words used to land in one block and the rest in another.
    words = ["Count", " the r's", " in strawberry."]
    chunks = [{"choices": [{"index": 0, "delta": {
        "role": "assistant", "content": "", "reasoning": word,
        "reasoning_details": [{"type": "reasoning.text", "text": word, "format": "unknown", "index": 0}]}}]}
        for word in words]
    chunks.append({"choices": [{"index": 0, "delta": {"content": "3"}}]})
    chunks.append({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                   "usage": {"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}})
    body = "".join(f"data: {json.dumps(dict(chunk, id='gen-e2e', model='z-ai/glm-4.6'))}\n\n" for chunk in chunks)
    body = (body + "data: [DONE]\n\n").encode()

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            self.rfile.read(int(self.headers.get("Content-Length", 0)))
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)

        def log_message(self, *args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    catalog = json.loads((ROOT / "crates/uh-models/tests/fixtures/catalog.v1.json").read_text())
    cache = {"url": "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json",
             "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": catalog}
    agent = Agent("openrouter-thinking", None,
                  settings={"defaultProvider": "openrouter", "defaultModel": "z-ai/glm-4.6"},
                  files={"agent/auth.json": json.dumps({"openrouter": {"type": "api_key", "key": "sk-or-e2e"}}),
                         "agent/models.json": json.dumps({"providers": {"openrouter": {
                             "baseUrl": f"http://127.0.0.1:{server.server_port}/api/v1"}}}),
                         "agent/models/provider-model-catalog.v1.json": json.dumps(cache)})
    answer = agent.prompt("how many r in strawberry?")
    check("openrouter: the prompt ends the turn", answer.get("result") == {"stopReason": "end_turn"}, answer)
    steps = []
    for _, update in agent.updates():
        thinking = unharnessed(update).get("thinking")
        if thinking:
            steps.append((thinking["state"], ""))
        elif update["sessionUpdate"] in ("agent_thought_chunk", "agent_message_chunk"):
            label = "thought" if update["sessionUpdate"] == "agent_thought_chunk" else "text"
            steps.append((label, update["content"]["text"]))
    labels = [step[0] for step in steps]
    order = [label for at, label in enumerate(labels) if at == 0 or labels[at - 1] != label]
    # The chat completions stream closes its blocks when it ends, after the
    # text: the panel ends the reasoning when the text comes.
    check("openrouter: the reasoning opens once, all its words inside, and closes once",
          order == ["start", "thought", "text", "end"], order)
    thought = "".join(text for label, text in steps if label == "thought")
    check("openrouter: the reasoning text streams whole", thought == "".join(words), thought)
    check("openrouter: the agent exits cleanly", agent.close() == 0)
    server.shutdown()


def scenario_cancel():
    # The third answer is the one an early stop may leave unread: short
    # enough that the next prompt streaming it stays quick.
    agent = Agent("cancel", ["word " * 400, "back again", "word " * 40, "once more"], script={"tokensPerSecond": 20})
    request = agent.send("session/prompt", {"sessionId": agent.session_id,
                                            "prompt": [{"type": "text", "text": "talk"}]})
    agent.wait_for(lambda f: f.get("params", {}).get("update", {}).get("sessionUpdate") == "agent_message_chunk")
    agent.notify("session/cancel", {"sessionId": agent.session_id})
    answer = agent.answer(request, timeout=30)
    check("cancel: the prompt answers cancelled", answer.get("result") == {"stopReason": "cancelled"}, answer)
    answer = agent.prompt("again")
    check("cancel: the next prompt runs", answer.get("result") == {"stopReason": "end_turn"}, answer)
    text = "".join(u["content"]["text"] for _, u in agent.updates() if u["sessionUpdate"] == "agent_message_chunk")
    check("cancel: and is answered", text.endswith("back again"), text[-40:])
    # A stop right after the prompt, before the model answers anything.
    request = agent.send("session/prompt", {"sessionId": agent.session_id,
                                            "prompt": [{"type": "text", "text": "talk"}]})
    agent.notify("session/cancel", {"sessionId": agent.session_id})
    answer = agent.answer(request, timeout=30)
    check("cancel: a stop before the model answers is a stop too",
          answer.get("result") == {"stopReason": "cancelled"}, answer)
    answer = agent.prompt("again")
    check("cancel: then the next prompt runs", answer.get("result") == {"stopReason": "end_turn"}, answer)
    check("cancel: the agent exits cleanly", agent.close() == 0)


def scenario_wake():
    agent = Agent("wake", [call('h = bash("sleep 1; echo bg-done")'), "started", "noticed bg-done"])
    agent.prompt("start a background command")
    agent.wait_for(lambda f: unharnessed(f.get("params", {}).get("update", {})).get("turnStart", {}).get("origin")
                   == "wake", timeout=60)
    starts = turn_starts(agent)
    check("wake: the agent's own turn says what woke it",
          [s for _, s in starts] == [{"origin": "prompt"}, {"origin": "wake", "wake": "bash"}], starts)
    wake_turn = starts[-1][0]
    agent.wait_for(lambda f: unharnessed(f.get("params", {}).get("update", {})).get("phase") == "responseBoundary"
                   and unharnessed(f["params"]["update"]).get("promptTurnId") == wake_turn, timeout=60)
    text = "".join(u["content"]["text"] for _, u in agent.updates(wake_turn)
                   if u["sessionUpdate"] == "agent_message_chunk")
    check("wake: its answer streams under its own turn", text == "noticed bg-done", text)
    check("wake: the agent exits cleanly", agent.close() == 0)


SKILL = "---\nname: pdf-reports\ndescription: Write PDF reports from data.\n---\nUse reportlab.\n"
PROMPTS = {
    "agent/prompts/review.md": "---\ndescription: Review the changes\nargument-hint: \"[focus]\"\n---\nReview $@\n",
    "agent/prompts/goal.md": "---\ndescription: My own goal prompt\n---\nAim for $1\n",
    "agent/skills/pdf-reports/SKILL.md": SKILL,
}


def advertised(agent):
    update = agent.wait_for(lambda f: f.get("params", {}).get("update", {}).get("sessionUpdate")
                            == "available_commands_update", timeout=30)["params"]["update"]
    return {c["name"]: (unharnessed(c).get("kind"), c["description"], (c.get("input") or {}).get("hint"))
            for c in update["availableCommands"]}


def scenario_commands():
    agent = Agent("commands", ["ok", "ok"], files=PROMPTS)
    commands = advertised(agent)
    check("commands: the session commands are advertised",
          commands.get("compact", ("",))[0] == "command" and commands.get("autonomous", ("",))[0] == "command",
          commands)
    check("commands: a prompt template, with its description and hint",
          commands.get("review") == ("prompt", "Review the changes", "[focus]"), commands.get("review"))
    check("commands: a template named like a command is what the name runs, listed once",
          commands.get("goal") == ("prompt", "My own goal prompt", None), commands.get("goal"))
    check("commands: a skill, as skill:<name>",
          commands.get("skill:pdf-reports") == ("skill", "Write PDF reports from data.", None),
          commands.get("skill:pdf-reports"))
    order = [kind for kind, _, _ in commands.values()]
    check("commands: commands, then templates, then skills", order == sorted(order, key=["command", "prompt",
                                                                                        "skill"].index), order)
    agent.prompt("/skill:pdf-reports for March")
    check("commands: /skill:<name> reaches the model as the skill", '<skill name=\\"pdf-reports\\"' in agent.sessions())
    check("commands: the agent exits cleanly", agent.close() == 0)

    agent = Agent("commands-off", ["ok"], files=PROMPTS, settings={"enableSkillCommands": False})
    commands = advertised(agent)
    check("commands off: no skill is advertised", not any(kind == "skill" for kind, _, _ in commands.values()),
          list(commands))
    check("commands off: the templates still are", "review" in commands, list(commands))
    agent.prompt("/skill:pdf-reports for March")
    sessions = agent.sessions()
    check("commands off: /skill:<name> reaches the model as typed",
          "/skill:pdf-reports for March" in sessions and '<skill name=\\"pdf-reports\\"' not in sessions)
    check("commands off: the agent exits cleanly", agent.close() == 0)


def command_outputs(agent):
    return [unharnessed(u)["command"] for _, u in agent.updates() if "command" in unharnessed(u)]


def scenario_session_commands():
    agent = Agent("session-commands", ["working on it"])
    agent.prompt("/goal")
    check("/goal: its status comes back", command_outputs(agent) == [
        {"name": "goal", "text": "No active goal.", "severity": "info"}], command_outputs(agent))
    agent.prompt("/compact")
    outputs = command_outputs(agent)[1:]
    check("/compact: a skipped compaction says why",
          len(outputs) == 1 and outputs[0]["name"] == "compact" and outputs[0]["severity"] == "warning"
          and outputs[0]["text"], outputs)
    check("session commands: the agent exits cleanly", agent.close() == 0)


def scenario_images():
    agent = Agent("images", ["I see it", "I see a big one", "fine"])
    answer = agent.prompt("what is this?", [PIXEL])
    check("images: a small image goes", answer.get("result") == {"stopReason": "end_turn"}, answer)
    check("images: it reaches the model", PIXEL in agent.sessions())
    big = base64.b64encode(os.urandom(3_600_000)).decode()
    answer = agent.prompt("and this?", [big])
    details = answer.get("error", {}).get("data", {}).get("details", "")
    check("images: under autoResize, one over the limit is refused, saying how big",
          "4.8 MB once encoded, over the 4.5 MB limit" in details, answer.get("error"))
    agent.request(SET, {"key": "images.autoResize", "value": False})
    answer = agent.prompt("and this?", [big])
    check("images: without autoResize it goes as it is (the next prompt reads the setting)",
          answer.get("result") == {"stopReason": "end_turn"}, answer)
    agent.request(SET, {"key": "images.blockImages", "value": True})
    answer = agent.prompt("one more?", [PIXEL])
    details = answer.get("error", {}).get("data", {}).get("details", "")
    check("images: blockImages refuses them", details.startswith("Images are blocked"), answer.get("error"))
    answer = agent.prompt("no image this time")
    check("images: blockImages leaves text prompts alone", "error" not in answer, answer)
    check("images: the agent exits cleanly", agent.close() == 0)


def listed(agent, cwd=None):
    """`session/list` as (title, current) pairs, the newest first."""
    answer = agent.request("session/list", {"cwd": str(cwd)} if cwd else {})
    return [(s.get("title"), unharnessed(s).get("current", False)) for s in answer["result"]["sessions"]]


def error_details(answer):
    return answer.get("error", {}).get("data", {}).get("details", "")


def scenario_history():
    first = Agent("history", [
        {"content": [{"type": "thinking", "thinking": "Adding them up."},
                     {"type": "text", "text": "Let me count."},
                     {"type": "toolCall", "name": "ipython", "arguments": {"code": "print(1 + 1)"}}]},
        "It is 2.",
        "I see it",
    ])
    first.prompt("what is\n1 + 1?")
    first.prompt("what is this?", [PIXEL])
    first.prompt("/goal")
    sessions = first.request("session/list", {})["result"]["sessions"]
    check("history: the conversation is listed, titled by its first prompt, as this process's own",
          [(s.get("title"), s["cwd"], unharnessed(s).get("current")) for s in sessions]
          == [("what is 1 + 1?", str(first.work), True)], sessions)
    session_id = sessions[0]["sessionId"] if sessions else None
    path = unharnessed(sessions[0]).get("path", "") if sessions else ""
    check("history: with the file that holds it", Path(path).is_file() and session_id in path, path)
    check("history: the agent exits cleanly", first.close() == 0)
    artifacts = first.agent_dir / "session-artifacts" / str(session_id)
    check("history: the kernel's state is saved beside it", artifacts.is_dir(), artifacts)

    other = Agent("history-other", ["over there"], home="history", work="elsewhere")
    other.prompt("a question from elsewhere")
    check("history: a conversation in another folder", other.close() == 0)

    second = Agent("history-load", ["4, then."], home="history", args=("--resume", path), load=session_id)
    capabilities = second.initialized["result"]["agentCapabilities"]
    check("load: the agent says it loads and lists sessions",
          capabilities["loadSession"] is True and capabilities["sessionCapabilities"] == {"close": {}, "list": {}},
          capabilities)
    check("load: the answer comes with the pickers", "configOptions" in second.loaded.get("result", {}),
          second.loaded)
    answer_at = second.frames.index(second.loaded)
    # The turns; a picker update (the catalog's refresh) may come in between, outside any turn.
    replayed = [f["params"]["update"] for f in second.frames[:answer_at] if f.get("method") == "session/update"
                and unharnessed(f["params"]["update"]).get("promptTurnId")]
    turns = {}
    for update in replayed:
        turns.setdefault(unharnessed(update)["promptTurnId"], []).append(update)
    check("load: the conversation comes back before the answer, a turn each",
          len(turns) == 3 and all("turnStart" in unharnessed(t[0]) for t in turns.values()), list(turns))
    one, two, three = (list(turns.values()) + [[], [], []])[:3]

    def chunks(turn, kind):
        return [u["content"] for u in turn if u["sessionUpdate"] == kind]

    def texts(turn, kind):
        found = {}
        for update in turn:
            if update["sessionUpdate"] == kind:
                found[update["messageId"]] = found.get(update["messageId"], "") + update["content"]["text"]
        return list(found.values())

    check("load: each turn starts as the prompt's", [unharnessed(t[0])["turnStart"] for t in (one, two, three)]
          == [{"origin": "prompt"}] * 3)
    check("load: then the prompt, as the user wrote it",
          chunks(one, "user_message_chunk") == [{"type": "text", "text": "what is\n1 + 1?"}],
          chunks(one, "user_message_chunk"))
    check("load: the reasoning, and each message's text", texts(one, "agent_thought_chunk") == ["Adding them up."]
          and texts(one, "agent_message_chunk") == ["Let me count.", "It is 2."], texts(one, "agent_message_chunk"))
    calls = [u for u in one if u["sessionUpdate"] in ("tool_call", "tool_call_update")]
    output = calls[1].get("content", [{}])[0].get("content", {}).get("text", "") if len(calls) == 2 else ""
    check("load: the cell, with its code and its output",
          len(calls) == 2 and calls[0]["rawInput"] == {"code": "print(1 + 1)"} and calls[1]["status"] == "completed"
          and output.strip() == "2", calls)
    check("load: each turn ends as it did", [unharnessed(t[-1]).get("turnEnd") for t in (one, two, three)]
          == [{"state": "done"}] * 3, [unharnessed(t[-1]) for t in (one, two, three)])
    check("load: an image comes back with its prompt", chunks(two, "user_message_chunk") == [
        {"type": "text", "text": "what is this?"}, {"type": "image", "mimeType": "image/png", "data": PIXEL}],
          chunks(two, "user_message_chunk"))
    check("load: a slash command, as typed, with its answer",
          chunks(three, "user_message_chunk") == [{"type": "text", "text": "/goal"}]
          and [unharnessed(u)["command"] for u in three if "command" in unharnessed(u)]
          == [{"name": "goal", "text": "No active goal.", "severity": "info"}], three)
    answer = second.prompt("and 2 + 2?")
    check("load: the conversation goes on", answer.get("result") == {"stopReason": "end_turn"}, answer)
    text = "".join(u["content"]["text"] for t, u in second.updates() if t not in turns
                   and u["sessionUpdate"] == "agent_message_chunk")
    check("load: in a turn of its own", text == "4, then.", text)
    check("list: this folder's only, with cwd", listed(second, second.work) == [("what is 1 + 1?", True)],
          listed(second, second.work))
    check("list: every folder's without, the newest first",
          listed(second) == [("what is 1 + 1?", True), ("a question from elsewhere", False)], listed(second))
    answer = second.request("_unharnessed/sessions/delete", {"sessionId": session_id})
    check("delete: not the conversation in progress",
          error_details(answer).startswith("This is the conversation in progress"), answer)

    third = Agent("history-third", [], home="history", session=False)
    answer = third.request("session/load", {"sessionId": session_id, "cwd": str(third.work), "mcpServers": []})
    check("load: only the session the process resumed", error_details(answer).startswith("This process holds"),
          answer)
    answer = third.request("_unharnessed/sessions/delete", {"sessionId": session_id})
    check("delete: not one another process holds",
          error_details(answer) == "This conversation is open in another window.", answer)
    check("load: the loaded agent exits cleanly", second.close() == 0)
    check("list: none is this process's once the other has exited",
          listed(third) == [("what is 1 + 1?", False), ("a question from elsewhere", False)], listed(third))
    answer = third.request("_unharnessed/sessions/delete", {"sessionId": session_id})
    check("delete: one no process holds", answer.get("result") == {}, answer)
    check("delete: its file and the kernel's state are gone", not Path(path).exists() and not artifacts.exists())
    check("delete: and it is no longer listed", listed(third) == [("a question from elsewhere", False)],
          listed(third))
    answer = third.request("_unharnessed/sessions/delete", {"sessionId": "../settings"})
    check("delete: only a saved conversation", error_details(answer) == "No saved conversation ../settings.",
          answer)
    check("history: the last agent exits cleanly", third.close() == 0)


def cut_and_answer(updates):
    """The reasoning, the cut and the answer of a turn, in order, once each."""
    seen = []
    for update in updates:
        kind = "cut" if unharnessed(update).get("outputLimit") else update["sessionUpdate"]
        if kind in ("agent_thought_chunk", "cut", "agent_message_chunk") and seen[-1:] != [kind]:
            seen.append(kind)
    return seen


def scenario_output_limit():
    first = Agent("output-limit", [
        {"content": [{"type": "thinking", "thinking": "Working through the acoustics."}], "stopReason": "length"},
        "Done.",
    ])
    answer = first.prompt("make the sound")
    check("output limit: the prompt ends once, after the answer",
          answer.get("result") == {"stopReason": "end_turn"}, answer)
    starts = turn_starts(first)
    seen = cut_and_answer(update for _, update in first.updates())
    check("output limit: one turn, the cut between the reasoning and the answer",
          len(starts) == 1 and seen == ["agent_thought_chunk", "cut", "agent_message_chunk"], (starts, seen))
    sessions = first.request("session/list", {})["result"]["sessions"]
    session_id = sessions[0]["sessionId"] if sessions else None
    path = unharnessed(sessions[0]).get("path", "") if sessions else ""
    check("output limit: the agent exits cleanly", first.close() == 0)

    again = Agent("output-limit-load", [], home="output-limit", args=("--resume", path), load=session_id)
    answer_at = again.frames.index(again.loaded)
    replayed = [f["params"]["update"] for f in again.frames[:answer_at] if f.get("method") == "session/update"
                and unharnessed(f["params"]["update"]).get("promptTurnId")]
    turns = {unharnessed(update)["promptTurnId"] for update in replayed}
    seen = cut_and_answer(replayed)
    check("load: the cut answer comes back as it went, in one turn",
          len(turns) == 1 and seen == ["agent_thought_chunk", "cut", "agent_message_chunk"]
          and unharnessed(replayed[-1]).get("turnEnd") == {"state": "done"}, (turns, seen))
    check("output limit: the loaded agent exits cleanly", again.close() == 0)


def usages(agent):
    """Every usage report so far, as (frame index, its `_meta.unharnessed`)."""
    with agent.lock:
        frames = list(agent.frames)
    return [(at, unharnessed(f["params"]["update"])) for at, f in enumerate(frames)
            if f.get("method") == "session/update" and "usage" in unharnessed(f["params"]["update"])]


def priced(usage):
    """What a model call's usage costs at PRICE."""
    return sum(usage[kind] * PRICE[kind] for kind in PRICE) / 1e6


def entries(agent):
    return [json.loads(line) for line in agent.sessions().splitlines() if line.strip()]


def scenario_usage():
    agent = Agent("usage", [
        {"content": [{"type": "text", "text": "Let me compute it."},
                     {"type": "toolCall", "name": "ipython", "arguments": {"code": "print(6 * 7)"}}]},
        "It is 42.",
        "Summary: the user asked for 6 * 7; it is 42.",
        "Still 42.",
        "Summary: 6 * 7 is 42, asked twice.",
        "42, again.",
    ], settings={"compaction": {"enabled": True, "keepRecentTokens": 10}}, script={"cost": PRICE})
    agent.wait_for(lambda f: "usage" in unharnessed(f.get("params", {}).get("update", {})))
    opened = usages(agent)[0][1]
    check("usage: the open session reports it, outside any turn, nothing measured yet",
          opened["promptTurnId"] == 0 and opened["usage"] == {
              "window": 100000, "compactAt": 79520, "cost": 0, "billed": False, "compacting": False}, opened)

    answer = agent.prompt("what is 6 * 7?")
    answered = agent.frames.index(answer)
    turn = [(at, meta) for at, meta in usages(agent) if at > usages(agent)[0][0]]
    replies = [e["message"]["usage"] for e in entries(agent)
               if e.get("type") == "message" and e["message"]["role"] == "assistant"]
    check("usage: each reply is priced at the model's rates",
          len(replies) == 2 and all(abs(u["cost"]["total"] - priced(u)) < 1e-12 and u["cost"]["total"] > 0
                                    for u in replies), replies)
    check("usage: the turn reports after each message, before its answer, outside the turn",
          len(turn) == 3 and all(at < answered and meta["promptTurnId"] == 0 for at, meta in turn),
          [(at, meta["usage"]) for at, meta in turn])
    costs = [meta["usage"]["cost"] for _, meta in turn]
    check("usage: the cost adds up each reply as it lands",
          len(costs) == 3 and abs(costs[0] - replies[0]["cost"]["total"]) < 1e-12 and costs[1] == costs[0]
          and abs(costs[2] - sum(u["cost"]["total"] for u in replies)) < 1e-12, costs)
    sizes = [meta["usage"].get("tokens") for _, meta in turn]
    check("usage: the context is the last reply's tokens, plus an estimate of what came after it",
          len(sizes) == 3 and sizes[0] == replies[0]["totalTokens"] and sizes[1] > sizes[0]
          and sizes[2] == replies[1]["totalTokens"], (sizes, [u["totalTokens"] for u in replies]))

    def compact(name):
        before = usages(agent)[-1]
        agent.prompt("/compact")
        reports = [meta["usage"] for at, meta in usages(agent) if at > before[0]]
        summaries = [e["usage"]["cost"]["total"] for e in entries(agent) if e.get("type") == "compaction"]
        check(f"usage: {name} compaction shows while it runs",
              len(reports) == 2 and reports[0]["compacting"] is True
              and reports[0].get("tokens") == before[1]["usage"].get("tokens"), reports)
        after = reports[-1] if reports else {}
        check(f"usage: after the {name}, the context waits for a reply to measure it, and the summary is paid for",
              after.get("compacting") is False and "tokens" not in after and summaries and summaries[-1] > 0
              and abs(after["cost"] - before[1]["usage"]["cost"] - summaries[-1]) < 1e-12, (after, summaries))
        agent.prompt("and now?")
        measured = usages(agent)[-1][1]["usage"]
        check(f"usage: after the {name}, the next reply measures the context again",
              measured.get("tokens", 0) > 0 and measured["cost"] > after.get("cost", 0), measured)

    compact("first")
    compact("second")
    final = usages(agent)[-1][1]["usage"]
    sessions = agent.request("session/list", {})["result"]["sessions"]
    saved = sessions[0]["sessionId"] if sessions else None
    path = unharnessed(sessions[0]).get("path", "") if sessions else ""
    check("usage: the agent exits cleanly", agent.close() == 0)

    again = Agent("usage-load", [], home="usage", args=("--resume", path), load=saved)
    again.wait_for(lambda f: "usage" in unharnessed(f.get("params", {}).get("update", {})))
    loaded = usages(again)[0]
    check("usage: a conversation that comes back reports it after its answer, what it cost and its context",
          loaded[0] > again.frames.index(again.loaded) and abs(loaded[1]["usage"]["cost"] - final["cost"]) < 1e-12
          and loaded[1]["usage"].get("tokens") == final.get("tokens"), (loaded, final))
    check("usage: the loaded agent exits cleanly", again.close() == 0)

    off = Agent("usage-off", [], settings={"compaction": {"enabled": False}})
    off.wait_for(lambda f: "usage" in unharnessed(f.get("params", {}).get("update", {})))
    check("usage: no threshold when the automatic compaction is off", "compactAt" not in usages(off)[0][1]["usage"],
          usages(off))
    check("usage: the agent without compaction exits cleanly", off.close() == 0)


for scenario in (scenario_turn, scenario_thinking, scenario_openrouter_thinking, scenario_cancel, scenario_wake, scenario_commands,
                 scenario_session_commands, scenario_images, scenario_history, scenario_usage,
                 scenario_output_limit):
    name = scenario.__name__[len("scenario_"):]
    if os.environ.get("UNHARNESSED_E2E_ONLY", name) != name:
        continue
    print(f"# {name}")
    try:
        scenario()
    except Exception as error:  # noqa: BLE001 - a scenario that breaks is a failure, the others still run
        check(f"{scenario.__name__}: ran to the end", False, repr(error))

print(f"\n{len(FAILED)} failed" if FAILED else "\nall passed")
sys.exit(1 if FAILED else 0)
