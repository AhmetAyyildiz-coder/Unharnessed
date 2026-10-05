#!/usr/bin/env python3
"""End-to-end checks of what the panel's chat reads over ACP on the real
binary (VSCODE_PLAN.md step 6): a streamed turn (reasoning, text, a Python
cell and its output), each reasoning block's start and end, what started
each turn (the prompt, or a wake the agent runs on its own), a cancelled
turn, the slash commands the session advertises (session commands, prompt
templates, `/skill:<name>` and `enableSkillCommands`), what `/goal` and
`/compact` answer, and the image settings (`images.blockImages`,
`images.autoResize`) on the images a prompt carries.

usage: scripts/unharnessed-chat-e2e.py <out-dir>

The model is the faux provider (UNHARNESSED_FAUX_SCRIPT): nothing reaches
the network. Each scenario runs in its own agent dir and HOME
(<out-dir>/<name>/) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start. The
kernel venv lives in <out-dir>. Provider keys and tokens from the caller's
environment are dropped. UNHARNESSED_BIN picks the binary (default
target/debug/unharnessed). Exits non-zero when a check fails.
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
# A 1x1 PNG.
PIXEL = ("iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNk+M9QDwADhgGAWjR9awAAAABJRU5ErkJggg==")


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or detail == "" else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def call(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


class Agent:
    """One `unharnessed --mode acp` process on a faux script, in its own
    agent dir and HOME, with one session open."""

    def __init__(self, name, responses, settings=None, files=None, script=None):
        base = OUT / name
        # A fresh agent dir: nothing from an earlier run.
        shutil.rmtree(base, ignore_errors=True)
        self.agent_dir = base / "agent"
        home = base / "home"
        self.work = base / "work"
        for folder in (self.agent_dir, home, self.work):
            folder.mkdir(parents=True, exist_ok=True)
        if settings is not None:
            (self.agent_dir / "settings.json").write_text(json.dumps(settings, indent=2))
        for path, text in (files or {}).items():
            target = base / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(text)
        faux = json.dumps(dict(script or {}, responses=responses))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir), "HOME": str(home),
                 "UNHARNESSED_FAUX_SCRIPT": faux}
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}='{value}'" for key, value in extra.items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=self.work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(base / "stderr.log", "w"))
        self.start = time.time()
        self.lock = threading.Lock()
        self.frames = []
        self.times = []
        self.arrived = threading.Condition(self.lock)
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
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


for scenario in (scenario_turn, scenario_thinking, scenario_cancel, scenario_wake, scenario_commands,
                 scenario_session_commands, scenario_images):
    print(f"# {scenario.__name__[len('scenario_'):]}")
    try:
        scenario()
    except Exception as error:  # noqa: BLE001 - a scenario that breaks is a failure, the others still run
        check(f"{scenario.__name__}: ran to the end", False, repr(error))

print(f"\n{len(FAILED)} failed" if FAILED else "\nall passed")
sys.exit(1 if FAILED else 0)
