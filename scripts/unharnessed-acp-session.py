#!/usr/bin/env python3
"""Drive one ACP session end to end for the Unharnessed smoke tests.

usage: unharnessed-acp-session.py <binary> <cwd> <session-dir> <model> <prompt>

Runs initialize -> session/new -> session/prompt -> session/close over
`--mode acp` and prints every frame the agent sends (one JSON per line), so
the smoke script can grep the transcript. Exits non-zero when a request
fails or times out.
"""
import json
import subprocess
import sys
import threading
import queue

TIMEOUT_S = 300

binary, cwd, session_dir, model, prompt = sys.argv[1:6]
child = subprocess.Popen(
    [binary, "--mode", "acp", "--session-dir", session_dir, "--model", model],
    cwd=cwd,
    stdin=subprocess.PIPE,
    stdout=subprocess.PIPE,
    text=True,
)
frames: "queue.Queue[dict]" = queue.Queue()


def read_frames() -> None:
    for line in child.stdout:
        line = line.strip()
        if line:
            print(line, flush=True)
            frames.put(json.loads(line))


threading.Thread(target=read_frames, daemon=True).start()
next_id = 0


def request(method: str, params: dict) -> dict:
    global next_id
    next_id += 1
    child.stdin.write(json.dumps({"jsonrpc": "2.0", "id": next_id, "method": method, "params": params}) + "\n")
    child.stdin.flush()
    while True:
        frame = frames.get(timeout=TIMEOUT_S)
        if frame.get("id") == next_id and "method" not in frame:
            if "error" in frame:
                sys.exit(f"{method} failed: {frame['error']}")
            return frame["result"]


try:
    request("initialize", {"protocolVersion": 1, "clientCapabilities": {}})
    session_id = request("session/new", {"cwd": cwd, "mcpServers": []})["sessionId"]
    request("session/prompt", {"sessionId": session_id, "prompt": [{"type": "text", "text": prompt}]})
    request("session/close", {"sessionId": session_id})
finally:
    child.stdin.close()
    child.wait(timeout=60)
