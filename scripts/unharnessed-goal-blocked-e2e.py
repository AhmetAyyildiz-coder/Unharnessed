#!/usr/bin/env python3
"""End-to-end checks of the goal's `blocked` status and of the end of its
relaunch loop (GOAL_BLOCKED_SPEC.md), on the real binary with the faux
provider (no network) and a real kernel.

usage: scripts/unharnessed-goal-blocked-e2e.py <out-dir> [--quick]

Ways it can fail, and the scenario that checks each:
- the loop is still there: `loop`, `loop_acp`, `loop_text`;
- work is cut too early: `work`, `worked_beside_a_server`, `user_resets`;
- a wake marker passes for the user: `marker_counts`;
- a wait is taken for a blocker: `wait`, `wait_acp`, `wait_settled`,
  `wait_prompt`;
- a model checking on a long command relaunches itself without pause:
  `polled_beside_a_server`;
- a wait outlives what it waits on: `wait_pause`, `kernel_dies`;
- a goal frozen by a server: `server`, `server_acp` (each waits the real
  10 minutes; --quick leaves them out);
- the model's exit is broken: `block`, `block_running`;
- something runs behind a blocked goal: `block_running_acp`, `autonomous_blocked`;
- a blocked goal exits non-zero when it meets an autonomous limit:
  `autonomous_turn_limit_blocked`;
- an empty answer hides the tool call before it: `tool_then_empty`;
- a batch of commands wakes the model once per command: `two_commands`,
  `notice_beside_server` (waits the real 2-minute window; --quick leaves it out);
- a completed goal's leftover commands wake it: `complete_stops_commands`,
  `complete_stops_commands_acp`;
- the resume is broken: `resume`;
- the state is lost: `reload`;
- a regression: `complete`, `budget`, `provider_error`, `empty_turns`;
- compaction: `compaction`, `compaction_acp`.

Each scenario writes <out-dir>/<name>.out: the command run, then every line
the binary printed with the seconds since start. The kernel venv and a
throwaway HOME live in <out-dir> (UNHARNESSED_BIN picks the binary, default
target/release/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import queue
import signal
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = str(Path(os.environ.get("UNHARNESSED_BIN", ROOT / "target/release/unharnessed")).resolve())
OUT = Path(sys.argv[1]).resolve()
QUICK = "--quick" in sys.argv[2:]
WORK = OUT / "work"
WORK.mkdir(parents=True, exist_ok=True)
# A throwaway HOME and agent dir: no credentials, so the faux provider is the
# only model a session can reach. The uv cache stays the real one.
HOME = OUT / "home"
(HOME / "agent").mkdir(parents=True, exist_ok=True)
BASE_ENV = {k: v for k, v in os.environ.items() if not k.startswith(("UNHARNESSED_", "UH_", "XDG_"))}
ENV = dict(BASE_ENV, HOME=str(HOME), UNHARNESSED_CODING_AGENT_DIR=str(HOME / "agent"),
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")),
           UNHARNESSED_PACKAGE_DIR=str(ROOT), UNHARNESSED_OFFLINE="1",
           UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), DO_NOT_TRACK="1")
FAILED = []
PRINT_LOCK = threading.Lock()
# The window of a goal waiting on a command that never ends.
RELAUNCH_SECONDS = 600
# How long a finished command's notice waits for the commands still running.
NOTICE_BATCH_SECONDS = 120
def call(code):
    return {"content": [{"type": "toolCall", "name": "ipython", "arguments": {"code": code}}]}


def paced(text):
    """An answer a few ms later: the goal judges only turns newer than the
    last one it saw, and a faux answer can land in the same millisecond."""
    return {"text": text, "delayMs": 20}


# A 20k window and 30k-char answers: the second and third answers cross the
# compaction threshold, without ever overflowing the window. The summarizer
# takes its answers from the same queue: two per compaction (the history and
# the split turn, racing for them), so both are short either way.
COMPACTING_SCRIPT = {"contextWindow": 20000, "maxTokens": 2000}
COMPACTING_ANSWER = "word " * 6000
COMPACTING_RESPONSES = [
    paced(COMPACTING_ANSWER), paced(COMPACTING_ANSWER), "summary", "summary",
    paced(COMPACTING_ANSWER), "summary", "summary", "must not run",
]
COMPACTING_SETTINGS = {"compaction": {"enabled": True, "reserveTokens": 1, "keepRecentTokens": 10}}


def endless(step, count=12):
    """A model that never stops answering: more than any scenario may use."""
    return [step] * count


def check(name, ok, detail=""):
    with PRINT_LOCK:
        print(("PASS  " if ok else "FAIL  ") + name + ("" if ok else f" ({detail})"), flush=True)
        if not ok:
            FAILED.append(name)


def scenario_dir(name):
    path = WORK / name
    path.mkdir(parents=True, exist_ok=True)
    return path


def agent_dir_with(name, settings):
    path = HOME / f"agent-{name}"
    path.mkdir(parents=True, exist_ok=True)
    (path / "settings.json").write_text(json.dumps(settings))
    return path


def run_print(name, responses, args, script=None, agent_dir=None, stop=None, timeout=300, cwd=None):
    """Run the binary on a faux script. `stop(events, now)` returns a signal to
    send it. Returns (exit code, seconds, [(seconds, event)])."""
    body = json.dumps({"responses": responses, **(script or {})})
    env = dict(ENV, UNHARNESSED_FAUX_SCRIPT=body)
    if agent_dir:
        env["UNHARNESSED_CODING_AGENT_DIR"] = str(agent_dir)
    trace = open(OUT / f"{name}.out", "w")
    trace.write(f"$ UNHARNESSED_FAUX_SCRIPT='{body}' unharnessed {' '.join(args)}\n")
    trace.flush()
    start = time.time()
    child = subprocess.Popen([BIN, *args], cwd=cwd or scenario_dir(name), stdin=subprocess.DEVNULL,
                             stdout=subprocess.PIPE, stderr=subprocess.DEVNULL, text=True, env=env)
    events = []

    def read():
        for line in child.stdout:
            stamp = round(time.time() - start, 2)
            trace.write(f"{stamp} {line}")
            trace.flush()
            if line.startswith("{"):
                events.append((stamp, json.loads(line)))
            else:
                events.append((stamp, {"type": "text", "text": line}))

    reader = threading.Thread(target=read, daemon=True)
    reader.start()
    while child.poll() is None:
        now = time.time() - start
        sent = stop(events, now) if stop else None
        if sent is None and now > timeout:
            sent = signal.SIGKILL
            trace.write(f"{round(now, 2)} killed: past the {timeout} s limit\n")
        if sent is not None:
            child.send_signal(sent)
            child.wait()
            break
        time.sleep(0.1)
    reader.join(timeout=5)
    seconds = time.time() - start
    trace.write(f"{round(seconds, 2)} exit {child.returncode}\n")
    trace.close()
    return child.returncode, seconds, events


def rows(events, kind=None):
    """(seconds, message) of each settled row, of one role or custom type."""
    found = [(at, event["message"]) for at, event in events if event.get("type") == "message_end"]
    return [(at, row) for at, row in found if kind is None or kind in (row.get("role"), row.get("customType"))]


def text_of(message):
    content = message.get("content")
    if isinstance(content, str):
        return content
    return "".join(block.get("text", "") for block in content or [])


def answers(events):
    """(seconds, text) of each assistant message that says something."""
    return [(at, text_of(row)) for at, row in rows(events, "assistant") if text_of(row)]


def goals(events):
    return [event["goal"] for _, event in events if event.get("type") == "goal_update"]


def last_goal(events):
    updates = goals(events)
    return updates[-1] if updates else {}


def relaunches(events):
    """Seconds of each goal continuation the run sent (the `--goal` seed rides
    the first turn: a `continuationsUsed` of 0 is not a relaunch)."""
    return [at for at, row in rows(events, "goal_context")
            if row.get("details", {}).get("kind") == "continuation"
            and row.get("details", {}).get("continuationsUsed", 0) > 0]


class Acp:
    """One ACP session on a faux script, its frames kept with their seconds."""

    def __init__(self, name, responses, script=None, agent_dir=None):
        body = json.dumps({"responses": responses, **(script or {})})
        env = dict(ENV, UNHARNESSED_FAUX_SCRIPT=body)
        if agent_dir:
            env["UNHARNESSED_CODING_AGENT_DIR"] = str(agent_dir)
        self.cwd = scenario_dir(name)
        self.child = subprocess.Popen([BIN, "--mode", "acp", "--session-dir", str(self.cwd / "sessions")],
                                      cwd=self.cwd, stdin=subprocess.PIPE, stdout=subprocess.PIPE,
                                      stderr=subprocess.DEVNULL, text=True, env=env)
        self.start = time.time()
        self.frames, self.inbox, self.answers = [], queue.Queue(), {}
        self.trace = open(OUT / f"{name}.out", "w")
        self.trace.write(f"$ UNHARNESSED_FAUX_SCRIPT='{body}' unharnessed --mode acp\n")
        self.ids = iter(range(1, 1000))
        threading.Thread(target=self._read, daemon=True).start()
        self.wait(self.send("initialize", {"protocolVersion": 1, "clientCapabilities": {}}))
        self.session = self.wait(self.send("session/new", {"cwd": str(self.cwd), "mcpServers": []}))["result"]["sessionId"]

    def _read(self):
        for line in self.child.stdout:
            if line.strip():
                frame = json.loads(line)
                stamp = round(time.time() - self.start, 2)
                self.trace.write(json.dumps({"t": stamp, "frame": frame}) + "\n")
                self.trace.flush()
                self.frames.append((stamp, frame))
                self.inbox.put(frame)

    def send(self, method, params):
        request_id = next(self.ids)
        self.trace.write(json.dumps({"send": method, "params": params}) + "\n")
        self.child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params}) + "\n")
        self.child.stdin.flush()
        return request_id

    def _pump(self, timeout):
        frame = self.inbox.get(timeout=timeout)
        if "id" in frame and "method" not in frame:
            self.answers[frame["id"]] = frame

    def wait(self, request_id, timeout=120):
        deadline = time.time() + timeout
        while request_id not in self.answers:
            self._pump(max(0.05, deadline - time.time()))
        return self.answers[request_id]

    def prompt(self, text):
        return self.wait(self.send("session/prompt", {"sessionId": self.session, "prompt": [{"type": "text", "text": text}]}))

    def listen(self, seconds):
        end = time.time() + seconds
        while time.time() < end:
            try:
                self._pump(max(0.05, end - time.time()))
            except queue.Empty:
                pass

    def close(self):
        self.wait(self.send("session/close", {"sessionId": self.session}))
        self.child.stdin.close()
        self.child.wait(timeout=60)
        self.trace.close()

    def metas(self, key):
        """(seconds, value) of one `_meta.unharnessed` key across the frames."""
        found = []
        for stamp, frame in self.frames:
            meta = ((frame.get("params") or {}).get("update") or {}).get("_meta", {}).get("unharnessed", {})
            if meta.get(key) is not None:
                found.append((stamp, meta[key]))
        return found

    def texts(self):
        """(seconds of the first chunk, prompt turn, text) of each turn's answers."""
        turns = {}
        for stamp, frame in self.frames:
            update = (frame.get("params") or {}).get("update") or {}
            if update.get("sessionUpdate") == "agent_message_chunk":
                turn = update["_meta"]["unharnessed"]["promptTurnId"]
                first, text = turns.get(turn, (stamp, ""))
                turns[turn] = (first, text + update["content"]["text"])
        return [(first, turn, text) for turn, (first, text) in sorted(turns.items())]


