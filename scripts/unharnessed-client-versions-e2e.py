#!/usr/bin/env python3
"""End-to-end checks, on the real binary, of the client versions the
subscription requests claim (`claude-cli/<version>` on an Anthropic login,
`antigravity/<version>` on a Google one): the compiled version, a newer one
cached by an earlier run, an older cached one, the environment override, and
the version an Anthropic refusal asks for, claimed on a second request.

usage: scripts/unharnessed-client-versions-e2e.py <out-dir> [--live]

Both backends are one stand-in on 127.0.0.1, which each run's models.json
points the providers at; it records the `User-Agent` of every request.
Nothing else reaches the network: UNHARNESSED_OFFLINE is set and the model
catalog is the uh-models test fixture, seeded as the agent's catalog cache.
With --live, two more runs go without UNHARNESSED_OFFLINE: the agent then
looks the published versions up (the npm registry, Antigravity's update
manifest) and the next run claims them.

Each run is `unharnessed -p` in <out-dir>/<name> (agent dir, home, work
dir); <out-dir>/<name>.out has the command, its output and exit code, and
the requests the stand-in received. Provider keys and tokens from the
caller's environment are dropped. UNHARNESSED_BIN picks the binary (default
target/debug/unharnessed). Exits non-zero when a check fails.
"""
import json
import os
import re
import subprocess
import sys
import threading
import time
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/debug/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
LIVE = "--live" in sys.argv[2:]
OUT.mkdir(parents=True, exist_ok=True)
ENV = {
    name: value
    for name, value in os.environ.items()
    if not name.endswith(("_API_KEY", "_TOKEN")) and not name.startswith(("UNHARNESSED_", "GOOGLE_CLOUD_"))
}
ENV.update(UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), DO_NOT_TRACK="1",
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")))
CATALOG = json.loads((ROOT / "crates/uh-models/tests/fixtures/catalog.v1.json").read_text())
CATALOG_URL = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json"
CLAUDE = "anthropic/" + next(m["id"] for m in CATALOG["models"] if m["provider"] == "anthropic")
GEMINI = "google-antigravity/gemini-3.8-flash-low"
CLAUDE_CACHE, GEMINI_CACHE = "claude-code-version.json", "antigravity-version.json"
ANSWER = "Hi."
FAILED = []


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def numbers(version):
    return [int(part) for part in version.split(".")]


class Backend:
    """Anthropic's messages endpoint and Cloud Code Assist's stream method,
    each answering ANSWER. `requires` makes the former refuse a Claude Code
    older than that version, as the API does for a gated model; `refusal`
    makes it refuse every request with that message. `delay` holds each
    answer back, leaving a background lookup the time to finish."""

    def __init__(self):
        self.agents = []
        self.requires = None
        self.refusal = None
        self.delay = 0
        backend = self

        class Handler(BaseHTTPRequestHandler):
            def stream(self, events):
                time.sleep(backend.delay)
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for name, data in events:
                    event = f"event: {name}\n" if name else ""
                    self.wfile.write(f"{event}data: {json.dumps(data)}\n\n".encode())

            def refuse(self, message):
                body = json.dumps({"type": "error", "error": {"type": "invalid_request_error", "message": message}})
                self.send_response(400)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body.encode())

            def do_POST(self):
                self.rfile.read(int(self.headers.get("Content-Length", 0)))
                agent = self.headers.get("User-Agent", "")
                backend.agents.append(agent)
                if self.path.startswith("/v1internal:streamGenerateContent"):
                    return self.stream([(None, {"response": {"candidates": [{
                        "content": {"role": "model", "parts": [{"text": ANSWER}]}, "finishReason": "STOP"}]}})])
                if backend.refusal:
                    return self.refuse(backend.refusal)
                claimed = agent.removeprefix("claude-cli/")
                if backend.requires and numbers(claimed) < numbers(backend.requires):
                    return self.refuse(f"Claude Code {claimed} does not support this model; version "
                                       f"{backend.requires} or newer is required. Run 'claude update'.")
                usage = {"input_tokens": 10, "output_tokens": 1}
                return self.stream([
                    ("message_start", {"type": "message_start", "message": {
                        "id": "msg_e2e", "type": "message", "role": "assistant", "model": "e2e", "content": [],
                        "stop_reason": None, "stop_sequence": None, "usage": usage}}),
                    ("content_block_start", {"type": "content_block_start", "index": 0,
                                             "content_block": {"type": "text", "text": ""}}),
                    ("content_block_delta", {"type": "content_block_delta", "index": 0,
                                             "delta": {"type": "text_delta", "text": ANSWER}}),
                    ("content_block_stop", {"type": "content_block_stop", "index": 0}),
                    ("message_delta", {"type": "message_delta", "delta": {"stop_reason": "end_turn",
                                                                           "stop_sequence": None},
                                       "usage": {"output_tokens": 3}}),
                    ("message_stop", {"type": "message_stop"})])

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"


