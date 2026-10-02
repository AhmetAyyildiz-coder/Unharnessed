#!/bin/sh
# Unharnessed merge gate, isolated from the installed Prime Agent.
#
# usage: scripts/unharnessed-check.sh [gate-dir]
#
# Runs the `make check` steps (fmt, clippy -D warnings, the workspace tests,
# the release build) plus the kernel runtime's Python tests, and compares the
# failing tests to UNHARNESSED_BASELINE.txt. Exits non-zero when a step fails
# or a test fails outside that list. The tests run with --no-fail-fast so one
# baseline failure cannot hide the binaries after it.
#
# Isolation (UNHARNESSED_PLAN.md, "Incident"): the tests run under a
# throwaway HOME in <gate-dir>, the kernel venv is built there from this
# checkout's runtime, the package dir points at the repo, any installed
# prime-agent leaves the PATH, and processes the tests leave behind are
# killed. The real ~/.prime is only read: the gate fails if its kernel venv
# changed, and lists anything under it modified during the run (the
# installed daemon may write there on its own).
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
GATE=${1:-${TMPDIR:-/tmp}/unharnessed-gate}
mkdir -p "$GATE/home"
GATE=$(cd "$GATE" && pwd -P)
LOG="$GATE/check.log"
BASELINE="$ROOT/UNHARNESSED_BASELINE.txt"
REAL_HOME=$HOME
REAL_VENV="$REAL_HOME/.prime/agent/kernel-venv"
BIN="$ROOT/target/release/prime-agent"
VENV="$GATE/kernel-venv"
RUN_ID="gate-$$-$(date +%s)"
FAILED=0
: > "$LOG"

step() { # step <name> <command...>: run from the repo root, log the output
    name=$1; shift
    echo "== $name: $*" | tee -a "$LOG"
    if (cd "$ROOT" && "$@") >> "$LOG" 2>&1; then
        echo "PASS  $name"
    else
        echo "FAIL  $name (see $LOG)"
        FAILED=1
    fi
}

mtime() { stat -f %m "$1" 2>/dev/null || echo absent; }

avail_kb=$(df -k "$ROOT" | awk 'NR == 2 { print $4 }')
if [ "$avail_kb" -lt $((15 * 1024 * 1024)) ]; then
    echo "STOP  less than 15 GB free on the repo's disk ($((avail_kb / 1024 / 1024)) GB)"
    exit 2
fi

# Environment: real toolchains and caches, throwaway HOME.
export CARGO_HOME=${CARGO_HOME:-$REAL_HOME/.cargo}
export RUSTUP_HOME=${RUSTUP_HOME:-$REAL_HOME/.rustup}
export UV_CACHE_DIR=${UV_CACHE_DIR:-$REAL_HOME/.cache/uv}
export UV_PYTHON_INSTALL_DIR=${UV_PYTHON_INSTALL_DIR:-$REAL_HOME/.local/share/uv/python}
[ -f "$REAL_HOME/.gitconfig" ] && export GIT_CONFIG_GLOBAL="$REAL_HOME/.gitconfig"
export HOME="$GATE/home"
export PRIME_AGENT_KERNEL_VENV="$VENV"
export PI_PACKAGE_DIR="$ROOT"
export PA_CORE_KERNEL_PYTHON="$VENV/bin/python"
export DO_NOT_TRACK=1 CARGO_TERM_COLOR=never
CLEAN_PATH=
old_ifs=$IFS; IFS=:
for dir in $PATH; do
    if [ -x "$dir/prime-agent" ] || [ -x "$dir/unharnessed" ]; then
        echo "note: $dir left out of PATH (it holds an installed agent binary)" >> "$LOG"
        continue
    fi
    CLEAN_PATH=${CLEAN_PATH:+$CLEAN_PATH:}$dir
done
IFS=$old_ifs
export PATH="$CLEAN_PATH"

venv_before=$(mtime "$REAL_VENV")
touch "$GATE/started"

