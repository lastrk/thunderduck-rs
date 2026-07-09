# /goal — implement the select-block review findings (witness-driven)

Main loop (Fable 5) = orchestrator AND architect; delegate only coding
(Sonnet 5) and reviewing (independent subagent).

## Goal

Drive `tasks/select-block-review-findings.md` to closure on branch
`feat/select-block-emission` (worktree
`/workspace/.claude/worktrees/select-block-emission`): every CONFIRMED
finding FIXED or DEFERRED-with-rationale in the report, every witness case
green, zero corpus regressions, gates green every cycle. Success is
measured by `tests/scripts/witness-progress.sh` only (already built +
validated at cycle 0, commit ae163a9): it runs both corpora and prints
`REGRESSIONS: n` (baseline-PASS now non-PASS — hard gate, exit 1) and
`WITNESS FLIPS: k/total` (manifest cases now PASS). Manifest:
`tests/integration/select_block_witness_manifest.json`; baseline
regeneration only in an explained commit. New witnesses added in a cycle
extend the manifest in the same commit.

## Cycle order (one cluster per cycle; re-derive if the report moved)

1. F1–F4 USING/default_projections cluster — ONE structural fix (hoisted
   slots must survive set_projections shadowing, into_pure_from re-wrap,
   extend_from), not four call-site patches.
2. F5 vis-exemption strand. 3. F7 duplicate `__td_jr`.
4. F9+F10 strand completion — witness FIRST (mirror filt-016/017), then fix.
5. F8+F11+F12 error semantics — record the Spark 4.1.1 behavior decision in
   the report before code; `expected_error` witnesses; F8's root cause is
   analyzer tier-(f), not the strip.
6. F13 debug_assert on untrusted input (unit-test pinned).
7. F14+F15 + deferred cleanup — only after correctness is closed.

## Per cycle

A. **Architect (main loop).** Read report evidence + code; write an
   executable plan to `.agent-output/NNN-plan-<finding>.md`: files/functions,
   mechanism, invariants, tests, acceptance (which witnesses flip).
B. **Code.** Spawn `rust-coder`, `model: sonnet`, with the plan + CLAUDE.md
   + rust-implementation cheatsheet. It implements + unit-tests, runs
   `cargo check`/scoped fmt/`cargo test -p thunderduck-core --lib`, logs to
   `.agent-output/NNN-implementation-<finding>.md`. No corpora, no commits.
C. **Review.** Spawn `rust-reviewer` (read-only, not the coder) with plan +
   diff + failure scenarios: does the mechanism close the SCENARIO (not
   just the witness), any same-class latent holes, style/ADR conformance.
D. **Fix loop.** Critical/High → fresh Sonnet coder, max 3 rounds; still
   red → revert to last green commit, record blocker in report, re-plan.
E. **Gate:** scoped `cargo fmt --check`; no new clippy warnings on touched
   files; `cargo test -p thunderduck-core` (+ connect-server `--bins` if
   wire-path touched); `witness-progress.sh` exit 0 with the cycle's
   witnesses flipped.
F. Mark finding FIXED in the report; commit code+tests+report together
   (co-author line). NEVER push/merge.
G. Final cycle only: `differential-progress.sh` ledger row, commit.

## Hard constraints

- Work only in the worktree above. Before differential runs:
  `export SPARK_HOME=/workspace/.spark/spark-4.1.1 THUNDERDUCK_VENV_DIR=/workspace/.venv`.
- Never `pkill` — `./tests/scripts/kill-test-servers.sh` only.
- Zero regressions is absolute; witness-first discipline (no fix without a
  red case); ADR-022 single path; Spark parity over DuckDB ergonomics
  (cite Spark source in plans when behavior is in question); SQL from typed
  AST only; INV suite stays green.

## Stop

Success: all manifest witnesses green, findings 1–13 FIXED/DEFERRED in the
report, hygiene done or deferred, ledger row committed. Blocked: same
finding fails its fix loop twice — leave tree green, write blocker
analysis into the report, summarize remainder.
