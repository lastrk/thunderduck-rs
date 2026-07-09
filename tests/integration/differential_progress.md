# Differential suite progress

One row per `tests/scripts/differential-progress.sh` invocation. Each row
records per-test outcomes of the FULL differential suite
(`run-differential-tests.sh all`) at the given commit, bucketed into the
DataFrame corpus, the SQL corpus, and the remaining legacy feature-family
files ("Other"). Bucket cells are passed/total. The goal is for the overall
Passed count to climb monotonically toward the suite total.

Supersedes the per-corpus ledgers `v2_progress.md` and `v2_sql_progress.md`
(frozen 2026-07-09).

| Timestamp UTC        | Commit  | DF corpus | SQL corpus |   Other | Passed | Failed | Skipped | Total | Δ passed |
| -------------------- | ------- | --------: | ---------: | ------: | -----: | -----: | ------: | ----: | -------: |
| 2026-07-09T09:16:56Z | bf17f49 |   369/384 |    308/396 | 408/625 |   1085 |    320 |       0 |  1405 |      n/a |
| 2026-07-09T09:26:37Z | 50ac9c4 |   369/384 |    308/396 | 408/625 |   1085 |    320 |       0 |  1405 |       +0 |
| 2026-07-09T09:38:34Z | d8899b5 |   369/384 |    308/396 | 439/625 |   1116 |    289 |       0 |  1405 |      +31 |
| 2026-07-09T10:10:23Z | 7c9fedb |   369/384 |    308/396 | 470/625 |   1147 |    258 |       0 |  1405 |      +31 |
| 2026-07-09T10:42:46Z | b1ee1ee |   369/384 |    308/396 | 512/625 |   1189 |    216 |       0 |  1405 |      +42 |
| 2026-07-09T11:02:22Z | 4d1bcb3 |   369/384 |    308/396 | 524/625 |   1201 |    204 |       0 |  1405 |      +12 |
| 2026-07-09T11:39:31Z | 8c0cc4e |   369/384 |    308/396 | 539/625 |   1216 |    189 |       0 |  1405 |      +15 |
| 2026-07-09T11:57:43Z | 2ef47de |   369/384 |    311/396 | 550/625 |   1230 |    175 |       0 |  1405 |      +14 |