BACKEND = Backend()


def cache(name, file):
    path = OUT / name / "agent" / "models" / file
    return json.loads(path.read_text()) if path.exists() else None


def run(name, model, cached=None, env=None, offline=True):
    """One `unharnessed -p` on `model`, both subscriptions stored, in the
    agent dir <out-dir>/<name> (kept from an earlier run of that name).
    `cached` seeds the version cache files. Returns the exit code, the
    answer, and the User-Agents the stand-in saw."""
    agent_dir, work, home = (OUT / name / part for part in ("agent", "work", "home"))
    for folder in (agent_dir / "models", work, home):
        folder.mkdir(parents=True, exist_ok=True)
    expires = int(time.time() * 1000) + 86_400_000
    (agent_dir / "auth.json").write_text(json.dumps({
        "anthropic": {"type": "oauth", "access": "sk-ant-oat01-e2e", "refresh": "e2e", "expires": expires},
        "google-antigravity": {"type": "oauth", "access": "e2e-access", "refresh": "e2e", "expires": expires,
                               "accountId": "e2e-project"}}))
    (agent_dir / "models.json").write_text(json.dumps({"providers": {
        "anthropic": {"baseUrl": BACKEND.base_url}, "google-antigravity": {"baseUrl": BACKEND.base_url}}}))
    catalog = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
    (agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(catalog))
    for file, version in (cached or {}).items():
        (agent_dir / "models" / file).write_text(json.dumps({"version": version, "checkedAtMs": 0}))
    extra = dict(env or {}, UNHARNESSED_CODING_AGENT_DIR=str(agent_dir), HOME=str(home))
    if offline:
        extra["UNHARNESSED_OFFLINE"] = "1"
    command = [BIN, "-p", "Say hi.", "--model", model]
    seen = len(BACKEND.agents)
    done = subprocess.run(command, cwd=work, env=dict(ENV, **extra), text=True, capture_output=True, timeout=300)
    agents = BACKEND.agents[seen:]
    with open(OUT / f"{name}.out", "a") as out:
        out.write("$ " + " ".join(f"{key}={value}" for key, value in extra.items())
                  + f" unharnessed -p 'Say hi.' --model {model}\n{done.stdout}{done.stderr}"
                  + f"$ exit {done.returncode}; requests: {json.dumps(agents)}\n")
    return done.returncode, done.stdout.strip(), agents


