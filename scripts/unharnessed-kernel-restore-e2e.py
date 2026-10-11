#!/usr/bin/env python3
"""End-to-end check of `_unharnessed/kernel/restored` on the real binary, with
a real kernel and no network: a first process runs one cell (the faux
provider) and closes, saving the kernel's namespace; a second one resumes the
session, revives it, and must tell the panel once.

usage: scripts/unharnessed-kernel-restore-e2e.py <out-dir>

Writes <out-dir>/<case>-first.out and <case>-resumed.out: the command run,
then every ACP frame with the seconds since start. The kernel venv lives in
<out-dir> (UNHARNESSED_BIN picks the binary, default
target/release/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import queue
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/release/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
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


class Agent:
    """One `--mode acp` process and the frames it sent."""

    def __init__(self, name, work, cell, resume=None):
        script = json.dumps({"responses": [{"content": [{"type": "toolCall", "name": "ipython",
                                                          "arguments": {"code": cell}}]}, "ok", "spare"]})
        args = [BIN, "--mode", "acp", "--session-dir", str(work / "sessions")]
        if resume:
            args += ["--resume", str(resume)]
        self.child = subprocess.Popen(args, cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.DEVNULL, text=True,
                                      env=dict(ENV, UNHARNESSED_FAUX_SCRIPT=script))
        self.start = time.time()
        self.frames, self.inbox, self.answers = [], queue.Queue(), {}
        self.ids = iter(range(1, 1000))
        self.out = open(OUT / f"{name}.out", "w")
        self.out.write(f"$ UNHARNESSED_FAUX_SCRIPT='{script}' {' '.join(args[:1] + ['--mode', 'acp'] + args[3:])}\n")
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.child.stdout:
            if line.strip():
                frame = json.loads(line)
                stamp = round(time.time() - self.start, 2)
                self.out.write(json.dumps({"t": stamp, "frame": frame}) + "\n")
                self.out.flush()
                self.frames.append(frame)
                self.inbox.put(frame)

    def send(self, method, params):
        request_id = next(self.ids)
        line = json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        self.out.write(f"> {line}\n")
        self.child.stdin.write(line + "\n")
        self.child.stdin.flush()
        return request_id

    def pump(self, timeout):
        frame = self.inbox.get(timeout=timeout)
        if "id" in frame and "method" not in frame:
            self.answers[frame["id"]] = frame

    def wait(self, request_id, timeout=180):
        deadline = time.time() + timeout
        while request_id not in self.answers:
            self.pump(max(0.05, deadline - time.time()))
        return self.answers[request_id]

    def listen(self, seconds, until=lambda: False):
        end = time.time() + seconds
        while time.time() < end and not until():
            try:
                self.pump(min(0.2, max(0.05, end - time.time())))
            except queue.Empty:
                pass

    def restored(self):
        """Each `restored` notification, and where it came among the frames."""
        return [(at, frame["params"]) for at, frame in enumerate(self.frames)
                if frame.get("method") == "_unharnessed/kernel/restored"]

    def at(self, request_id):
        return next(at for at, frame in enumerate(self.frames) if frame.get("id") == request_id
                    and "method" not in frame)

    def variables(self):
        return [[row["name"] for row in frame["params"]["variables"]] for frame in self.frames
                if frame.get("method") == "_unharnessed/kernel/changed" and "variables" in frame["params"]]

    def close(self, session):
        self.wait(self.send("session/close", {"sessionId": session}))
        self.child.stdin.close()
        self.child.wait(timeout=60)
        self.out.close()


def first(case, cell):
    """A session runs `cell` and closes: its saved id (the one `session/list`
    gives, which `session/load` takes) and file."""
    work = OUT / case / "work"
    work.mkdir(parents=True, exist_ok=True)
    agent = Agent(f"{case}-first", work, cell)
    agent.wait(agent.send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
    session = agent.wait(agent.send("session/new", {"cwd": str(work), "mcpServers": []}))["result"]["sessionId"]
    agent.wait(agent.send("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "go"}]}))
    agent.close(session)
    check(f"{case}: a new session tells of no revive", agent.restored() == [], f"{agent.restored()}")
    files = sorted((work / "sessions").glob("*.jsonl"))
    saved = json.loads(files[0].read_text().splitlines()[0])["id"] if len(files) == 1 else None
    snapshot = work / "session-artifacts" / str(saved) / "kernel-state.dill"
    check(f"{case}: the closed session saved its kernel", snapshot.exists(), f"files {files}, no {snapshot}")
    return work, saved, files[0] if files else None


def resumed(case, work, session, file, wait_before_load=0.0):
    agent = Agent(f"{case}-resumed", work, "spare", resume=file)
    # The kernel revives at startup: given time, before the session opens.
    time.sleep(wait_before_load)
    agent.wait(agent.send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
    load = agent.send("session/load", {"sessionId": session, "cwd": str(work), "mcpServers": []})
    answer = agent.wait(load)
    check(f"{case}: the session loads", "result" in answer, f"{answer}")
    agent.listen(90, until=agent.restored)
    # Long enough for a second notification, were there one.
    agent.listen(5)
    return agent, load


# A revive after the session opened: the watcher tells.
work, session, file = first("at-once", "n = 42")
agent, load = resumed("at-once", work, session, file)
restored = agent.restored()
check("at-once: one notification, restored", [params for _, params in restored]
      == [{"sessionId": session, "restored": True}], f"{restored}")
where = "after" if restored and restored[0][0] > agent.at(load) else "before"
print(f"INFO  at-once: it came {where} the session/load answer")
print(f"INFO  at-once: variables lists the panel got: {agent.variables()}")
agent.close(session)

# A revive before the session opened: session/load tells, after the replay.
agent, load = resumed("late", work, session, file, wait_before_load=20)
restored = agent.restored()
check("late: one notification, restored", [params for _, params in restored]
      == [{"sessionId": session, "restored": True}], f"{restored}")
check("late: it comes with the replay, before the session/load answer",
      bool(restored) and restored[0][0] < agent.at(load), f"{restored}, answer at {agent.at(load)}")
print(f"INFO  late: variables lists the panel got: {agent.variables()}")
agent.close(session)

# Nothing was saved but the runtime's own names: nothing to tell.
work, session, file = first("empty", "print(6 * 7)")
agent, _ = resumed("empty", work, session, file)
agent.listen(15)
check("empty: an empty namespace tells of no revive", agent.restored() == [], f"{agent.restored()}")
agent.close(session)

# Every name fails to revive (its class's module is gone): restored false.
mods = OUT / "mods"
mods.mkdir(parents=True, exist_ok=True)
(mods / "gone_mod.py").write_text("class Thing:\n    pass\n")
cell = f'thing = __import__("sys").path.insert(0, {str(mods)!r}) or __import__("gone_mod").Thing()'
work, session, file = first("failed", cell)
(mods / "gone_mod.py").unlink()
agent, _ = resumed("failed", work, session, file)
restored = agent.restored()
check("failed: one notification, not restored", [params for _, params in restored]
      == [{"sessionId": session, "restored": False}], f"{restored}")
agent.close(session)

print(f"traces: {OUT}/*.out")
sys.exit(1 if FAILED else 0)
