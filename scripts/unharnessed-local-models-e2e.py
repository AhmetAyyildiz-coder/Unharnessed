#!/usr/bin/env python3
"""End-to-end checks of the local model servers over ACP on the real binary
(`_unharnessed/providers/local_*`): what a server serves (probe), the
servers at their usual addresses (scan), a server saved into models.json
and removed, the provider rows and the model picker following, and a turn
answered by the local model.

usage: scripts/unharnessed-local-models-e2e.py <out-dir>

The local server is a stand-in on 127.0.0.1 that answers as Ollama does
(`/v1/models`, `/api/version`, `/api/show`) and streams one answer on
`/v1/chat/completions`; under `/locked/` it asks for the key `secret`.
Nothing else reaches the network: UNHARNESSED_OFFLINE is set and the model
catalog is the uh-models test fixture, seeded as the agent's catalog cache.
The agent dir is <out-dir>/local/agent; <out-dir>/local.out has the
command, then every frame sent (>) and received (<) with the seconds since
start; the requests the stand-in received go to <out-dir>/local/requests.json.
Provider keys and tokens from the caller's environment are dropped.
UNHARNESSED_BIN picks the binary (default target/debug/unharnessed). Exits
non-zero when a check fails.
"""
import itertools
import json
import os
import queue
import socket
import stat
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
USUAL = ["http://localhost:11434/v1", "http://localhost:1234/v1", "http://localhost:8080/v1",
         "http://localhost:8000/v1"]
FAILED = []

LIST = "_unharnessed/providers/list"
SCAN = "_unharnessed/providers/local_scan"
PROBE = "_unharnessed/providers/local_probe"
SAVE = "_unharnessed/providers/local_save"
REMOVE = "_unharnessed/providers/local_remove"
ANSWER = "Hello from the local model."


def check(name, ok, detail=""):
    print(("PASS  " if ok else "FAIL  ") + name + ("" if ok or not detail else f" ({detail})"))
    if not ok:
        FAILED.append(name)


def value(provider, model_id):
    return json.dumps([provider, model_id], separators=(",", ":"))


def option(options, option_id):
    return next((o for o in options if o["id"] == option_id), None)


def current(options, option_id):
    found = option(options, option_id)
    return found and found["currentValue"]


def choices(options):
    found = option(options, "model")
    return [c["value"] for c in found["options"]] if found else []


def free_port():
    with socket.socket() as probe:
        probe.bind(("127.0.0.1", 0))
        return probe.getsockname()[1]


class Local:
    """An Ollama stand-in: one chat model, `tiny:1b` (8K context, reads
    images), and one embedding model. Each chat request is recorded."""

    def __init__(self, path):
        self.path = path
        self.requests = []
        local = self

        class Handler(BaseHTTPRequestHandler):
            def answer(self, code, body):
                data = json.dumps(body).encode()
                self.send_response(code)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(data)))
                self.end_headers()
                self.wfile.write(data)

            def do_GET(self):
                if self.path.startswith("/locked/"):
                    if self.headers.get("Authorization") != "Bearer secret":
                        return self.answer(401, {"error": "a key, please"})
                    if self.path == "/locked/v1/models":
                        return self.answer(200, {"object": "list", "data": [{"id": "guarded", "owned_by": "e2e"}]})
                if self.path == "/v1/models":
                    return self.answer(200, {"object": "list", "data": [
                        {"id": "tiny:1b", "object": "model", "created": 0, "owned_by": "library"},
                        {"id": "embed:latest", "object": "model", "created": 0, "owned_by": "library"}]})
                if self.path == "/api/version":
                    return self.answer(200, {"version": "0.0.0-e2e"})
                return self.answer(404, {"error": "not found"})

            def do_POST(self):
                body = json.loads(self.rfile.read(int(self.headers.get("Content-Length", 0))) or b"null")
                if self.path == "/api/show":
                    if body.get("model") == "tiny:1b":
                        return self.answer(200, {"capabilities": ["completion", "vision"], "model_info": {
                            "general.architecture": "tiny", "tiny.context_length": 8192}})
                    return self.answer(200, {"capabilities": ["embedding"], "model_info": {}})
                if self.path.endswith("/chat/completions"):
                    local.requests.append({"path": self.path, "authorization": self.headers.get("Authorization"),
                                           "body": body})
                    local.path.write_text(json.dumps(local.requests, indent=2))
                    self.send_response(200)
                    self.send_header("Content-Type", "text/event-stream")
                    self.end_headers()
                    head = {"id": "e2e", "object": "chat.completion.chunk", "created": 0, "model": body["model"]}
                    words = ANSWER.split(" ")
                    chunks = [{"choices": [{"index": 0, "delta": {"role": "assistant", "content": ""}}]}]
                    chunks += [{"choices": [{"index": 0, "delta": {"content": word + " "}}]} for word in words[:-1]]
                    chunks += [{"choices": [{"index": 0, "delta": {"content": words[-1]}}]},
                               {"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]},
                               {"choices": [], "usage": {"prompt_tokens": 12, "completion_tokens": 5,
                                                         "total_tokens": 17}}]
                    for chunk in chunks:
                        self.wfile.write(f"data: {json.dumps(dict(head, **chunk))}\n\n".encode())
                    self.wfile.write(b"data: [DONE]\n\n")
                    return None
                return self.answer(404, {"error": "not found"})

            def log_message(self, *args):
                pass

        self.server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.server.serve_forever, daemon=True).start()
        self.address = f"127.0.0.1:{self.server.server_port}"
        self.base_url = f"http://{self.address}/v1"


