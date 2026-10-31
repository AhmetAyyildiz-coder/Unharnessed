#!/usr/bin/env python3
"""End-to-end check of /compact when the provider refuses the turn-prefix
summary (Anthropic `stop_reason: "refusal"` on the split turn's prefix,
read alone). Real binary, a local fake Anthropic Messages API (no network).

usage: scripts/unharnessed-compaction-refusal-e2e.py <out-dir>

Ways it can fail, each checked below:
1. the refusal still fails the whole compaction;
2. the compaction commits but still cuts mid-turn, so the turn's start is
   neither summarized nor kept;
3. another turn-prefix error (a rejected request) is swallowed instead of
   failing the compaction;
4. a split turn with no history before it falls back to a summary of
   nothing instead of failing.

Writes <out-dir>/<case>.out (the command run, then its json-mode stdout),
<case>.err, and <case>-requests.json for every run (UNHARNESSED_BIN picks
the binary, default target/release/unharnessed). Exits non-zero when a
check fails.
"""
import json
import os
import subprocess
import sys
import threading
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
REQUESTS = []
# How the fake answers the turn-prefix summary request: "refusal" or "reject".
PREFIX_REPLY = ["refusal"]
SUMMARY = "E2E_HISTORY_SUMMARY_41d2"


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"), flush=True)
    if not ok:
        FAILED.append(name)


def last_text(body):
    return json.dumps(body["messages"][-1])


class Provider(BaseHTTPRequestHandler):
    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
        REQUESTS.append(body)
        # Long enough that the last reply alone passes keepRecentTokens.
        text, stop = "A model answer long enough to fill the kept tail. " * 4, "end_turn"
        if "This is the PREFIX of a turn" in last_text(body):
            if PREFIX_REPLY[0] == "reject":
                error = json.dumps({"type": "error", "error": {"type": "invalid_request_error",
                                                               "message": "E2E rejected request"}}).encode()
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(error)))
                self.end_headers()
                self.wfile.write(error)
                return
            text, stop = "", "refusal"
        elif "<conversation>" in last_text(body):
            text = SUMMARY
        events = [
            ("message_start", {"type": "message_start", "message": {
                "id": f"msg_{len(REQUESTS)}", "type": "message", "role": "assistant", "content": [],
                "model": "e2e-model", "stop_reason": None, "usage": {"input_tokens": 100, "output_tokens": 1}}}),
            ("content_block_start", {"type": "content_block_start", "index": 0,
                                     "content_block": {"type": "text", "text": ""}}),
            ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                     "delta": {"type": "text_delta", "text": text}}),
            ("content_block_stop", {"type": "content_block_stop", "index": 0}),
            ("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop},
                               "usage": {"output_tokens": 10}}),
            ("message_stop", {"type": "message_stop"}),
        ]
        wire = "".join(f"event: {name}\ndata: {json.dumps(data)}\n\n" for name, data in events).encode()
        self.send_response(200)
        self.send_header("Content-Type", "text/event-stream")
        self.send_header("request-id", f"req_e2e_{len(REQUESTS)}")
        self.send_header("Content-Length", str(len(wire)))
        self.end_headers()
        self.wfile.write(wire)

    def log_message(self, *args):
        pass


SERVER = ThreadingHTTPServer(("127.0.0.1", 0), Provider)
threading.Thread(target=SERVER.serve_forever, daemon=True).start()


