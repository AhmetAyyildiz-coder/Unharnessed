#!/usr/bin/env python3
"""End-to-end checks, on the real binary, of going back to an earlier prompt
to send it again changed (`_unharnessed/session/rewind`): each prompt turn
says where it goes back to (`_meta.unharnessed.turnStart.rewindTo`), live
and in a loaded conversation; once back there, the model gets the
conversation up to that point and the changed prompt, never what came
after; that stays in the session file on a branch of its own, and a loaded
conversation shows the new branch only; going back is refused while a turn
runs, to an entry that does not exist, and to one off the current branch.

usage: scripts/unharnessed-rewind-e2e.py <out-dir>

The model is a stand-in on 127.0.0.1 that answers as OpenRouter does
("reply 1", "reply 2", ... in order) and keeps every request it gets;
nothing reaches the network. Each process runs in <out-dir>/rewind/ (its
agent dir and HOME) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start. The
kernel venv lives in <out-dir>. Provider keys and tokens from the caller's
environment are dropped. UNHARNESSED_BIN picks the binary (default
target/debug/unharnessed). Exits non-zero when a check fails.
"""
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
REWIND = "_unharnessed/session/rewind"
MODEL = "z-ai/glm-4.6"
FAILED = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or detail == "" else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def text_of(content):
    if isinstance(content, str):
        return content
    return "".join(part.get("text", "") for part in content or [] if isinstance(part, dict))


class Model:
    """The OpenRouter stand-in: "reply N" to the Nth request, after 3 s
    when the last user message asks to go slowly."""

    def __init__(self):
        self.requests = []
        model = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))))
                model.requests.append(body)
                users = [m for m in body.get("messages", []) if m.get("role") == "user"]
                if users and "slowly" in text_of(users[-1].get("content")):
                    time.sleep(3)
                chunks = [
                    {"choices": [{"index": 0, "delta": {"role": "assistant", "content": f"reply {len(model.requests)}"}}]},
                    {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
                     "usage": {"prompt_tokens": 10, "completion_tokens": 2, "total_tokens": 12}},
                ]
                data = "".join(f"data: {json.dumps(dict(c, id='gen-e2e', model=MODEL))}\n\n" for c in chunks)
                data = (data + "data: [DONE]\n\n").encode()
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()

    def conversation(self, request=-1):
        """The user and assistant texts of a request, in order, as (role, text)."""
        return [(m["role"], text_of(m.get("content"))) for m in self.requests[request]["messages"]
                if m.get("role") in ("user", "assistant")]


class Agent:
    """One `unharnessed --mode acp` process on the stand-in, in
    <out-dir>/rewind/ (a fresh one unless `again`), with one session open:
    a new one, or the one it `load`s."""

    def __init__(self, name, model, again=False, args=(), load=None):
        base = OUT / "rewind"
        if not again:
            shutil.rmtree(base, ignore_errors=True)
        self.agent_dir = base / "agent"
        home = base / "home"
        self.work = base / "work"
        for folder in (self.agent_dir, home, self.work):
            folder.mkdir(parents=True, exist_ok=True)
        catalog = json.loads((ROOT / "crates/uh-models/tests/fixtures/catalog.v1.json").read_text())
        cache = {"url": "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json",
                 "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": catalog}
        files = {
            "settings.json": {"defaultProvider": "openrouter", "defaultModel": MODEL},
            "auth.json": {"openrouter": {"type": "api_key", "key": "sk-or-e2e"}},
            "models.json": {"providers": {"openrouter": {"baseUrl": f"http://127.0.0.1:{model.server.server_port}/api/v1"}}},
            "models/provider-model-catalog.v1.json": cache,
        }
        for path, value in files.items():
            target = self.agent_dir / path
            target.parent.mkdir(parents=True, exist_ok=True)
            target.write_text(json.dumps(value, indent=2))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir), "HOME": str(home)}
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}='{value}'" for key, value in extra.items())
        self.out.write(f"$ cd {self.work} && {command} unharnessed --mode acp {' '.join(args)}\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp", *args], cwd=self.work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(base / f"stderr-{name}.log", "w"))
        self.start = time.time()
        self.lock = threading.Lock()
        self.frames = []
        self.arrived = threading.Condition(self.lock)
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        if load:
            self.loaded = self.request("session/load", {"sessionId": load, "cwd": str(self.work), "mcpServers": []})
            self.session_id = load
        else:
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
                    self.arrived.notify_all()

    def send(self, method, params=None):
        request_id = next(self.ids)
        frame = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        self._log(">", frame)
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()
        return request_id

    def wait_for(self, test, timeout=120):
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

    def prompt(self, text):
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": [{"type": "text", "text": text}]})

    def rewind(self, to):
        return self.request(REWIND, {"sessionId": self.session_id, "to": to})

    def turns(self):
        """Each turn's start and its prompt's text, in order: [(turnStart, text)]."""
        with self.lock:
            frames = list(self.frames)
        turns = {}
        for frame in frames:
            if frame.get("method") != "session/update":
                continue
            update = frame["params"]["update"]
            meta = update.get("_meta", {}).get("unharnessed", {})
            at = meta.get("promptTurnId")
            if "turnStart" in meta:
                turns[at] = [meta["turnStart"], ""]
            elif update["sessionUpdate"] == "user_message_chunk" and at in turns:
                turns[at][1] += update["content"].get("text", "")
        return [tuple(turn) for turn in turns.values()]

    def entries(self):
        """The session file's entries."""
        return [json.loads(line) for path in self.agent_dir.rglob("sessions/*.jsonl")
                for line in path.read_text().splitlines() if line.strip()]

    def close(self):
        self.request("session/close", {"sessionId": self.session_id})
        self.child.stdin.close()
        code = self.child.wait(timeout=60)
        self.out.close()
        return code


