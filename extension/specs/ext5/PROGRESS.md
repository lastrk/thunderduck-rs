# ext5 progress ledger — COMPLETE

Definition of Done met: all 10 specs resolved (7 native-with-proof + 3 implemented),
each with a real-Spark-4.1.1 test, and `make release` + `make format-check` +
`make test` all green. See `GOAL.md`.

| Spec | Kind | Verdict | Test file | Corpus | Notes |
|------|------|---------|-----------|--------|-------|
| spark_count_if | agg | **native** | native_count_if_parity.test | agg2-006 | → `COUNT_IF` |
| spark_corr | agg | **native** | native_corr_parity.test | agg-012 | → `CORR` |
| spark_covar_samp | agg | **native** | native_covar_samp_parity.test | agg-012 | → `COVAR_SAMP` |
| spark_kurtosis | agg | **native** | native_kurtosis_parity.test | agg-009 | → `KURTOSIS_POP` (pop, not sample) |
| spark_regr_slope | agg | **native** | native_regr_slope_parity.test | agg2-003 | → `REGR_SLOPE(y,x)` |
| spark_regr_r2 | agg | **native** | native_regr_r2_parity.test | agg2-003 | → `REGR_R2(y,x)` |
| spark_try_cast | scalar | **native** | native_try_cast_parity.test | cast-012 | → `TRY_CAST(e AS T)` (emission-side) |
| spark_try_divide | scalar | **implemented** | spark_try_divide.test | math-016 | decimal→spark_decimal_div; else DOUBLE; ÷0→NULL |
| spark_try_sum | agg | **implemented** | spark_try_sum.test | agg2-004 | int→BIGINT overflow→NULL; decimal overflow→NULL |
| spark_try_avg | agg | **implemented** | spark_try_avg.test | agg2-004 | int/float→DOUBLE; decimal reuses spark_avg |

## Test results (v1.5.0 build, ext5 branch)
- `make release`: **rc=0** (with `DUCKDB_PLATFORM=osx_arm64`).
- `make format-check`: **Passed**.
- `./build/release/test/unittest "test/*"`: **All tests passed (368 assertions in 21 test cases).**
- Per-function ext5 tests: **10 passed / 0 failed** (3 new + 7 native parity).

## Corpus cases unblocked (all 7)
math-016, cast-012, agg-012, agg2-003, agg2-004, agg2-006, agg-009.

## Environment notes / gotchas
- DuckDB v1.5.0 build needs `DUCKDB_PLATFORM=osx_arm64` (platform-detect helper is
  killed by the local code-security service). Use `specs/ext5/run_gate.sh` (build+test)
  and `specs/ext5/run_tests.sh` (test-only, no rebuild).
- A local endpoint code-security service (EDR) SIGKILLs freshly-built binaries until
  allowed; allow `build/release/{duckdb,test/unittest}` in the EDR after a build.
  This is a local-machine constraint only; CI (GitHub Actions) is unaffected.

## Hand-off (out of scope for this loop — see GOAL.md)
1. `make tidy-check` (clang-tidy) — C++ changed; run
   `PATH="$(brew --prefix llvm)/bin:$PATH" make tidy-check` (rebuilds; EDR-allow).
2. Cut the `ext5` release tag + platform binaries on this repo.
3. thunderduck-rs: pin ext5 in `crates/core/build.rs`, add the functions to
   `emission::extension_targets()` / `render_function_call` / `render_aggregate`
   (mappings in each `specs/ext5/resolutions/*.md`), archive the specs, drop the
   readiness-map DEFER, run the differential corpus.
