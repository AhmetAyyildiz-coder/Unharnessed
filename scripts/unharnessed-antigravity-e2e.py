#!/usr/bin/env python3
"""End-to-end checks of the Google Antigravity subscription over ACP on the
real binary: its provider row, the browser login up to its first network
call (the sign-in URL, the callback server, a redirect and a paste from
another login, a cancel), and a turn with a stored login: the model in the
picker, the request the backend receives, a tool call and its result.

usage: scripts/unharnessed-antigravity-e2e.py <out-dir>

The backend is a stand-in for Cloud Code Assist on 127.0.0.1, which the
turn's models.json points the provider at. Nothing else reaches the
network: UNHARNESSED_OFFLINE is set and the model catalog is the uh-models
test fixture, seeded as the agent's catalog cache (it does not list the
provider, as the live one). Each scenario runs in <out-dir>/<name> (agent
dir, home, work dir) and writes <out-dir>/<name>.out: the command, then
every frame sent (>) and received (<) with the seconds since start; the
requests the stand-in received go to <out-dir>/turn/requests.json. Provider
keys and tokens from the caller's environment are dropped. UNHARNESSED_BIN
picks the binary (default target/debug/unharnessed). Exits non-zero when a
check fails.
"""
import itertools
import json
import os
import queue
import re
import socket
import subprocess
import sys
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
BIN = os.environ.get("UNHARNESSED_BIN", str(ROOT / "target/debug/unharnessed"))
OUT = Path(sys.argv[1]).resolve()
OUT.mkdir(parents=True, exist_ok=True)
ENV = {
    name: value
    for name, value in os.environ.items()
    if not name.endswith(("_API_KEY", "_TOKEN")) and not name.startswith(("UNHARNESSED_", "GOOGLE_CLOUD_"))
}
ENV.update(UNHARNESSED_KERNEL_VENV=str(OUT / "kernel-venv"), UNHARNESSED_OFFLINE="1", DO_NOT_TRACK="1",
           UV_CACHE_DIR=os.environ.get("UV_CACHE_DIR", str(Path.home() / ".cache/uv")))
CATALOG = json.loads((ROOT / "crates/uh-models/tests/fixtures/catalog.v1.json").read_text())
CATALOG_URL = "https://raw.githubusercontent.com/PrimeIntellect-ai/prime-agent-catalog/main/models/catalog.v1.json"
FAILED = []

PROVIDER = "google-antigravity"
MODEL = "gemini-3.8-flash-low"
PORT = 51121
LIST = "_unharnessed/providers/list"
LOGIN = "_unharnessed/providers/login"
LOGIN_ANSWER = "_unharnessed/providers/login_answer"
LOGIN_CANCEL = "_unharnessed/providers/login_cancel"
LOGIN_UPDATE = "_unharnessed/providers/login_update"
LOGOUT = "_unharnessed/providers/logout"
SIGNATURE = "ZTJlLXNpZ25hdHVyZQ=="
ANSWER = "Six times seven is 42."


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def listening(port):
    with socket.socket() as probe:
        probe.settimeout(1)
        return probe.connect_ex(("127.0.0.1", port)) == 0


class Backend:
    """A Cloud Code Assist stand-in: the first request of a turn is answered
    with a thought and an `ipython` call, the one carrying its result with
    the answer. Each request is recorded."""

    def __init__(self, path):
        self.path = path
        self.requests = []
        backend = self

        class Handler(BaseHTTPRequestHandler):
            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"null")
                backend.requests.append({
                    "path": self.path, "authorization": self.headers.get("Authorization"),
                    "userAgent": self.headers.get("User-Agent"), "body": body})
                backend.path.write_text(json.dumps(backend.requests, indent=2))
                parts = [part for content in body["request"]["contents"] for part in content["parts"]]
                if any("functionResponse" in part for part in parts):
                    chunks = [{"candidates": [{"content": {"role": "model", "parts": [{"text": ANSWER}]},
                                               "finishReason": "STOP"}],
                               "usageMetadata": {"promptTokenCount": 40, "candidatesTokenCount": 8,
                                                 "totalTokenCount": 48}}]
                else:
                    chunks = [
                        {"candidates": [{"content": {"role": "model", "parts": [
                            {"thought": True, "text": "A cell computes it."}]}}]},
                        {"candidates": [{"content": {"role": "model", "parts": [
                            {"functionCall": {"name": "ipython", "args": {"code": "print(6 * 7)"}},
                             "thoughtSignature": SIGNATURE}]}, "finishReason": "STOP"}],
                         "usageMetadata": {"promptTokenCount": 30, "candidatesTokenCount": 6,
                                           "thoughtsTokenCount": 4, "totalTokenCount": 40}}]
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream")
                self.end_headers()
                for chunk in chunks:
                    self.wfile.write(f"data: {json.dumps({'response': chunk, 'traceId': 'e2e'})}\n\n".encode())

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.base_url = f"http://127.0.0.1:{self.server.server_port}"


