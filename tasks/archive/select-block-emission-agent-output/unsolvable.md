# Unsolvable / documented-red ledger

The stop-condition ledger for the corpus-driven differential goal (2026-07-09):
a failing case in the full differential suite is "documented" iff it has an
entry here. `tests/scripts/differential-progress.sh` failures are diffed
against this file to decide whether the goal holds.

Force-tracked in git despite `.agent-output/` being ignored — the previous
(gitignored) incarnation of this file was lost in a worktree cleanup; the
stop-condition ledger must survive.

Entry format (one `###` per case or per shared-root-cause cluster):

```
### <case-id(s)> — <one-line root cause>
- Front-end: sql | dataframe | legacy-file <name>
- Root cause: <what actually breaks, which τ layer>
- Why unreasonable: <ADR conflict / disproportionate complexity / needs extension fn / Spark-side limitation>
- What WOULD be needed: <sketch>
```

Historical note: the pre-2026-07-09 version of this file documented 5
confirmed-invalid SQL corpus cases (sq-011/012/013/016 quantified-subquery /
grandparent-correlation shapes, fn-018 reference-side DIVIDE_BY_ZERO) — all
since rewritten, deleted, or converted to tri-state cases in the corpus by
the rust-architect corpus-repair pass; and 5 latent-bug witness cases
(agg-024, jn-017, tbl-013, sq-023, jn-018) which are deliberately FIXABLE
red corpus cases, not unresolvable. None of those entries carry forward.

## Documented cases

(none yet)

## Blocked on thdck_spark_funcs extension

(none yet — record the designed function signature + Spark semantics here;
ships from nubank/thunderduck-duckdb-extension)

## Architectural constraint notes

(cases where a materially better design exists only by lifting an
architectural constraint — record case, constraint, better design; the
constraint stays.)
