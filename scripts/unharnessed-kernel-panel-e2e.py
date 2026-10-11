#!/usr/bin/env python3
"""End-to-end check of the kernel on the panel (the background commands and
the REPL variables) on the real binary, with a real kernel and no network:
the faux provider runs the cells; then the real provider assembly (an
offline API key), whose kernel starts before the session opens (the
prewarm).

usage: scripts/unharnessed-kernel-panel-e2e.py <out-dir>

Writes <out-dir>/kernel_panel.out and kernel_panel_prewarm.out: the command
run, then every ACP frame with the seconds since start. The kernel venv
lives in <out-dir> (UNHARNESSED_BIN picks the binary, default
target/release/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import queue
import socket
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/release/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
WORK = OUT / "work"
WORK.mkdir(parents=True, exist_ok=True)
# A throwaway HOME and agent dir: no credentials, so the faux provider is
# the only model the session can reach. The uv cache stays the real one.
HOME = OUT / "home"
(HOME / "agent").mkdir(parents=True, exist_ok=True)
ENV = dict(os.environ, HOME=str(HOME), UNHARNESSED_AGENT_DIR=str(HOME / "agent"),
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
           UNHARNESSED_PACKAGE_DIR=os.environ.get("UNHARNESSED_PACKAGE_DIR", str(ROOT)),
           UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), DO_NOT_TRACK="1")
FAILED = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def call(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


PORT = free_port()
CELL = f'''
n = 42
cfg = {{"model": "claude", "retries": 3}}
def fetch(url, *, retries=3):
    """GET url, retrying on 5xx."""
server = bash("exec python3 -u -m http.server {PORT} --bind 127.0.0.1")
server.pid
quick = bash("sleep 1; echo quick-done")
fg = await bash("echo foreground-only")
'''
RESPONSES = [call(CELL), "started", "quick noted", "server stop noted", "spare", "spare"]



def launch(name, env, header):
    """Start one `--mode acp` process; the helpers below talk to the last one."""
    global child, start, frames, inbox, answers, out, ids
    child = subprocess.Popen([BIN, "--mode", "acp", "--session-dir", str(WORK / "sessions")],
                             cwd=WORK, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True, env=env)
    start = time.time()
    frames, inbox, answers = [], queue.Queue(), {}
    out = open(OUT / f"{name}.out", "w")
    out.write(f"$ {header} unharnessed --mode acp\n")
    ids = iter(range(1, 1000))

    def read(child, out, frames, inbox):
        for line in child.stdout:
            if line.strip():
                frame = json.loads(line)
                stamp = round(time.time() - start, 2)
                out.write(json.dumps({"t": stamp, "frame": frame}) + "\n")
                out.flush()
                frames.append((stamp, frame))
                inbox.put(frame)

    threading.Thread(target=read, args=(child, out, frames, inbox), daemon=True).start()


def close(session):
    wait(send("session/close", {"sessionId": session}))
    child.stdin.close()
    child.wait(timeout=60)
    out.close()


def send(method, params):
    request_id = next(ids)
    line = json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
    out.write(f"> {line}\n")
    child.stdin.write(line + "\n")
    child.stdin.flush()
    return request_id


def pump(timeout):
    frame = inbox.get(timeout=timeout)
    if "id" in frame and "method" not in frame:
        answers[frame["id"]] = frame


def wait(request_id, timeout=180):
    deadline = time.time() + timeout
    while request_id not in answers:
        pump(max(0.05, deadline - time.time()))
    return answers[request_id]


def listen(seconds):
    end = time.time() + seconds
    while time.time() < end:
        try:
            pump(max(0.05, end - time.time()))
        except queue.Empty:
            pass


def changed(key):
    return [frame["params"][key] for _, frame in frames
            if frame.get("method") == "_unharnessed/kernel/changed" and key in frame["params"]]


def until(predicate, seconds):
    end = time.time() + seconds
    while time.time() < end and not predicate():
        listen(0.2)
    return predicate()


script = json.dumps({"responses": RESPONSES})
launch("kernel_panel", dict(ENV, UNHARNESSED_FAUX_SCRIPT=script), f"UNHARNESSED_FAUX_SCRIPT='{script}'")
wait(send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
session = wait(send("session/new", {"cwd": str(WORK), "mcpServers": []}))["result"]["sessionId"]
wait(send("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}))

# The variables, in the order the cell made them.
until(lambda: changed("variables"), 10)
variables = (changed("variables") or [[]])[-1]
names = [row["name"] for row in variables]
check("the variables list what the cell made, in order",
      names == ["n", "cfg", "fetch", "server", "quick", "fg"], f"names {names}")
by_name = {row["name"]: row for row in variables}
check("a scalar shows its value", by_name.get("n", {}).get("value") == "42", f"{by_name.get('n')}")
check("a dict shows its key count",
      (by_name.get("cfg", {}).get("len"), by_name.get("cfg", {}).get("unit")) == (2, "keys"), f"{by_name.get('cfg')}")
check("a function shows its signature",
      by_name.get("fetch", {}).get("signature") == "(url, *, retries=3)", f"{by_name.get('fetch')}")

# The commands: the server and the short one, never the awaited one-shot.
def server_row():
    for commands in reversed(changed("commands")):
        for row in commands:
            if "http.server" in row["command"]:
                return row
    return None


until(lambda: (server_row() or {}).get("ports") == [PORT], 15)
row = server_row() or {}
check("the server runs in the background, on its port",
      row.get("status") == "running" and row.get("ports") == [PORT], f"row {row}")
every = [command for commands in changed("commands") for command in commands]
check("an awaited one-shot never shows", all("foreground-only" not in c["command"] for c in every),
      f"commands {[c['command'] for c in every]}")


def quick_row():
    rows = [c for c in (changed("commands") or [[]])[-1] if "quick-done" in c["command"]]
    return rows[0] if rows else None


until(lambda: (quick_row() or {}).get("status") == "finished", 10)
quick = quick_row() or {}
check("the short command ends well", quick.get("status") == "finished" and quick.get("exitCode") == 0
      and isinstance(quick.get("durationMs"), int), f"row {quick}")

# A line's preview, between cells; an unknown name is refused.
answer = wait(send("_unharnessed/kernel/preview", {"sessionId": session, "name": "cfg"}))
check("a dict previews as its repr",
      answer.get("result", {}).get("preview") == "{'model': 'claude', 'retries': 3}", f"answer {answer}")
answer = wait(send("_unharnessed/kernel/preview", {"sessionId": session, "name": "fetch"}))
check("a function previews as its def and docstring",
      answer.get("result", {}).get("preview")
      == 'def fetch(url, *, retries=3):\n    """GET url, retrying on 5xx."""', f"answer {answer}")
answer = wait(send("_unharnessed/kernel/preview", {"sessionId": session, "name": "asyncio"}))
check("a name the agent did not make is refused",
      answer.get("error", {}).get("data", {}).get("details") == "Unknown variable", f"answer {answer}")

# The server's last lines, then its stop from the panel.
server_id = row.get("id")
until(lambda: "Serving HTTP" in (wait(send("_unharnessed/kernel/tail",
      {"sessionId": session, "id": server_id, "lines": 7})).get("result", {}).get("tail") or ""), 10)
answer = wait(send("_unharnessed/kernel/tail", {"sessionId": session, "id": server_id, "lines": 7}))
check("the tail shows the server's output", "Serving HTTP" in answer.get("result", {}).get("tail", ""),
      f"answer {answer}")
answer = wait(send("_unharnessed/kernel/kill", {"sessionId": session, "id": server_id}))
check("the panel stops the server", answer.get("result", {}).get("killed") is True, f"answer {answer}")
until(lambda: (server_row() or {}).get("status") == "finished", 10)
row = server_row() or {}
check("the stopped server is marked killed", row.get("status") == "finished" and row.get("killed") is True
      and row.get("ports") == [], f"row {row}")
with socket.socket() as probe:
    refused = probe.connect_ex(("127.0.0.1", PORT)) != 0
check("the port is free again", refused)

close(session)

# The real provider assembly starts the kernel before the session opens; the
# panel must hear that kernel too (its bootstrap cell is the first change).
# An API key that is never sent: the session runs offline, and no prompt goes.
PREWARM_HOME = OUT / "home-prewarm"
(PREWARM_HOME / "agent").mkdir(parents=True, exist_ok=True)
(PREWARM_HOME / "agent" / "auth.json").write_text(json.dumps({"openai": {"type": "api_key", "key": "sk-e2e"}}))
launch("kernel_panel_prewarm", dict(ENV, HOME=str(PREWARM_HOME), UNHARNESSED_AGENT_DIR=str(PREWARM_HOME / "agent"),
                                    UNHARNESSED_OFFLINE="1"), "UNHARNESSED_OFFLINE=1 (offline API key)")
wait(send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
session = wait(send("session/new", {"cwd": str(WORK), "mcpServers": []}))["result"]["sessionId"]
heard = until(lambda: changed("commands") or changed("variables"), 90)
check("the panel hears a kernel that started before the session", heard,
      "no _unharnessed/kernel/changed within 90 s")
close(session)

print(f"traces: {OUT / 'kernel_panel.out'}, {OUT / 'kernel_panel_prewarm.out'}")
sys.exit(1 if FAILED else 0)
