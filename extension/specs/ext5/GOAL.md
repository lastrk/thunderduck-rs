# ext5 goal brief — implement/resolve the 10 Spark-parity functions

This file is the self-contained brief for a `/goal` run. It drives `/new-feature`
(and `/fix-bug` when a stage regresses) over the 10 specs in this directory until
every one is either **implemented** or **resolved as native**, with per-function
differential tests whose expected values are **real Apache Spark 4.1.1** output,
and `make test` fully green.

Work on the current `ext5` branch. Read `CLAUDE.md` (Spark-Parity Contract,
Quality Gate, C++11 + DuckDB conventions) before touching code.

---

## Definition of Done (stop condition)

The goal is complete when **all** of these hold:

1. Every spec in `specs/ext5/*.md` has a recorded resolution in
   `specs/ext5/resolutions/<fn>.md` that is one of:
   - **implemented** — a new `spark_<fn>` scalar/aggregate registered in the
     extension; or
   - **native** — the verify-first check proved DuckDB's native function matches
     Spark 4.1.1 at the pinned DuckDB version, so no C++ was written and RS should
     wire the native function.
2. Every function (implemented **or** native-resolved) has a
   `test/sql/spark_<fn>.test` (implemented) or `test/sql/native_<fn>_parity.test`
   (native) whose expected values were captured from **real Spark 4.1.1** and
   which covers the spec's cases plus the edge cases below and the named corpus
   case ID(s).
3. `make release && make format-check && make test` are all green, and
   `PATH="$(brew --prefix llvm)/bin:$PATH" make tidy-check` is clean for any spec
   that added C++ under `src/`.
4. `specs/ext5/PROGRESS.md` maps every spec → resolution → corpus case ID(s) and
   shows all rows done.

**Out of scope for this loop** (hand-off, do NOT do here): cutting the `ext5`
release tag / building platform binaries; pinning ext5 in thunderduck-rs
`build.rs`; wiring `extension_targets()` / `render_function_call`. Note these as
remaining steps in the final summary.

---

## Function inventory & triage

Two buckets. **Do the verify-first triage first** — it may close up to 7 specs
with zero C++.

### Definite new work (implement)
| Spec | Kind | Notes | Corpus |
|------|------|-------|--------|
| `spark_try_divide` | scalar | `divisor == 0` → NULL; else Spark-promoted division. DECIMAL branch reuses `spark_decimal_div` (ext4). | `math-016` |
| `spark_try_sum` | aggregate | Overflow → NULL; else `spark_sum` (ext4). Extend spark_sum's overflow state with a null-on-overflow flag. | `agg2-004` |
| `spark_try_avg` | aggregate | Overflow → NULL; else `spark_avg` (ext4). Reuse try_sum's overflow pattern. | `agg2-004` |

### Verify-first (native candidate → close as native if it matches Spark 4.1.1)
| Spec | Native candidate | Watch for | Corpus |
|------|------------------|-----------|--------|
| `spark_count_if` | `COUNT_IF(bool)` | NULL treated as FALSE (not counted); empty group → 0. | `agg2-006` |
| `spark_corr` | `CORR(x,y)` | sample Pearson; empty/zero-variance → NULL vs NaN. | `agg-012` |
| `spark_covar_samp` | `COVAR_SAMP(x,y)` | sample (n-1); single-row group → NULL. | `agg-012` |
| `spark_kurtosis` | `KURTOSIS_POP(x)` | Spark = **excess population** kurtosis (normal ⇒ 0); DuckDB `KURTOSIS` is sample — compare `KURTOSIS_POP`. n<4 / zero-variance edge. | `agg-009` |
| `spark_regr_slope` | `REGR_SLOPE(y,x)` | **arg order (y,x)**; may be absent in DuckDB v1.5.0 — if absent, implement. slope = covar_pop(y,x)/var_pop(x). | `agg2-003` |
| `spark_regr_r2` | `REGR_R2(y,x)` | **arg order (y,x)**; may be absent — if absent, implement. r2 = corr(y,x)². constant x → NULL. | `agg2-003` |
| `spark_try_cast` | `TRY_CAST(expr AS T)` | bad string / overflow / malformed date → NULL. If it matches, resolution is emission-side in RS (`try_cast(e,T)`→`TRY_CAST(e AS T)`) — **no extension function**. | `cast-012` |

