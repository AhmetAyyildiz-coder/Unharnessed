#!/usr/bin/env python3
"""End-to-end checks of what the VS Code extension needs to keep one agent per
conversation alive (BACKGROUND_PLAN.md step 0), on the real binary, a real
kernel and the faux provider (no network).

usage: scripts/unharnessed-background-e2e.py <out-dir> [case...]

Cases: identity (a new conversation's saved id and file), reload (a login in
one agent reaches another), venv-lock (no venv build without its lock), stop
(an agent
stopped by closing its input or by SIGTERM saves its kernel first), stop-big
(how long that save takes for a big namespace).
Writes <out-dir>/<case>-*.out: the command run, then every ACP frame with the
seconds since start. The kernel venv lives in <out-dir> (UNHARNESSED_BIN picks
the binary, default target/release/unharnessed). Exits non-zero when a check
fails.
"""
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/release/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
CASES = sys.argv[2:] or ["identity", "reload", "venv-lock", "stop", "stop-big"]
# A throwaway HOME and agent dirs: no credentials, so the faux provider is
# the only model the session can reach. The uv cache stays the real one.
HOME = OUT / "home"
HOME.mkdir(parents=True, exist_ok=True)
BASE_ENV = {k: v for k, v in os.environ.items() if not k.startswith(("UNHARNESSED_", "UH_", "XDG_"))}
ENV = dict(BASE_ENV, HOME=str(HOME),
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
           UNHARNESSED_PACKAGE_DIR=os.environ.get("UNHARNESSED_PACKAGE_DIR", str(ROOT)),
           UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), UNHARNESSED_OFFLINE="1", DO_NOT_TRACK="1")
FAILED = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"), flush=True)
    if not ok:
        FAILED.append(name)


def info(text):
    print(f"INFO  {text}", flush=True)


def cell(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


class Agent:
    """One `--mode acp` process and the frames it sent."""

    def __init__(self, name, work, responses, resume=None, env=None):
        script = json.dumps({"responses": responses + ["spare"]})
        args = [BIN, "--mode", "acp", "--session-dir", str(work / "sessions")]
        if resume:
            args += ["--resume", str(resume)]
        # Each case has its own agent dir: a key one case saves stays out of the others.
        agent_dir = str(work.parent / "agent")
        self.child = subprocess.Popen(args, cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=open(OUT / f"{name}.err", "w"), text=True,
                                      env=dict(ENV, UNHARNESSED_FAUX_SCRIPT=script, UNHARNESSED_AGENT_DIR=agent_dir,
                                               UNHARNESSED_CODING_AGENT_DIR=agent_dir, **(env or {})))
        self.start = time.time()
        self.frames, self.inbox, self.answers = [], queue.Queue(), {}
        self.ids = iter(range(1, 1000))
        self.out = open(OUT / f"{name}.out", "w")
        self.out.write(f"$ UNHARNESSED_FAUX_SCRIPT='{script}' {' '.join(args)}\n")
        threading.Thread(target=self.read, daemon=True).start()

    def read(self):
        for line in self.child.stdout:
            if line.strip():
                frame = json.loads(line)
                self.out.write(json.dumps({"t": round(time.time() - self.start, 2), "frame": frame}) + "\n")
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

    def wait(self, request_id, timeout=600):
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

    def open(self, work):
        self.wait(self.send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
        return self.wait(self.send("session/new", {"cwd": str(work), "mcpServers": []}))

    def load(self, work, session):
        self.wait(self.send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
        return self.wait(self.send("session/load", {"sessionId": session, "cwd": str(work), "mcpServers": []}))

    def prompt(self, session, text="go", wait=True):
        request = self.send("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": text}]})
        return self.wait(request) if wait else request

    def variables(self):
        """The names in the last variables list the panel got."""
        lists = [[row["name"] for row in frame["params"]["variables"]] for frame in self.frames
                 if frame.get("method") == "_unharnessed/kernel/changed" and "variables" in frame["params"]]
        return lists[-1] if lists else []

    def stop(self, how, timeout=120):
        """Stop the process by closing its input or by a signal: the seconds it
        took to exit, and its exit code."""
        began = time.time()
        if how == "eof":
            self.child.stdin.close()
        else:
            self.child.send_signal(how)
        try:
            code = self.child.wait(timeout=timeout)
        except subprocess.TimeoutExpired:
            self.child.kill()
            code = "killed"
        took = round(time.time() - began, 2)
        self.out.write(f"# stopped by {how if how == 'eof' else signal.Signals(how).name}: exit {code} after {took}s\n")
        self.out.close()
        return took, code


