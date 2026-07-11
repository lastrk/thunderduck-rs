# /goal — drive corpus compliance to 100% (pass-log-driven)

Main loop (Fable 5) = orchestrator AND architect; delegate diagnosis
(`rust-diagnostician`), coding (`rust-coder`, Sonnet 5), and review
(`rust-reviewer`, independent). CLAUDE.md governs the gate, ADR-022, the
corpora, and Rust standards — this file is only the loop.

## Goal

Zero UNDOCUMENTED corpus failures on `feat/v2-transpiler`. Metric is the
recorder row from `tests/scripts/differential-progress.sh`
(`N passed / M failed / T total`; raw log via `DIFFERENTIAL_PROGRESS_LOG=`).
"Documented" = an entry in `.agent-output/unsolvable.md` with a
Thunderduck-boundary rationale. `tasks/v2-corpus-pass-log.md` is the
activity log AND the pass-0 regression oracle (baseline 1085/320 @
`50ac9c4`): no pass-0-green case may go red, ever. Cap 40 passes;
continue from the last recorded pass.

## Per pass

1. **Pick** ONE case or a tight cluster (≤3 same-shape); prefer the
   highest-cascade cluster, ties → simplest fixture. One root cause, not
   N call-site patches.
2. **Diagnose** (`rust-diagnostician`): layer-boundary trace to the
   failure site, classify per ADR-022, write `.agent-output/diagnostic-<case>.md`.
   Adding `tracing::debug!` there is in scope.
3. **Architect** (main loop): minimal faithful shape — reuse an existing
   `CommonOp`/converter arm before adding a node; cite Spark 4.1.1 source
   when behavior is in question. Hand the plan to the coder verbatim (no
   speculative surface without a Spark check).
4. **Code** (`rust-coder`, `model: sonnet`): implement + unit-test, log to
   `.agent-output/`. No corpora, no commits.
5. **Review** (`rust-reviewer`, not the coder): does the mechanism close
   the SCENARIO, same-class holes, ADR/style. Skip ONLY for a trivial
   cited-and-unit-locked change, stated. Critical/High → fresh coder, ≤5
   rounds; still red → revert to last green, log the blocker.
6. **Gate + commit**: CLAUDE.md gate, then `differential-progress.sh` with
   **zero regressions vs the pass-0 oracle**. Append a pass entry to
   `v2-corpus-pass-log.md` (Hypothesis / Fix / Review / Gate: Δ +k, zero
   regressions, collateral / Reflect). Commit code + tests + log together.
   NEVER push/merge.

## Constraints

- Before differential runs: `export SPARK_HOME=/workspace/.spark/spark-4.1.1
  THUNDERDUCK_VENV_DIR=/workspace/.venv`. Never `pkill` —
  `./tests/scripts/kill-test-servers.sh` only.
- Compare the SET, not the count: a Δ that hides a red↔green swap is a
  regression.
- Diagnose schema/nullability mismatches before value mismatches.

## Stop

Success: every corpus case green or documented in `unsolvable.md`, final
pass logged and committed. Blocked: a case fails its fix loop twice —
leave the tree green, log the blocker, move on; same root cause blocks 3
clusters → HALT and summarize.
