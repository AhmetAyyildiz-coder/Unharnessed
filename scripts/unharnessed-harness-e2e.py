#!/usr/bin/env python3
"""End-to-end checks of the harness fixes from HARNESS_AUDIT.md on the real
binary, a real kernel and a local fake provider (no network): long Unicode
results, tool results in the compaction summary, the threshold stop inside a
tool chain, the error flag of a raising cell, and the kernel snapshot after
background work and for cell-defined functions and locks.

usage: scripts/unharnessed-harness-e2e.py <out-dir>

Writes <out-dir>/<case>.out (the command run, then its json-mode stdout) and
<case>.err for every run, plus <case>-requests.json (the provider requests).
The kernel venv lives in <out-dir> (UNHARNESSED_BIN picks the binary, default
target/release/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = str(Path(os.environ.get("UNHARNESSED_BIN", ROOT / "target/release/unharnessed")).resolve())
OUT = Path(sys.argv[1]).resolve()
HOME = OUT / "home"
HOME.mkdir(parents=True, exist_ok=True)
# A throwaway HOME: no credentials, so the fake provider is the only model
# the runs can reach. Inherited agent variables would steer them elsewhere.
BASE_ENV = {k: v for k, v in os.environ.items() if not k.startswith(("UNHARNESSED_", "UH_", "XDG_"))}
BASE_ENV.update(HOME=str(HOME), UNHARNESSED_OFFLINE="1", DO_NOT_TRACK="1",
                UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
                UNHARNESSED_PACKAGE_DIR=str(ROOT), UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"))
FAILED = []
# The scripted replies to tool-bearing requests; summarizer requests (the
# serialized `<conversation>`) get a fixed summary and never consume one.
STEPS = []
REQUESTS = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"), flush=True)
    if not ok:
        FAILED.append(name)


def is_summary(body):
    return "<conversation>" in json.dumps(body["messages"][-1])


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQUESTS.append(body)
        if is_summary(body):
            step = {"text": "Summary of the earlier work.", "usage": 500}
        else:
            step = STEPS.pop(0) if STEPS else {"text": "DONE"}
            if callable(step):
                step = step(body)
        if "code" in step:
            delta = {"role": "assistant", "tool_calls": [{"index": 0, "id": f"call_{len(REQUESTS)}", "type": "function",
                                                          "function": {"name": "ipython",
                                                                       "arguments": json.dumps({"code": step["code"]})}}]}
            finish = "tool_calls"
        else:
            delta = {"role": "assistant", "content": step["text"]}
            finish = "stop"
        usage = step.get("usage", 100)
        chunks = [{"choices": [{"index": 0, "delta": delta}]},
                  {"choices": [{"index": 0, "delta": {}, "finish_reason": finish}],
                   "usage": {"prompt_tokens": usage, "completion_tokens": 10, "total_tokens": usage + 10}}]
        wire = "".join("data: " + json.dumps(dict(c, id="e2e", model="e2e-model")) + "\n\n" for c in chunks)
        wire = (wire + "data: [DONE]\n\n").encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("Content-Length", str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)

    def log_message(self, *args):
        pass


SERVER = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=SERVER.serve_forever, daemon=True).start()


def setup(name, window=128000, keep=10):
    work = OUT / name
    agent = work / "agent"
    agent.mkdir(parents=True, exist_ok=True)
    model = {"id": "e2e-model", "name": "E2E", "reasoning": False, "input": ["text"], "contextWindow": window,
             "maxTokens": 4096, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}
    (agent / "models.json").write_text(json.dumps({"providers": {"e2e": {
        "api": "openai-completions", "baseUrl": f"http://127.0.0.1:{SERVER.server_port}/v1",
        "apiKey": "e2e-local-dummy", "models": [model]}}}))
    (agent / "settings.json").write_text(json.dumps(
        {"compaction": {"enabled": True, "reserveTokens": 4096, "keepRecentTokens": keep}}))
    return work


def run(work, label, prompt="go", resume=False, timeout=120):
    """One `-p --mode json` process; returns its json frames (None when it failed)."""
    env = dict(BASE_ENV, UNHARNESSED_AGENT_DIR=str(work / "agent"), UNHARNESSED_CODING_AGENT_DIR=str(work / "agent"))
    args = [BIN, "-p", "--mode", "json", "--model", "e2e/e2e-model", "--session-dir", str(work / "sessions")]
    if resume:
        args.append("-c")
    args.append(prompt)
    start = len(REQUESTS)
    try:
        result = subprocess.run(args, cwd=work, env=env, text=True, capture_output=True, timeout=timeout,
                                stdin=subprocess.DEVNULL)
        stdout, stderr, code = result.stdout, result.stderr, result.returncode
    except subprocess.TimeoutExpired as expired:
        stdout, stderr, code = expired.stdout or "", expired.stderr or "", "timeout"
        stdout = stdout.decode() if isinstance(stdout, bytes) else stdout
        stderr = stderr.decode() if isinstance(stderr, bytes) else stderr
    (OUT / f"{label}.out").write_text(f"$ {' '.join(args)}\n# exit {code}\n{stdout}")
    (OUT / f"{label}.err").write_text(stderr)
    (OUT / f"{label}-requests.json").write_text(json.dumps(REQUESTS[start:], indent=1))
    check(f"{label}: exits 0", code == 0, f"exit {code}: {stderr[-300:]}")
    return [json.loads(line) for line in stdout.splitlines() if line.startswith("{")] if code == 0 else None


def tool_results(frames):
    return [f["message"] for f in frames or []
            if f.get("type") == "message_end" and f.get("message", {}).get("role") == "toolResult"]


def result_text(message):
    return "".join(block.get("text", "") for block in message.get("content", []))


# 1. A long result of multi-byte chars is cut in chars, never mid-char.
work = setup("unicode")
STEPS[:] = [{"code": "'é' * 40000"}, {"text": "DONE"}, {"code": "'é' * 70000"}, {"text": "DONE"}]
# The first run provisions the kernel venv.
short = tool_results(run(work, "unicode-40000", timeout=600))
check("unicode: 40000 chars come back whole", len(short) == 1 and result_text(short[0]).count("é") == 40000
      and "truncated" not in result_text(short[0]), short and result_text(short[0])[-120:])
long = tool_results(run(work, "unicode-70000", resume=True))
# The repr's opening quote is the first of the 65536 chars kept.
check("unicode: 70000 chars cut at 65536 chars", len(long) == 1 and result_text(long[0]).startswith(
    "'" + "é" * 65535 + "\n[... output truncated at 65536 chars ...]"), long and result_text(long[0])[-120:])

# 2. The summary request carries what a tool returned.
work = setup("compaction")
(work / "facts.txt").write_text("E2E_TOOL_OBSERVATION_9c871: the measured value is 1739")
STEPS[:] = [{"code": 'print(open("facts.txt").read())'}, {"text": "Read the file."}]
run(work, "compaction-seed")
STEPS[:] = [{"text": "Continuing with a separate recent conversation turn."}]
run(work, "compaction-recent", "continue", resume=True)
before = len(REQUESTS)
run(work, "compaction-summary", "/compact", resume=True)
summaries = [r for r in REQUESTS[before:] if is_summary(r)]
check("compaction: summary input holds the tool's output",
      bool(summaries) and any("E2E_TOOL_OBSERVATION_9c871" in json.dumps(r) for r in summaries),
      f"{len(summaries)} summary requests")

# 3. A tool turn over the threshold compacts before the next model request,
# then the run goes on. 32768 window - 4096 output - 4096 reserve = 24576.
work = setup("threshold", window=32768)
STEPS[:] = [{"code": 'print("first")', "usage": 25000}, {"code": 'print("second")', "usage": 3000},
            {"text": "DONE", "usage": 3100}]
before = len(REQUESTS)
frames = run(work, "threshold-loop")
events = [f["type"] for f in frames or [] if f.get("type") in ("turn_start", "compaction_start", "compaction_end")]
kinds = ["summary" if is_summary(r) else "turn" for r in REQUESTS[before:]]
check("threshold: compaction runs right after the first tool turn",
      events[:3] == ["turn_start", "compaction_start", "compaction_end"], events)
check("threshold: one model request before the summary, the run resumes after it",
      kinds[0] == "turn" and kinds[1] == "summary" and kinds.count("turn") == 3 and kinds[-1] == "turn", kinds)
check("threshold: the run finishes its tool chain", len(tool_results(frames)) == 2
      and any(f.get("type") == "message_end" and "DONE" in json.dumps(f.get("message", {}).get("content"))
              for f in frames or []), "")

# 4. A raising cell is an error result; a plain one is not.
work = setup("error-flag")
STEPS[:] = [{"code": 'raise ValueError("e2e-cell-failure")'}, {"code": "print('fine')"}, {"text": "DONE"}]
results = tool_results(run(work, "error-flag"))
check("error flag: the raising cell has isError true", len(results) == 2 and results[0].get("isError") is True
      and results[0].get("details", {}).get("errorEname") == "ValueError", results and results[0].get("isError"))
check("error flag: the next cell has isError false", len(results) == 2 and results[1].get("isError") is False, "")

# 5. Background work that mutates after the debounced snapshot is saved.
work = setup("background-snapshot")
cell = """import asyncio
from pathlib import Path
state = {'value': 'before'}
async def mutate_later():
    while not Path('release').exists():
        await asyncio.sleep(0.01)
    state['value'] = 'after'
    Path('changed').write_text(state['value'])
