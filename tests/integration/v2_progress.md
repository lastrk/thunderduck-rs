# v2 transpiler progress

One row per `tests/scripts/v2-progress.sh` invocation. Each row records the
`core_v2` suite (DataFrame corpus routed through `THUNDERDUCK_TRANSPILER=v2`)
PASSED count at the given commit. The goal is for PASSED to climb monotonically
toward 324 (the corpus total) without regressing the `core` (legacy) suite.

| Timestamp UTC        | Commit  | Passed | Failed | Total | Δ vs prev |
| -------------------- | ------- | -----: | -----: | ----: | --------: |
| 2026-06-30T23:31:53Z | 49ba1cb |     12 |    312 |   324 |       n/a |
| 2026-07-01T08:02:36Z | 28f74b4 |     12 |    312 |   324 |       n/a |
| 2026-07-01T08:04:51Z | 28f74b4 |    134 |    190 |   324 |       n/a |
| 2026-07-01T10:48:25Z | f5756b5 |    134 |    190 |   324 |       n/a |
| 2026-07-01T11:51:31Z | 7391016 |    149 |    175 |   324 |       n/a |
| 2026-07-01T13:12:01Z | 960995b |    151 |    173 |   324 |       n/a |
| 2026-07-01T13:56:15Z | c269ba4 |    151 |    173 |   324 |       n/a |
| 2026-07-01T15:01:37Z | c88f04d |    152 |    172 |   324 |       n/a |