class Agent:
    """One `unharnessed --mode acp` process in a throwaway agent dir and
    home, with the catalog fixture as its catalog cache."""

    def __init__(self, name, auth=None, models=None):
        self.agent_dir = OUT / name / "agent"
        self.work = OUT / name / "work"
        home = OUT / name / "home"
        for folder in (self.agent_dir / "models", self.work, home):
            folder.mkdir(parents=True, exist_ok=True)
        cache = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
        (self.agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(cache))
        for file, content in (("auth.json", auth), ("models.json", models)):
            (self.agent_dir / file).unlink(missing_ok=True)
            if content is not None:
                (self.agent_dir / file).write_text(json.dumps(content, indent=2))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir), "HOME": str(home)}
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}={value}" for key, value in dict(extra, UNHARNESSED_OFFLINE="1").items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=self.work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(OUT / name / "stderr.log", "w"))
        self.start = time.time()
        self.inbox = queue.Queue()
        self.answers = {}
        self.login_updates = []
        self.updates = []
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
        elif frame.get("method") == LOGIN_UPDATE:
            self.login_updates.append(frame["params"]["update"])
        elif frame.get("method") == "session/update":
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

    def login_update(self, kind, timeout=30):
        """The next login update of `kind` (earlier ones of other kinds are skipped)."""
        deadline = time.time() + timeout
        while True:
            while self.login_updates:
                found = self.login_updates.pop(0)
                if found["kind"] == kind:
                    return found
            self._pump(deadline)

    def text(self, kind):
        return "".join(update["content"].get("text", "") for update in self.updates
                       if update.get("sessionUpdate") == kind)

    def auth(self):
        path = self.agent_dir / "auth.json"
        return json.loads(path.read_text()) if path.exists() else {}

    def close(self):
        self.child.stdin.close()
        try:
            code = self.child.wait(timeout=60)
        except subprocess.TimeoutExpired:
            self.child.kill()
            code = "still running"
        self.out.close()
        return code


def row(providers):
    return next((p for p in providers if p["id"] == PROVIDER), None)


