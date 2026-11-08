#!/usr/bin/env python3
"""End-to-end checks of the per-request output budget on the real binary:
a request asks for the model's whole max output, a long context clamps it
to the room the window leaves (measured from the last answer's usage), a
folded thinking budget stays inside that room, and a response the API cut
at that limit goes on (three times in a row at most).

usage: scripts/unharnessed-output-budget-e2e.py <out-dir>

The model is a stand-in on 127.0.0.1 that answers as the Anthropic Messages
API does, saved as the `fake` provider in a throwaway agent dir's
models.json; its answers report the context sizes the scenarios need. Each
scenario writes <out-dir>/<name>.out: the command, then every line the
binary printed; the requests the stand-in received go to
<out-dir>/<name>.requests.json. UNHARNESSED_BIN picks the binary (default
target/debug/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import subprocess
import sys
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/debug/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
WORK = OUT / "work"
WORK.mkdir(parents=True, exist_ok=True)
# A throwaway HOME and agent dir: the stand-in is the only model the session
# can reach. The uv cache stays the real one.
HOME = OUT / "home"
AGENT = HOME / ".unharnessed"
AGENT.mkdir(parents=True, exist_ok=True)
ENV = {key: value for key, value in os.environ.items()
       if not key.startswith(("UNHARNESSED_", "UH_", "ANTHROPIC_"))}
ENV.update(HOME=str(HOME), UNHARNESSED_CODING_AGENT_DIR=str(AGENT),
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
           UNHARNESSED_PACKAGE_DIR=os.environ.get("UNHARNESSED_PACKAGE_DIR", str(ROOT)),
           UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), UNHARNESSED_OFFLINE="1", DO_NOT_TRACK="1")
FAILED = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def tool_call(code, context_tokens):
    return {"tool": code, "input": context_tokens}


def text(answer, context_tokens):
    return {"text": answer, "input": context_tokens}


def cut(thinking, output_tokens):
    """A response the API cut at max_tokens while the model still thought."""
    return {"thinking": thinking, "input": 2_000, "output": output_tokens}


def sse(answer):
    """One Messages API stream: a tool call, a text block, or a thinking
    block cut at max_tokens."""
    events = [("message_start", {"type": "message_start", "message": {
        "id": "msg_e2e", "type": "message", "role": "assistant", "model": "fake", "content": [],
        "stop_reason": None, "usage": {"input_tokens": answer["input"], "output_tokens": 1,
                                       "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}})]
    deltas = []
    if "tool" in answer:
        block = {"type": "tool_use", "id": "toolu_e2e", "name": "ipython", "input": {}}
        deltas.append({"type": "input_json_delta", "partial_json": json.dumps({"code": answer["tool"]})})
        stop = "tool_use"
    elif "thinking" in answer:
        block = {"type": "thinking", "thinking": "", "signature": ""}
        deltas += [{"type": "thinking_delta", "thinking": answer["thinking"]},
                   {"type": "signature_delta", "signature": "sig-e2e"}]
        stop = "max_tokens"
    else:
        block = {"type": "text", "text": ""}
        deltas.append({"type": "text_delta", "text": answer["text"]})
        stop = "end_turn"
    events.append(("content_block_start", {"type": "content_block_start", "index": 0, "content_block": block}))
    events += [("content_block_delta", {"type": "content_block_delta", "index": 0, "delta": delta})
               for delta in deltas]
    events += [("content_block_stop", {"type": "content_block_stop", "index": 0}),
               # The final usage, as the Messages API sends it: input counts included.
               ("message_delta", {"type": "message_delta", "delta": {"stop_reason": stop}, "usage": {
                   "input_tokens": answer["input"], "output_tokens": answer.get("output", 20),
                   "cache_read_input_tokens": 0, "cache_creation_input_tokens": 0}}),
               ("message_stop", {"type": "message_stop"})]
    return "".join(f"event: {name}\ndata: {json.dumps(data)}\n\n" for name, data in events).encode()


class StandIn:
    """Answers each POST .../messages with the next scripted answer."""

    def __init__(self):
        self.answers, self.requests = [], []
        stand_in = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers["Content-Length"])))
                stand_in.requests.append(body)
                answer = stand_in.answers.pop(0) if stand_in.answers else text("spare", 1000)
                payload = sse(answer)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.send_header("Content-Length", str(len(payload)))
                self.end_headers()
                self.wfile.write(payload)

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base_url = f"http://127.0.0.1:{self.server.server_address[1]}"


STAND_IN = StandIn()


def run(name, model, thinking, answers):
    """One `-p` turn against the stand-in; return the main-loop requests."""
    (AGENT / "models.json").write_text(json.dumps({"providers": {"fake": {
        "name": "Fake", "baseUrl": STAND_IN.base_url, "api": "anthropic-messages", "apiKey": "sk-e2e",
        "models": [dict(model, name=model["id"], reasoning=True, input=["text"],
                        cost={"input": 0, "output": 0, "cacheRead": 0, "cacheWrite": 0})]}}}))
    STAND_IN.answers, STAND_IN.requests = list(answers), []
    args = ["--mode", "json", "--no-session", "--provider", "fake", "--model", model["id"],
            "--thinking", thinking, "go"]
    with open(OUT / f"{name}.out", "w") as out:
        out.write(f"$ unharnessed {' '.join(args)}  # models.json: {json.dumps(model)}, answers: "
                  f"{json.dumps(answers)}\n")
        out.flush()
        code = subprocess.run([BIN, *args], cwd=WORK, stdout=out, stderr=subprocess.STDOUT,
                              env=ENV, timeout=300).returncode
    requests = [request for request in STAND_IN.requests if request.get("tools")]
    (OUT / f"{name}.requests.json").write_text(json.dumps(STAND_IN.requests, indent=1))
    return code, requests


OPUS = {"id": "claude-opus-5-5", "contextWindow": 1_000_000, "maxTokens": 128_000}

# 1. A fresh request asks for the model's whole max output.
code, requests = run("whole_max_output", OPUS, "max", [text("done", 2_000)])
sent = [request.get("max_tokens") for request in requests]
check("a request asks for the model's whole max output", code == 0 and sent == [128_000],
      f"exit {code}, max_tokens {sent}")

# 2. The last answer's usage measures a long context: the next request is
# clamped to the room the window leaves (chars/4 alone sees a few thousand
# tokens and would leave 128_000).
small = dict(OPUS, contextWindow=200_000)
code, requests = run("anchored_clamp", small, "max",
                     [tool_call("print(1)", 150_000), text("done", 150_100)])
sent = [request.get("max_tokens") for request in requests]
check("a long context clamps the request to the room left",
      code == 0 and len(sent) == 2 and sent[0] == 128_000 and 45_000 < sent[1] <= 50_000,
      f"exit {code}, max_tokens {sent}")

# 3. A model with a thinking budget folds it inside that room.
sonnet = {"id": "claude-sonnet-4-5", "contextWindow": 200_000, "maxTokens": 64_000}
code, requests = run("budget_inside_room", sonnet, "high",
                     [tool_call("print(1)", 175_000), text("done", 175_100)])
sent = [(request.get("max_tokens"), (request.get("thinking") or {}).get("budget_tokens")) for request in requests]
check("a fresh budgeted request asks the model's max with its budget inside",
      code == 0 and len(sent) == 2 and sent[0] == (64_000, 16_384), f"exit {code}, (max_tokens, budget) {sent}")
check("a long context keeps max_tokens and the budget inside the room",
      len(sent) == 2 and sent[1][0] is not None and sent[1][0] <= 25_000
      and sent[1][1] is not None and sent[1][1] < sent[1][0], f"(max_tokens, budget) {sent}")

# 4. A response the API cut at the output limit, the model still thinking,
# goes on in the same run: the next request carries the cut thinking, then
# the row that asks the model to go on.
THOUGHT = "Weighing the muzzle blast against the room's reflections"
code, requests = run("cut_goes_on", OPUS, "max", [cut(THOUGHT, 128_000), text("done", 2_100)])
tail = requests[1]["messages"][-2:] if len(requests) == 2 else []
check("a response cut at the output limit goes on in the same run",
      code == 0 and len(tail) == 2 and tail[0]["role"] == "assistant"
      and any(block.get("thinking") == THOUGHT for block in tail[0]["content"])
      and tail[1]["role"] == "user" and "output limit" in json.dumps(tail[1]["content"]),
      f"exit {code}, {len(requests)} requests, last messages {json.dumps(tail)[:600]}")
events = [json.loads(line) for line in (OUT / "cut_goes_on.out").read_text().splitlines()[1:]
          if line.startswith("{")]
ends = [event["message"].get("content") for event in events if event.get("type") == "message_end"]
check("and the run answers after it",
      ends[-1:] == [[{"type": "text", "text": "done"}]]
      and [event["type"] for event in events].count("agent_end") == 1, f"last message {ends[-1:]}")

# 5. Cut again and again: three times it goes on, then the run ends on the cut.
code, requests = run("cut_three_times_at_most", OPUS, "max", [cut(THOUGHT, 128_000)] * 5)
check("a response cut again and again goes on three times, then the run ends",
      code == 0 and len(requests) == 4, f"exit {code}, {len(requests)} requests")

# 6. Cut before any output: not an answer to go on with.
code, requests = run("cut_before_output", OPUS, "max", [cut("", 0)])
check("a response cut before any output does not go on", len(requests) == 1,
      f"exit {code}, {len(requests)} requests")

STAND_IN.server.shutdown()
sys.exit(1 if FAILED else 0)