_task = asyncio.create_task(mutate_later())
print('background task ready')
"""


def release_after_snapshot(_body):
    deadline = time.monotonic() + 20
    while not list(work.rglob("kernel-state.dill")):
        if time.monotonic() > deadline:
            raise TimeoutError("no debounced snapshot committed")
        time.sleep(0.01)
    (work / "release").write_text("go")
    while not (work / "changed").exists():
        if time.monotonic() > deadline:
            raise TimeoutError("background mutation did not finish")
        time.sleep(0.01)
    return {"text": "DONE"}


STEPS[:] = [{"code": cell}, release_after_snapshot]
run(work, "background-before")
STEPS[:] = [{"code": "print(state)"}, {"text": "DONE"}]
restored = tool_results(run(work, "background-after", resume=True))
check("background: the resumed kernel has the late mutation",
      len(restored) == 1 and "'after'" in result_text(restored[0]), restored and result_text(restored[0]))

# 6. Every restored cell-defined function reads live globals, wherever it
# sits, and restored locks are free.
work = setup("class-snapshot")
cell = """import collections, functools, threading, types
G = 1
class Base:
    def base(self):
        return G
class Reader(Base):
    def read(self):
        return G
    @staticmethod
    def static():
        return G
    @classmethod
    def klass(cls):
        return G
    @property
    def prop(self):
        return G
    @functools.cached_property
    def cached(self):
        return G
class Slotted:
    __slots__ = ('fn',)
reader = Reader()
callback = reader.read
slotted = Slotted()
slotted.fn = lambda: G
space = types.SimpleNamespace(fn=lambda: G)
factory = collections.defaultdict(lambda: G)
queue = collections.deque([lambda: G])
cond = threading.Condition()
print(reader.read())
"""
STEPS[:] = [{"code": cell}, {"text": "DONE"}]
run(work, "class-before")
probe = ("G = 2\nr = Reader()\nwith cond:\n    cond.notify_all()\n"
         "print(G, reader.read(), r.read(), r.base(), Reader.static(), Reader.klass(), r.prop, r.cached, reader.base(),"
         " callback(), slotted.fn(), space.fn(), factory['x'], queue[0]())")
STEPS[:] = [{"code": probe}, {"text": "DONE"}]
after = tool_results(run(work, "class-after", resume=True))
check("class: every restored function sees G = 2 and the Condition is free", len(after) == 1
      and result_text(after[0]).strip().startswith(" ".join(["2"] * 14)), after and result_text(after[0]))

SERVER.shutdown()
print(f"{len(FAILED)} failed" if FAILED else "all passed")
sys.exit(1 if FAILED else 0)
