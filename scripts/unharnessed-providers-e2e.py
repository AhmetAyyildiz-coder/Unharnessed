#!/usr/bin/env python3
"""End-to-end checks of the provider logins over ACP on the real binary
(`_unharnessed/providers/*`, VSCODE_PLAN.md step 3): the provider list, an
API key saved and removed, a key from the environment, a stored Anthropic
subscription and its warning, and the browser logins up to their first
network call (the sign-in URL, the paste prompts, a bad paste, a cancel, a
client that disconnects mid-login). Nothing reaches the network.

usage: scripts/unharnessed-providers-e2e.py <out-dir>

Each scenario runs in its own agent dir (<out-dir>/<name>/agent) and writes
<out-dir>/<name>.out: the command, then every frame sent (>) and received
(<) with the seconds since start. The kernel venv lives in <out-dir>.
Provider keys and tokens from the caller's environment are dropped.
UNHARNESSED_BIN picks the binary (default target/debug/unharnessed). Exits
non-zero when a check fails.
"""
import itertools
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
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/debug/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
ENV = {
    name: value
    for name, value in os.environ.items()
    if not name.endswith(("_API_KEY", "_TOKEN")) and not name.startswith("UNHARNESSED_")
}
ENV.update(UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), DO_NOT_TRACK="1")
FAILED = []

LIST = "_unharnessed/providers/list"
SET_API_KEY = "_unharnessed/providers/set_api_key"
LOGOUT = "_unharnessed/providers/logout"
LOGIN = "_unharnessed/providers/login"
LOGIN_ANSWER = "_unharnessed/providers/login_answer"
LOGIN_CANCEL = "_unharnessed/providers/login_cancel"
LOGIN_OPEN = "_unharnessed/providers/login_open"
LOGIN_UPDATE = "_unharnessed/providers/login_update"
ANTHROPIC_PORT = 53692
CODEX_PORT = 1455


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def listening(port):
    with socket.socket() as probe:
        probe.settimeout(1)
        return probe.connect_ex(("127.0.0.1", port)) == 0


def row(providers, provider_id, method):
    return next((p for p in providers if p["id"] == provider_id and p["method"] == method), None)


class Agent:
    """One `unharnessed --mode acp` process in a throwaway agent dir."""

    def __init__(self, name, env=None, auth=None):
        self.agent_dir = OUT / name / "agent"
        work = OUT / name / "work"
        self.agent_dir.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        if auth is not None:
            (self.agent_dir / "auth.json").write_text(json.dumps(auth, indent=2))
        extra = dict(env or {}, UNHARNESSED_CODING_AGENT_DIR=str(self.agent_dir))
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}={value}" for key, value in extra.items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(OUT / name / "stderr.log", "w"))
        self.start = time.time()
        self.inbox = queue.Queue()
        self.answers = {}
        self.updates = []
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})

    def _log(self, arrow, frame):
        self.out.write(f"{time.time() - self.start:6.2f} {arrow} {json.dumps(frame)}\n")
        self.out.flush()

    def _read(self):
        for line in self.child.stdout:
            if line.strip():
                frame = json.loads(line)
                self._log("<", frame)
                self.inbox.put(frame)
        self.inbox.put(None)

    def _send(self, frame):
        self._log(">", frame)
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()

    def _pump(self, deadline):
        frame = self.inbox.get(timeout=max(0.05, deadline - time.time()))
        if frame is None:
            raise EOFError("the agent closed stdout")
        if "id" in frame and "method" not in frame:
            self.answers[frame["id"]] = frame
        elif frame.get("method") == LOGIN_UPDATE:
            self.updates.append(frame["params"]["update"])

    def send(self, method, params):
        request_id = next(self.ids)
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        return request_id

    def notify(self, method, params):
        self._send({"jsonrpc": "2.0", "method": method, "params": params})

    def wait(self, request_id, timeout=30):
        deadline = time.time() + timeout
        while request_id not in self.answers:
            self._pump(deadline)
        return self.answers[request_id]

    def request(self, method, params, timeout=30):
        return self.wait(self.send(method, params), timeout)

    def update(self, kind, timeout=30):
        """The next login update of `kind` (earlier ones of other kinds are skipped)."""
        deadline = time.time() + timeout
        while True:
            while self.updates:
                found = self.updates.pop(0)
                if found["kind"] == kind:
                    return found
            self._pump(deadline)

    def auth(self):
        path = self.agent_dir / "auth.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def close(self, timeout=10):
        """Close stdin; return (exit code, seconds to exit)."""
        closed = time.time()
        self.child.stdin.close()
        try:
            code = self.child.wait(timeout)
        except subprocess.TimeoutExpired:
            self.child.kill()
            code = "still running"
        self.out.write(f"$ exit {code} after {time.time() - closed:.2f}s\n")
        self.out.close()
        return code, time.time() - closed


