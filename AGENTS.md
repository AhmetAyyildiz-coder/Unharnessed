# AGENTS.md

Development rules for Unharnessed, a stripped-down fork of Prime Agent (Rust). The upstream
rules below still apply unless a section says otherwise; `UNHARNESSED_PLAN.md` records what
was removed and why. Every contributor (human or agent) must read this before working on
this repo.

## Style and structure

- Workspace crates are prefixed `uh-`. The hard ownership rules: one owned area per crate,
  uh-types is the only shared crate, cycle-free dependency direction, minimal public APIs,
  no god-modules. The dependency direction is pinned in the Crates table below; each crate's
  README.md states its scope, non-goals, and public API surface.
- Prefer private modules with an explicitly exported public crate API. Internals are `pub(crate)`.
- Aim for files under 2,000 lines, tests included. The 2,000-2,100 range is a soft review
  signal, not a merge limit: when adding to an already-large file, consider splitting by
  responsibility rather than growing it further. Move related tests and docs with extracted code.
- Name modules for what they own, not `utils` or `common`. Follow existing structure: in
  `crates/uh-core/src/kernel/manager/`, `mod.rs` connects `execution.rs`, `requests.rs`, and
  `teardown.rs`; put a new kernel-manager concern beside them.
- Inline format args: always prefer `format!("{x}")` over positional.
- Collapse if statements per clippy::collapsible_if.
- Prefer method references over closures per clippy::redundant_closure_for_method_calls.
- Make `match` statements exhaustive; avoid wildcard arms.
- New traits need doc comments explaining their role and how implementations are expected to behave.
- No opaque positional `bool`/`Option` parameters (`foo(false)` is unreadable). Prefer enums,
  named methods, or newtypes. If you must pass an opaque literal by position, use an exact
  `/*param_name*/` comment matching the callee signature.
- Prefer native RPITIT trait methods with explicit `Send` bounds
  (`fn foo(&self) -> impl Future<Output = T> + Send;`) over `#[async_trait]` or
  `#[allow(async_fn_in_trait)]`. Implementations may use `async fn` when they satisfy the contract.
- No single-use helper methods. Do not create a helper referenced only once.
- Instrument async work at the definition (`#[tracing::instrument(...)]`), not with
  `.instrument(...)` at call sites. Check whether the callee is already instrumented first.

## Change hygiene

- One logical change per PR. Fixes ride with the change that introduced them when possible; a PR
  that mixes a feature with unrelated lint fixes or refactors hides both from review.
- If you change dependencies (`Cargo.toml`), regenerate/commit `Cargo.lock` in the same change.
  A new dependency is an architectural change: state in the PR why the existing crates cannot
  host the functionality, and `make deny` must stay clean for it.
- If a change starts forcing edits across many crate internals, stop and fix the boundary instead.

## Tests

- Prefer whole-object equality comparisons over field-by-field checks.
- Do not add tests for statically defined values.
- Do not add negative tests for logic that was removed.
- Bug fixes need a regression test that fails without the fix.
- Verifiers over self-assessment: end-to-end runs of the real binary
  (`scripts/unharnessed-smoke.sh`, `scripts/unharnessed-wakes-e2e.py`), golden corpora
  replayed against real captured data. No lane merges without its verifier passing, rerun
  by the reviewer where feasible.
- Tests must wait for observable readiness, not fixed sleeps or retry-to-green loops. If a
  test must be disabled, give a reason at the test. Use isolated ports and temporary paths, and
  restore shared state. Run focused tests when changing their behavior.

## Lint discipline

- Zero-warning posture: the merge gates run fmt + clippy + tests at `-D warnings`, so a `warn`
  lint is a merge blocker. Keep the enabled set deliberate and selective; enable a new lint family
  only with every current site fixed in the same change.
- Explain lint exceptions next to the `#[allow(...)]` or workspace configuration; keep
  exceptions narrow. Advisory ignores in `deny.toml` need a reason and review date.

## Merge gates

- `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`,
  `cargo test --workspace` must pass before every merge. Run `make check` — the local mirror of
  the same gates; `.github/workflows/ci.yml` runs them (plus `make deny`) on pushes to `main`
  and on PRs. `scripts/unharnessed-check.sh` runs them isolated from your environment (throwaway `HOME`,
  installed agent binaries off the `PATH`) and compares the failures to
  `UNHARNESSED_BASELINE.txt`.