def setup(name):
    work = OUT / name
    agent = work / "agent"
    agent.mkdir(parents=True, exist_ok=True)
    model = {"id": "e2e-model", "name": "E2E", "reasoning": False, "input": ["text"], "contextWindow": 128000,
             "maxTokens": 4096, "cost": {"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0}}
    (agent / "models.json").write_text(json.dumps({"providers": {"e2e": {
        "api": "anthropic-messages", "baseUrl": f"http://127.0.0.1:{SERVER.server_port}",
        "apiKey": "e2e-local-dummy", "models": [model]}}}))
    # The user's setting: keep 10 tokens, so /compact cuts inside the last turn.
    (agent / "settings.json").write_text(json.dumps({"compaction": {"keepRecentTokens": 10}}))
    return work


def run(work, label, prompt, resume=True):
    """One `-p --mode json` process; returns (exit code, stdout)."""
    env = dict(BASE_ENV, UNHARNESSED_AGENT_DIR=str(work / "agent"), UNHARNESSED_CODING_AGENT_DIR=str(work / "agent"))
    args = [BIN, "-p", "--mode", "json", "--model", "e2e/e2e-model", "--session-dir", str(work / "sessions")]
    if resume:
        args.append("-c")
    args.append(prompt)
    start = len(REQUESTS)
    result = subprocess.run(args, cwd=work, env=env, text=True, capture_output=True, timeout=120,
                            stdin=subprocess.DEVNULL)
    (OUT / f"{label}.out").write_text(f"$ {' '.join(args)}\n# exit {result.returncode}\n{result.stdout}")
    (OUT / f"{label}.err").write_text(result.stderr)
    (OUT / f"{label}-requests.json").write_text(json.dumps(REQUESTS[start:], indent=1))
    return result.returncode, result.stdout + result.stderr


def entries(work):
    [session] = (work / "sessions").glob("*.jsonl")
    return [json.loads(line) for line in session.read_text().splitlines() if line.strip()]


def user_entry_id(work, marker):
    return next(e["id"] for e in entries(work) if e.get("type") == "message"
                and e["message"].get("role") == "user" and marker in json.dumps(e["message"]))


def compactions(work):
    return [e for e in entries(work) if e.get("type") == "compaction"]


# 1 + 2. The refused prefix: the compaction commits, cut at the turn start.
work = setup("refusal")
code, _ = run(work, "refusal-turn1", "E2E_TURN_ONE_77ab", resume=False)
check("refusal: first turn exits 0", code == 0, code)
code, _ = run(work, "refusal-turn2", "E2E_TURN_TWO_5e03")
check("refusal: second turn exits 0", code == 0, code)
before = len(REQUESTS)
code, output = run(work, "refusal-compact", "/compact")
prefix_requests = [r for r in REQUESTS[before:] if "This is the PREFIX of a turn" in last_text(r)]
check("refusal: the turn prefix was summarized on its own and refused", len(prefix_requests) == 1,
      f"{len(prefix_requests)} prefix requests")
check("refusal: /compact succeeds", code == 0 and "summarization failed" not in output, output[-400:])
found = compactions(work)
check("refusal: one compaction entry", len(found) == 1, len(found))
if found:
    check("refusal: the whole second turn is kept (cut at its user message)",
          found[0]["firstKeptEntryId"] == user_entry_id(work, "E2E_TURN_TWO_5e03"), found[0]["firstKeptEntryId"])
    check("refusal: the summary is the history summary alone",
          found[0]["summary"].startswith(SUMMARY) and "Turn Context" not in found[0]["summary"],
          found[0]["summary"][:200])
before = len(REQUESTS)
code, _ = run(work, "refusal-after", "E2E_TURN_THREE_c9f1")
sent = json.dumps(REQUESTS[before:][:1])
check("refusal: the next request carries the summary and the kept turn verbatim",
      code == 0 and SUMMARY in sent and "E2E_TURN_TWO_5e03" in sent and "E2E_TURN_ONE_77ab" not in sent,
      sent[:400])

# 3. Any other turn-prefix error still fails the compaction.
PREFIX_REPLY[0] = "reject"
work = setup("reject")
run(work, "reject-turn1", "E2E_TURN_ONE_77ab", resume=False)
run(work, "reject-turn2", "E2E_TURN_TWO_5e03")
code, output = run(work, "reject-compact", "/compact")
check("reject: /compact reports the turn-prefix failure",
      "Turn prefix summarization failed" in output, output[-400:])
check("reject: no compaction entry", not compactions(work), len(compactions(work)))

# 4. A refused prefix with no history before the turn still fails.
PREFIX_REPLY[0] = "refusal"
work = setup("no-history")
run(work, "no-history-turn1", "E2E_TURN_ONE_77ab", resume=False)
code, output = run(work, "no-history-compact", "/compact")
check("no-history: /compact reports the refusal",
      "Turn prefix summarization failed" in output and "refused" in output, output[-400:])
check("no-history: no compaction entry", not compactions(work), len(compactions(work)))

SERVER.shutdown()
print(f"\n{len(FAILED)} failed" if FAILED else "\nall passed")
sys.exit(1 if FAILED else 0)