def saved_session(work):
    """The saved id (the one `session/list` gives, which `session/load` takes)
    and file of the one conversation in `work`."""
    files = sorted((work / "sessions").glob("*.jsonl"))
    if len(files) != 1:
        return None, None
    return json.loads(files[0].read_text().splitlines()[0])["id"], files[0]


def snapshot(work, saved):
    return work / "session-artifacts" / str(saved) / "kernel-state.dill"


def workdir(name):
    work = OUT / name / "work"
    work.mkdir(parents=True, exist_ok=True)
    (OUT / name / "agent").mkdir(exist_ok=True)
    return work


def revived(name, work, saved, file):
    """Resume the conversation and wait for the kernel to revive: the names it
    shows."""
    agent = Agent(f"{name}-resumed", work, [], resume=file)
    agent.load(work, saved)
    agent.listen(90, until=lambda: any(f.get("method") == "_unharnessed/kernel/restored" for f in agent.frames))
    agent.listen(3)
    names = agent.variables()
    agent.stop("eof")
    return names


def stop_after_cell(name, how, code="b = 2"):
    """A first cell, saved by the debounce, then a second one and an
    immediate stop: the second must be in the saved kernel."""
    work = workdir(name)
    agent = Agent(name, work, [cell("a = 1"), "ok", cell(code), "ok"])
    session = agent.open(work)["result"]["sessionId"]
    agent.prompt(session)
    # Past the snapshot debounce (1.5 s): `a` is on disk.
    agent.listen(4)
    agent.prompt(session)
    took, exit_code = agent.stop(how)
    saved, file = saved_session(work)
    label = "input closed" if how == "eof" else signal.Signals(how).name
    info(f"{name}: exit {exit_code} {took}s after {label}")
    check(f"{name}: exits 0", exit_code == 0, f"exit {exit_code}")
    names = revived(name, work, saved, file) if file else []
    check(f"{name}: the last cell's variable is saved", "b" in names, f"revived names {names}")
    return took


def host_gone(name):
    """The client dies (VS Code closing): the agent's input, output and
    error pipes all close at once, mid-work. It still saves its kernel."""
    work = workdir(name)
    agent_dir = str(work.parent / "agent")
    # About 10 MB, so the save is still running when the pipes go.
    big = "b = [{'i': i, 's': str(i) * 4} for i in range(300_000)]"
    script = json.dumps({"responses": [cell("a = 1"), "ok", cell(big), "ok", "spare"]})
    args = [BIN, "--mode", "acp", "--session-dir", str(work / "sessions")]
    child = subprocess.Popen(args, cwd=work, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                             env=dict(ENV, UNHARNESSED_FAUX_SCRIPT=script, UNHARNESSED_AGENT_DIR=agent_dir,
                                      UNHARNESSED_CODING_AGENT_DIR=agent_dir))
    out = open(OUT / f"{name}.out", "w")
    out.write(f"$ UNHARNESSED_FAUX_SCRIPT='{script}' {' '.join(args)}\n")

    def ask(request_id, method, params):
        line = json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        out.write(f"> {line}\n")
        child.stdin.write((line + "\n").encode())
        child.stdin.flush()
        while True:
            frame = json.loads(child.stdout.readline())
            out.write(json.dumps(frame) + "\n")
            if frame.get("id") == request_id and "method" not in frame:
                return frame

    ask(1, "initialize", {"protocolVersion": 1, "clientCapabilities": {}})
    session = ask(2, "session/new", {"cwd": str(work), "mcpServers": []})["result"]["sessionId"]
    ask(3, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "go"}]})
    time.sleep(4)
    ask(4, "session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": "go"}]})
    began = time.time()
    for pipe in (child.stdin, child.stdout, child.stderr):
        pipe.close()
    try:
        code = child.wait(timeout=60)
    except subprocess.TimeoutExpired:
        child.kill()
        code = "killed"
    took = round(time.time() - began, 2)
    out.write(f"# every pipe closed: exit {code} after {took}s\n")
    out.close()
    info(f"{name}: exit {code} {took}s after every pipe closed")
    check(f"{name}: exits 0", code == 0, f"exit {code}")
    saved, file = saved_session(work)
    names = revived(name, work, saved, file) if file else []
    check(f"{name}: the last cell's variable is saved", "b" in names, f"revived names {names}")


