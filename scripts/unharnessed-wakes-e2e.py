#!/usr/bin/env python3
"""End-to-end checks of the provider retry and the idle wakes on the real
binary, with the faux provider (no network) and a real kernel
(UNHARNESSED_PLAN.md Q6 and Q7).

usage: scripts/unharnessed-wakes-e2e.py <out-dir>

Each scenario writes <out-dir>/<name>.out: the command run, then every line
the binary printed (ACP frames with the seconds since start). The kernel
venv lives in <out-dir> (UNHARNESSED_BIN picks the binary, default
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
WORK = OUT / "work"
WORK.mkdir(parents=True, exist_ok=True)
# A throwaway HOME and agent dir: no credentials, so the faux provider is
# the only model the session can reach. The uv cache stays the real one.
HOME = OUT / "home"
(HOME / "agent").mkdir(parents=True, exist_ok=True)
ENV = dict(os.environ, HOME=str(HOME), UNHARNESSED_CODING_AGENT_DIR=str(HOME / "agent"),
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
           UNHARNESSED_PACKAGE_DIR=os.environ.get("UNHARNESSED_PACKAGE_DIR", str(ROOT)),
           UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), DO_NOT_TRACK="1")
FAILED = []


def call(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def run_print(name, responses, args):
    """Run `-p` on a faux script; return (exit code, seconds, json events)."""
    script = json.dumps({"responses": responses})
    with open(OUT / f"{name}.out", "w") as out:
        out.write(f"$ UNHARNESSED_FAUX_SCRIPT='{script}' unharnessed {' '.join(args)}\n")
        out.flush()
        start = time.time()
        code = subprocess.run([BIN, *args], cwd=WORK, stdout=out, stderr=subprocess.DEVNULL,
                              env=dict(ENV, UNHARNESSED_FAUX_SCRIPT=script), timeout=300).returncode
    seconds = time.time() - start
    lines = (OUT / f"{name}.out").read_text().splitlines()[1:]
    return code, seconds, [json.loads(line) for line in lines if line.startswith("{")]


def messages(events, role=None):
    found = [event["message"] for event in events if event.get("type") == "message_end"]
    return [message for message in found if role is None or message.get("role") == role]


def text_of(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    return "".join(block.get("text", "") for block in content or [])


def run_acp(name, responses, steps):
    """Drive one ACP session; return the (seconds, frame) list."""
    script = json.dumps({"responses": responses})
    child = subprocess.Popen([BIN, "--mode", "acp", "--session-dir", str(WORK / "sessions")],
                             cwd=WORK, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                             stderr=subprocess.DEVNULL, text=True,
                             env=dict(ENV, UNHARNESSED_FAUX_SCRIPT=script))
    start = time.time()
    frames, inbox, answers = [], queue.Queue(), {}
    out = open(OUT / f"{name}.out", "w")
    out.write(f"$ UNHARNESSED_FAUX_SCRIPT='{script}' unharnessed --mode acp  <<< {json.dumps(steps)}\n")

    def read():
        for line in child.stdout:
            if line.strip():
                frame = json.loads(line)
                stamp = round(time.time() - start, 2)
                out.write(json.dumps({"t": stamp, "frame": frame}) + "\n")
                frames.append((stamp, frame))
                inbox.put(frame)

    threading.Thread(target=read, daemon=True).start()
    ids = iter(range(1, 1000))

    def send(method, params):
        request_id = next(ids)
        child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n")
        child.stdin.flush()
        return request_id

    def pump(timeout):
        frame = inbox.get(timeout=timeout)
        if "id" in frame and "method" not in frame:
            answers[frame["id"]] = frame

    def wait(request_id, timeout=120):
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

    wait(send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
    session = wait(send("session/new", {"cwd": str(WORK), "mcpServers": []}))["result"]["sessionId"]
    for step in steps:
        if "prompt" in step:
            wait(send("session/prompt", {"sessionId": session, "prompt": [{"type": "text", "text": step["prompt"]}]}))
        elif "listen" in step:
            listen(step["listen"])
        elif "kill" in step:
            # The panel's stop: the first command row matching the text.
            def row():
                for _, frame in reversed(frames):
                    if frame.get("method") == "_unharnessed/kernel/changed" and "commands" in frame["params"]:
                        return next((c for c in frame["params"]["commands"] if step["kill"] in c["command"]), None)
            end = time.time() + 10
            while row() is None and time.time() < end:
                listen(0.2)
            wait(send("_unharnessed/kernel/kill", {"sessionId": session, "id": (row() or {}).get("id")}))
    wait(send("session/close", {"sessionId": session}))
    child.stdin.close()
    child.wait(timeout=60)
    out.close()
    return frames


def acp_texts(frames):
    """(seconds of the first chunk, prompt turn, text) of each turn's agent
    message, its streamed chunks joined."""
    turns = {}
    for stamp, frame in frames:
        update = (frame.get("params") or {}).get("update") or {}
        if update.get("sessionUpdate") == "agent_message_chunk":
            turn = update["_meta"]["unharnessed"]["promptTurnId"]
            first, text = turns.get(turn, (stamp, ""))
            turns[turn] = (first, text + update["content"]["text"])
    return [(first, turn, text) for turn, (first, text) in sorted(turns.items())]


def acp_answer(frames, request_id):
    return next(((stamp, frame) for stamp, frame in frames if frame.get("id") == request_id
                 and "method" not in frame), (None, {}))


# 1. -p: a 503 is retried and the turn recovers.
code, _, events = run_print("print_retry", [
    {"stopReason": "error", "errorMessage": "503 Service Unavailable"}, "recovered"],
    ["--mode", "json", "--no-session", "hello"])
retry = [event for event in events if event.get("type", "").startswith("auto_retry")]
check("-p retries a 503", code == 0 and [event["type"] for event in retry] == ["auto_retry_start", "auto_retry_end"]
      and retry[-1].get("success") is True and text_of(messages(events, "assistant")[-1]) == "recovered",
      f"exit {code}, retry events {retry}")

# 2. -p: a background bash() that outlives its cell wakes the model.
code, _, events = run_print("print_bash_wake", [
    call('h = bash("sleep 2; echo bg-done")'), "started; waiting", "notified: bg-done"],
    ["--mode", "json", "--no-session", "start a background command"])
done = [message for message in messages(events) if message.get("customType") == "async_bash_completion"]
check("-p wakes on a background bash() completion",
      code == 0 and done and "[bash-done pid:" in text_of(done[0])
      and text_of(messages(events, "assistant")[-1]) == "notified: bg-done",
      f"exit {code}, completion rows {len(done)}")

# 3. -p: the wait stops at --background-wait-ms and the command dies with the kernel.
marker = f"wake-limit-{os.getpid()}"
code, seconds, _ = run_print("print_wait_limit", [
    call(f'h = bash("sleep 60; echo {marker}")'), "started a long command"],
    ["--mode", "text", "--no-session", "--background-wait-ms", "1500", "start a long command"])
time.sleep(1)
alive = subprocess.run(["pgrep", "-f", marker], capture_output=True, text=True).stdout.split()
check("-p stops waiting at the limit and kills the command",
      code == 0 and seconds < 20 and not alive, f"exit {code}, {seconds:.1f}s, alive pids {alive}")
for pid in alive:
    subprocess.run(["kill", pid])

# 4. -p: an empty goal turn backs off 10 s, then the wake resumes the goal.
# The empty answer follows a kernel call: the goal only judges turns newer
# than its creation, and a faux answer can land in the same millisecond.
code, seconds, events = run_print("print_goal_wake", [
    call("print('working')"), "", call("print(await goal.complete())"), "goal done"],
    ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
marker_turns = [message for message in messages(events, "user") if "<goal_backoff_wake>" in text_of(message)]
check("-p resumes a goal after its backoff",
      code == 0 and seconds >= 9 and marker_turns and text_of(messages(events, "assistant")[-1]) == "goal done",
      f"exit {code}, {seconds:.1f}s, marker turns {len(marker_turns)}")

# 5. ACP: a 503 is retried and the prompt ends normally.
frames = run_acp("acp_retry", [
    {"stopReason": "error", "errorMessage": "503 Service Unavailable"}, "recovered"], [{"prompt": "hello"}])
_, answer = acp_answer(frames, 3)
check("ACP retries a 503", answer.get("result", {}).get("stopReason") == "end_turn"
      and [text for _, _, text in acp_texts(frames)] == ["recovered"], f"answer {answer}")

# 6. ACP: the background completion starts a turn on its own.
frames = run_acp("acp_bash_wake", [
    call('h = bash("sleep 2; echo bg-done")'), "started; waiting", "notified: bg-done"],
    [{"prompt": "start a background command"}, {"listen": 6}])
stamp, answer = acp_answer(frames, 3)
woken = [(at, turn) for at, turn, text in acp_texts(frames) if text == "notified: bg-done"]
check("ACP starts a wake turn on a background bash() completion",
      answer.get("result", {}).get("stopReason") == "end_turn" and woken
      and woken[0][0] > stamp and woken[0][1] == 2, f"answer at {stamp}, woken {woken}")

# 7. ACP: a prompt sent during a wake turn waits for it, then runs.
frames = run_acp("acp_prompt_during_wake", [
    call('h = bash("sleep 1; echo bg-done")'), "started",
    {"text": "notified slowly", "delayMs": 3000}, "second prompt answer"],
    [{"prompt": "start"}, {"listen": 1.6}, {"prompt": "second"}])
_, answer = acp_answer(frames, 4)
texts = [text for _, _, text in acp_texts(frames)]
check("ACP holds a client prompt until the wake turn settles",
      answer.get("result", {}).get("stopReason") == "end_turn"
      and texts[-2:] == ["notified slowly", "second prompt answer"], f"answer {answer}, texts {texts}")

# 8. ACP: the goal backoff wakes the idle session.
frames = run_acp("acp_goal_wake", [
    call("print('working')"), "", call("print(await goal.complete())"), "goal done"],
    [{"prompt": "/goal finish the task"}, {"listen": 14}])
woken = [(at, turn) for at, turn, text in acp_texts(frames) if text == "goal done"]
check("ACP resumes a goal after its backoff", woken and woken[0][0] >= 9 and woken[0][1] == 2,
      f"goal done chunks {woken}")

# 9. -p: a background bash() its own cell kills does not wake the model.
code, seconds, events = run_print("print_cell_kill", [
    call('h = bash("sleep 30; echo never")\nh.kill()'), "stopped it", "woken"],
    ["--mode", "json", "--no-session", "start then stop a command"])
done = [message for message in messages(events) if message.get("customType") == "async_bash_completion"]
check("-p does not wake on a command its own cell killed",
      code == 0 and not done and text_of(messages(events, "assistant")[-1]) == "stopped it",
      f"exit {code}, completion rows {len(done)}")

# 10. -p: nor when a later cell kills it.
code, seconds, events = run_print("print_later_kill", [
    call('h = bash("sleep 30; echo never")'), call("h.kill()"), "stopped it later", "woken"],
    ["--mode", "json", "--no-session", "start a command, stop it later"])
done = [message for message in messages(events) if message.get("customType") == "async_bash_completion"]
check("-p does not wake on a command a later cell killed",
      code == 0 and not done and text_of(messages(events, "assistant")[-1]) == "stopped it later",
      f"exit {code}, completion rows {len(done)}")

# 11. ACP: a stop from the panel still wakes the model, which did not ask for it.
frames = run_acp("acp_panel_kill_wake", [
    call('h = bash("sleep 30; echo never")'), "started", "notified of the stop"],
    [{"prompt": "start a background command"}, {"kill": "sleep 30"}, {"listen": 6}])
texts = [text for _, _, text in acp_texts(frames)]
check("ACP wakes on a command the panel stopped", texts == ["started", "notified of the stop"],
      f"texts {texts}")

# 12. ACP: a background command that ends during the turn wakes a turn of
# its own, once the prompt has answered: the panel shows the two answers apart.
frames = run_acp("acp_bash_wake_during_turn", [
    call('h = bash("sleep 1; echo bg-done")'), call("import asyncio\nawait asyncio.sleep(3)"),
    "first answer", "notified: bg-done"],
    [{"prompt": "start a background command, then work"}, {"listen": 6}])
stamp, answer = acp_answer(frames, 3)
turns = [(turn, text) for _, turn, text in acp_texts(frames)]
woken = [at for at, _, text in acp_texts(frames) if text == "notified: bg-done"]
check("ACP wakes after the turn that a background command ended during",
      answer.get("result", {}).get("stopReason") == "end_turn"
      and turns == [(1, "first answer"), (2, "notified: bg-done")] and woken[0] > stamp,
      f"answer at {stamp}, turns {turns}, woken {woken}")

sys.exit(1 if FAILED else 0)
