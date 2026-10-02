#!/usr/bin/env python3
"""End-to-end checks of the model pickers over ACP on the real binary
(VSCODE_PLAN.md step 4): the `model`, `thought_level` and `fast_mode`
config options of `session/new`, each model's details in `_meta`, a
selection applied, persisted and restored by the next process, the fast
mode reaching the provider request, and the pickers following a login
and a logout (including the switch off a model left without credentials).

usage: scripts/unharnessed-models-e2e.py <out-dir>

Nothing reaches the network: UNHARNESSED_OFFLINE is set, the model catalog
is the uh-models test fixture seeded as the agent's catalog cache, and the
OpenAI requests go to a local server (models.json `baseUrl`) that records
them and answers 400. Each scenario runs in its own agent dir
(<out-dir>/<name>/agent) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start; the
recorded requests go to <out-dir>/<name>/requests.json. Provider keys and
tokens from the caller's environment are dropped. UNHARNESSED_BIN picks the
binary (default target/debug/unharnessed). Exits non-zero when a check
fails.
"""
import itertools
import json
import os
import queue
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
CATALOG = json.loads((ROOT / "crates/uh-models/tests/fixtures/catalog.v1.json").read_text())
CATALOG_URL = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json"
OPENAI_KEY = {"openai": {"type": "api_key", "key": "sk-e2e-test"}}
FAILED = []

SET_API_KEY = "_unharnessed/providers/set_api_key"
LOGOUT = "_unharnessed/providers/logout"


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def value(provider, model_id):
    return json.dumps([provider, model_id], separators=(",", ":"))


def catalog_models(provider):
    return [m for m in CATALOG["models"] if m["provider"] == provider]


def option(options, option_id):
    return next((o for o in options if o["id"] == option_id), None)


def current(options, option_id):
    found = option(options, option_id)
    return found and found["currentValue"]


class Requests:
    """A local OpenAI stand-in: records each request body, answers 400."""

    def __init__(self, path):
        self.path = path
        self.bodies = []
        requests = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                requests.bodies.append({"path": self.path, "body": json.loads(body or b"null")})
                requests.path.write_text(json.dumps(requests.bodies, indent=2))
                answer = json.dumps({"error": {"message": "e2e: recorded", "type": "invalid_request_error"}})
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(answer)))
                self.end_headers()
                self.wfile.write(answer.encode())

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}/v1"


class Agent:
    """One `unharnessed --mode acp` process in a throwaway agent dir, with
    the catalog fixture as its catalog cache."""

    def __init__(self, name, auth=None, models_json=None, run=1):
        self.agent_dir = OUT / name / "agent"
        work = OUT / name / "work"
        self.agent_dir.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        if auth is not None:
            (self.agent_dir / "auth.json").write_text(json.dumps(auth, indent=2))
        if models_json is not None:
            (self.agent_dir / "models.json").write_text(json.dumps(models_json, indent=2))
        cache = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
        (self.agent_dir / "models").mkdir(exist_ok=True)
        (self.agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(cache))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir)}
        suffix = "" if run == 1 else f"-{run}"
        self.out = open(OUT / f"{name}{suffix}.out", "w")
        command = " ".join(f"{key}={value}" for key, value in dict(extra, UNHARNESSED_OFFLINE="1").items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(OUT / name / f"stderr{suffix}.log", "w"))
        self.work = work
        self.start = time.time()
        self.inbox = queue.Queue()
        self.answers = {}
        self.config_updates = []
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        self.session_id = None

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
        elif frame.get("method") == "session/update":
            update = frame["params"]["update"]
            if update.get("sessionUpdate") == "config_option_update":
                self.config_updates.append(update["configOptions"])

    def request(self, method, params, timeout=30):
        request_id = next(self.ids)
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.time() + timeout
        while request_id not in self.answers:
            self._pump(deadline)
        return self.answers[request_id]

    def new_session(self):
        answer = self.request("session/new", {"cwd": str(self.work), "mcpServers": []})
        self.session_id = answer["result"]["sessionId"]
        return answer["result"]["configOptions"]

    def set_option(self, config_id, option_value):
        params = {"sessionId": self.session_id, "configId": config_id, "value": option_value}
        if isinstance(option_value, bool):
            params["type"] = "boolean"
        return self.request("session/set_config_option", params)

    def prompt(self, text):
        prompt = [{"type": "text", "text": text}]
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": prompt}, timeout=120)

    def config_update(self, matches, since, timeout=30):
        """The first config update from the `since`-th on that `matches`."""
        deadline = time.time() + timeout
        seen = since
        while True:
            for options in self.config_updates[seen:]:
                if matches(options):
                    return options
            seen = len(self.config_updates)
            try:
                self._pump(deadline)
            except queue.Empty:
                return None

    def settings(self):
        path = self.agent_dir / "settings.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def close(self):
        self.child.stdin.close()
        code = self.child.wait(timeout=30)
        self.out.close()
        return code