def scenario_login():
    if listening(PORT):
        check(f"login: port {PORT} is free before the test", False, "another process holds it")
        return
    agent = Agent("login")
    found = row(agent.request(LIST, {})["result"]["providers"])
    check("list: the subscription row logs in through the browser, not connected",
          found is not None and found["method"] == "subscription" and found["login"] == "browser"
          and not found["connected"] and found["name"] == "Google AI Pro/Ultra (Antigravity)", found)

    login = agent.send(LOGIN, {"providerId": PROVIDER})
    url = urllib.parse.urlsplit(agent.login_update("url")["url"])
    query = dict(urllib.parse.parse_qsl(url.query))
    check("login: the sign-in URL is Google's", f"{url.scheme}://{url.netloc}{url.path}"
          == "https://accounts.google.com/o/oauth2/v2/auth", url.geturl())
    check("login: for Antigravity's client, redirecting to localhost:51121, with a refresh token asked",
          query.get("client_id", "").startswith("1071006060591-")
          and query.get("redirect_uri") == f"http://localhost:{PORT}/oauth-callback"
          and query.get("response_type") == "code" and query.get("access_type") == "offline"
          and query.get("prompt") == "consent", query)
    check("login: with a PKCE challenge and a state that is not the verifier",
          query.get("code_challenge_method") == "S256" and len(query.get("code_challenge", "")) == 43
          and len(query.get("state", "")) == 43 and query["state"] != query["code_challenge"], query)
    check("login: with the scopes of the backend",
          "https://www.googleapis.com/auth/cloud-platform" in query.get("scope", "").split()
          and "https://www.googleapis.com/auth/cclog" in query.get("scope", "").split(), query.get("scope"))
    prompt = agent.login_update("prompt")
    check("login: the paste prompt follows, optional beside the browser",
          "Paste" in prompt["message"] and prompt["optional"], prompt)
    check("login: the callback server listens", listening(PORT))

    # A redirect from another login: refused, and the login keeps waiting.
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{PORT}/oauth-callback?code=x&state=not-the-state", timeout=5)
        status, page = 200, ""
    except urllib.error.HTTPError as error:
        status, page = error.code, error.read().decode()
    check("login: a redirect with another state is refused", status == 400 and "State mismatch" in page,
          (status, page[-200:]))
    try:
        urllib.request.urlopen(f"http://127.0.0.1:{PORT}/oauth-callback?error=access_denied", timeout=5)
        status, page = 200, ""
    except urllib.error.HTTPError as error:
        status, page = error.code, error.read().decode()
    check("login: a refused consent shows Google's error on the page",
          status == 400 and "Google authentication did not complete." in page and "access_denied" in page,
          (status, page[-300:]))

    pasted = f"http://localhost:{PORT}/oauth-callback?code=e2e-code&state=not-the-state"
    reply = agent.request(LOGIN_ANSWER, {"promptId": prompt["promptId"], "answer": {"kind": "text", "value": pasted}})
    check("login: a pasted redirect is accepted", reply.get("result") == {}, reply)
    result = agent.wait(login)["result"]
    check("login: a paste from another login fails on the state",
          result["outcome"] == "failed" and "state mismatch" in result.get("message", ""), result.get("message"))
    check("login: nothing is stored", agent.auth() == {}, agent.auth())
    check("login: the callback server is closed", not listening(PORT))

    login = agent.send(LOGIN, {"providerId": PROVIDER})
    agent.login_update("prompt")
    cancelled_at = time.time()
    agent.notify(LOGIN_CANCEL, {})
    result = agent.wait(login, timeout=5)["result"]
    check("cancel: the login answers cancelled within 2 s",
          result["outcome"] == "cancelled" and time.time() - cancelled_at < 2, result)
    check("cancel: the callback server is closed, nothing stored", not listening(PORT) and agent.auth() == {})
    check("login: the agent exits when stdin closes", agent.close() == 0)


