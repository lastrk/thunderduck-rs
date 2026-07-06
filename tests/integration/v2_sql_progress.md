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
| 2026-07-05T01:05:35Z | 0ada44b |    114 |    148 |   262 |        +6 |
| 2026-07-05T06:09:38Z | c2e2d62 |    132 |    130 |   262 |       +18 |
| 2026-07-05T06:32:56Z | f85cb0d |    137 |    125 |   262 |        +5 |
| 2026-07-05T06:55:27Z | 45db517 |    142 |    120 |   262 |        +5 |
| 2026-07-05T07:12:10Z | 9d0bbfe |    147 |    115 |   262 |        +5 |
| 2026-07-05T07:23:08Z | 2d3520c |    150 |    112 |   262 |        +3 |
| 2026-07-05T07:35:18Z | d2b5ec4 |    153 |    109 |   262 |        +3 |
| 2026-07-05T08:38:54Z | 71e519a |    160 |    102 |   262 |        +7 |
| 2026-07-05T09:26:16Z | 1e51ece |    164 |     98 |   262 |        +4 |
| 2026-07-05T09:52:11Z | 598a8cc |    168 |     94 |   262 |        +4 |
| 2026-07-05T11:07:30Z | bb811d9 |    171 |     91 |   262 |        +2 |
| 2026-07-05T11:15:21Z | 9b3295a |    172 |     90 |   262 |        +1 |
| 2026-07-05T11:33:33Z | 80e166e |    173 |     89 |   262 |        +1 |
| 2026-07-05T23:37:30Z | eacedc5 |    184 |     78 |   262 |       n/a |
| 2026-07-06T05:16:29Z | a7d01b1 |    188 |     74 |   262 |       n/a |
| 2026-07-06T05:41:00Z | c8b74ef |    191 |     71 |   262 |       n/a |
| 2026-07-06T06:01:08Z | 36c69af |    192 |     70 |   262 |       n/a |
| 2026-07-06T06:25:42Z | 1a993fd |    201 |     61 |   262 |       n/a |
| 2026-07-06T07:00:12Z | 078a3d1 |    205 |     57 |   262 |       n/a |
