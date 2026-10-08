#!/usr/bin/env python3
"""End-to-end checks of the instructions per model (MODEL_INSTRUCTIONS.md)
on the real binary: the `modelInstructions` setting written and refused over
`_unharnessed/settings/set`, its texts in the system prompt the provider
receives (right after core.md, in group order, only for the groups that
list the model), rebuilt on a model switch, a change counted from the next
conversation, and `unharnessed prompt --model`.

usage: scripts/unharnessed-model-instructions-e2e.py <out-dir>

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
CORE = (ROOT / "crates/uh-core/src/prompts/layers/core.md").read_text().strip()
OPENAI_KEY = {"openai": {"type": "api_key", "key": "sk-e2e-test"}}
SET = "_unharnessed/settings/set"
GET = "_unharnessed/settings/get"
FAILED = []

ALPHA = "E2E-ALPHA: be terse, no summary at the end."
BRAVO = "E2E-BRAVO: read a file before editing it.\nTwo lines."
CHARLIE = "E2E-CHARLIE: a second group for gpt-5.4."
DELTA = "E2E-DELTA: written while the conversation runs."
GROUPS = [
    # Claude is not connected here: the agent keeps it all the same.
    {"models": ["openai/gpt-5.4", "anthropic/claude-opus-4-7"], "text": ALPHA},
    {"models": ["openai/gpt-4.1"], "text": BRAVO},
    {"models": ["openai/gpt-5.4"], "text": CHARLIE},
]
TEXTS = (ALPHA, BRAVO, CHARLIE, DELTA)


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def value(provider, model_id):
    return json.dumps([provider, model_id], separators=(",", ":"))


def seed(agent_dir, settings=None, auth=None, models_json=None):
    agent_dir.mkdir(parents=True, exist_ok=True)
    if settings is not None:
        (agent_dir / "settings.json").write_text(json.dumps(settings, indent=2))
    if auth is not None:
        (agent_dir / "auth.json").write_text(json.dumps(auth, indent=2))
    if models_json is not None:
        (agent_dir / "models.json").write_text(json.dumps(models_json, indent=2))
    cache = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
    (agent_dir / "models").mkdir(exist_ok=True)
    (agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(cache))


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

    def last(self):
        """The model and the system prompt of the last request."""
        body = self.bodies[-1]["body"] if self.bodies else {}
        items = body.get("input") or body.get("messages") or []
        system = [
            item["content"] if isinstance(item["content"], str) else "".join(
                part.get("text", "") for part in item["content"])
            for item in items
            if isinstance(item, dict) and item.get("role") in ("system", "developer")
        ]
        return body.get("model"), "\n".join(system)


class Agent:
    """One `unharnessed --mode acp` process in a throwaway agent dir."""

    def __init__(self, name, run=1):
        self.agent_dir = OUT / name / "agent"
        self.work = OUT / name / "work"
        self.work.mkdir(parents=True, exist_ok=True)
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir)}
        suffix = "" if run == 1 else f"-{run}"
        self.out = open(OUT / f"{name}{suffix}.out", "w")
        command = " ".join(f"{key}={value}" for key, value in dict(extra, UNHARNESSED_OFFLINE="1").items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=self.work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE,
            stderr=open(OUT / name / f"stderr{suffix}.log", "w"))
        self.start = time.time()
        self.inbox = queue.Queue()
        self.answers = {}
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

    def request(self, method, params=None, timeout=30):
        request_id = next(self.ids)
        frame = {"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}}
        self._log(">", frame)
        self.child.stdin.write(json.dumps(frame) + "\n")
        self.child.stdin.flush()
        deadline = time.time() + timeout
        while request_id not in self.answers:
            frame = self.inbox.get(timeout=max(0.05, deadline - time.time()))
            if frame is None:
                raise EOFError("the agent closed stdout")
            if "id" in frame and "method" not in frame:
                self.answers[frame["id"]] = frame
        return self.answers[request_id]

    def new_session(self):
        answer = self.request("session/new", {"cwd": str(self.work), "mcpServers": []})
        self.session_id = answer["result"]["sessionId"]
        options = answer["result"]["configOptions"]
        return next(o for o in options if o["id"] == "model")["currentValue"]

    def set_model(self, provider, model_id):
        params = {"sessionId": self.session_id, "configId": "model", "value": value(provider, model_id)}
        return self.request("session/set_config_option", params)

    def prompt(self, text):
        prompt = [{"type": "text", "text": text}]
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": prompt}, timeout=120)

    def settings(self):
        path = self.agent_dir / "settings.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def close(self):
        self.child.stdin.close()
        code = self.child.wait(timeout=30)
        self.out.close()
        return code


def details(answer):
    return answer.get("error", {}).get("data", {}).get("details")


def scenario_setting():
    seed(OUT / "setting" / "agent")
    agent = Agent("setting")
    view = agent.request(GET)["result"]
    check("setting: unset in a fresh agent dir", view["values"].get("modelInstructions", "missing") is None,
          view["values"].get("modelInstructions", "missing"))
    check("setting: its default is no group", view["defaults"].get("modelInstructions") == [],
          view["defaults"].get("modelInstructions"))
    check("setting: it applies when the agent next starts", "modelInstructions" in view["restart"], view["restart"])

    sent = [
        {"models": [" openai/gpt-5.4 ", "anthropic/claude-opus-4-7", "openai/gpt-5.4"], "text": "Be terse.\n"},
        {"models": ["ollama/qwen3:8b"], "text": "Read first."},
    ]
    answer = agent.request(SET, {"key": "modelInstructions", "value": sent})
    stored = [
        {"models": ["openai/gpt-5.4", "anthropic/claude-opus-4-7"], "text": "Be terse.\n"},
        {"models": ["ollama/qwen3:8b"], "text": "Read first."},
    ]
    check("setting: two groups are written, models trimmed and each once",
          agent.settings().get("modelInstructions") == stored, agent.settings())
    check("setting: the answer shows them",
          answer.get("result", {}).get("values", {}).get("modelInstructions") == stored, answer)

    refusals = [
        ("not a list", {"models": ["openai/gpt-5.4"], "text": "x"}, "modelInstructions takes a list of groups"),
        ("no model", [{"models": [], "text": "x"}], "group 1 has no model"),
        ("no models list", [{"text": "x"}], "group 1: models takes a list of models"),
        ("a bare model id", [{"models": ["gpt-5.4"], "text": "x"}],
         'group 1: "gpt-5.4" is not a provider/model reference'),
        ("an empty provider", [{"models": ["/gpt-5.4"], "text": "x"}],
         'group 1: "/gpt-5.4" is not a provider/model reference'),
        ("a model that is not a string", [{"models": [7], "text": "x"}],
         "group 1: 7 is not a provider/model reference"),
        ("a blank text", [{"models": ["openai/gpt-5.4"], "text": "x"}, {"models": ["openai/gpt-4.1"], "text": " \n "}],
         "group 2 has no text"),
        ("no text", [{"models": ["openai/gpt-5.4"]}], "group 1: text takes a string"),
    ]
    for label, refused, reason in refusals:
        answer = agent.request(SET, {"key": "modelInstructions", "value": refused})
        check(f"refuse: {label}", answer.get("error", {}).get("code") == -32602 and details(answer) == reason,
              answer.get("error"))
    check("refuse: nothing was written", agent.settings().get("modelInstructions") == stored, agent.settings())

    agent.request(SET, {"key": "modelInstructions", "value": None})
    check("setting: null removes the key", "modelInstructions" not in agent.settings(), agent.settings())
    check("setting: the agent exits when stdin closes", agent.close() == 0)


def scenario_prompt():
    requests = Requests(OUT / "prompt" / "requests.json")
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    seed(OUT / "prompt" / "agent", settings={"modelInstructions": GROUPS}, auth=OPENAI_KEY, models_json=models_json)
    agent = Agent("prompt")
    model = agent.new_session()
    check("prompt: the session starts on gpt-5.4", model == value("openai", "gpt-5.4"), model)

    agent.prompt("hello")
    model, system = requests.last()
    check("prompt: a request reached the stand-in", model == "gpt-5.4", model)
    check("prompt: gpt-5.4 gets its two groups right after core.md, in order",
          system.startswith(f"{CORE}\n\n{ALPHA}\n\n{CHARLIE}\n\nPre-installed Python packages:"),
          system[len(CORE):len(CORE) + 200])
    check("prompt: and not the gpt-4.1 group", BRAVO not in system)

    agent.set_model("openai", "gpt-4.1")
    agent.prompt("hello on gpt-4.1")
    model, system = requests.last()
    check("switch: the next request is gpt-4.1's", model == "gpt-4.1", model)
    check("switch: its prompt carries its own group in place of gpt-5.4's",
          system.startswith(f"{CORE}\n\n{BRAVO}\n\nPre-installed Python packages:"),
          system[len(CORE):len(CORE) + 200])
    check("switch: none of gpt-5.4's texts are left", ALPHA not in system and CHARLIE not in system)

    agent.set_model("openai", "gpt-5.5")
    agent.prompt("hello on gpt-5.5")
    model, system = requests.last()
    check("no group: gpt-5.5's prompt has none of the texts",
          model == "gpt-5.5" and not any(text in system for text in TEXTS), model)
    check("no group: it is core.md, then the tail, as without the setting",
          system.startswith(f"{CORE}\n\nPre-installed Python packages:"), system[len(CORE):len(CORE) + 80])

    # A change while the conversation runs counts from the next one.
    changed = [GROUPS[0], {"models": ["openai/gpt-4.1"], "text": DELTA}, GROUPS[2]]
    answer = agent.request(SET, {"key": "modelInstructions", "value": changed})
    check("change: the new text is written", "error" not in answer and agent.settings().get("modelInstructions") == changed,
          answer.get("error"))
    agent.set_model("openai", "gpt-4.1")
    agent.prompt("hello after the change")
    model, system = requests.last()
    check("change: this conversation keeps the text it started with",
          model == "gpt-4.1" and BRAVO in system and DELTA not in system, model)
    check("change: the agent exits when stdin closes", agent.close() == 0)

    agent = Agent("prompt", run=2)
    model = agent.new_session()
    check("next: the next conversation starts on the saved model", model == value("openai", "gpt-4.1"), model)
    agent.prompt("hello from the next conversation")
    model, system = requests.last()
    check("next: it gets the new text",
          model == "gpt-4.1" and system.startswith(f"{CORE}\n\n{DELTA}\n\nPre-installed Python packages:"),
          system[len(CORE):len(CORE) + 200])
    check("next: and not the old one", BRAVO not in system)
    check("next: the agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def prompt_command(name, settings, model):
    agent_dir = OUT / name / "agent"
    seed(agent_dir, settings=settings)
    work = OUT / name / "work"
    work.mkdir(parents=True, exist_ok=True)
    command = [BIN, "prompt", "--model", model, "--json", "--cwd", str(work)]
    env = dict(ENV, UNHARNESSED_CODING_AGENT_DIR=str(agent_dir))
    done = subprocess.run(command, env=env, capture_output=True, text=True, timeout=60)
    with open(OUT / f"{name}.out", "w") as out:
        out.write(f"$ UNHARNESSED_CODING_AGENT_DIR={agent_dir} unharnessed {' '.join(command[1:])}\n")
        out.write(f"exit {done.returncode}\n{done.stdout}{done.stderr}")
    return done.returncode, json.loads(done.stdout) if done.returncode == 0 else {}


def scenario_prompt_command():
    code, dump = prompt_command("command", {"modelInstructions": GROUPS}, "openai/gpt-5.4")
    segments = [(s["name"], s["kind"], s["source"]) for s in dump.get("segments", [])]
    check("command: exits 0", code == 0, code)
    check("command: the model's instructions follow core.md in the cached prefix",
          segments[:3] == [("core", "static", "prompts/layers/core.md"),
                           ("model-instructions", "static", "settings modelInstructions"),
                           ("packages", "dynamic", "kernel bootstrap defaults")], segments[:3])
    prompt = dump.get("prompt", "")
    check("command: the cached prefix ends after them",
          prompt[:dump.get("cachedPrefixLen", 0)] == f"{CORE}\n\n{ALPHA}\n\n{CHARLIE}",
          prompt[:dump.get("cachedPrefixLen", 0)][-120:])

    code, dump = prompt_command("command-other", {"modelInstructions": GROUPS}, "anthropic/claude-opus-4-7")
    prompt = dump.get("prompt", "")
    check("command: a model the agent cannot reach still gets its group",
          code == 0 and prompt.startswith(f"{CORE}\n\n{ALPHA}\n\nPre-installed"), prompt[len(CORE):len(CORE) + 120])

    code, dump = prompt_command("command-broken", {"modelInstructions": "be terse"}, "openai/gpt-5.4")
    names = [s["name"] for s in dump.get("segments", [])]
    check("command: a value it cannot read loads as no group",
          code == 0 and "model-instructions" not in names, (code, names))


for scenario in (scenario_setting, scenario_prompt, scenario_prompt_command):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the others still run
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
