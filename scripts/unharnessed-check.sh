#!/bin/sh
# Unharnessed merge gate, isolated from your environment.
#
# usage: scripts/unharnessed-check.sh [gate-dir]
#
# Runs the `make check` steps (fmt, clippy -D warnings, the workspace tests,
# the release build) plus the kernel runtime's Python tests, and compares the
# failing tests to UNHARNESSED_BASELINE.txt. Exits non-zero when a step fails
# or a test fails outside that list. The tests run with --no-fail-fast so one
# baseline failure cannot hide the binaries after it, and both suites' exit
# codes are checked against their logs (scripts/unharnessed-check-failures.py):
# a suite that fails without naming a test fails the gate.
#
# Isolation: the tests run under a throwaway HOME in <gate-dir>, the
# agent's variables inherited from the caller (UNHARNESSED_*, UH_*, XDG_*)
# are unset, the kernel venv is built there from this checkout's runtime,
# the package dir points at the repo, any installed unharnessed leaves the
# PATH, and processes the tests leave behind are killed. The real ~/.prime
# and ~/.unharnessed are never written: the gate fails if either kernel
# venv changed, and lists anything under them modified during the run (an
# installed daemon may write there on its own).
set -u
ROOT=$(cd "$(dirname "$0")/.." && pwd -P)
GATE=${1:-${TMPDIR:-/tmp}/unharnessed-gate}
mkdir -p "$GATE/home"
GATE=$(cd "$GATE" && pwd -P)
LOG="$GATE/check.log"
BASELINE="$ROOT/UNHARNESSED_BASELINE.txt"
REAL_HOME=$HOME
REAL_VENVS="$REAL_HOME/.prime/agent/kernel-venv $REAL_HOME/.unharnessed/kernel-venv"
BIN="$ROOT/target/release/unharnessed"
VENV="$GATE/kernel-venv"
RUN_ID="gate-$$-$(date +%s)"
FAILED=0
: > "$LOG"

# An inherited agent dir, session dir, kernel python or XDG dir would steer
# the tests to real data or another kernel: the gate sets its own below.
for name in $(env | sed -n 's/^\([A-Za-z_][A-Za-z0-9_]*\)=.*/\1/p' \
    | grep -E '^(UNHARNESSED|UH|XDG)_'); do
    echo "note: inherited $name unset" | tee -a "$LOG"
    unset "$name"
done

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
export UNHARNESSED_KERNEL_VENV="$VENV"
export UNHARNESSED_PACKAGE_DIR="$ROOT"
export UH_CORE_KERNEL_PYTHON="$VENV/bin/python"
export DO_NOT_TRACK=1 CARGO_TERM_COLOR=never
CLEAN_PATH=
old_ifs=$IFS; IFS=:
for dir in $PATH; do
    if [ -x "$dir/unharnessed" ]; then
        echo "note: $dir left out of PATH (it holds an installed agent binary)" >> "$LOG"
        continue
    fi
    CLEAN_PATH=${CLEAN_PATH:+$CLEAN_PATH:}$dir
done
IFS=$old_ifs
export PATH="$CLEAN_PATH"

venvs_before=$(for venv in $REAL_VENVS; do mtime "$venv"; done)
touch "$GATE/started"

step fmt cargo fmt --all --check
step clippy cargo clippy --workspace --all-targets -- -D warnings
step build-release cargo build --release --workspace
step kernel-venv "$BIN" --unharnessed-bootstrap
echo "== test: cargo test --workspace --no-fail-fast" | tee -a "$LOG"
(cd "$ROOT" && env UNHARNESSED_GATE_RUN="$RUN_ID" cargo test --workspace --no-fail-fast) > "$GATE/test.log" 2>&1
cargo_rc=$?
cat "$GATE/test.log" >> "$LOG"
echo "== python: unittest discover (unharnessed-runtime/test)" | tee -a "$LOG"
(cd "$ROOT/unharnessed-runtime" && env UNHARNESSED_GATE_RUN="$RUN_ID" PYTHONPATH=src \
    "$VENV/bin/python" -m unittest discover -s test) > "$GATE/python.log" 2>&1
python_rc=$?
echo "exit codes: cargo test $cargo_rc, unittest $python_rc" | tee -a "$LOG"
cat "$GATE/python.log" >> "$LOG"

# Leftover processes: anything carrying this run's marker in its
# environment, or whose command runs from this checkout's target dir or the
# gate dir, or the bash() status shells. The installed release is never
# matched. (The gate's own environment names the gate dir, hence the marker
# for the environment match and command lines only for the paths.)
leftovers=$( { ps -AEww -o pid=,command= | grep -e "UNHARNESSED_GATE_RUN=$RUN_ID"
    ps -Aww -o pid=,command= | grep -e "$ROOT/target/" -e "$GATE/" -e '_unharnessed_gate'; } \
    | grep -v -e ' grep ' | awk '{ print $1 }' | sort -u)
for pid in $leftovers; do
    { [ "$pid" = "$$" ] || [ "$pid" = "$PPID" ]; } && continue
    echo "killed leftover: $(ps -ww -o pid=,command= -p "$pid")" | tee -a "$LOG"
    kill -9 "$pid" 2>/dev/null
done

# Failing tests, as "<test binary> :: <test>" like the baseline. A parser
# failure must not read as an empty list.
if ! python3 "$ROOT/scripts/unharnessed-check-failures.py" "$GATE/test.log" "$cargo_rc" \
    "$GATE/python.log" "$python_rc" > "$GATE/failures.txt"; then
    echo "PARSER :: scripts/unharnessed-check-failures.py failed" >> "$GATE/failures.txt"
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

venvs_after=$(for venv in $REAL_VENVS; do mtime "$venv"; done)
if [ "$venvs_before" = "$venvs_after" ]; then
    echo "PASS  real kernel venvs untouched ($(echo $venvs_after))"
else
    echo "FAIL  a real kernel venv changed ($(echo $venvs_before) -> $(echo $venvs_after))"
    FAILED=1
fi
for dir in "$REAL_HOME/.prime" "$REAL_HOME/.unharnessed"; do
    [ -d "$dir" ] || continue
    touched=$(find "$dir" -newer "$GATE/started" 2>/dev/null)
    if [ -n "$touched" ]; then
        echo "note: modified under $dir during the run (installed daemon?):"
        echo "$touched" | sed 's/^/      /'
    fi
done
exit $FAILED
