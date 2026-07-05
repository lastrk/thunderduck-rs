# τ SQL front-end progress

One row per `tests/scripts/v2-sql-progress.sh` invocation. Each row records the
`sql_v2` suite (Spark SQL conformance corpus `differential/sql_corpus.py`,
run through τ) PASSED count at the given commit. The goal is for PASSED to climb
monotonically toward 262 (the corpus total) as τ grows temp-view
registration, the catalog bridge, and SQL grammar coverage.

| Timestamp UTC        | Commit  | Passed | Failed | Total | Δ vs prev |
| -------------------- | ------- | -----: | -----: | ----: | --------: |
| 2026-07-05T00:25:39Z | 096c55d |      2 |    260 |   262 |       n/a |
| 2026-07-05T00:51:52Z | c627912 |    108 |    154 |   262 |      +106 |