- TS parity is no longer a merge gate: Unharnessed diverges from the TS product on purpose
  (no TUI, no daemon, a minimal system prompt). Keep wire formats readable both ways where they
  still exist (session files, settings, ACP).
- PRs must state ownership compliance (crate README scope/non-goals/public API, dependency
  direction) and classify the change: internals, new `pub` surface, or a new
  dependency/cross-crate re-export. The latter two are architectural changes — an innocent-looking
  `pub use` is a very simple way to break encapsulation.
- The compiled fallback model catalog `crates/uh-ai/src/models_generated.rs` is
  hand-maintained Rust mirroring the TS `packages/ai/src/models.generated.ts` object
  literal — edit it by hand, exactly like its TS source (there is no generator). The
  parity fixture `crates/uh-models/tests/fixtures/catalog.v1.json` refreshes with
  `scripts/generate-catalog-fixture.py`. Bulk data files are exempt from the size guidance.

## Branding

The product is Unharnessed. User-visible surfaces (docs, READMEs, CLI help text, error
messages, code comments that quote user-facing strings) say Unharnessed, and the wire
identifiers follow: the `unharnessed` binary, the `UNHARNESSED_*` environment variables,
the `unharnessed` ACP `_meta` namespace. Prime names stay only where they name Prime's
own services or the upstream origin:

- the Prime Inference provider (`prime-inference`, `api.primeintellect.ai`,
  `app.primeintellect.ai`) and `~/.prime/config.json`, the `prime` CLI's file that its
  login reads;
- the model and MCP catalogs in `PrimeIntellect-ai/prime-agent-catalog`;
- the origin note in README.md and LICENSE (MIT).

Settings keys and provider IDs stay unchanged, so files migrated from an upstream install
keep working.

## Surface contract

- One tool exposed to the model: `ipython`, a persistent CPython REPL.
- Kernel API in the REPL: `bash()` handles, and the four built-in skills `goal`, `compact`,
  `mcp`, `attach_image` (plus any user or project skill), documented in `layers/core.md`.
  `prompt_guards` pins the documented surface to the registered one.
- System prompt: the cache-stable `core.md` layer (plus the per-model map) followed by one
  dynamic tail (packages, project context, skills inventory, MCP servers, environment).
  `unharnessed prompt` dumps the assembled prompt with its layer breakdown.
- CLI: headless only — `-p` in `text`/`json` modes, and `--mode acp` (Agent Client Protocol
  over stdio, the future VS Code front-end).

## Crates

One owned area per crate; `uh-types` is the only shared vocabulary crate.
Dependencies flow from the composition root toward lower crates, never back upward.
The table summarizes each crate's current direct workspace dependencies:

| Crate | Purpose | Direct workspace dependencies |
| --- | --- | --- |
| `uh-types` | Shared wire and domain types | none |
| `uh-agent` | Provider-independent agent loop | none |
| `uh-ai` | Providers, model registry, streaming | `uh-types` |
| `uh-models` | Live model catalog and transport | `uh-ai`, `uh-types` |
| `uh-core` | Session engine, kernel, skills, prompts, settings | `uh-types`, `uh-ai`, `uh-models`, `uh-agent` |
| `uh-daemon` | ACP transport, session leases, the lease-hold refusal | `uh-types`, `uh-core`, `uh-agent`, `uh-ai` |
| `uh-cli` | `unharnessed` binary, composition root | `uh-types`, `uh-core`, `uh-ai`, `uh-agent`, `uh-daemon` |

Layering rules:

- Dependencies only point from higher crates to lower crates shown above. No cycles or
  reverse edges. The leaf crates are peers; add only the dependencies needed for their work.
  Shared wire/domain vocabulary belongs in `uh-types`, not in a higher crate or a duplicate.
- `uh-cli` is a composition root: it wires crates together and contains no business logic.

## Reliability model

- Sessions run in the `unharnessed` process itself (`-p` or `--mode acp`); there is no daemon.
  Sessions persist on disk (append-only JSONL under `~/.unharnessed/sessions`), a session lease
  keeps two processes off the same file, and the kernel namespace is snapshotted so `-c`/`-r`
  revive it.
- No stubs, no `todo!()`, no swallowed errors (`anyhow` bubbling to UI is fine).

## References

- Cursor Bugbot's PR review rules (`.cursor/BUGBOT.md`) mirror the standards in this file - update both whenever either changes.