Analytical prior (still must be verified against Spark 4.1.1, not assumed): corr,
covar_samp, regr_slope, regr_r2 all normalize so sample-vs-pop cancels and DuckDB
likely matches; kurtosis_pop likely matches Spark's excess-population definition;
count_if and try_cast likely match. Treat every "likely" as unconfirmed until the
oracle says so.

### Build / dependency order
1. Verify-first triage batch (all 7) — fast; closes the matches.
2. `spark_try_divide` (scalar; independent).
3. `spark_try_sum`, then `spark_try_avg` (avg reuses sum's overflow pattern).
4. Any verify-first that **diverged or is absent** → implement, sharing infra:
   aggregates with paired sums (corr/covar/regr) share a moments helper; kurtosis
   mirrors ext4 `spark_skewness`; count_if is a trivial counter.

---

## The Spark 4.1.1 oracle (how to get expected values)

All goldens must come from **real Spark 4.1.1**, then be embedded as expected
values in `.test` files (this repo does not run Spark at test time).

**Get Spark 4.1.1** (reuse the sibling repo's install; do not re-download if present):
- Preferred: thunderduck-rs already pins/install it. If
  `/Users/laszlo.torok/dev/thunderduck-rs/.spark/spark-4.1.1` is absent, run once:
  `/Users/laszlo.torok/dev/thunderduck-rs/tests/scripts/setup-differential-testing.sh`
  Then use `SPARK_HOME=/Users/laszlo.torok/dev/thunderduck-rs/.spark/spark-4.1.1`.
- Fallback: `uv pip install pyspark==4.1.1` (or `pip install --user pyspark==4.1.1`)
  and drive it with a plain python3 script.

**Build a reusable capture harness** in this repo (first task of the loop) at
`test/spark_oracle/capture.py`:
- Starts a local `SparkSession` (Spark 4.1.1 defaults — ANSI mode is on by default
  in Spark 4.x, which is what the corpus's `spark4`-flagged cases assume).
- Takes a small table of literals / a tiny fixture matching the spec's "Testing
  notes" (and, for aggregates, the corpus fixtures: an `emp`-like table with
  nulls/NaNs, plus constant-column and single-row and empty groups).
- Runs the exact Spark expression (`try_divide(a,b)`, `corr(x,y)`, `kurtosis(c)`,
  `try_cast(s AS INT)`, …) and prints each result **and its type**, plus a copy
  rounded to a fixed precision for floats.
- Emit output in a form easy to paste into SQLLogicTest expected blocks.
- Commit the harness + a short `test/spark_oracle/README.md` (how to run, Spark
  version, that outputs are the parity oracle).

**Golden conventions in `.test` files** (SQLLogicTest, text comparison):
- Integers/BIGINT/text: exact match.
- **Floating aggregates** (corr, covar, kurtosis, regr_*, try_avg on doubles):
  wrap both sides at the same precision — e.g. `SELECT ROUND(spark_corr(x,y), 10)`
  and record the Spark value rounded to 10 decimals. Pick precision per function
  so it's stable but still catches divergence (10–12 sig figs typical).
- Always include: NULL-in-operand rows, empty group, single-row group (n-1=0),
  constant-column (zero variance), and overflow rows (for try_sum/try_avg use a
  column at BIGINT max). Encode the exact Spark 4.1.1 answer for each (NULL vs NaN
  is a real divergence to capture, not guess).
- Every `.test` header comment must state: "Parity oracle: Apache Spark 4.1.1
  (captured via test/spark_oracle/capture.py on <inputs>)".

---

## Per-function procedure

### A. Verify-first specs
1. Ensure the oracle harness exists. Capture Spark 4.1.1 values for the spec's
   distributions + edge cases.
2. Build this repo's DuckDB once and run the **native** candidate
   (`corr`/`covar_samp`/`count_if`/`kurtosis_pop`/`regr_slope`/`regr_r2`/`try_cast`)
   on the same inputs. First check the function even exists in DuckDB v1.5.0
   (`regr_*` may not).
3. **If native exists and matches Spark 4.1.1** across all distributions/edges:
   - Write `specs/ext5/resolutions/<fn>.md`: verdict **native**, the native
     function name, the inputs tested, Spark vs DuckDB values, and the exact
     RS emission mapping to wire (e.g. `count_if(x)` → `COUNT_IF(x)`).
   - Add `test/sql/native_<fn>_parity.test` asserting the native function equals
     the Spark 4.1.1 goldens (regression guard at the pinned DuckDB version).
   - Do **not** write C++. Mark the spec done in `PROGRESS.md`.
4. **If native is absent or diverges**: treat as definite work → procedure B, and
   record in the resolution note *why* native was insufficient (the diverging
   case + both values).

### B. Definite / diverged specs — run `/new-feature`
Invoke `/new-feature` with a prompt shaped like:

> Implement `spark_<fn>` per `specs/ext5/spark_<fn>.md`. It is a
> [scalar|aggregate] function. Follow the Spark-Parity Contract and C++11 +
> DuckDB conventions in `CLAUDE.md`. Reuse ext4 precedent: `<spark_decimal_div |
> spark_sum | spark_avg | spark_skewness>` and the shared
> `<paired-sums/moments>` infrastructure where applicable — do not duplicate.
> Add `test/sql/spark_<fn>.test` whose expected values are **real Spark 4.1.1**
> goldens captured via `test/spark_oracle/capture.py`, covering the spec's cases,
> the edge cases (NULL operand, empty group, single-row group, constant column,
> overflow at type max), and the corpus case `<id>`. Preserve NULL-skip semantics
> and correct nullable return types. Quality Gate must pass (build, format-check,
> test, and tidy-check since C++ under src/ changed).

Then:
- Read the `/new-feature` summary (`.agent-output/005-summary.md`). If its review
  or perf stage left blocking issues, or a later function regresses this one, run
  `/fix-bug` with the specific symptom.
- Write `specs/ext5/resolutions/<fn>.md`: verdict **implemented**, files added,
  the `.test` name, and the corpus case covered.
- Update `PROGRESS.md`.

---

## Guardrails
- **Never** violate the Spark-Parity Contract in `CLAUDE.md` (NULL-skip, signed
  hash returns / no CAST, seed 42, recursive unsupported-type rejection, Spark 4.1
  decimal rules). The new functions have their own nullable semantics — honor each
  spec exactly (try_* are NULL-on-error, not skip).
- C++11 only; `make_uniq`; honor selection vector + validity in generic paths;
  no third-party deps (`vcpkg.json` stays empty).
- Do not edit `CMakeLists.txt` build flags, the `duckdb/` submodule, or CI.
- Prefer closing a verify-first spec as native over writing redundant C++ — but
  only on proof, never on the analytical prior alone.
- Keep the oracle harness and captured goldens committed so results are
  reproducible.

## Progress ledger
Maintain `specs/ext5/PROGRESS.md` as a table: `spec | kind | verdict
(pending/native/implemented) | test file | corpus case | notes`. Initialize all
10 rows as pending. Update after each function. The loop stops when all rows are
native or implemented and the Definition of Done holds.

## Final summary
When done, report: per-spec verdicts, which corpus cases (`math-016`, `cast-012`,
`agg-012`, `agg2-003`, `agg2-004`, `agg2-006`, `agg-009`) are unblocked, the full
`make test` result, and the remaining hand-off steps (cut ext5 release; pin +
wire in thunderduck-rs).