class Agent:
    """One `unharnessed --mode acp` process in a throwaway agent dir, with
    the catalog fixture as its catalog cache and no credential."""

    def __init__(self, name):
        self.agent_dir = OUT / name / "agent"
        work = OUT / name / "work"
        home = OUT / name / "home"
        for folder in (self.agent_dir, work, home):
            folder.mkdir(parents=True, exist_ok=True)
        (self.agent_dir / "models.json").unlink(missing_ok=True)
        cache = {"url": CATALOG_URL, "scope": "public", "fetchedAt": int(time.time() * 1000), "payload": CATALOG}
        (self.agent_dir / "models").mkdir(exist_ok=True)
        (self.agent_dir / "models" / "provider-model-catalog.v1.json").write_text(json.dumps(cache))
        extra = {"UNHARNESSED_CODING_AGENT_DIR": str(self.agent_dir), "HOME": str(home)}
        self.out = open(OUT / f"{name}.out", "w")
        command = " ".join(f"{key}={value}" for key, value in dict(extra, UNHARNESSED_OFFLINE="1").items())
        self.out.write(f"$ {command} unharnessed --mode acp\n")
        self.child = subprocess.Popen(
            [BIN, "--mode", "acp"], cwd=work, text=True, env=dict(ENV, **extra),
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=open(OUT / name / "stderr.log", "w"))
        self.work = work
        self.start = time.time()
        self.inbox = queue.Queue()
        self.answers = {}
        self.config_updates = []
        self.text = []
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
            elif update.get("sessionUpdate") == "agent_message_chunk":
                self.text.append(update["content"].get("text", ""))

    def request(self, method, params, timeout=30):
        request_id = next(self.ids)
        self._send({"jsonrpc": "2.0", "id": request_id, "method": method, "params": params})
        deadline = time.time() + timeout
        while request_id not in self.answers:
            self._pump(deadline)
        return self.answers[request_id]

    def new_session(self):
        answer = self.request("session/new", {"cwd": str(self.work), "mcpServers": []}, timeout=120)
        self.session_id = answer["result"]["sessionId"]
        return answer["result"]["configOptions"]

    def set_option(self, config_id, option_value):
        params = {"sessionId": self.session_id, "configId": config_id, "value": option_value}
        return self.request("session/set_config_option", params)

    def prompt(self, text):
        prompt = [{"type": "text", "text": text}]
        return self.request("session/prompt", {"sessionId": self.session_id, "prompt": prompt}, timeout=300)

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

    def models_json(self):
        path = self.agent_dir / "models.json"
        return json.loads(path.read_text()) if path.exists() else None

    def close(self):
        self.child.stdin.close()
        code = self.child.wait(timeout=60)
        self.out.close()
        return code


def probed(agent, address, api_key=None):
    params = {"baseUrl": address}
    if api_key is not None:
        params["apiKey"] = api_key
    return agent.request(PROBE, params)["result"]


def saved(agent, params):
    return agent.request(SAVE, params)["result"]


def rows(result):
    return {row["id"]: row for row in result["providers"] if "local" in row}


