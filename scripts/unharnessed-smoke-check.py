#!/usr/bin/env python3
"""Check a smoke transcript against what the kernel actually returned.

usage:
  unharnessed-smoke-check.py json <transcript> [--forbid-code REGEX] PATTERN...
  unharnessed-smoke-check.py acp <transcript> CODE_SUBSTRING

json: a `--mode json` transcript (first line: the command run). Passes when
every PATTERN occurs in the text of successful ipython results
(`tool_execution_end`, `details.status == "ok"`), never in the prompt or the
model's prose. With --forbid-code, fails when an ipython call's code matches
REGEX (a resumed run must read a value, not recompute it).

acp: an ACP transcript. Passes when an ipython call whose code contains
CODE_SUBSTRING reached status "completed".
"""
import json
import re
import sys


def frames(path: str) -> list:
    with open(path) as transcript:
        lines = transcript.read().splitlines()[1:]
    return [json.loads(line) for line in lines if line.strip().startswith("{")]


def check_json(path: str, args: list) -> str | None:
    forbid = None
    if args[:1] == ["--forbid-code"]:
        forbid, args = re.compile(args[1]), args[2:]
    results = []
    for event in frames(path):
        if event.get("toolName") != "ipython":
            continue
        if event.get("type") == "tool_execution_start" and forbid:
            code = (event.get("args") or {}).get("code", "")
            if forbid.search(code):
                return f"an ipython call recomputes the value: {code!r}"
        if event.get("type") == "tool_execution_end":
            result = event.get("result") or {}
            if (result.get("details") or {}).get("status") == "ok":
                results.extend(block.get("text", "") for block in result.get("content", []))
    if not results:
        return "no successful ipython result"
    output = "\n".join(results)
    missing = [pattern for pattern in args if pattern not in output]
    return f"not in the ipython results: {missing}" if missing else None


def check_acp(path: str, code: str) -> str | None:
    calls = set()
    for frame in frames(path):
        update = (frame.get("params") or {}).get("update") or {}
        if update.get("sessionUpdate") == "tool_call":
            if code in ((update.get("rawInput") or {}).get("code") or ""):
                calls.add(update.get("toolCallId"))
        elif update.get("sessionUpdate") == "tool_call_update" and update.get("toolCallId") in calls:
            if update.get("status") == "completed":
                return None
    return f"no completed ipython call running {code!r}"


def main() -> int:
    mode, path, *args = sys.argv[1:]
    error = check_json(path, args) if mode == "json" else check_acp(path, args[0])
    if error:
        print(error, file=sys.stderr)
        return 1
    return 0


sys.exit(main())
