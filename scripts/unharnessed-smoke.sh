#!/bin/sh
# Unharnessed smoke tests (UNHARNESSED_PLAN.md, "Tests de fumée"): drive the
# real binary end to end and print each command with its checked outcome.
#
# usage: scripts/unharnessed-smoke.sh <out-dir> [model]
#
# Isolation: sessions and the kernel venv live under <out-dir>, so the
# installed prime-agent's venv (~/.prime/agent/kernel-venv) is never rebuilt
# with this checkout's runtime. Credentials are read from the real agent dir
# (a copy would rotate OAuth refresh tokens away from it). Telemetry is off.
set -u
OUT=${1:?usage: unharnessed-smoke.sh <out-dir> [model]}
MODEL=${2:-anthropic/claude-haiku-4-5}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${PRIME_AGENT_BIN:-$ROOT/target/release/prime-agent}
mkdir -p "$OUT/sessions" "$OUT/work"
OUT=$(cd "$OUT" && pwd)
export PRIME_AGENT_KERNEL_VENV="$OUT/kernel-venv"
export DO_NOT_TRACK=1 PRIME_AGENT_TELEMETRY=0
WORK="$OUT/work"
FAILED=0

check() { # check <name> <command-exit> <grep-pattern> <file>
    if [ "$2" -eq 0 ] && grep -q -- "$3" "$4"; then
        echo "PASS  $1"
    else
        echo "FAIL  $1 (exit $2, pattern '$3' in $4)"
        FAILED=1
    fi
}

run() { # run <name> <args...>: stdout in $OUT/<name>.out (after the command line), stderr in .err
    name=$1; shift
    echo "\$ prime-agent $*" > "$OUT/$name.out"
    (cd "$WORK" && perl -e 'alarm shift; exec @ARGV' 600 "$BIN" "$@") >> "$OUT/$name.out" 2> "$OUT/$name.err"
}

# 1. The model computes in the kernel through the ipython tool.
run ipython --mode json --session-dir "$OUT/sessions" --model "$MODEL" \
    "Compute 2**100 with your ipython tool and answer with the number only."
check "ipython tool call" $? '"name":"ipython"' "$OUT/ipython.out"
check "2**100 result" 0 '1267650600228229401496703205376' "$OUT/ipython.out"

# 2. --mode json emits one valid JSON value per line.
if python3 -c "import json,sys; [json.loads(l) for l in open(sys.argv[1]).read().splitlines()[1:] if l.strip()]" "$OUT/ipython.out"; then
    echo "PASS  json mode lines parse"
else
    echo "FAIL  json mode lines parse"; FAILED=1
fi

# 3. The kernel's runtime API: bash, goal, compact, mcp.
run runtime_api --mode json --session-dir "$OUT/sessions" --model "$MODEL" \
    "In ONE ipython cell run exactly: r = bash('echo smoke-bash-ok'); print(r.output); print('goal:', await goal.get()); print('compact:', await compact.status()); print('mcp:', type(await mcp.list_connections()).__name__). Then reply DONE."
rc=$?
check "bash() in the kernel" $rc 'smoke-bash-ok' "$OUT/runtime_api.out"
check "goal.get()" $rc 'goal:' "$OUT/runtime_api.out"
check "compact.status()" $rc 'compact:' "$OUT/runtime_api.out"
check "mcp.list_connections()" $rc 'mcp:' "$OUT/runtime_api.out"

# 4. A kernel variable survives a session resume (kernel state snapshot):
# an ACP session (the VS Code transport) sets it and closes, then a print
# run continues that session and reads it back.
echo "\$ unharnessed-acp-session.py (sets smoke_var, then session/close)" > "$OUT/state_set.out"
python3 "$ROOT/scripts/unharnessed-acp-session.py" "$BIN" "$WORK" "$OUT/sessions" "$MODEL" \
    "Run exactly this in your ipython tool: smoke_var = 'kept-' + str(6*7). Then reply OK." \
    >> "$OUT/state_set.out" 2> "$OUT/state_set.err"
check "acp session sets the variable" $? '"stopReason":"end_turn"' "$OUT/state_set.out"
run state_get -p -c --session-dir "$OUT/sessions" --model "$MODEL" \
    "Without redefining it, print the existing Python variable smoke_var with your ipython tool and reply with its value."
rc=$?
if [ $rc -eq 0 ] && grep -q 'kept-42' "$OUT/state_get.out" && ! grep -qi 'not defined\|not persist' "$OUT/state_get.out"; then
    echo "PASS  kernel variable survives resume"
else
    echo "FAIL  kernel variable survives resume (see $OUT/state_get.out)"; FAILED=1
fi

# 5. ACP: the initialize handshake answers.
echo '$ prime-agent --mode acp  <<< initialize' > "$OUT/acp.out"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}' \
    | (cd "$WORK" && perl -e 'alarm shift; exec @ARGV' 60 "$BIN" --mode acp --session-dir "$OUT/sessions") >> "$OUT/acp.out" 2> "$OUT/acp.err"
check "acp initialize" 0 '"protocolVersion"' "$OUT/acp.out"

# 6. An AGENTS.md in the working directory reaches the system prompt.
printf '# Project rules\nsmoke-agents-md-marker\n' > "$WORK/AGENTS.md"
run agents_md prompt --cwd "$WORK"
check "AGENTS.md in the system prompt" $? 'smoke-agents-md-marker' "$OUT/agents_md.out"
rm -f "$WORK/AGENTS.md"

exit $FAILED