def scenario_local():
    local = Local(OUT / "local" / "requests.json")
    agent = Agent("local")
    listed = agent.request(LIST, {})["result"]
    check("list: no local server to begin with", rows(listed) == {}, rows(listed))
    check("list: and nothing connected", not any(row["connected"] for row in listed["providers"]))
    # As in the panel: the session is open before a server is added.
    options = agent.new_session()
    check("session: no model available yet", choices(options) == [current(options, "model")], choices(options))

    # What a server serves.
    server = probed(agent, local.base_url).get("server")
    check("probe: Ollama is recognized", server is not None and server["name"] == "Ollama" and server["id"] == "ollama",
          server)
    check("probe: its chat model, with its context and its image input; the embedding model left out",
          server is not None and server["models"] == [
              {"id": "tiny:1b", "contextWindow": 8192, "image": True, "reasoning": False}], server)
    root = probed(agent, local.address).get("server")
    check("probe: an address without a scheme or /v1 is the server's /v1",
          root is not None and root["baseUrl"] == local.base_url, root)
    port = free_port()
    error = probed(agent, f"http://127.0.0.1:{port}/v1").get("error")
    check("probe: nothing answering is unreachable",
          error == {"kind": "unreachable", "message": f"Nothing answers at http://127.0.0.1:{port}/v1."}, error)
    error = probed(agent, "ftp://example.com").get("error")
    check("probe: only http addresses", error is not None and error["kind"] == "invalid", error)
    error = probed(agent, f"http://{local.address}/nothing/v1").get("error")
    check("probe: a server that lists no models is not one",
          error == {"kind": "invalid",
                    "message": f"http://{local.address}/nothing/v1 answers, but lists no models (HTTP 404)."}, error)
    locked = f"http://{local.address}/locked/v1"
    error = probed(agent, locked).get("error")
    check("probe: a server that asks for a key says so",
          error == {"kind": "needsKey", "message": f"{locked} asks for an API key."}, error)
    error = probed(agent, locked, "wrong").get("error")
    check("probe: a key it refuses is told apart",
          error == {"kind": "needsKey", "message": f"{locked} refused this API key."}, error)
    guarded = probed(agent, locked, "secret").get("server")
    check("probe: with its key, its models; an unknown server is named by its address",
          guarded is not None and guarded["name"] == local.address and guarded["id"] == "local"
          and [m["id"] for m in guarded["models"]] == ["guarded"], guarded)
    answer = agent.request(PROBE, {})
    check("probe: an address is needed", "error" in answer, answer)

    # The servers at their usual addresses: whatever runs on this machine.
    servers = agent.request(SCAN, {}, timeout=60)["result"]["servers"]
    check("scan: the servers found are at the usual addresses",
          all(found["baseUrl"] in USUAL for found in servers), [found["baseUrl"] for found in servers])

    # Saved into models.json.
    since = len(agent.config_updates)
    result = saved(agent, dict(server, models=server["models"]))
    check("save: done", result["outcome"] == "done" and result["message"] == "1 model of ollama in the model menu",
          result)
    row = rows(result).get("ollama")
    check("save: the server's row, by its name, connected",
          row is not None and row["name"] == "Ollama" and row["connected"] and not row["removable"]
          and row["local"] == {"baseUrl": local.base_url, "models": ["tiny:1b"]}, row)
    written = agent.models_json()
    check("save: models.json has the provider, the chat completions API and what every local server takes",
          written == {"providers": {"ollama": {
              "name": "Ollama", "baseUrl": local.base_url, "api": "openai-completions", "apiKey": "local",
              "compat": {"supportsDeveloperRole": False, "supportsStore": False, "maxTokensField": "max_tokens"},
              "models": [{"id": "tiny:1b", "contextWindow": 8192, "maxTokens": 2048,
                          "input": ["text", "image"]}]}}}, written)
    mode = stat.S_IMODE((agent.agent_dir / "models.json").stat().st_mode)
    check("save: models.json is private to its owner", mode == 0o600, oct(mode))

    # The picker moves onto the local model, the one available.
    options = agent.config_update(lambda o: current(o, "model") == value("ollama", "tiny:1b"), since)
    check("picker: the local model comes in, and the session moves onto it",
          options is not None and choices(options) == [value("ollama", "tiny:1b")],
          [(choices(o), current(o, "model")) for o in agent.config_updates[since:]])
    options = options or agent.set_option("model", value("ollama", "tiny:1b"))["result"]["configOptions"]
    details = option(options, "model")["options"][0].get("_meta", {}).get("unharnessed", {}).get("model")
    check("session: its details: name, context, image input, no cost",
          details is not None and details["name"] == "tiny:1b" and details["contextWindow"] == 8192
          and details["input"] == ["text", "image"] and details["cost"]["input"] == 0, details)
    answer = agent.prompt("Say hello.")
    check("turn: the turn ends", answer.get("result", {}).get("stopReason") == "end_turn", answer)
    check("turn: the local model's answer streams back", "".join(agent.text).strip() == ANSWER, agent.text)
    chat = local.requests[0] if local.requests else {}
    body = chat.get("body") or {}
    check("turn: the request reaches the server's chat completions",
          chat.get("path") == "/v1/chat/completions" and body.get("model") == "tiny:1b" and body.get("stream"),
          {key: body.get(key) for key in ("model", "stream")})
    check("turn: with max_tokens, no store, and the system prompt as a system message",
          body.get("max_tokens") == 2048 and "max_completion_tokens" not in body and "store" not in body
          and body["messages"][0]["role"] == "system", {key: body.get(key) for key in ("max_tokens", "store")})
    check("turn: the stand-in key is sent", chat.get("authorization") == "Bearer local", chat.get("authorization"))

    # A second server, with its key: the picker follows.
    since = len(agent.config_updates)
    result = saved(agent, dict(guarded, apiKey="secret"))
    check("save: a server with a key", result["outcome"] == "done" and "local" in rows(result), result)
    check("save: its key is in models.json", agent.models_json()["providers"]["local"]["apiKey"] == "secret")
    options = agent.config_update(lambda o: value("local", "guarded") in choices(o), since)
    check("picker: the new server's model comes in", options is not None,
          [choices(o) for o in agent.config_updates[since:]])

    # The same server again: its entry, its models picked again.
    since = len(agent.config_updates)
    result = saved(agent, dict(server, models=[{"id": "tiny:1b"}, {"id": "extra:2b"}]))
    ollama = agent.models_json()["providers"]["ollama"]
    check("save: the same address is the same provider",
          result["outcome"] == "done" and "ollama-2" not in rows(result)
          and rows(result)["ollama"]["local"]["models"] == ["tiny:1b", "extra:2b"], rows(result))
    check("save: a model that was there keeps its definition",
          ollama["models"] == [written["providers"]["ollama"]["models"][0], {"id": "extra:2b"}], ollama["models"])
    options = agent.config_update(lambda o: value("ollama", "extra:2b") in choices(o), since)
    check("picker: the model added comes in", options is not None)

    result = saved(agent, dict(server, models=[]))
    check("save: a model is needed",
          result["outcome"] == "failed" and result["message"] == "Pick at least one model.", result)
    result = saved(agent, dict(server, providerId="anthropic"))
    check("save: only into a local server",
          result["outcome"] == "failed" and result["message"] == "No local server anthropic in models.json.", result)

    # Removed.
    since = len(agent.config_updates)
    result = agent.request(REMOVE, {"providerId": "local"})["result"]
    check("remove: done, and its row is gone", result["outcome"] == "done" and "local" not in rows(result), result)
    check("remove: out of models.json", list(agent.models_json()["providers"]) == ["ollama"], agent.models_json())
    options = agent.config_update(lambda o: value("local", "guarded") not in choices(o), since)
    check("picker: its model leaves", options is not None)
    result = agent.request(REMOVE, {"providerId": "openai"})["result"]
    check("remove: only a local server",
          result["outcome"] == "failed" and result["message"] == "No local server openai in models.json.", result)

    # A models.json that does not parse is left as it is.
    path = agent.agent_dir / "models.json"
    kept = path.read_text()
    path.write_text("{ not json")
    result = saved(agent, dict(guarded, apiKey="secret"))
    check("save: refused on a models.json that is not JSON",
          result["outcome"] == "failed" and result["message"].startswith("models.json is not valid JSON"), result)
    check("save: which is left untouched", path.read_text() == "{ not json")
    path.write_text(kept)
    check("local: the agent exits when stdin closes", agent.close() == 0)


for scenario in (scenario_local,):
    try:
        scenario()
    except Exception as error:  # a crash fails the scenario, the others still run
        check(scenario.__name__, False, f"{type(error).__name__}: {error}")

print(f"\n{'FAILED: ' + ', '.join(FAILED) if FAILED else 'all checks passed'}")
sys.exit(1 if FAILED else 0)