def stop_during_cell(name, how):
    """A stop while a cell runs: bounded, and the earlier variable kept."""
    work = workdir(name)
    agent = Agent(name, work, [cell("a = 1"), "ok", cell("import time\nb = 2\ntime.sleep(300)"), "ok"])
    session = agent.open(work)["result"]["sessionId"]
    agent.prompt(session)
    agent.listen(4)
    agent.prompt(session, wait=False)
    agent.listen(3)
    took, exit_code = agent.stop(how)
    saved, file = saved_session(work)
    label = "input closed" if how == "eof" else signal.Signals(how).name
    info(f"{name}: exit {exit_code} {took}s after {label}, during a 300 s cell")
    check(f"{name}: exits within 30 s", exit_code == 0 and took < 30, f"exit {exit_code} after {took}s")
    names = revived(name, work, saved, file) if file else []
    check(f"{name}: the earlier variable is saved", "a" in names, f"revived names {names}")


def stop_big(name, count):
    """How long the final save of a big namespace takes: `count` variables
    of about 10 MB each (the caps are 16 MB per variable, 256 MB in all)."""
    work = workdir(name)
    code = (f"for k in range({count}):\n"
            "    globals()[f'v{k}'] = [{'i': i, 's': str(i) * 4} for i in range(300_000)]")
    agent = Agent(name, work, [cell("a = 1"), "ok", cell(code), "ok"])
    session = agent.open(work)["result"]["sessionId"]
    agent.prompt(session)
    agent.listen(4)
    agent.prompt(session)
    took, exit_code = agent.stop("eof")
    saved, _ = saved_session(work)
    path = snapshot(work, saved)
    size = path.stat().st_size if path.exists() else 0
    info(f"{name}: {count} variables, snapshot {size // 1_000_000} MB, exit {exit_code} {took}s after input closed")


def identity():
    """A new conversation's answer names its saved file, before the file
    exists: the row `session/list` shows once it has a prompt."""
    work = workdir("identity")
    agent = Agent("identity", work, [cell("a = 1"), "ok"])
    answer = agent.open(work)
    session = answer["result"]["sessionId"]
    saved = answer["result"].get("_meta", {}).get("unharnessed", {}).get("saved", {})
    info(f"identity: session/new {session}, saved {saved}")
    check("identity: session/new names the saved id and file",
          bool(saved.get("sessionId")) and bool(saved.get("path")), f"{answer['result']}")
    check("identity: the file is not written before the first prompt",
          not Path(saved.get("path", "/")).is_file() or not saved.get("path"), saved.get("path"))
    agent.prompt(session)
    rows = agent.wait(agent.send("session/list", {"cwd": str(work)}))["result"]["sessions"]
    row = [{"sessionId": r["sessionId"], "path": r["_meta"]["unharnessed"]["path"]} for r in rows]
    check("identity: session/list shows that id and file", row == [saved], f"rows {row}")
    agent.stop("eof")
    # The resumed process names the same conversation.
    resumed = Agent("identity-resumed", work, [], resume=saved["path"])
    loaded = resumed.load(work, saved["sessionId"])
    again = loaded.get("result", {}).get("_meta", {}).get("unharnessed", {}).get("saved", {})
    check("identity: session/load names the same", again == saved, f"{loaded}")
    resumed.stop("eof")


