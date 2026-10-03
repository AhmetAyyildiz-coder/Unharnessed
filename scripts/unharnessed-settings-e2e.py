#!/usr/bin/env python3
"""End-to-end checks of the settings screen's methods over ACP on the real
binary (VSCODE_PLAN.md step 5): `_unharnessed/settings/get` and `set` on
the global settings.json, what a write keeps and refuses, a live key (the
retries) reaching the next request without a restart, a restart key
(compaction.agentCallable) reaching the next process, the settings removed
from the agent (terminal, themes, daemon...) loading and surviving
untouched; the MCP servers (`_unharnessed/mcp/*`, a sign-in started as the
provider login `mcp:<id>`) and the skills (`_unharnessed/skills/*`, each
place the agent reads them from, under a throwaway HOME).

usage: scripts/unharnessed-settings-e2e.py <out-dir>

Nothing reaches the network: UNHARNESSED_OFFLINE is set, the model catalog
is the uh-models test fixture seeded as the agent's catalog cache, and the
OpenAI requests go to a local server (models.json `baseUrl`) that records
them and answers 503. Each scenario runs in its own agent dir
(<out-dir>/<name>/agent) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start.
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

GET = "_unharnessed/settings/get"
SET = "_unharnessed/settings/set"
MCP_LIST = "_unharnessed/mcp/list"
MCP_LOGOUT = "_unharnessed/mcp/logout"
LOGIN = "_unharnessed/providers/login"
LOGIN_CANCEL = "_unharnessed/providers/login_cancel"
SKILLS_LIST = "_unharnessed/skills/list"
SKILLS_SET = "_unharnessed/skills/set"
SKILLS_CREATE = "_unharnessed/skills/create"
SKILLS_ADD_FOLDER = "_unharnessed/skills/add_folder"
SKILLS_REMOVE_FOLDER = "_unharnessed/skills/remove_folder"

# Keys the agent no longer has; a file that still carries them loads, and
# every write keeps them as they are.
REMOVED = {
    "theme": "dark",
    "themes": ["~/my-themes"],
    "terminal": {"showImages": False, "clearOnShrink": True},
    "markdown": {"codeBlockIndent": "    "},
    "quietStartup": True,
    "shellPath": "/bin/zsh",
    "autonomous": {"maxTurns": 40},
    "idleEvictionMinutes": 30,
    "transport": "sse",
    "recentModels": ["openai/gpt-4.1"],
    "enabledModels": ["openai/*"],
}


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def details(answer):
    return answer.get("error", {}).get("data", {}).get("details")


class Requests:
    """A local OpenAI stand-in: records each request body, answers 503."""

    def __init__(self, path):
        self.path = path
        self.bodies = []
        requests = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = self.rfile.read(int(self.headers.get("Content-Length", 0)))
                requests.bodies.append({"path": self.path, "body": json.loads(body or b"null")})
                requests.path.write_text(json.dumps(requests.bodies, indent=2))
                answer = json.dumps({"error": {"message": "e2e: unavailable", "type": "server_error"}})
                self.send_response(503)
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

    def __init__(self, name, auth=None, models_json=None, settings=None, run=1, home=None, at_home=False):
        self.agent_dir = OUT / name / "agent"
        work = OUT / name / "work"
        if at_home:
            # VS Code without a folder open: the agent runs in the home
            # folder, its agent dir the default ~/.unharnessed.
            home = work = OUT / name / "home"
            self.agent_dir = home / ".unharnessed"
        self.agent_dir.mkdir(parents=True, exist_ok=True)
        work.mkdir(parents=True, exist_ok=True)
        if auth is not None:
            (self.agent_dir / "auth.json").write_text(json.dumps(auth, indent=2))
        if models_json is not None:
            (self.agent_dir / "models.json").write_text(json.dumps(models_json, indent=2))
        if settings is not None:
            self.settings_path.write_text(settings if isinstance(settings, str) else json.dumps(settings, indent=2))
        cache = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
        (self.agent_dir / "models").mkdir(exist_ok=True)
        (self.agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(cache))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir)}
        if home is not None:
            # The skills under ~/.agents come from this folder, not the caller's.
            extra["HOME"] = str(home)
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
        self.ids = itertools.count(1)
        threading.Thread(target=self._read, daemon=True).start()
        self.request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
        self.session_id = None

    @property
    def settings_path(self):
        return self.agent_dir / "settings.json"

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

    def request(self, method, params=None, timeout=30):
        request_id = next(self.ids)
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params or {}})
        deadline = time.time() + timeout
        while request_id not in self.answers:
            frame = self.inbox.get(timeout=max(0.05, deadline - time.time()))
            if frame is None:
                raise EOFError("the agent closed stdout")
            if "id" in frame and "method" not in frame:
                self.answers[frame["id"]] = frame
        return self.answers[request_id]

    def set(self, key, value):
        return self.request(SET, {"key": key, "value": value})

    def new_session(self):
        answer = self.request("session/new", {"cwd": str(self.work), "mcpServers": []})
        self.session_id = answer["result"]["sessionId"]
        return answer["result"]["configOptions"]

    def prompt(self, text):
        prompt = [{"type": "text", "text": text}]
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": prompt}, timeout=120)

    def settings(self):
        return json.loads(self.settings_path.read_text()) if self.settings_path.exists() else None

    def close(self):
        self.child.stdin.close()
        code = self.child.wait(timeout=30)
        self.out.close()
        return code


def scenario_read_write():
    agent = Agent("read-write")
    view = agent.request(GET).get("result", {})
    values, defaults = view.get("values", {}), view.get("defaults", {})
    check("read: a fresh agent dir has every key unset",
          bool(values) and all(v is None for v in values.values()), values)
    check("read: the defaults are the agent's",
          (defaults.get("compaction.enabled"), defaults.get("compaction.reserveTokens"),
           defaults.get("compaction.keepRecentTokens"), defaults.get("retry.maxRetries"),
           defaults.get("retry.baseDelayMs"), defaults.get("retry.failover.maxRetries"),
           defaults.get("retry.provider.waitForUsage.maxPauseMs"), defaults.get("images.autoResize"))
          == (True, 16384, 20000, 3, 2000, 3, 86400000, True), defaults)
    check("read: the restart keys are the ones the engine keeps",
          sorted(view.get("restart", [])) == sorted(["compaction.enabled", "compaction.reserveTokens",
                                                     "compaction.keepRecentTokens", "compaction.agentCallable",
                                                     "mcpServers"]), view.get("restart"))
    check("read: reading writes nothing", agent.settings() is None, agent.settings())

    answer = agent.set("compaction.reserveTokens", 32000)
    check("write: a number lands at its nested path",
          agent.settings() == {"compaction": {"reserveTokens": 32000}}, agent.settings())
    check("write: the answer shows the new value",
          answer.get("result", {}).get("values", {}).get("compaction.reserveTokens") == 32000, answer)
    agent.set("compaction.enabled", False)
    agent.set("imageModel", "  openai/gpt-5.4  ")
    check("write: a sibling key joins its parent, a model is trimmed",
          agent.settings() == {"compaction": {"reserveTokens": 32000, "enabled": False},
                               "imageModel": "openai/gpt-5.4"}, agent.settings())
    agent.set("compaction.reserveTokens", None)
    agent.set("compaction.enabled", None)
    check("write: null removes the key and the parent it left empty",
          agent.settings() == {"imageModel": "openai/gpt-5.4"}, agent.settings())

    before = agent.settings_path.read_text()
    refused = [
        ("compaction.enabled", "yes", "compaction.enabled takes true or false"),
        ("retry.maxRetries", -1, "retry.maxRetries takes a whole number from 0 to 100"),
        ("retry.maxRetries", 1.5, "retry.maxRetries takes a whole number from 0 to 100"),
        ("retry.maxRetries", 101, "retry.maxRetries takes a whole number from 0 to 100"),
        ("imageModel", "   ", "imageModel takes a provider/model reference"),
        ("theme", "dark", "unknown setting: theme"),
        ("mcpServers", [], "mcpServers takes an object of servers"),
        ("mcpServers", {"x": {"type": "http", "url": " "}}, "MCP server x: the URL is empty"),
        ("mcpServers", {"x": {"type": "stdio"}}, None),
        ("mcpServers", {"x": {"type": "sse", "url": "https://x"}}, None),
    ]
    for key, bad, expected in refused:
        answer = agent.set(key, bad)
        got = details(answer)
        ok = answer.get("error", {}).get("code") == -32602 and (got == expected if expected else bool(got))
        check(f"refuse: {key} = {json.dumps(bad)}", ok, answer.get("error") or answer.get("result"))
    check("refuse: nothing was written", agent.settings_path.read_text() == before)

    servers = {"docs": {"type": "http", "url": "https://mcp.example/mcp", "enabled": False},
               "local": {"type": "stdio", "command": "npx", "args": ["-y", "some-server"]}}
    agent.set("mcpServers", servers)
    check("write: the MCP servers are written as given", agent.settings().get("mcpServers") == servers,
          agent.settings())
    check("read-write: the agent exits when stdin closes", agent.close() == 0)


def scenario_removed_keys():
    requests = Requests(OUT / "removed-keys" / "requests.json")
    (OUT / "removed-keys").mkdir(parents=True, exist_ok=True)
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    settings = dict(REMOVED, retry={"maxRetries": 0})
    agent = Agent("removed-keys", auth=OPENAI_KEY, models_json=models_json, settings=settings)
    options = agent.new_session()
    check("removed: a settings file with the removed keys loads", bool(options), options)
    agent.set("compaction.keepRecentTokens", 10000)
    kept = {key: agent.settings().get(key) for key in REMOVED}
    check("removed: a settings write keeps them untouched", kept == REMOVED, kept)
    model = json.dumps(["openai", "gpt-4.1"], separators=(",", ":"))
    answer = agent.request("session/set_config_option",
                           {"sessionId": agent.session_id, "configId": "model", "value": model})
    check("removed: a model switch saves", "result" in answer, answer)
    after = agent.settings()
    kept = {key: after.get(key) for key in REMOVED}
    check("removed: the model switch keeps them too", kept == REMOVED, kept)
    check("removed: and writes no null", "null" not in agent.settings_path.read_text(), agent.settings())
    check("removed: and keeps the settings written by the screen",
          after.get("compaction") == {"keepRecentTokens": 10000} and after.get("defaultModel") == "gpt-4.1",
          after)
    check("removed: the agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def scenario_live():
    """A retry key is read on each failed turn: a change applies to the
    next prompt of the same process."""
    requests = Requests(OUT / "live" / "requests.json")
    (OUT / "live").mkdir(parents=True, exist_ok=True)
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    settings = {"retry": {"maxRetries": 2, "baseDelayMs": 0}}
    agent = Agent("live", auth=OPENAI_KEY, models_json=models_json, settings=settings)
    agent.new_session()
    agent.prompt("first")
    first = len(requests.bodies)
    check("live: with 2 retries, a failing request is sent 3 times", first == 3, first)
    agent.set("retry.enabled", False)
    agent.prompt("second")
    second = len(requests.bodies) - first
    check("live: retries turned off in the same process, it is sent once", second == 1, second)
    agent.set("retry.enabled", None)
    agent.set("retry.maxRetries", 1)
    agent.prompt("third")
    third = len(requests.bodies) - first - second
    check("live: back on with 1 retry, it is sent twice", third == 2, third)
    check("live: the agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def scenario_restart():
    """compaction.agentCallable is read when the agent starts: off, the
    next process has no `compact` skill in its prompt."""
    requests = Requests(OUT / "restart" / "requests.json")
    (OUT / "restart").mkdir(parents=True, exist_ok=True)
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    settings = {"retry": {"enabled": False}}
    agent = Agent("restart", auth=OPENAI_KEY, models_json=models_json, settings=settings)
    agent.new_session()
    agent.prompt("before")
    before = json.dumps(requests.bodies[-1]["body"]) if requests.bodies else ""
    check("restart: by default the prompt offers the compact skill", "compact/SKILL.md" in before)
    agent.set("compaction.agentCallable", False)
    agent.prompt("same process")
    same = json.dumps(requests.bodies[-1]["body"]) if requests.bodies else ""
    check("restart: the running process keeps it until it restarts", "compact/SKILL.md" in same)
    check("restart: the agent exits when stdin closes", agent.close() == 0)

    agent = Agent("restart", models_json=models_json, run=2)
    agent.new_session()
    agent.prompt("after")
    after = json.dumps(requests.bodies[-1]["body"]) if requests.bodies else ""
    check("restart: the next process has no compact skill", "compact/SKILL.md" not in after and after != "")
    check("restart: but still the other built-in skills", "goal/SKILL.md" in after)
    check("restart: the second agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def scenario_nulls():
    """A file full of nulls, as the agent wrote it before: a settings write
    cleans it, keeping every key that is set."""
    nulls = {"defaultProvider": "openai", "defaultModel": "gpt-5.4", "theme": None, "compaction": None,
             "recentModels": ["openai/gpt-5.4"], "retry": {"enabled": None, "maxRetries": 4}}
    agent = Agent("nulls", settings=nulls)
    view = agent.request(GET).get("result", {})
    check("nulls: a null reads as unset, a sibling value still reads",
          view.get("values", {}).get("retry.enabled") is None and view.get("values", {}).get("retry.maxRetries") == 4,
          view.get("values"))
    agent.set("images.blockImages", True)
    check("nulls: a settings write leaves only what is set",
          agent.settings() == {"defaultProvider": "openai", "defaultModel": "gpt-5.4",
                               "recentModels": ["openai/gpt-5.4"], "retry": {"maxRetries": 4},
                               "images": {"blockImages": True}}, agent.settings())
    check("nulls: the agent exits when stdin closes", agent.close() == 0)


def scenario_broken_file():
    agent = Agent("broken-file", settings='{ "compaction": { "enabled": false }, oops')
    answer = agent.set("compaction.enabled", True)
    check("broken: a write to a file that does not parse is refused",
          answer.get("error", {}).get("code") == -32602 and "failed to parse" in (details(answer) or ""), answer)
    check("broken: the file is left as it was",
          agent.settings_path.read_text() == '{ "compaction": { "enabled": false }, oops')
    check("broken: the agent exits when stdin closes", agent.close() == 0)


def scenario_mcp():
    """The MCP servers as the screen lists them, a sign-out, and sign-ins
    started from the client: the MCP flow runs and its failure comes back."""
    now = int(time.time() * 1000)
    closed = ThreadingHTTPServer(("127.0.0.1", 0), BaseHTTPRequestHandler)
    closed_port = closed.server_port
    closed.server_close()  # nothing listens there: the sign-in cannot reach it
    servers = {
        "docs": {"type": "http", "url": "https://mcp.example/mcp", "oauth": True},
        "tools": {"type": "stdio", "command": "echo", "enabled": False},
        "plain": {"type": "http", "url": "https://plain.example/mcp"},
        "keyed": {"type": "http", "url": "https://keyed.example/mcp", "bearerTokenEnvVar": "E2E_UNSET_TOKEN"},
        "unreachable": {"type": "http", "url": f"https://127.0.0.1:{closed_port}/mcp", "oauth": True},
    }
    auth = dict(OPENAI_KEY, **{"mcp:docs": {"type": "oauth", "access": "e2e-access", "refresh": "e2e-refresh",
                                            "expires": now + 3_600_000, "endpoint": "https://mcp.example/mcp"}})
    agent = Agent("mcp", auth=auth, settings={"mcpServers": servers}, home=OUT / "mcp" / "home")
    records_path = agent.agent_dir / "mcp-connections.json"
    records_path.write_text(json.dumps({"version": 1, "connections": {"docs": {
        "connectionId": "docs", "serviceId": "docs", "endpoint": "https://mcp.example/mcp", "label": "docs",
        "status": "connected", "createdAt": now, "updatedAt": now, "verifiedAt": now, "toolCount": 3}}}))
    auth_path = agent.agent_dir / "auth.json"

    listed = {server["id"]: server for server in agent.request(MCP_LIST).get("result", {}).get("servers", [])}
    pick = lambda server, *keys: tuple(listed.get(server, {}).get(key) for key in keys)
    check("mcp: the built-in services are listed, ready for a sign-in",
          [pick(name, "source", "status", "signIn", "signedIn") for name in ("linear", "notion")]
          == [("catalog", "not_connected", True, False)] * 2, listed)
    check("mcp: a signed-in server with a verified connection is connected",
          pick("docs", "source", "status", "signIn", "signedIn") == ("user", "connected", False, True), listed.get("docs"))
    check("mcp: a server turned off reads disabled", pick("tools", "status") == ("disabled",), listed.get("tools"))
    check("mcp: a server without sign-in needs nothing",
          pick("plain", "status", "signIn") == ("connected", False), listed.get("plain"))
    check("mcp: a server whose token variable is unset says which",
          listed.get("keyed", {}).get("status") != "connected"
          and "E2E_UNSET_TOKEN" in (listed.get("keyed", {}).get("hint") or ""), listed.get("keyed"))

    listed = {server["id"]: server for server in
              agent.request(MCP_LOGOUT, {"id": "docs"}).get("result", {}).get("servers", [])}
    stored = json.loads(auth_path.read_text())
    check("mcp: sign-out removes the stored sign-in",
          "mcp:docs" not in stored and pick("docs", "status", "signIn", "signedIn") == ("not_connected", True, False),
          listed.get("docs"))
    check("mcp: and keeps the other credentials", stored.get("openai") == OPENAI_KEY["openai"], list(stored))
    check("mcp: and the connection record",
          "docs" not in json.loads(records_path.read_text()).get("connections", {}), records_path.read_text())
    answer = agent.request(MCP_LOGOUT, {"id": ""})
    check("mcp: a sign-out names its server", details(answer) == "id must be a non-empty string", answer)

    for server, reason in (("nope", "an unknown server"), ("plain", "a server without OAuth"),
                           ("unreachable", "a server that does not answer")):
        answer = agent.request(LOGIN, {"providerId": f"mcp:{server}"}, timeout=60).get("result", {})
        check(f"mcp: a sign-in to {reason} fails with the reason",
              answer.get("outcome") == "failed" and bool(answer.get("message")), answer)
    # A server that takes the connection and never answers: the sign-in
    # waits on it until the client cancels.
    silent = socket.socket()
    silent.bind(("127.0.0.1", 0))
    silent.listen()
    servers["silent"] = {"type": "http", "url": f"https://127.0.0.1:{silent.getsockname()[1]}/mcp", "oauth": True}
    agent.set("mcpServers", servers)
    request_id = next(agent.ids)
    agent._send({"jsonrpc": "2.0", "id": request_id, "method": LOGIN, "params": {"providerId": "mcp:silent"}})
    time.sleep(1)
    started = time.time()
    agent._send({"jsonrpc": "2.0", "method": LOGIN_CANCEL})
    while request_id not in agent.answers:
        frame = agent.inbox.get(timeout=30)
        if frame and "id" in frame and "method" not in frame:
            agent.answers[frame["id"]] = frame
    answer = agent.answers[request_id].get("result", {})
    check("mcp: a cancel stops a sign-in that waits on its server",
          answer.get("outcome") == "cancelled" and time.time() - started < 5, (answer.get("outcome"), time.time() - started))
    silent.close()
    check("mcp: no credential was stored on the way", set(json.loads(auth_path.read_text())) == {"openai"},
          list(json.loads(auth_path.read_text())))

    before = agent.settings_path.read_text()
    for name, expected in (("linear", "MCP server linear: the name is taken by the built-in linear server"),
                           ("bad name", None), ("-x", None)):
        answer = agent.set("mcpServers", {name: {"type": "stdio", "command": "echo"}})
        check(f"mcp: the server name {name!r} is refused",
              answer.get("error", {}).get("code") == -32602
              and (details(answer) == expected if expected else "a name is 1 to 64" in (details(answer) or "")),
              answer.get("error") or answer.get("result"))
    check("mcp: nothing was written", agent.settings_path.read_text() == before)
    check("mcp: the agent exits when stdin closes", agent.close() == 0)


def skill(folder, name, description=None):
    folder.mkdir(parents=True, exist_ok=True)
    head = f"name: {name}\n" + (f"description: {description}\n" if description else "")
    (folder / "SKILL.md").write_text(f"---\n{head}---\n\n# {name}\n")
    return folder


def scenario_skills():
    """The skills as the screen lists them, from every place the agent
    reads; turning one off and on; a new skill; a folder of the skills
    setting; and the next process loading what the list said."""
    root = OUT / "skills"
    home = root / "home"
    mine = skill(root / "agent" / "skills" / "mine", "mine", "Mine.")
    agents_one = skill(home / ".agents" / "skills" / "agents-one", "agents-one", "From ~/.agents.")
    project = skill(root / "work" / ".agents" / "skills" / "proj", "proj", "From the project.")
    extra = root / "extra"
    skill(extra / "ext", "ext", "From a folder.")
    skill(extra / "broken", "broken")
    requests = Requests(root / "requests.json")
    models_json = {"providers": {"openai": {"baseUrl": requests.base_url}}}
    settings = {"skills": ["!**/agents-one"], "retry": {"enabled": False}}
    agent = Agent("skills", auth=OPENAI_KEY, models_json=models_json, settings=settings, home=home)

    def listed(answer):
        view = answer.get("result", {})
        return view, {item["name"]: item for item in view.get("skills", [])}

    view, skills = listed(agent.request(SKILLS_LIST))
    pick = lambda name, *keys: tuple(skills.get(name, {}).get(key) for key in keys)
    check("skills: each place the agent reads is listed with its source",
          [pick(name, "source") for name in ("mine", "agents-one", "proj", "goal")]
          == [("user",), ("user",), ("project",), ("builtin",)], {k: v.get("source") for k, v in skills.items()})
    check("skills: a skill reads on and loaded, one a pattern turns off reads off",
          pick("mine", "enabled", "active") == (True, True) and pick("agents-one", "enabled", "active") == (False, False),
          skills)
    check("skills: its file, and its folder under ~",
          pick("agents-one", "path", "location")
          == (str(agents_one / "SKILL.md"), "~/.agents/skills/agents-one"), skills.get("agents-one"))
    check("skills: nothing waits for a restart", view.get("restart") is False and view.get("folders") == [], view)

    view, skills = listed(agent.request(SKILLS_SET, {"path": str(mine / "SKILL.md"), "enabled": False}))
    check("skills: off writes the skill's folder in the skills setting",
          agent.settings().get("skills") == ["!**/agents-one", f"-{mine}"], agent.settings())
    check("skills: it reads off, still loaded until a restart",
          pick("mine", "enabled", "active") == (False, True) and view.get("restart") is True, skills.get("mine"))
    view, skills = listed(agent.request(SKILLS_SET, {"path": str(project / "SKILL.md"), "enabled": False}))
    check("skills: the global switch reaches a project skill", pick("proj", "enabled") == (False,), skills.get("proj"))
    view, skills = listed(agent.request(SKILLS_SET, {"path": str(project / "SKILL.md"), "enabled": True}))
    check("skills: on again removes the entry",
          pick("proj", "enabled") == (True,) and agent.settings().get("skills") == ["!**/agents-one", f"-{mine}"],
          agent.settings())
    view, skills = listed(agent.request(SKILLS_SET, {"path": str(agents_one / "SKILL.md"), "enabled": True}))
    check("skills: on over a ! pattern adds a + entry and keeps the pattern",
          pick("agents-one", "enabled") == (True,)
          and agent.settings().get("skills") == ["!**/agents-one", f"-{mine}", f"+{agents_one}"], agent.settings())
    answer = agent.request(SKILLS_SET, {"path": str(root / "nowhere" / "SKILL.md"), "enabled": False})
    check("skills: an unknown skill is refused", "no skill at" in (details(answer) or ""), answer)

    view, skills = listed(agent.request(SKILLS_ADD_FOLDER, {"path": str(extra)}))
    check("skills: a folder joins the skills setting and its skills are listed",
          view.get("folders") == [str(extra)] and pick("ext", "source", "enabled", "active") == ("folder", True, False),
          view.get("folders"))
    check("skills: a skill that does not load is a problem, with its reason",
          any(problem.get("message") == "description is required" and "broken" in problem.get("path", "")
              for problem in view.get("problems", [])), view.get("problems"))
    for what, path, expected in (("a missing folder", str(root / "missing"), "no folder at"),
                                 ("a relative path", "relative/skills", "full path"),
                                 ("a pattern", "-" + str(extra), "cannot start with")):
        answer = agent.request(SKILLS_ADD_FOLDER, {"path": path})
        check(f"skills: {what} is refused as a folder", expected in (details(answer) or ""), answer)

    answer = agent.request(SKILLS_CREATE, {"name": "new-skill", "description": "Says: hi, \"quoted\""})
    view, skills = listed(answer)
    created = answer.get("result", {}).get("created", "")
    check("skills: a new skill is written in the agent dir",
          created == str(agent.agent_dir / "skills" / "new-skill" / "SKILL.md") and Path(created).exists(), answer)
    check("skills: and reads back with its description as typed",
          pick("new-skill", "description", "source", "enabled") == ('Says: hi, "quoted"', "user", True),
          skills.get("new-skill"))
    for params, expected in (({"name": "new-skill", "description": "Again."}, "already exists"),
                             ({"name": "mine", "description": "Taken."}, "already exists"),
                             ({"name": "Bad_Name", "description": "x"}, "lowercase"),
                             ({"name": "x", "description": "  "}, "description must be a non-empty string")):
        answer = agent.request(SKILLS_CREATE, params)
        check(f"skills: creating {params['name']!r} is refused", expected in (details(answer) or ""), answer)
    check("skills: the agent exits when stdin closes", agent.close() == 0)

    agent = Agent("skills", models_json=models_json, run=2, home=home)
    view, skills = listed(agent.request(SKILLS_LIST))
    check("skills: the next process loads what the list said",
          pick("mine", "active") == (False,) and pick("ext", "active") == (True,)
          and pick("new-skill", "active") == (True,) and pick("agents-one", "active") == (True,)
          and view.get("restart") is False, {k: v.get("active") for k, v in skills.items()})
    agent.new_session()
    agent.prompt("which skills")
    body = json.dumps(requests.bodies[-1]["body"]) if requests.bodies else ""
    check("skills: and the model sees the same",
          "ext/SKILL.md" in body and "new-skill/SKILL.md" in body and "mine/SKILL.md" not in body and body != "")
    view, skills = listed(agent.request(SKILLS_REMOVE_FOLDER, {"path": str(extra)}))
    check("skills: removing the folder takes its skills off the list",
          view.get("folders") == [] and "ext" not in skills and view.get("restart") is True, view.get("folders"))
    check("skills: the second agent exits when stdin closes", agent.close() == 0)
    requests.server.shutdown()


def scenario_skills_at_home():
    """The home folder as cwd: its `.unharnessed` is the agent dir, not a
    project. Its skills, and the folders of its settings, are the user's."""
    home = OUT / "skills-at-home" / "home"
    mine = skill(home / ".unharnessed" / "skills" / "mine", "mine", "Mine.")
    extra = OUT / "skills-at-home" / "extra"
    skill(extra / "ext", "ext", "From a folder.")
    agent = Agent("skills-at-home", settings={"skills": [str(extra)]}, at_home=True)
    skills = {item["name"]: item for item in agent.request(SKILLS_LIST).get("result", {}).get("skills", [])}
    check("at home: the agent dir's skills are the user's, not a project's",
          (skills.get("mine", {}).get("source"), skills.get("ext", {}).get("source")) == ("user", "folder"),
          {name: item.get("source") for name, item in skills.items()})
    agent.request(SKILLS_SET, {"path": str(mine / "SKILL.md"), "enabled": False})
    skills = {item["name"]: item for item in agent.request(SKILLS_LIST).get("result", {}).get("skills", [])}
    check("at home: the switch turns it off",
          skills.get("mine", {}).get("enabled") is False
          and agent.settings().get("skills") == [str(extra), f"-{mine}"], agent.settings())
    check("at home: the agent exits when stdin closes", agent.close() == 0)


for scenario in (scenario_read_write, scenario_removed_keys, scenario_live, scenario_restart, scenario_nulls,
                 scenario_broken_file, scenario_mcp, scenario_skills, scenario_skills_at_home):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the others still run
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