def scenario_details():
    agent = Agent("details", auth=OPENAI_KEY)
    options = agent.new_session()
    check("details: the agent starts on OpenAI's default model",
          current(options, "model") == value("openai", "gpt-5.4"), current(options, "model"))
    choices = option(options, "model")["options"]
    expected = catalog_models("openai")
    check("details: one choice per OpenAI model of the catalog",
          sorted(c["value"] for c in choices) == sorted(value("openai", m["id"]) for m in expected),
          f"{len(choices)} choices, {len(expected)} in the catalog")
    by_value = {c["value"]: c for c in choices}
    mismatches = []
    for model in expected:
        meta = (by_value.get(value("openai", model["id"])) or {}).get("_meta", {}).get("unharnessed", {}).get("model")
        wanted = {"id": model["id"], "name": model["name"], "provider": "openai",
                  "contextWindow": model["contextWindow"], "input": model["input"], "cost": model["cost"]}
        if meta != wanted:
            mismatches.append((model["id"], meta, wanted))
    check("details: each choice carries the model's id, name, context window, input and cost",
          not mismatches, mismatches[:1])
    levels = option(options, "thought_level")
    check("details: gpt-5.4 has a reasoning effort picker",
          levels is not None and levels["type"] == "select" and len(levels["options"]) > 1, levels)
    fast = option(options, "fast_mode")
    check("details: gpt-5.4 has the fast mode toggle, off",
          fast == {"id": "fast_mode", "name": "Fast mode", "type": "boolean",
                   "category": "model_config", "currentValue": False}, fast)
    switched = agent.set_option("model", value("openai", "gpt-4.1"))["result"]["configOptions"]
    check("details: gpt-4.1 has neither reasoning effort nor fast mode",
          option(switched, "thought_level") is None and option(switched, "fast_mode") is None,
          [o["id"] for o in switched])
    refused = agent.set_option("fast_mode", True)
    check("details: fast mode is refused on gpt-4.1",
          refused.get("error", {}).get("data", {}).get("reason") == "Fast mode is not available for this model",
          refused)
    agent.set_option("model", value("openai", "gpt-5.4"))
    refused = agent.set_option("fast_mode", "on")
    check("details: fast mode takes a boolean",
          refused.get("error", {}).get("data", {}).get("reason") == "Fast mode takes a boolean value", refused)
    check("details: the agent exits when stdin closes", agent.close() == 0)