def scenario_offline():
    code, answer, claude = run("compiled", CLAUDE)
    check("compiled: the Anthropic turn answers", code == 0 and answer == ANSWER, (code, answer))
    check("compiled: one request, as claude-cli with a version",
          len(claude) == 1 and re.fullmatch(r"claude-cli/\d+(\.\d+)+", claude[0]), claude)
    code, answer, gemini = run("compiled", GEMINI)
    check("compiled: the Antigravity turn answers", code == 0 and answer == ANSWER, (code, answer))
    check("compiled: one request, as antigravity with a version",
          len(gemini) == 1 and re.fullmatch(r"antigravity/\d+(\.\d+)+ darwin/arm64", gemini[0]), gemini)
    check("compiled: offline, nothing is looked up or cached",
          cache("compiled", CLAUDE_CACHE) is None and cache("compiled", GEMINI_CACHE) is None)
    if not (len(claude) == 1 and len(gemini) == 1):
        return None

    newer = {CLAUDE_CACHE: "99.1.2", GEMINI_CACHE: "99.3.4"}
    _, _, agents = run("cached-newer", CLAUDE, cached=newer)
    check("cached: a newer Claude Code version from an earlier run is claimed",
          agents == ["claude-cli/99.1.2"], agents)
    _, _, agents = run("cached-newer", GEMINI)
    check("cached: a newer Antigravity version from an earlier run is claimed",
          agents == ["antigravity/99.3.4 darwin/arm64"], agents)
    older = {CLAUDE_CACHE: "0.0.1", GEMINI_CACHE: "0.0.1"}
    _, _, agents = run("cached-older", CLAUDE, cached=older)
    check("cached: an older Claude Code version is not", agents == claude, agents)
    _, _, agents = run("cached-older", GEMINI)
    check("cached: an older Antigravity version is not", agents == gemini, agents)
    _, _, agents = run("cached-broken", GEMINI, cached={GEMINI_CACHE: "latest"})
    check("cached: nor one that is not a version", agents == gemini, agents)

    _, _, agents = run("override", GEMINI, cached=newer, env={"UNHARNESSED_ANTIGRAVITY_VERSION": "1.2.3"})
    check("override: UNHARNESSED_ANTIGRAVITY_VERSION is claimed over the cached version",
          agents == ["antigravity/1.2.3 darwin/arm64"], agents)
    _, _, agents = run("override", CLAUDE, cached=newer, env={"UNHARNESSED_CLAUDE_CODE_VERSION": "1.2.3"})
    check("override: UNHARNESSED_CLAUDE_CODE_VERSION likewise", agents == ["claude-cli/1.2.3"], agents)

    BACKEND.requires = "98.7.6"
    code, answer, agents = run("required", CLAUDE)
    check("required: the refused request goes once more with the version asked for",
          agents == [claude[0], "claude-cli/98.7.6"], agents)
    check("required: and the turn answers", code == 0 and answer == ANSWER, (code, answer))
    check("required: the version is cached", (cache("required", CLAUDE_CACHE) or {}).get("version") == "98.7.6",
          cache("required", CLAUDE_CACHE))
    code, answer, agents = run("required", CLAUDE)
    check("required: the next run claims it from the first request",
          code == 0 and agents == ["claude-cli/98.7.6"], agents)
    code, _, agents = run("required-overridden", CLAUDE, env={"UNHARNESSED_CLAUDE_CODE_VERSION": "1.2.3"})
    check("required: an overridden version is not replaced: one request, and the turn fails",
          code != 0 and agents == ["claude-cli/1.2.3"], (code, agents))
    BACKEND.requires = None

    BACKEND.refusal = "version 98.7.6 or newer is required"
    code, _, agents = run("refused-twice", CLAUDE)
    check("refused: a backend that still refuses the version it asked for gets two requests, not more",
          code != 0 and agents == [claude[0], "claude-cli/98.7.6"], (code, agents))
    BACKEND.refusal = "e2e: this request is refused"
    code, _, agents = run("refused", CLAUDE)
    check("refused: another refusal is not sent again", code != 0 and agents == claude, (code, agents))
    BACKEND.refusal = None
    return claude[0], gemini[0]


def scenario_live(compiled):
    # The answers wait two seconds: the lookups finish before the process exits.
    BACKEND.delay = 2
    for model, file, compiled_agent in ((CLAUDE, CLAUDE_CACHE, compiled[0]), (GEMINI, GEMINI_CACHE, compiled[1])):
        floor = re.search(r"/([\d.]+)", compiled_agent).group(1)
        _, _, first = run("live", model, offline=False)
        check(f"live: {model}: the first request goes with the compiled version", first == [compiled_agent], first)
        found = cache("live", file) or {}
        version = found.get("version", "")
        check(f"live: {model}: the published version is cached, with the time of the lookup",
              re.fullmatch(r"\d+(\.\d+)+", version) is not None and numbers(version) >= numbers(floor)
              and abs(found.get("checkedAtMs", 0) / 1000 - time.time()) < 60, found)
        print(f"      compiled {floor}, published {version}")
        _, _, second = run("live", model, offline=False)
        check(f"live: {model}: the next run claims it", second == [compiled_agent.replace(floor, version)], second)
        check(f"live: {model}: and does not look it up again", cache("live", file) == found, cache("live", file))
    BACKEND.delay = 0


compiled = None
try:
    compiled = scenario_offline()
except Exception as error:  # a crash fails the scenario
    check("scenario_offline", False, f"{type(error).__name__}: {error}")
if LIVE and compiled:
    try:
        scenario_live(compiled)
    except Exception as error:
        check("scenario_live", False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
