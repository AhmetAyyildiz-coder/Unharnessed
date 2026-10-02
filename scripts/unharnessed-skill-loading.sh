#!/bin/sh
# Unharnessed step 7 check (UNHARNESSED_PLAN.md): skill loading works
# without the package manager. Drives the real binary's `prompt --json`
# dump (no model call) over an isolated agent dir and project, and prints
# each checked source with its outcome.
#
# usage: scripts/unharnessed-skill-loading.sh <out-dir>
set -u
OUT=${1:?usage: unharnessed-skill-loading.sh <out-dir>}
ROOT=$(cd "$(dirname "$0")/.." && pwd)
BIN=${UNHARNESSED_BIN:-$ROOT/target/release/unharnessed}
rm -rf "$OUT" && mkdir -p "$OUT"
OUT=$(cd "$OUT" && pwd)
AGENT="$OUT/agent"
PROJECT="$OUT/project"
FAILED=0

skill() { # skill <dir> <name>
    mkdir -p "$1/$2"
    printf -- '---\nname: %s\ndescription: %s test skill\n---\nBody\n' "$2" "$2" > "$1/$2/SKILL.md"
}

skill "$AGENT/skills" user-auto-skill                 # user auto-discovery
skill "$PROJECT/.unharnessed/skills" project-auto-skill # project auto-discovery
skill "$PROJECT/.agents/skills" agents-dir-skill      # .agents/skills ancestor scan
skill "$OUT/extra" settings-array-skill               # settings `skills` array
skill "$OUT/pkg/skills" package-skill                 # removed `packages` setting
mkdir -p "$PROJECT/.git"
printf '{"skills": ["../extra/settings-array-skill"], "packages": ["../pkg"]}\n' > "$AGENT/settings.json"

echo "\$ unharnessed prompt --json --cwd $PROJECT" > "$OUT/prompt.out"
UNHARNESSED_CODING_AGENT_DIR="$AGENT" "$BIN" prompt --json --cwd "$PROJECT" >> "$OUT/prompt.out" 2> "$OUT/prompt.err"
code=$?

check() { # check <name> <expect: present|absent>
    if grep -q "<name>$1</name>" "$OUT/prompt.out"; then found=present; else found=absent; fi
    if [ "$code" -eq 0 ] && [ "$found" = "$2" ]; then
        echo "PASS  $1 $2"
    else
        echo "FAIL  $1 (exit $code, expected $2, got $found)"
        FAILED=1
    fi
}

check user-auto-skill present
check project-auto-skill present
check agents-dir-skill present
check settings-array-skill present
check goal present          # bundled skill
check package-skill absent  # the `packages` setting is no longer read
if grep -q '"packages"' "$AGENT/settings.json"; then
    echo "PASS  settings.json keeps the packages entry"
else
    echo "FAIL  settings.json lost the packages entry"
    FAILED=1
fi
exit $FAILED
