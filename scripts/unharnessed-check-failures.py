#!/usr/bin/env python3
"""The failing tests of a gate run, one per line, in the format of
UNHARNESSED_BASELINE.txt (scripts/unharnessed-check.sh calls it).

usage: unharnessed-check-failures.py <cargo-log> <cargo-exit> <python-log> <python-exit>

  <binary> :: <test>        a failed Rust test
  COMPILE :: <cargo error>  a test target that does not compile, or no
                            test binary ran at all
  CRASH :: <binary>         a test binary killed by a signal
  TARGET :: <cargo target>  a target cargo reports failed with no failed
                            test or crash to explain it (an exit status
                            without FAILED lines)
  CARGO :: exit status N    cargo failed and named no failure
  py :: FAIL: ... / ERROR: ...
  py :: exit status N       unittest failed without naming a test
  py :: no test ran

The exit codes are checked against the logs, so a suite cannot fail
without leaving a line here.
"""
import re
import sys

cargo_log, cargo_exit, python_log, python_exit = sys.argv[1:5]
cargo_exit, python_exit = int(cargo_exit), int(python_exit)
cargo_text = open(cargo_log, encoding="utf-8", errors="replace").read()
python_text = open(python_log, encoding="utf-8", errors="replace").read()


def key(name):
    """Cargo writes `-` in target names as `_` in the binaries' names."""
    return name.replace("-", "_")


failures = []
explained = set()  # binaries with a failed test or a crash
binary = "?"
targets = []  # failed targets as cargo names them: `-p uh-cli --test foo`
for line in cargo_text.splitlines():
    if match := re.match(r"\s+Running .*\((?:.*/)?([^/)]+?)(?:-[0-9a-f]{16})?\)$", line):
        binary = match.group(1)
    elif match := re.match(r"\s+Doc-tests (\S+)$", line):
        binary = f"doc-tests {match.group(1)}"
    elif match := re.match(r"test (.+) \.\.\. FAILED$", line):
        failures.append(f"{binary} :: {match.group(1)}")
        explained.add(key(binary))
    elif line.startswith("error: could not compile"):
        failures.append(f"COMPILE :: {line}")
    elif match := re.search(r"process didn't exit successfully: `(?:[^`]*/)?([^/`\s]+?)(?:-[0-9a-f]{16})?(?: [^`]*)?` \(signal: ", line):
        failures.append(f"CRASH :: {match.group(1)}")
        explained.add(key(match.group(1)))
    if match := re.search(r"failed, to rerun pass `([^`]+)`", line):
        targets.append(match.group(1))
    elif match := re.match(r"\s+`(--?[^`]+)`$", line):
        targets.append(match.group(1))

for target in dict.fromkeys(targets):
    words = target.split()
    package = words[words.index("-p") + 1] if "-p" in words else ""
    if "--doc" in words:
        name = f"doc-tests {key(package)}"
    elif "--lib" in words:
        name = package
    else:
        kinds = [i for i, word in enumerate(words) if word in ("--test", "--bin", "--example", "--bench")]
        name = words[kinds[0] + 1] if kinds and kinds[0] + 1 < len(words) else ""
    if not name or key(name) not in explained:
        failures.append(f"TARGET :: {target}")
if cargo_exit != 0 and not failures:
    failures.append(f"CARGO :: exit status {cargo_exit}")
if not re.search(r"^test result: ", cargo_text, re.M):
    failures.append("COMPILE :: cargo test ran no test binary")

python_failures = [f"py :: {line}" for line in python_text.splitlines() if re.match(r"(FAIL|ERROR): ", line)]
failures += python_failures
ran = re.search(r"^Ran (\d+) tests? in ", python_text, re.M)
if not ran or int(ran.group(1)) == 0:
    failures.append("py :: no test ran")
if python_exit != 0 and not python_failures:
    failures.append(f"py :: exit status {python_exit}")

for failure in failures:
    print(failure)