def scenario_persist():
    requests = Requests(OUT / "persist" / "requests.json")
    (OUT / "persist").mkdir(parents=True, exist_ok=True)
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    agent = Agent("persist", auth=OPENAI_KEY, models_json=models_json)
    agent.new_session()
    options = agent.set_option("fast_mode", True)["result"]["configOptions"]
    check("persist: fast mode on", current(options, "fast_mode") is True, current(options, "fast_mode"))
    options = agent.set_option("thought_level", "high")["result"]["configOptions"]
    check("persist: reasoning effort high", current(options, "thought_level") == "high")
    settings = agent.settings()
    check("persist: settings.json keeps the fast mode and the effort",
          settings.get("defaultServiceTier") == "priority" and settings.get("defaultThinkingLevel") == "high",
          settings)
    agent.prompt("hello")
    body = requests.bodies[-1]["body"] if requests.bodies else {}
    check("persist: the request carries the priority tier", body.get("service_tier") == "priority",
          {k: body.get(k) for k in ("model", "service_tier", "reasoning")})
    check("persist: the request carries the effort", (body.get("reasoning") or {}).get("effort") == "high",
          body.get("reasoning"))
    options = agent.set_option("model", value("openai", "gpt-4.1"))["result"]["configOptions"]
    check("persist: gpt-4.1 shows no fast mode", option(options, "fast_mode") is None)
    agent.prompt("hello again")
    body = requests.bodies[-1]["body"] if requests.bodies else {}
    check("persist: a gpt-4.1 request carries no tier",
          body.get("model") == "gpt-4.1" and "service_tier" not in body,
          {k: body.get(k) for k in ("model", "service_tier")})
    options = agent.set_option("model", value("openai", "gpt-5.5"))["result"]["configOptions"]
    check("persist: back on a fast model, fast mode is still on", current(options, "fast_mode") is True,
          current(options, "fast_mode"))
    settings = agent.settings()
    check("persist: settings.json keeps the model",
          (settings.get("defaultProvider"), settings.get("defaultModel")) == ("openai", "gpt-5.5"), settings)
    check("persist: the agent exits when stdin closes", agent.close() == 0)

    # The next process starts where the last one stopped.
    agent = Agent("persist", models_json=models_json, run=2)
    options = agent.new_session()
    check("persist: a new process starts on the saved model",
          current(options, "model") == value("openai", "gpt-5.5"), current(options, "model"))
    check("persist: with the saved effort and fast mode",
          current(options, "thought_level") == "high" and current(options, "fast_mode") is True,
          (current(options, "thought_level"), current(options, "fast_mode")))
    agent.prompt("hello from the next process")
    body = requests.bodies[-1]["body"] if requests.bodies else {}
    check("persist: its first request carries the priority tier",
          body.get("model") == "gpt-5.5" and body.get("service_tier") == "priority",
          {k: body.get(k) for k in ("model", "service_tier")})
    check("persist: the second agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def scenario_login():
    agent = Agent("login")
    options = agent.new_session()
    before = current(options, "model")
    check("login: with no provider, the only choice is the startup model",
          [c["value"] for c in option(options, "model")["options"]] == [before], before)
    since = len(agent.config_updates)
    answer = agent.request(SET_API_KEY, {"providerId": "openai", "apiKey": "sk-e2e-test"})
    check("login: the API key is saved", answer.get("result", {}).get("outcome") == "done", answer)
    options = agent.config_update(lambda o: current(o, "model") == value("openai", "gpt-5.4"), since)
    check("login: the pickers switch to OpenAI's default model", options is not None,
          [current(o, "model") for o in agent.config_updates[since:]])
    if options:
        check("login: and list the OpenAI models",
              len(option(options, "model")["options"]) == len(catalog_models("openai")),
              len(option(options, "model")["options"]))
    check("login: the switch is saved",
          (agent.settings().get("defaultProvider"), agent.settings().get("defaultModel")) == ("openai", "gpt-5.4"),
          agent.settings())
    since = len(agent.config_updates)
    answer = agent.request(LOGOUT, {"providerId": "openai"})
    check("login: logout", answer.get("result", {}).get("outcome") == "done", answer)
    options = agent.config_update(lambda o: len(option(o, "model")["options"]) == 1, since)
    check("login: after logout only the current model is left, still selected",
          options is not None and current(options, "model") == value("openai", "gpt-5.4"),
          options and current(options, "model"))
    since = len(agent.config_updates)
    agent.request(SET_API_KEY, {"providerId": "anthropic", "apiKey": "sk-ant-e2e-test"})
    options = agent.config_update(lambda o: current(o, "model") == value("anthropic", "claude-opus-4-7"), since)
    check("login: a key for another provider moves off the model without credentials", options is not None,
          [current(o, "model") for o in agent.config_updates[since:]])
    check("login: the agent exits when stdin closes", agent.close() == 0)


for scenario in (scenario_details, scenario_persist, scenario_login):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the others still run
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