# The loop: a model that keeps writing that it is blocked.
BLOCKED_ANSWER = ("My earlier crash irreversibly violated the requirement. Operator action is required; "
                  "I will not falsely mark the goal complete. " + "x" * 600)


def scenario_loop():
    code, _, events = run_print("loop", endless(paced(BLOCKED_ANSWER)),
                                ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    said = [text for _, text in answers(events)]
    final = last_goal(events)
    check("loop: 3 requests, then blocked, exit 0",
          code == 0 and len(said) == 3 and final.get("status") == "blocked" and final.get("active") is False,
          f"exit {code}, {len(said)} answers, goal {final}")
    check("loop: the reason is the start of the last answer, 500 characters",
          final.get("lastReason") == BLOCKED_ANSWER[:500], f"reason {final.get('lastReason')!r}")
    check("loop: the last goal_update says blocked, no continuation after it",
          events and len(relaunches(events)) == 2, f"relaunches {relaunches(events)}")


def scenario_loop_text():
    code, _, events = run_print("loop_text", endless(paced("still stuck")),
                                ["--mode", "text", "--no-session", "--goal", "finish the task", "start"])
    printed = "".join(event.get("text", "") for _, event in events).strip()
    check("loop, text mode: the last answer on stdout, exit 0", code == 0 and printed == "still stuck",
          f"exit {code}, stdout {printed!r}")


def scenario_loop_acp():
    agent = Acp("loop_acp", endless(paced("still stuck")))
    answer = agent.prompt("/goal finish the task")
    final = [goal for _, goal in agent.metas("goal")][-1]
    said = [text for _, _, text in agent.texts()]
    check("ACP loop: the prompt ends, the hand comes back",
          answer.get("result", {}).get("stopReason") == "end_turn", f"answer {answer}")
    check("ACP loop: 3 requests, then blocked with its reason",
          said == ["still stuck" * 3] and final["status"] == "blocked" and final.get("lastReason") == "still stuck",
          f"said {said}, goal {final}")
    agent.listen(3)
    check("ACP loop: nothing runs after it", [text for _, _, text in agent.texts()] == said, agent.texts())
    agent.close()


def scenario_work():
    code, _, events = run_print("work", [
        paced("idle one"), paced("idle two"),
        call("print('work')"), paced("worked"),
        paced("idle three"), paced("idle four"),
        call("print('more work')"), paced("worked again"),
        call("print(await goal.complete())"), paced("finished")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    statuses = [goal["status"] for goal in goals(events)]
    streaks = [goal.get("idleStreak", 0) for goal in goals(events)]
    check("work: idle, idle, tool, idle, idle, tool never goes blocked",
          code == 0 and "blocked" not in statuses and last_goal(events).get("status") == "complete",
          f"exit {code}, statuses {statuses}")
    check("work: a tool call resets the counter", 2 in streaks and max(streaks) == 2 and streaks[-1] == 0,
          f"streaks {streaks}")


def scenario_worked_beside_a_server():
    agent = Acp("worked_beside_a_server", [
        call('server = bash("sleep 1000")'), paced("server started"),
        call("print('work')"), paced("worked"),
        paced("nothing to do while it runs"), paced("must not run")])
    agent.prompt("start the server")
    agent.prompt("/goal finish the task")
    agent.listen(4)
    said = [text for _, _, text in agent.texts()]
    check("a turn that worked beside an older command relaunches at once",
          said[-1] == "workednothing to do while it runs", f"said {said}")
    check("the turn without action behind it waits", "must not run" not in "".join(said), f"said {said}")
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("it stays active", goal["status"] == "active", goal)
    agent.close()


def scenario_polled_beside_a_server():
    agent = Acp("polled_beside_a_server", [
        call('server = bash("sleep 1000")'), paced("server started"),
        call("print(server.running)"), paced("checked"),
        call("print(server.running)"), paced("checked again"),
        paced("must not run")])
    agent.prompt("start the server")
    agent.prompt("/goal finish the task")
    agent.listen(8)
    said = "".join(text for _, _, text in agent.texts())
    check("a first check beside an older command relaunches at once", "checked again" in said, said)
    check("a second check in a row waits", "must not run" not in said, said)
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("the checks are not idle turns",
          goal["status"] == "active" and goal.get("idleStreak", 0) == 0, goal)
    agent.close()


def scenario_wait():
    code, _, events = run_print("wait", [
        call('h = bash("sleep 6; echo done")'), paced("waiting for it"),
        paced("noted"),
        call("print(await goal.complete())"), paced("finished")],
        # The goal's wait outlasts this limit: the command would die with it.
        ["--mode", "json", "--no-session", "--background-wait-ms", "1000", "--goal", "finish the task", "start"])
    said = dict((text, at) for at, text in answers(events))
    check("wait: the goal finishes", code == 0 and last_goal(events).get("status") == "complete",
          f"exit {code}, goal {last_goal(events)}")
    waited = said.get("noted", 0) - said.get("waiting for it", 0)
    check("wait: no request before the command ends", waited >= 4, f"answers {said}")
    sent = relaunches(events)
    check("wait: no relaunch before it ends, one after it",
          len(sent) == 1 and sent[0] >= said.get("waiting for it", 0) + 4, f"relaunches {sent}, answers {said}")
    check("wait: the turn without action does not count",
          all(goal.get("idleStreak", 0) <= 1 for goal in goals(events)), goals(events))


def scenario_wait_acp():
    agent = Acp("wait_acp", [
        call('h = bash("sleep 6; echo done")'), paced("waiting for it"),
        paced("noted"),
        call("print(await goal.complete())"), paced("finished")])
    answer = agent.prompt("/goal finish the task")
    answered_at = time.time() - agent.start
    check("ACP wait: the prompt ends while the command runs",
          answer.get("result", {}).get("stopReason") == "end_turn", answer)
    agent.listen(10)
    said = agent.texts()
    woken = [(at, turn) for at, turn, text in said if "noted" in text]
    check("ACP wait: a wake turn after the command, not before",
          woken and woken[0][0] >= answered_at + 3, f"answered at {answered_at}, said {said}")
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("ACP wait: the goal finishes", goal["status"] == "complete", goal)
    agent.close()


def scenario_wait_prompt():
    agent = Acp("wait_prompt", [
        call('import threading\nh = bash("sleep 1000")\nthreading.Timer(6, h.kill).start()'), paced("waiting for it"),
        paced("an answer to the user"),
        call("print(await goal.complete())"), paced("finished")])
    agent.prompt("/goal finish the task")
    agent.listen(1)
    answer = agent.prompt("how is it going?")
    answered_at = time.time() - agent.start
    check("a message during the wait is answered, the goal still waits",
          answer.get("result", {}).get("stopReason") == "end_turn"
          and [text for _, _, text in agent.texts()][-1] == "an answer to the user", agent.texts())
    agent.listen(8)
    finished = [at for at, _, text in agent.texts() if "finished" in text]
    check("the relaunch goes when the command ends, not behind the message",
          finished and finished[0] >= answered_at + 2, f"answered at {answered_at}, said {agent.texts()}")
    agent.close()


def scenario_wait_pause():
    agent = Acp("wait_pause", [
        call('import threading\nh = bash("sleep 1000")\nthreading.Timer(4, h.kill).start()'), paced("waiting for it"),
        paced("must not run")])
    agent.prompt("/goal finish the task")
    agent.prompt("/goal pause")
    agent.listen(7)
    said = [text for _, _, text in agent.texts()]
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("a pause drops the relaunch the wait owed",
          goal["status"] == "paused" and all("must not run" not in text for text in said), f"said {said}, goal {goal}")
    agent.close()


def scenario_kernel_dies():
    code, _, events = run_print("kernel_dies", [
        call('import os, threading\nh = bash("sleep 1000")\nthreading.Timer(4, lambda: os.kill(os.getpid(), 9)).start()'),
        paced("waiting for it"),
        call("print(await goal.complete())"), paced("finished")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"], timeout=120)
    said = dict((text, at) for at, text in answers(events))
    sent = relaunches(events)
    check("a dead kernel ends the wait: nothing runs any more",
          code == 0 and sent and sent[0] - said.get("waiting for it", 0) < 60
          and last_goal(events).get("status") == "complete",
          f"exit {code}, relaunches {sent}, answers {said}, goal {last_goal(events)}")


def scenario_wait_settled():
    # No completion notice: the command is killed by the agent, so only the
    # settlement of the kernel's last handle ends the wait.
    code, _, events = run_print("wait_settled", [
        call('import threading\nh = bash("sleep 1000")\nthreading.Timer(5, h.kill).start()'),
        paced("waiting for it"),
        call("print(await goal.complete())"), paced("finished")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    said = dict((text, at) for at, text in answers(events))
    sent = relaunches(events)
    completions = rows(events, "async_bash_completion")
    check("settled wait: no completion notice", not completions, completions)
    check("settled wait: the relaunch goes when the last command ends",
          code == 0 and len(sent) == 1 and sent[0] >= said.get("waiting for it", 0) + 3
          and last_goal(events).get("status") == "complete",
          f"exit {code}, relaunches {sent}, answers {said}, goal {last_goal(events)}")


def scenario_server():
    seen = {}

    def stop(events, now):
        said = dict((text, at) for at, text in answers(events))
        if "still up" in said and now > said["still up"] + 60:
            seen.update(said)
            return signal.SIGTERM
        return None

    code, _, events = run_print("server", [
        call('server = bash("sleep 100000")'), paced("server started"),
        paced("still up"), paced("must not run")],
        ["--mode", "json", "--no-session", "--background-wait-ms", "1000", "--goal", "finish the task", "start"],
        stop=stop, timeout=RELAUNCH_SECONDS + 180)
    said = dict((text, at) for at, text in answers(events))
    gap = said.get("still up", 0) - said.get("server started", 0)
    check("server: the relaunch comes after the 10-minute window",
          RELAUNCH_SECONDS - 5 <= gap <= RELAUNCH_SECONDS + 30, f"gap {gap}, answers {said}")
    check("server: the next one does not come sooner", "must not run" not in said, said)
    statuses = [goal["status"] for goal in goals(events)]
    check("server: not a blocker, not counted",
          "blocked" not in statuses and all(goal.get("idleStreak", 0) == 0 for goal in goals(events)), goals(events))


def scenario_server_acp():
    agent = Acp("server_acp", [
        call('server = bash("sleep 100000")'), paced("server started"),
        paced("still up"), paced("must not run")])
    agent.prompt("/goal finish the task")
    asked_at = time.time() - agent.start
    agent.listen(RELAUNCH_SECONDS + 40)
    said = agent.texts()
    woken = [at for at, _, text in said if text == "still up"]
    check("ACP server: a wake turn after the 10-minute window",
          woken and RELAUNCH_SECONDS - 5 <= woken[0] - asked_at <= RELAUNCH_SECONDS + 30,
          f"asked at {asked_at}, said {said}")
    check("ACP server: the next one does not come sooner", all("must not run" not in text for _, _, text in said), said)
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("ACP server: not a blocker", goal["status"] == "active", goal)
    agent.close()


def scenario_block():
    code, _, events = run_print("block", [
        call('print(await goal.block("x"))\n'
             'try:\n    await goal.create("another")\nexcept Exception as error:\n    print("create:", error)'),
        paced("blocked, asking the user"), paced("must not run")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    final = last_goal(events)
    check("goal.block: blocked, its reason, exit 0",
          code == 0 and final.get("status") == "blocked" and final.get("lastReason") == "x",
          f"exit {code}, goal {final}")
    said = [text for _, text in answers(events)]
    check("goal.block: no relaunch, nothing more runs",
          said == ["blocked, asking the user"] and not relaunches(events), f"said {said}, relaunches {relaunches(events)}")
    output = "".join(text_of(row) for _, row in rows(events, "toolResult"))
    check("goal.create treats a blocked goal as pending", "create: " in output and "a blocked goal exists" in output, output)


def scenario_block_running():
    code, seconds, events = run_print("block_running", [
        call('h = bash("sleep 1000")\nawait goal.block("x")'), paced("stopping"), paced("must not run")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"], timeout=120)
    check("goal.block with a command running: the run stops at once, exit 0",
          code == 0 and seconds < 60 and last_goal(events).get("status") == "blocked",
          f"exit {code}, {seconds:.1f} s, goal {last_goal(events)}")


def scenario_block_running_acp():
    agent = Acp("block_running_acp", [
        call('h = bash("sleep 3; echo result")\nawait goal.block("user action required")'),
        paced("blocked for the user"), paced("must not run")])
    agent.prompt("/goal finish the task")
    agent.listen(7)
    said = [text for _, _, text in agent.texts()]
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("ACP: a command that ends after goal.block wakes nothing",
          said == ["blocked for the user"] and goal["status"] == "blocked", f"said {said}, goal {goal}")
    agent.close()


def scenario_autonomous_blocked():
    code, _, events = run_print("autonomous_blocked", endless(paced("stuck")),
        ["--mode", "json", "--no-session", "--goal", "finish the task", "--autonomous",
         "--autonomous-max-continuations", "5", "start"])
    said = answers(events)
    check("--autonomous stops at the blocked goal, exit 0",
          code == 0 and len(said) == 3 and last_goal(events).get("status") == "blocked",
          f"exit {code}, {len(said)} answers, goal {last_goal(events)}")


def scenario_autonomous_turn_limit_blocked():
    # The goal blocks on the very turn that reaches the autonomous turn limit.
    code, _, events = run_print("autonomous_turn_limit_blocked", endless(paced("stuck")),
        ["--mode", "json", "--no-session", "--goal", "finish the task", "--autonomous",
         "--autonomous-max-turns", "3", "start"])
    check("a blocked goal on the autonomous limit's turn still exits 0",
          code == 0 and len(answers(events)) == 3 and last_goal(events).get("status") == "blocked",
          f"exit {code}, {len(answers(events))} answers, goal {last_goal(events)}")


def scenario_tool_then_empty():
    code, _, events = run_print("tool_then_empty", [
        paced("idle one"), paced("idle two"), call("print('real work')"), "",
        paced("idle after work"), call("print(await goal.complete())"), paced("finished")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    check("a tool call ends the idle series even when its turn ends on an empty answer",
          code == 0 and last_goal(events).get("status") == "complete",
          f"exit {code}, answers {answers(events)}, goal {last_goal(events)}")


def scenario_two_commands():
    code, _, events = run_print("two_commands", [
        call('first = bash("sleep 3; echo first")\nlast = bash("sleep 12; echo last")'),
        paced("waiting for both"), paced("both noted"),
        call("print(await goal.complete())"), paced("finished"), paced("must not run")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    said = answers(events)
    # The last command's end may bring the goal's relaunch a moment before
    # the notices: either way, nothing runs before it.
    woken = dict((text, at) for at, text in said).get("both noted", 0)
    early = [at for at, _ in rows(events, "async_bash_completion") if at < 11]
    check("two commands: one turn once the last one ends, none for the first",
          [text for _, text in said] == ["waiting for both", "both noted", "finished"] and woken >= 11 and not early,
          f"answers {said}, early notices {early}")
    check("two commands: no goal relaunch before the last one ends",
          code == 0 and all(at >= 11 for at in relaunches(events))
          and last_goal(events).get("status") == "complete",
          f"exit {code}, relaunches {relaunches(events)}, goal {last_goal(events)}")


def scenario_notice_beside_server():
    agent = Acp("notice_beside_server", [
        call('server = bash("sleep 100000")'), paced("server started"),
        call('job = bash("sleep 3; echo job")'), paced("job started"),
        paced("job noted"), paced("must not run")])
    agent.prompt("start the server")
    agent.prompt("start the job")
    asked_at = time.time() - agent.start
    agent.listen(NOTICE_BATCH_SECONDS + 40)
    said = agent.texts()
    noted = [at for at, _, text in said if text == "job noted"]
    check("a notice beside a server waits for the 2-minute window, then goes",
          noted and NOTICE_BATCH_SECONDS - 5 <= noted[0] - asked_at <= NOTICE_BATCH_SECONDS + 30,
          f"asked at {asked_at}, said {said}")
    check("one notice, one turn", all("must not run" not in text for _, _, text in said), said)
    agent.close()


def scenario_complete_stops_commands():
    code, seconds, events = run_print("complete_stops_commands", [
        call('jobs = [bash(f"sleep {i}; echo {i}") for i in range(2, 7)]\nkeep = bash("sleep 1000")\n'
             'print(await goal.complete())'),
        paced("finished"), paced("must not run")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"], timeout=120)
    said = [text for _, text in answers(events)]
    check("a completed goal stops its commands: the run ends, no notice wakes it",
          code == 0 and said == ["finished"] and not rows(events, "async_bash_completion") and seconds < 30,
          f"exit {code}, {seconds:.1f} s, said {said}, notices {rows(events, 'async_bash_completion')}")


def scenario_complete_stops_commands_acp():
    agent = Acp("complete_stops_commands_acp", [
        call('h = bash("sleep 3; echo late")\nprint(await goal.complete())'),
        paced("finished"), paced("must not run")])
    agent.prompt("/goal finish the task")
    agent.listen(7)
    said = [text for _, _, text in agent.texts()]
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("ACP: a command left by a completed goal wakes nothing",
          said == ["finished"] and goal["status"] == "complete", f"said {said}, goal {goal}")
    agent.close()


def scenario_resume():
    agent = Acp("resume",["a1", paced("a2"), paced("a3"), paced("b1"), paced("b2"), paced("b3"), paced("must not run")])
    agent.prompt("/goal finish the task")
    goal = [goal for _, goal in agent.metas("goal")][-1]
    check("resume: blocked first", goal["status"] == "blocked" and goal.get("lastReason") == "a3", goal)
    agent.prompt("/goal")
    said = [text for _, command in agent.metas("command") for text in [command["text"]]]
    check("/goal says why", said[-1] == "Goal blocked: finish the task\nBlocked by: a3", said)
    answer = agent.prompt("/goal resume")
    goals_after = [goal for _, goal in agent.metas("goal")]
    final = goals_after[-1]
    statuses = [goal["status"] for goal in goals_after]
    check("resume: active again", answer.get("result", {}).get("stopReason") == "end_turn"
          and "active" in statuses[statuses.index("blocked"):], statuses)
    texts = [text for _, _, text in agent.texts()]
    check("resume: the counter starts from zero, 3 more requests, blocked again",
          texts[-1] == "b1b2b3" and final["status"] == "blocked" and final.get("lastReason") == "b3",
          f"said {texts}, goal {final}")
    agent.close()


def session_goal_rows(sessions):
    """The goal rows of the only session file under `sessions`."""
    found = []
    for path in sessions.rglob("*.jsonl"):
        for line in path.read_text().splitlines():
            row = json.loads(line)
            if row.get("customType") == "thread_goal_state":
                found.append(row["data"])
    return found


def scenario_reload():
    cwd = scenario_dir("reload")
    sessions = cwd / "sessions"
    base = ["--mode", "json", "--session-dir", str(sessions)]

    def killed(events, now):
        return signal.SIGKILL if len(answers(events)) == 2 and now > answers(events)[-1][0] + 1 else None

    run_print("reload_active", ["a1", paced("a2"), {"text": "never", "delayMs": 120000}],
              [*base, "--goal", "finish the task", "start"], stop=killed, timeout=120, cwd=cwd)
    durable = session_goal_rows(sessions)
    check("reload: the counter is on disk", durable and durable[-1].get("idleStreak") == 2,
          durable[-1] if durable else "no row")

    def status(name):
        _, _, events = run_print(name, ["spare"], [*base, "-c", "/goal"], cwd=cwd)
        return last_goal(events)

    goal = status("reload_active_status")
    check("reload: active, counter kept", goal.get("status") == "active" and goal.get("idleStreak") == 2, goal)
    run_print("reload_pause", ["spare"], [*base, "-c", "/goal pause"], cwd=cwd)
    goal = status("reload_paused_status")
    check("reload: paused, counter kept", goal.get("status") == "paused" and goal.get("idleStreak") == 2, goal)
    code, _, events = run_print("reload_resume", [paced("b1"), paced("b2"), paced("b3"), paced("must not run")],
                                [*base, "-c", "/goal resume"], cwd=cwd)
    said = [text for _, text in answers(events)]
    check("reload: resume starts from zero, 3 requests, blocked, exit 0",
          code == 0 and said == ["b1", "b2", "b3"] and last_goal(events).get("status") == "blocked",
          f"exit {code}, said {said}, goal {last_goal(events)}")
    goal = status("reload_blocked_status")
    check("reload: blocked, reason and counter kept",
          goal.get("status") == "blocked" and goal.get("idleStreak") == 3 and goal.get("lastReason") == "b3", goal)
    code, _, events = run_print("reload_blocked_prompt", [paced("just an answer"), paced("must not run")],
                                [*base, "-c", "hello"], cwd=cwd)
    said = [text for _, text in answers(events)]
    check("reload: a blocked goal does not relaunch after a restart", code == 0 and said == ["just an answer"], said)


def scenario_user_resets():
    cwd = scenario_dir("user_resets")
    base = ["--mode", "json", "--session-dir", str(cwd / "sessions")]

    def killed(events, now):
        return signal.SIGKILL if len(answers(events)) == 2 and now > answers(events)[-1][0] + 1 else None

    run_print("user_resets_first", ["a1", paced("a2"), {"text": "never", "delayMs": 120000}],
              [*base, "--goal", "finish the task", "start"], stop=killed, timeout=120, cwd=cwd)
    code, _, events = run_print("user_resets", [paced("b1"), paced("b2"), paced("b3"), paced("must not run")],
                                [*base, "-c", "a message from the user"], cwd=cwd)
    said = [text for _, text in answers(events)]
    check("a user message resets the counter: 3 more requests, not 1",
          code == 0 and said == ["b1", "b2", "b3"] and last_goal(events).get("status") == "blocked",
          f"exit {code}, said {said}, goal {last_goal(events)}")


def scenario_marker_counts():
    code, _, events = run_print("marker_counts", [paced("a1"), paced("a2"), "", paced("a3"), paced("must not run")],
                                ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    said = [text for _, text in answers(events)]
    check("a wake marker is not a user message: the count goes on across it",
          code == 0 and said == ["a1", "a2", "a3"] and last_goal(events).get("status") == "blocked",
          f"exit {code}, said {said}, goal {last_goal(events)}")


def scenario_complete():
    code, _, events = run_print("complete", [
        call("print(await goal.complete())"), paced("done"), paced("must not run")],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    check("regression: goal.complete still ends the goal",
          code == 0 and last_goal(events).get("status") == "complete" and [text for _, text in answers(events)] == ["done"],
          f"exit {code}, goal {last_goal(events)}, said {answers(events)}")


def scenario_budget():
    code, _, events = run_print("budget", ["goal turn", "wrap-up", paced("must not run")],
                                ["--mode", "json", "--no-session", "--goal", "finish the task", "--goal-token-budget", "5", "start"])
    check("regression: the budget still limits the goal",
          code == 0 and last_goal(events).get("status") == "budget_limited"
          and [text for _, text in answers(events)] == ["goal turn", "wrap-up"],
          f"exit {code}, goal {last_goal(events)}, said {answers(events)}")


def scenario_provider_error():
    # Enough failures for the retries too: each one takes a scripted answer.
    code, _, events = run_print("provider_error", [
        call("print('work')"), *endless({"stopReason": "error", "errorMessage": "400 invalid request"})],
        ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    final = last_goal(events)
    check("regression: a provider error still fails the goal",
          final.get("status") == "error" and "400 invalid request" in final.get("lastError", ""),
          f"exit {code}, goal {final}")


def scenario_empty_turns():
    code, seconds, events = run_print("empty_turns", [call("print('working')"), "", "", "", "must not run"],
                                      ["--mode", "json", "--no-session", "--goal", "finish the task", "start"])
    final = last_goal(events)
    check("regression: empty answers still back off, then hit their cap",
          code == 0 and seconds >= 29 and final.get("status") == "error"
          and "cap reached" in final.get("lastError", "") and final.get("noProgressStreak") == 3,
          f"exit {code}, {seconds:.1f} s, goal {final}")


def scenario_compaction():
    code, _, events = run_print("compaction", COMPACTING_RESPONSES,
                                ["--mode", "json", "--no-session", "--goal", "finish the task", "start"],
                                script=COMPACTING_SCRIPT, agent_dir=agent_dir_with("compaction", COMPACTING_SETTINGS))
    said = answers(events)
    ran = [event for _, event in events if event.get("type") == "compaction_end" and event.get("result")]
    check("compaction: a threshold during the series adds no relaunch",
          code == 0 and len(said) == 3 and last_goal(events).get("status") == "blocked",
          f"exit {code}, {len(said)} answers, goal {last_goal(events)}")
    check("compaction: it did compact, the threshold of the last answer too", len(ran) == 2,
          [str(event)[:200] for _, event in events if event.get("type") == "compaction_end"])


def scenario_compaction_acp():
    agent = Acp("compaction_acp", COMPACTING_RESPONSES, script=COMPACTING_SCRIPT,
                agent_dir=agent_dir_with("compaction_acp", COMPACTING_SETTINGS))
    response = agent.prompt("/goal finish the task")
    goal = [goal for _, goal in agent.metas("goal")][-1]
    said = "".join(text for _, _, text in agent.texts())
    check("ACP compaction: a threshold during the series adds no relaunch",
          response.get("result", {}).get("stopReason") == "end_turn" and said == COMPACTING_ANSWER * 3 and goal["status"] == "blocked",
          f"answer {str(response)[:200]}, {len(said) / len(COMPACTING_ANSWER)} answers, goal {str(goal)[:200]}")
    summaries = [meta["summary"] for _, meta in agent.metas("compaction") if meta.get("summary")]
    check("ACP compaction: it did compact, the threshold of the last answer too", len(summaries) == 2,
          [summary[:80] for summary in summaries])
    agent.close()


SCENARIOS = [
    scenario_loop, scenario_loop_text, scenario_loop_acp,
    scenario_work, scenario_worked_beside_a_server, scenario_polled_beside_a_server,
    scenario_user_resets, scenario_marker_counts,
    scenario_wait, scenario_wait_acp, scenario_wait_settled, scenario_wait_prompt,
    scenario_wait_pause, scenario_kernel_dies,
    scenario_block, scenario_block_running, scenario_block_running_acp, scenario_autonomous_blocked,
    scenario_autonomous_turn_limit_blocked,
    scenario_tool_then_empty, scenario_two_commands,
    scenario_complete_stops_commands, scenario_complete_stops_commands_acp,
    scenario_resume, scenario_reload,
    scenario_complete, scenario_budget, scenario_provider_error, scenario_empty_turns,
    scenario_compaction, scenario_compaction_acp,
]
if not QUICK:
    SCENARIOS += [scenario_server, scenario_server_acp, scenario_notice_beside_server]


def guarded(scenario):
    try:
        scenario()
    except Exception as error:  # noqa: BLE001 - a scenario that dies is a failed check
        check(f"{scenario.__name__} ran to its end", False, repr(error))


subprocess.run([BIN, "--unharnessed-bootstrap"], env=ENV, check=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
with ThreadPoolExecutor(max_workers=len(SCENARIOS)) as pool:
    list(pool.map(guarded, SCENARIOS))
print(f"{len(FAILED)} failed" if FAILED else "all passed")
sys.exit(1 if FAILED else 0)