def scenario_list():
    agent = Agent("list")
    providers = agent.request(LIST, {})["result"]["providers"]
    check("list: no provider connected in an empty agent dir",
          not any(p["connected"] for p in providers), [p["id"] for p in providers if p["connected"]])
    for provider_id in ("anthropic", "github-copilot", "openai-codex", "xai"):
        found = row(providers, provider_id, "subscription")
        check(f"list: {provider_id} subscription logs in through the browser",
              found is not None and found["login"] == "browser", found)
    openai = row(providers, "openai", "apiKey")
    check("list: openai takes an API key", openai is not None and openai["login"] == "apiKey", openai)
    prime = row(providers, "prime-inference", "apiKey")
    check("list: prime-inference logs in through its browser flow",
          prime is not None and prime["login"] == "browser", prime)
    # Opening the page of a running login would open a real browser tab: only
    # the refusal is checked here.
    error = agent.request(LOGIN_OPEN, {}).get("error")
    check("list: there is no sign-in page to open without a login",
          error is not None and "no sign-in page" in error["data"]["details"], error)
    check("list: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_api_key():
    agent = Agent("api-key")
    result = agent.request(SET_API_KEY, {"providerId": "openai", "apiKey": "  sk-e2e-test\n"})["result"]
    openai = row(result["providers"], "openai", "apiKey")
    check("api key: saving answers done", result["outcome"] == "done", result.get("message"))
    check("api key: openai is connected and removable",
          openai["connected"] and openai["removable"] and "note" not in openai, openai)
    check("api key: auth.json holds the trimmed key",
          agent.auth().get("openai") == {"type": "api_key", "key": "sk-e2e-test"}, agent.auth())

    result = agent.request(SET_API_KEY, {"providerId": "openai", "apiKey": "   "})["result"]
    check("api key: a blank key fails with a message",
          result["outcome"] == "failed" and "empty" in result.get("message", ""), result.get("message"))
    check("api key: a blank key leaves the stored one",
          agent.auth().get("openai", {}).get("key") == "sk-e2e-test", agent.auth())

    error = agent.request(SET_API_KEY, {"providerId": "openai-codex", "apiKey": "x"}).get("error")
    check("api key: a browser-only provider refuses a key",
          error is not None and error["code"] == -32602, error)

    result = agent.request(LOGOUT, {"providerId": "openai"})["result"]
    openai = row(result["providers"], "openai", "apiKey")
    check("api key: logout answers done", result["outcome"] == "done", result.get("message"))
    check("api key: openai is disconnected after logout", not openai["connected"], openai)
    check("api key: auth.json no longer has openai", "openai" not in agent.auth(), agent.auth())
    check("api key: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_env_key():
    agent = Agent("env-key", env={"OPENAI_API_KEY": "sk-env-e2e"})
    providers = agent.request(LIST, {})["result"]["providers"]
    openai = row(providers, "openai", "apiKey")
    check("env key: openai is connected from the environment",
          openai["connected"] and not openai["removable"] and openai.get("note") == "env: OPENAI_API_KEY",
          openai)
    error = agent.request(LOGOUT, {"providerId": "openai"}).get("error")
    check("env key: logout has no stored credential to remove", error is not None, error)
    check("env key: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_anthropic_stored():
    # Expires in a day: nothing refreshes it, so nothing reaches the network.
    expires = int(time.time() * 1000) + 86_400_000
    auth = {"anthropic": {"type": "oauth", "access": "sk-ant-oat01-e2e", "refresh": "e2e", "expires": expires}}
    agent = Agent("anthropic-stored", auth=auth)
    providers = agent.request(LIST, {})["result"]["providers"]
    subscription = row(providers, "anthropic", "subscription")
    api_key = row(providers, "anthropic", "apiKey")
    check("anthropic stored: the subscription is connected and removable",
          subscription["connected"] and subscription["removable"], subscription)
    check("anthropic stored: the subscription row carries the terms warning",
          "Claude Code" in subscription.get("warning", ""), subscription)
    check("anthropic stored: the API key row says the subscription holds the slot",
          not api_key["connected"] and api_key.get("note") == "subscription configured", api_key)
    result = agent.request(LOGOUT, {"providerId": "anthropic"})["result"]
    subscription = row(result["providers"], "anthropic", "subscription")
    check("anthropic stored: logout disconnects it and drops the warning",
          result["outcome"] == "done" and not subscription["connected"] and "warning" not in subscription,
          subscription)
    check("anthropic stored: auth.json no longer has anthropic", "anthropic" not in agent.auth(), agent.auth())
    check("anthropic stored: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_anthropic_login():
    if listening(ANTHROPIC_PORT):
        check("anthropic login: port 53692 is free before the test", False, "another process holds it")
        return
    agent = Agent("anthropic-login")
    login = agent.send(LOGIN, {"providerId": "anthropic"})
    url = agent.update("url")
    check("anthropic login: the sign-in URL is claude.ai's, redirecting to localhost:53692",
          url["url"].startswith("https://claude.ai/oauth/authorize?")
          and "redirect_uri=http%3A%2F%2Flocalhost%3A53692%2Fcallback" in url["url"], url["url"])
    check("anthropic login: the URL comes with instructions", bool(url.get("instructions")), url)
    prompt = agent.update("prompt")
    check("anthropic login: the paste prompt follows, optional beside the browser",
          "Paste" in prompt["message"] and prompt["optional"], prompt)
    check("anthropic login: the callback server listens", listening(ANTHROPIC_PORT))

    error = agent.request(LOGIN, {"providerId": "openai-codex"}).get("error")
    check("anthropic login: a second login is refused while one runs",
          error is not None and "already running" in error["data"]["details"], error)
    error = agent.request(LOGIN_ANSWER, {"promptId": 999, "answer": {"kind": "text", "value": "x"}}).get("error")
    check("anthropic login: an unknown prompt id is refused", error is not None, error)
    error = agent.request(LOGIN_ANSWER, {"promptId": prompt["promptId"], "answer": {"kind": "text", "value": " "}}).get("error")
    check("anthropic login: a blank paste is refused and the prompt stays", error is not None, error)

    answer = {"kind": "text", "value": "e2e-code#not-the-state"}
    reply = agent.request(LOGIN_ANSWER, {"promptId": prompt["promptId"], "answer": answer})
    check("anthropic login: the paste is accepted", reply.get("result") == {}, reply)
    result = agent.wait(login)["result"]
    check("anthropic login: a paste from another login fails on the state",
          result["outcome"] == "failed" and "state mismatch" in result.get("message", ""), result.get("message"))
    check("anthropic login: nothing is stored", "anthropic" not in agent.auth(), agent.auth())
    check("anthropic login: the callback server is closed", not listening(ANTHROPIC_PORT))
    check("anthropic login: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_cancel():
    agent = Agent("cancel")
    for provider_id, port in (("openai-codex", CODEX_PORT), ("anthropic", ANTHROPIC_PORT)):
        # A flow whose callback port is taken runs without its server: the
        # port checks below would see the other process.
        if listening(port):
            check(f"cancel: localhost:{port} is free before the {provider_id} login", False,
                  "another process holds it")
            continue
        login = agent.send(LOGIN, {"providerId": provider_id})
        url = agent.update("url")
        agent.update("prompt")
        check(f"cancel: {provider_id} shows its sign-in URL", url["url"].startswith("https://"), url)
        check(f"cancel: {provider_id} listens on localhost:{port}", listening(port))
        cancelled_at = time.time()
        agent.notify(LOGIN_CANCEL, {})
        result = agent.wait(login, timeout=5)["result"]
        check(f"cancel: {provider_id} answers cancelled",
              result["outcome"] == "cancelled", result)
        check(f"cancel: {provider_id} stops within 2 s", time.time() - cancelled_at < 2,
              f"{time.time() - cancelled_at:.2f}s")
        check(f"cancel: {provider_id} closes localhost:{port}", not listening(port))

    login = agent.send(LOGIN, {"providerId": "github-copilot"})
    prompt = agent.update("prompt")
    check("cancel: copilot asks for the enterprise domain first, required, blank allowed",
          prompt["allowEmpty"] and not prompt["secret"] and not prompt["optional"], prompt)
    agent.notify(LOGIN_CANCEL, {})
    result = agent.wait(login, timeout=5)["result"]
    check("cancel: copilot answers cancelled", result["outcome"] == "cancelled", result)
    check("cancel: nothing is stored", agent.auth() == {}, agent.auth())
    check("cancel: the agent exits when stdin closes", agent.close()[0] == 0)


def scenario_disconnect():
    agent = Agent("disconnect")
    agent.send(LOGIN, {"providerId": "anthropic"})
    agent.update("prompt")
    code, seconds = agent.close()
    check("disconnect: the agent exits mid-login", code == 0, code)
    check("disconnect: within 2 s", seconds < 2, f"{seconds:.2f}s")
    check("disconnect: the callback server is closed", not listening(ANTHROPIC_PORT))
    check("disconnect: nothing is stored", agent.auth() == {}, agent.auth())


for scenario in (scenario_list, scenario_api_key, scenario_env_key, scenario_anthropic_stored,
                 scenario_anthropic_login, scenario_cancel, scenario_disconnect):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the others still run
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