def user_entry(entries, text):
    return next((e for e in entries if e.get("type") == "message" and e["message"]["role"] == "user"
                 and text_of(e["message"]["content"]) == text), None)


def assistant_entry(entries, text):
    return next((e for e in entries if e.get("type") == "message" and e["message"]["role"] == "assistant"
                 and text_of(e["message"]["content"]) == text), None)


def error_of(answer):
    error = answer.get("error", {})
    return error.get("code"), (error.get("data") or {}).get("details", "")


model = Model()
agent = Agent("rewind", model)
try:
    agent.prompt("first question")
    agent.prompt("second question")
    starts = agent.turns()
    marks = [start.get("rewindTo") for start, _ in starts]
    check("each prompt turn says where it goes back to",
          len(marks) == 2 and all(isinstance(mark, str) and mark for mark in marks) and marks[0] != marks[1], starts)
    first_mark, second_mark = (marks + [None, None])[:2]
    entries = agent.entries()
    check("the point is where the conversation stood: the prompt's parent in the session file",
          [(user_entry(entries, t) or {}).get("parentId") for t in ("first question", "second question")] == marks,
          marks)

    code, details = error_of(agent.rewind("nowhere"))
    check("going back to an entry that does not exist is refused", code == -32602 and "not in the conversation" in details,
          (code, details))

    slow = agent.send("session/prompt", {"sessionId": agent.session_id,
                                         "prompt": [{"type": "text", "text": "answer slowly"}]})
    # The third turn has started: it runs for 3 s.
    agent.wait_for(lambda f: "turnStart" in f.get("params", {}).get("update", {}).get("_meta", {})
                   .get("unharnessed", {}) and f["params"]["update"]["_meta"]["unharnessed"].get("promptTurnId") == 3)
    code, details = error_of(agent.rewind(second_mark))
    check("going back while a turn runs is refused", code == -32603, (code, details))
    check("the running turn ends as usual", agent.answer(slow).get("result") == {"stopReason": "end_turn"})

    asked = len(model.requests)
    back = agent.rewind(second_mark)
    check("going back to before the second prompt answers {}", back.get("result") == {}, back)
    check("going back asks the model nothing", len(model.requests) == asked, len(model.requests) - asked)
    answer = agent.prompt("second question, changed")
    check("the changed prompt runs its turn", answer.get("result") == {"stopReason": "end_turn"}, answer)
    check("the model gets the conversation up to there, then the changed prompt: nothing that came after",
          model.conversation() == [("user", "first question"), ("assistant", "reply 1"),
                                   ("user", "second question, changed")], model.conversation())
    check("the changed prompt's turn goes back to the same point", agent.turns()[-1][0].get("rewindTo") == second_mark,
          agent.turns()[-1])

    entries = agent.entries()
    check("what came after stays in the session file",
          all(user_entry(entries, t) for t in ("second question", "answer slowly"))
          and assistant_entry(entries, "reply 2") is not None, [e.get("type") for e in entries])
    check("the changed prompt branches off where the conversation went back to",
          (user_entry(entries, "second question, changed") or {}).get("parentId") == second_mark)
    off_branch = (assistant_entry(entries, "reply 2") or {}).get("id")
    code, details = error_of(agent.rewind(off_branch))
    check("going back to an entry off the current branch is refused", code == -32602 and "not in the conversation" in details,
          (code, details))
    saved = agent.request("session/list", {})["result"]["sessions"][0]
    check("the agent exits cleanly", agent.close() == 0)

    loaded = Agent("rewind-load", model, again=True, args=("--resume", saved["_meta"]["unharnessed"]["path"]),
                   load=saved["sessionId"])
    replayed = loaded.turns()
    check("a loaded conversation shows the branch it went on with",
          [text for _, text in replayed] == ["first question", "second question, changed"], replayed)
    check("its turns say where they go back to, as they did live",
          [start.get("rewindTo") for start, _ in replayed] == [first_mark, second_mark], replayed)
    loaded.prompt("third question")
    check("it goes on from the branch",
          model.conversation() == [("user", "first question"), ("assistant", "reply 1"),
                                   ("user", "second question, changed"), ("assistant", "reply 4"),
                                   ("user", "third question")], model.conversation())
    back = loaded.rewind(second_mark)
    check("a loaded conversation goes back too", back.get("result") == {}, back)
    loaded.prompt("second question, again")
    check("the model gets the conversation up to there, then the prompt",
          model.conversation() == [("user", "first question"), ("assistant", "reply 1"),
                                   ("user", "second question, again")], model.conversation())
    check("the loaded agent exits cleanly", loaded.close() == 0)
except Exception as error:  # noqa: BLE001 - a scenario that breaks is a failure
    check("the scenario ran to the end", False, repr(error))

model.server.shutdown()
print(f"\n{len(FAILED)} failed" if FAILED else "\nall passed")
sys.exit(1 if FAILED else 0)
