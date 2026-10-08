#!/bin/sh
# Unharnessed smoke tests (UNHARNESSED_PLAN.md, "Tests de fumée"): drive the
# real binary end to end and print each command with its checked outcome.
#
# usage: scripts/unharnessed-smoke.sh <out-dir> [model]
#
# Isolation: sessions and the kernel venv live under <out-dir>, so the
# agent dir's own kernel venv is never rebuilt with this checkout's runtime. Credentials are read from the real agent dir
# (a copy would rotate OAuth refresh tokens away from it). Telemetry is off.
#
# Kernel checks read only what the kernel returned (successful ipython
# results, scripts/unharnessed-smoke-check.py): never the prompt, which
# names the expected functions, nor the model's prose.
set -u
OUT=${1:?usage: unharnessed-smoke.sh <out-dir> [model]}
MODEL=${2:-anthropic/claude-haiku-4-5}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${UNHARNESSED_BIN:-$ROOT/target/release/unharnessed}
mkdir -p "$OUT/sessions" "$OUT/work"
OUT=$(cd "$OUT" && pwd)
export UNHARNESSED_KERNEL_VENV="$OUT/kernel-venv"
export DO_NOT_TRACK=1
WORK="$OUT/work"
CHECK="$ROOT/scripts/unharnessed-smoke-check.py"
FAILED=0

check() { # check <name> <command-exit> <grep-pattern> <file>
    if [ "$2" -eq 0 ] && grep -q -- "$3" "$4"; then
        echo "PASS  $1"
    else
        echo "FAIL  $1 (exit $2, pattern '$3' in $4)"
        FAILED=1
    fi
}

kernel_check() { # kernel_check <name> <command-exit> <json|acp> <transcript> <check args...>
    name=$1 rc=$2; shift 2
    if [ "$rc" -eq 0 ] && python3 "$CHECK" "$@" 2> "$OUT/check.err"; then
        echo "PASS  $name"
    else
        echo "FAIL  $name (exit $rc: $(cat "$OUT/check.err"))"
        FAILED=1
    fi
}

run() { # run <name> <args...>: stdout in $OUT/<name>.out (after the command line), stderr in .err
    name=$1; shift
    echo "\$ unharnessed $*" > "$OUT/$name.out"
    (cd "$WORK" && perl -e 'alarm shift; exec @ARGV' 600 "$BIN" "$@") >> "$OUT/$name.out" 2> "$OUT/$name.err"
}

# 1. The model computes in the kernel through the ipython tool.
run ipython --mode json --session-dir "$OUT/sessions" --model "$MODEL" \
    "Compute 2**100 with your ipython tool and answer with the number only."
kernel_check "2**100 computed in the kernel" $? json "$OUT/ipython.out" '1267650600228229401496703205376'

# 2. --mode json emits one valid JSON value per line.
if python3 -c "import json,sys; [json.loads(l) for l in open(sys.argv[1]).read().splitlines()[1:] if l.strip()]" "$OUT/ipython.out"; then
    echo "PASS  json mode lines parse"
else
    echo "FAIL  json mode lines parse"; FAILED=1
fi

# 3. The kernel's runtime API: bash, goal, mcp. Each expected
# string only exists once the call ran (smoke-42, the returned dict keys).
run runtime_api --mode json --session-dir "$OUT/sessions" --model "$MODEL" \
    'In ONE ipython cell run exactly: r = await bash("echo smoke-$((6*7))"); print("bash:", r.exit_code, r.output.strip()); print("goal:", sorted(await goal.get())); print("mcp:", type(await mcp.list_connections()).__name__). Then reply DONE.'
rc=$?
kernel_check "bash() in the kernel" $rc json "$OUT/runtime_api.out" 'bash: 0 smoke-42'
kernel_check "goal.get()" $rc json "$OUT/runtime_api.out" "'remaining_tokens'"
kernel_check "mcp.list_connections()" $rc json "$OUT/runtime_api.out" 'mcp: list'

# 4. A kernel variable survives a session resume (kernel state snapshot):
# an ACP session (the VS Code transport) sets it and closes, then a print
# run continues that session and reads it back.
echo "\$ unharnessed-acp-session.py (sets smoke_var, then session/close)" > "$OUT/state_set.out"
python3 "$ROOT/scripts/unharnessed-acp-session.py" "$BIN" "$WORK" "$OUT/sessions" "$MODEL" \
    "Run exactly this in your ipython tool: smoke_var = 'kept-' + str(6*7). Then reply OK." \
    >> "$OUT/state_set.out" 2> "$OUT/state_set.err"
kernel_check "acp session sets the variable" $? acp "$OUT/state_set.out" 'smoke_var'
run state_get -p -c --mode json --session-dir "$OUT/sessions" --model "$MODEL" \
    "Without redefining it, print the existing Python variable smoke_var with your ipython tool and reply with its value."
kernel_check "kernel variable survives resume" $? json "$OUT/state_get.out" \
    --forbid-code 'smoke_var\s*=(?!=)' 'kept-42'

# 4b. The same through print runs only: `-p` sets it, `-p -c` reads it.
mkdir -p "$WORK/print-resume"
run print_set -p --session-dir "$OUT/sessions" --cwd "$WORK/print-resume" --model "$MODEL" \
    "Run exactly this in your ipython tool: smoke_print_var = 'print-' + str(6*7). Then reply OK."
run print_get -p -c --mode json --session-dir "$OUT/sessions" --cwd "$WORK/print-resume" --model "$MODEL" \
    "Without redefining it, print the existing Python variable smoke_print_var with your ipython tool and reply with its value."
kernel_check "kernel variable survives a print resume" $? json "$OUT/print_get.out" \
    --forbid-code 'smoke_print_var\s*=(?!=)' 'print-42'

# 5. ACP: the initialize handshake answers.
echo '$ unharnessed --mode acp  <<< initialize' > "$OUT/acp.out"
printf '%s\n' '{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":1,"clientCapabilities":{}}}' \
    | (cd "$WORK" && perl -e 'alarm shift; exec @ARGV' 60 "$BIN" --mode acp --session-dir "$OUT/sessions") >> "$OUT/acp.out" 2> "$OUT/acp.err"
check "acp initialize" 0 '"protocolVersion"' "$OUT/acp.out"

# 6. An AGENTS.md in the working directory reaches the system prompt.
printf '# Project rules\nsmoke-agents-md-marker\n' > "$WORK/AGENTS.md"
run agents_md prompt --cwd "$WORK"
check "AGENTS.md in the system prompt" $? 'smoke-agents-md-marker' "$OUT/agents_md.out"
rm -f "$WORK/AGENTS.md"

exit $FAILED