step fmt cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step build-release cargo build --release --workspace
step kernel-venv "$BIN" --prime-agent-bootstrap
echo "== test: cargo test --workspace --no-fail-fast" | tee -a "$LOG"
(cd "$ROOT" && env UNHARNESSED_GATE_RUN="$RUN_ID" cargo test --workspace --no-fail-fast) > "$GATE/test.log" 2>&1
cat "$GATE/test.log" >> "$LOG"
echo "== python: unittest discover (prime-agent-runtime/test)" | tee -a "$LOG"
(cd "$ROOT/prime-agent-runtime" && env UNHARNESSED_GATE_RUN="$RUN_ID" PYTHONPATH=src \
    "$VENV/bin/python" -m unittest discover -s test) > "$GATE/python.log" 2>&1
cat "$GATE/python.log" >> "$LOG"

# Leftover processes: anything carrying this run's marker in its
# environment, or whose command runs from this checkout's target dir or the
# gate dir, or the bash() status shells. The installed release is never
# matched. (The gate's own environment names the gate dir, hence the marker
# for the environment match and command lines only for the paths.)
leftovers=$( { ps -AEww -o pid=,command= | grep -e "UNHARNESSED_GATE_RUN=$RUN_ID"
    ps -Aww -o pid=,command= | grep -e "$ROOT/target/" -e "$GATE/" -e '_prime_agent_gate'; } \
    | grep -v -e '\.local/share/prime-agent' -e ' grep ' | awk '{ print $1 }' | sort -u)
for pid in $leftovers; do
    { [ "$pid" = "$$" ] || [ "$pid" = "$PPID" ]; } && continue
    echo "killed leftover: $(ps -ww -o pid=,command= -p "$pid")" | tee -a "$LOG"
    kill -9 "$pid" 2>/dev/null
done

# Failing tests, as "<test binary> :: <test>" like the baseline.
awk '
    /^ *Running / {
        bin = $0
        sub(/.*\//, "", bin); sub(/\)$/, "", bin); sub(/-[0-9a-f]+$/, "", bin)
    }
    /^ *Doc-tests / { bin = "doc-tests " $2 }
    /^test .* \.\.\. FAILED$/ {
        name = $0; sub(/^test /, "", name); sub(/ \.\.\. FAILED$/, "", name)
        print bin " :: " name
    }
    /^error: could not compile/ { print "COMPILE :: " $0 }
    /process didn.t exit successfully: .*\(signal: / { print "CRASH :: " bin }
' "$GATE/test.log" > "$GATE/failures.txt"
grep -E '^(FAIL|ERROR): ' "$GATE/python.log" | sed 's/^/py :: /' >> "$GATE/failures.txt"
if ! grep -q '^test result: ' "$GATE/test.log"; then
    echo "COMPILE :: cargo test ran no test binary" >> "$GATE/failures.txt"
fi
grep -v '^#' "$BASELINE" | sort -u > "$GATE/baseline.txt"
sort -u "$GATE/failures.txt" > "$GATE/failures.sorted"
comm -23 "$GATE/failures.sorted" "$GATE/baseline.txt" > "$GATE/new-failures.txt"
echo "test failures: $(wc -l < "$GATE/failures.sorted" | tr -d ' ') (all listed in $GATE/failures.sorted)"
if [ -s "$GATE/new-failures.txt" ]; then
    echo "FAIL  tests: failures outside the baseline:"
    sed 's/^/      /' "$GATE/new-failures.txt"
    FAILED=1
else
    echo "PASS  tests: every failure is in the baseline"
    sed 's/^/      /' "$GATE/failures.sorted"
fi

venv_after=$(mtime "$REAL_VENV")
if [ "$venv_before" = "$venv_after" ]; then
    echo "PASS  $REAL_VENV untouched (mtime $venv_after)"
else
    echo "FAIL  $REAL_VENV changed (mtime $venv_before -> $venv_after)"
    FAILED=1
fi
if [ -d "$REAL_HOME/.prime" ]; then
    touched=$(find "$REAL_HOME/.prime" -newer "$GATE/started" 2>/dev/null)
    if [ -n "$touched" ]; then
        echo "note: modified under ~/.prime during the run (installed daemon?):"
        echo "$touched" | sed 's/^/      /'
    fi
fi
exit $FAILED