def scenario_turn():
    backend = Backend(OUT / "turn" / "requests.json")
    # Expires in a day: nothing refreshes it, so nothing reaches Google.
    stored = {"type": "oauth", "access": "e2e-access", "refresh": "e2e-refresh",
              "expires": int(time.time() * 1000) + 86_400_000, "accountId": "e2e-project"}
    agent = Agent("turn", auth={PROVIDER: stored},
                  models={"providers": {PROVIDER: {"baseUrl": backend.base_url}}})
    found = row(agent.request(LIST, {})["result"]["providers"])
    check("stored: the subscription is connected and removable",
          found is not None and found["connected"] and found["removable"], found)

    answer = agent.request("session/new", {"cwd": str(agent.work), "mcpServers": []}, timeout=120)
    agent.session_id = answer["result"]["sessionId"]
    picker = next(o for o in answer["result"]["configOptions"] if o["id"] == "model")
    value = json.dumps([PROVIDER, MODEL], separators=(",", ":"))
    choice = next((c for c in picker["options"] if c["value"] == value), None)
    check("picker: the subscription's model is the one available, though the catalog does not list it",
          [c["value"] for c in picker["options"]] == [value] and picker["currentValue"] == value,
          ([c["value"] for c in picker["options"]], picker["currentValue"]))
    details = (choice or {}).get("_meta", {}).get("unharnessed", {}).get("model")
    check("picker: its details: name, context, image input, no cost",
          details is not None and details["name"] == "Gemini 3.8 Flash" and details["contextWindow"] == 1_048_576
          and details["input"] == ["text", "image"] and details["cost"]["input"] == 0, details)

    prompt = [{"type": "text", "text": "What is six times seven? Compute it."}]
    answer = agent.request("session/prompt", {"sessionId": agent.session_id, "prompt": prompt}, timeout=300)
    check("turn: the turn ends", answer.get("result", {}).get("stopReason") == "end_turn", answer)
    check("turn: the thought streams back", agent.text("agent_thought_chunk") == "A cell computes it.",
          agent.text("agent_thought_chunk"))
    check("turn: the answer streams back", agent.text("agent_message_chunk").strip() == ANSWER,
          agent.text("agent_message_chunk"))
    check("turn: two requests, the call then its result", len(backend.requests) == 2, len(backend.requests))
    first, second = (backend.requests + [{}, {}])[:2]
    body = first.get("body") or {}
    request = body.get("request") or {}
    check("request: to the backend's stream method, as Antigravity, with the stored token",
          first.get("path") == "/v1internal:streamGenerateContent?alt=sse"
          and first.get("authorization") == "Bearer e2e-access"
          and re.fullmatch(r"antigravity/\d+(\.\d+)+ darwin/arm64", first.get("userAgent") or ""),
          {key: first.get(key) for key in ("path", "authorization", "userAgent")})
    check("request: the envelope names the model and the stored project",
          body.get("model") == MODEL and body.get("project") == "e2e-project"
          and body.get("requestType") == "agent" and body.get("userAgent") == "antigravity"
          and body.get("requestId", "").startswith("agent-"), {key: body.get(key) for key in body if key != "request"})
    instruction = request.get("systemInstruction") or {}
    texts = [part.get("text", "") for part in instruction.get("parts", [])]
    check("request: the system instruction opens with Antigravity's preamble, the agent's prompt after it",
          instruction.get("role") == "user" and len(texts) == 3 and texts[0].startswith("You are Antigravity")
          and texts[1].startswith("Please ignore following [ignore]You are Antigravity")
          and "ipython" in texts[2], [text[:60] for text in texts])
    declarations = (request.get("tools") or [{}])[0].get("functionDeclarations", [])
    check("request: the one tool, with an upper-case parameters schema",
          [d.get("name") for d in declarations] == ["ipython"]
          and declarations[0]["parameters"].get("type") == "OBJECT"
          and declarations[0]["parameters"]["properties"]["code"].get("type") == "STRING"
          and declarations[0]["parameters"].get("required") == ["code"]
          and "additionalProperties" not in declarations[0]["parameters"], declarations)
    config = request.get("generationConfig") or {}
    check("request: thinking asked with its thoughts, at a level the model takes",
          config.get("thinkingConfig", {}).get("includeThoughts") is True
          and config["thinkingConfig"].get("thinkingLevel") in ("LOW", "MEDIUM", "HIGH"), config)
    check("request: the user's message is the only content",
          [content.get("role") for content in request.get("contents", [])] == ["user"], request.get("contents"))

    follow_up = (second.get("body") or {}).get("request") or {}
    check("request: one session id rides both requests",
          bool(request.get("sessionId")) and follow_up.get("sessionId") == request["sessionId"],
          (request.get("sessionId"), follow_up.get("sessionId")))
    contents = follow_up.get("contents", [])
    model_parts = next((c["parts"] for c in contents if c.get("role") == "model"), [])
    call = next((part for part in model_parts if "functionCall" in part), {})
    check("result: the thought and the call are sent back, the call with its signature",
          any(part.get("thought") is True for part in model_parts)
          and call.get("functionCall") == {"name": "ipython", "args": {"code": "print(6 * 7)"}}
          and call.get("thoughtSignature") == SIGNATURE, model_parts)
    results = [part["functionResponse"] for c in contents for part in c["parts"] if "functionResponse" in part]
    check("result: the cell ran, and its output is the function response",
          len(results) == 1 and results[0].get("name") == "ipython"
          and "42" in results[0].get("response", {}).get("output", ""), results)

    result = agent.request(LOGOUT, {"providerId": PROVIDER})["result"]
    check("logout: done, the row disconnected, auth.json without it",
          result["outcome"] == "done" and not row(result["providers"])["connected"] and agent.auth() == {}, result)
    check("turn: the agent exits when stdin closes", agent.close() == 0)


for scenario in (scenario_login, scenario_turn):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the other still runs
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
