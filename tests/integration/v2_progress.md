# v2 transpiler progress

One row per `tests/scripts/v2-progress.sh` invocation. Each row records the
`core_v2` suite (DataFrame corpus routed through `THUNDERDUCK_TRANSPILER=v2`)
PASSED count at the given commit. The goal is for PASSED to climb monotonically
toward 324 (the corpus total) without regressing the `core` (legacy) suite.

| Timestamp UTC        | Commit  | Passed | Failed | Total | Δ vs prev |
| -------------------- | ------- | -----: | -----: | ----: | --------: |
| 2026-06-30T23:31:53Z | 49ba1cb |     12 |    312 |   324 |       n/a |