def model_values(options):
    """The model picker's values in a configOptions list."""
    for option in options or []:
        if option.get("id") == "model":
            return [value.get("value") for value in option.get("options", [])]
    return None


def picker_updates(agent):
    return [frame["params"]["update"] for frame in agent.frames if frame.get("method") == "session/update"
            and frame["params"]["update"].get("sessionUpdate") == "config_option_update"]


def reload():
    """A key saved in one agent reaches the other agent's model picker when
    it is told to read the credentials again."""
    work = workdir("reload")
    a = Agent("reload-a", work, [])
    b = Agent("reload-b", work, [])
    session_a = a.open(work)["result"]["sessionId"]
    opened = b.open(work)["result"]
    info(f"reload: B's models at start: {model_values(opened.get('configOptions'))}")
    # B's startup refresh of the models settles first.
    b.listen(8)
    settled = len(picker_updates(b))
    answer = a.wait(a.send("_unharnessed/providers/set_api_key", {"providerId": "openai", "apiKey": "sk-e2e"}))
    check("reload: A saves the key", answer.get("result", {}).get("outcome") == "done", f"{answer}")
    b.listen(5)
    check("reload: B's picker has not moved on its own", len(picker_updates(b)) == settled,
          f"{picker_updates(b)[settled:]}")
    answer = b.wait(b.send("_unharnessed/providers/reload", {}))
    rows = {row["id"]: row for row in answer.get("result", {}).get("providers", [])}
    check("reload: B lists openai connected", rows.get("openai", {}).get("connected") is True, f"{rows.get('openai')}")
    b.listen(15, until=lambda: len(picker_updates(b)) > settled)
    after = [model_values(update.get("configOptions")) for update in picker_updates(b)[settled:]]
    providers = sorted({json.loads(value)[0] for values in after for value in values or []})
    info(f"reload: B's picker providers after reload: {providers}")
    check("reload: B's picker now has openai models", "openai" in providers, f"{providers}")
    a.stop("eof")
    b.stop("eof")


def venv_lock():
    """A venv whose setup lock cannot be taken is not built: another agent
    may be building it."""
    work = workdir("venv-lock")
    readonly = OUT / "venv-lock" / "readonly"
    readonly.mkdir(exist_ok=True)
    readonly.chmod(0o555)
    try:
        agent = Agent("venv-lock", work, [cell("print(1)"), "ok"],
                      env={"UNHARNESSED_KERNEL_VENV": str(readonly / "kernel-venv")})
        session = agent.open(work)["result"]["sessionId"]
        agent.prompt(session)
        results = [json.dumps(frame["params"]["update"]) for frame in agent.frames
                   if frame.get("method") == "session/update"
                   and frame["params"]["update"].get("sessionUpdate") == "tool_call_update"]
        agent.stop("eof")
    finally:
        readonly.chmod(0o755)
    said = next((text for text in results if "kernel" in text.lower()), results[-1] if results else "")
    info(f"venv-lock: the cell's result: {said[:400]}")
    check("venv-lock: the cell fails on the lock", "locking" in said, said[:300])
    check("venv-lock: nothing was built", list(readonly.iterdir()) == [], f"{list(readonly.iterdir())}")


if "venv-lock" in CASES:
    venv_lock()

if "reload" in CASES:
    reload()

if "identity" in CASES:
    identity()

if "stop" in CASES:
    # The first agent provisions the kernel venv.
    stop_after_cell("stop-eof", "eof")
    stop_after_cell("stop-sigterm", signal.SIGTERM)
    # About 10 MB: the kernel's own last-chance save, when its host dies,
    # cannot finish this one.
    stop_after_cell("stop-sigterm-10mb", signal.SIGTERM,
                    "b = [{'i': i, 's': str(i) * 4} for i in range(300_000)]")
    stop_during_cell("stop-eof-busy", "eof")
    stop_during_cell("stop-sigterm-busy", signal.SIGTERM)
    host_gone("stop-host-gone")

if "stop-big" in CASES:
    for count in (1, 5, 20):
        stop_big(f"stop-big-{count}", count)

print(f"traces: {OUT}/*.out")
sys.exit(1 if FAILED else 0)
