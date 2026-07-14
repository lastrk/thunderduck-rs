# v2 Corpus Remaining Blockers — 2026-07-03

**State at commit `8d232df`** (branch `feat/v2-transpiler`).

- Corpus: **294 / 324 (90.7%)** — session lift +89 from 205 baseline across 20 passes (57–76).
- Build: green. Workspace warnings: 34 (baseline 37, −3 net).
- Regressions: none.
- Session commit: `8d232df` on `feat/v2-transpiler`. Uncommitted: `.claude/settings.local.json`, `tasks/v2-architecture-review-2026-07-03.md` (unrelated to session).

**Remaining failures: 30.** Full list at bottom.

---

## Blocker Taxonomy

### Group A — Not fixable inside Thunderduck (11 cases, 37% of remaining)

These fail because both Spark and Thunderduck throw or hit an upstream limitation — the differential harness treats "both errored" as a mismatch, or PySpark's client-side decoder rejects a data type that Spark's own server produces.

#### A1. Spark ANSI reference-side throws (2)
| Case | Symptom |
|------|---------|
| `math-010` | `pyspark.errors...ArithmeticException: [REMAINDER_BY_ZERO]` from Spark reference server |
| `math-011` | `pyspark.errors...ArithmeticException: [DIVIDE_BY_ZERO]` from Spark reference server |

Both servers throw. Harness treats it as a diff. Fix requires either (a) harness-level "both-errored-with-matching-error-code = PASS" policy, or (b) tagging cases with `flags=("both_error_expected",)` and skipping the assertion.

#### A2. PySpark Arrow decoder limitation (4)
| Case | Symptom |
|------|---------|
| `intv-001` | `PySparkTypeError: month_day_nano_interval is not supported in conversion to Arrow` |
| `intv-002` | `month_interval is not supported` |
| `intv-003` | `month_day_nano_interval is not supported` |
| `intv-005` | `month_day_nano_interval is not supported` |

τ emits correct interval-typed SQL. Spark's own server produces the same Arrow schema, which PySpark 4.1.1's client-side decoder rejects on `.collect()`. Client-side blocker, not server-side.

#### A3. Spark's own runtime throws (5)
| Case | Symptom |
|------|---------|
| `arr-008` | `element_at` OOB — Spark throws `ArrayIndexOutOfBoundsException` |
| `parse-003` | Spark throws `IllegalArgumentException: [INVALID_FORMAT.MISMATCH_INPUT]` on bad `to_number` format |
| `cond-004` | Schema mismatch (see below) — coalesce/Decimal nullability |
| `set-004` | Spark-emulated union-by-name column-mismatch — Spark itself would reject |
| `arr-012` | `arrays_zip("tags","tags")` yields duplicate field names client-side rejects |

Some are "both throw" (like A1); `cond-004` and `arr-012` may straddle the boundary. Need per-case investigation to confirm whether Spark-parity is achievable.

---

### Group B — Needs new τ substrate (9 cases, 30%)

Fixable in Thunderduck but requires architectural work beyond a single-arm remap.

#### B1. Generator / LATERAL VIEW substrate (3, + json-002 in Group C = 4)
| Case | Feature |
|------|---------|
| `inl-001` | `inline(array_of_struct)` — expand array-of-struct to N rows × M cols |
| `inl-002` | `inline(...)` — same shape as inl-001 |
| `piv-006` | `F.expr("stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary) as (metric, value)")` — `stack()` generator + multi-name-alias parser path |

Current τ handles `explode/explode_outer/posexplode` via UNNEST-in-SELECT (Pass 68). `inline` and `stack` need broader Generate/LATERAL support:
- Emit `SELECT id, u.name, u.age FROM t, UNNEST(t.arr) AS u(name, age)` for inline.
- `stack(N, ...)` transposes N rows from expression list.
- Need multi-name-alias in SparkSQL parser path (proto `Alias { name: repeated string }`) — currently τ's `Expression::Alias` is single-name only for the SQL-string path.

**Owning layer**: converter (multi-name-alias flatten), analyzer (Generate typed op), emission (LATERAL rewrite).

#### B2. `RelType::*` DataFrameStatFunctions (5)
| Case | Proto shape |
|------|-------------|
| `misc-001` | `RelType::Describe` — `df.describe([cols])` — count/mean/stddev/min/max per column |
| `misc-002` | `RelType::Summary` — `df.summary(stats)` — configurable percentiles + describe |
| `misc-006` | `RelType::Crosstab` — `df.stat.crosstab(c1, c2)` — 2-column pivot |
| `misc-007` | `RelType::FreqItems` — `df.stat.freqItems([cols], support)` — return frequent values |
| `samp-002` | `RelType::SampleBy` — `df.stat.sampleBy(col, fractions)` — per-key stratified sample |

Each is a distinct converter arm + analyzer + emission. Well-scoped; ~30–60 min per operator. Legacy `crates/core/src/generator/mod.rs` probably has references for describe/summary at minimum.

#### B3. Session-injected DISTINCT for eager pivot (2)
| Case | Symptom |
|------|---------|
| `grp-005` | `groupBy("active").pivot("dept_id").agg(F.avg("salary"))` — no explicit values, needs eager DISTINCT |
| `chain-002` | Same substrate (likely a chained pivot) |

Analyzer currently punts with honest `PuntedOperator("Pivot[implicit-values]")`. Fix requires the analyzer or converter to execute `SELECT DISTINCT pivot_col FROM input ORDER BY pivot_col` at plan-analysis time — needs session/runtime access from the analyzer, which is a cross-crate architecture change.

**Options**:
- **A**: Preprocess in converter (converter has session context) — inject a synthetic Pivot with the discovered values before analyzer sees it.
- **B**: Analyzer-time callback into runtime — cleaner but adds a new capability contract between analyzer and session.

---

### Group C — Unimplemented DuckDB functions (5 cases, 17%)

Small remaps or minor extension work.

| Case | Function | Fix |
|------|----------|-----|
| `hash-001` | `crc32(str)` | Add `spark_crc32` to `thdck_spark_funcs` extension, or emulate via existing hash |
| `json-002` | `json_tuple(str, k1..kN)` | Generator returning N columns — same LATERAL path as B1 |
| `json-007` | `from_csv(str, ddl)` | Mirror `from_json` DDL parser (Pass 76 already implemented `from_json`) — small port |
| `win2-002` | `window(time_col, '1 day')` | Tumbling windows via `DATE_TRUNC` + `GROUP BY` on interval buckets |
| `json-005` | `to_json(struct)` value fidelity | Follow-up queued from Pass 62 — Spark's JSON encoding differs from DuckDB native |

---

### Group D — τ boundary rejections needing new proto/parser shapes (5 cases, 17%)

| Case | Boundary error | Fix |
|------|---------------|-----|
| `intv-004` | `sql::expr::interval` (`col + INTERVAL 90 DAYS`) | SparkSQL parser gap for interval literal in expression position |
| `intv-006` | `[SPARK-EMULATED] cannot resolve column 'MONTH'` from `timestampadd(MONTH, 3, ...)` | Parser treats `MONTH` unit keyword as column ref |
| `struc-002` | `Expression::UnresolvedRegex` | `df.colRegex(...)` — schema-walking projection |
| `struc-006` | `sql::expr::other` | SparkSQL parser produces unknown expression shape |

---

## Priority Recommendations

### Priority 1 — Batch DuckDB remaps (~1 pass, +3 cases)
`hash-001` (crc32), `json-007` (from_csv DDL — port from json-003/004), `json-005` (to_json fidelity). Total ~3 cases. Each ≤ 30 min. Contained; no substrate needed.

### Priority 2 — RelType::Stats family (~2–3 passes, +4–5 cases)
`RelType::Describe` first — legacy has emission reference. Then `Summary` (superset), `Crosstab` (2-column pivot), `FreqItems` (aggregate approx), `SampleBy` (stratified sampling). ~5 cases.

### Priority 3 — Generator / LATERAL substrate (~1 architect + 1 code pass, +4 cases)
Extend the posexplode multi-alias splitter into a full Generate operator. Handles `inline`, `stack`, `json_tuple`, and unblocks future generator work.

### Priority 4 — Interval-expression parser (~1 pass, +2 cases)
Extend SparkDialect / v2_lowering for interval literals in expression position + MONTH/DAY/... keyword handling. Unblocks intv-004, intv-006.

### Priority 5 — Session-injected DISTINCT for eager pivot (~1 architect pass, +2 cases)
Requires a decision (see B3 options). Unblocks grp-005, chain-002.

### Priority 6 — Investigate ambiguous Group A3 (~1 pass, +1–2 cases)
`cond-004` schema mismatch and `arr-012` duplicate field names may be fixable in τ if Spark's actual server-side behavior differs from what the client shows. Diagnostic first.

### Priority 7 — Harness policy for Group A1–A2 (~1 policy change, +6 cases)
Tag cases with `flags=("both_error_expected",)` or `flags=("pyspark_arrow_incompat",)` and skip the differential assertion when both sides fail with matching error codes. Unblocks 6 cases.

### **Path to ceiling**

| Effort | Delta | Cumulative |
|--------|-------|-----------|
| Baseline (commit `8d232df`) | — | 294/324 (90.7%) |
| Priority 1 (DuckDB remaps) | +3 | 297/324 (91.7%) |
| Priority 2 (RelType stats) | +5 | 302/324 (93.2%) |
| Priority 3 (LATERAL/Generate) | +4 | 306/324 (94.4%) |
| Priority 4 (interval parser) | +2 | 308/324 (95.1%) |
| Priority 5 (pivot DISTINCT) | +2 | 310/324 (95.7%) |
| Priority 6 (ambiguous Group A) | +2 (optimistic) | 312/324 (96.3%) |
| Priority 7 (harness policy) | +6 | 318/324 (98.1%) |
| **Ceiling in τ + harness** | — | **~318–320 / 324** |

Cases that can only be closed by **client-side (PySpark) fixes upstream**: `intv-001/002/003/005` (Arrow decoder). Truly untouchable at the current PySpark version.

---

## Follow-up passes already queued (from pass log)

These were flagged as "findings queued as follow-up pass" during Passes 57–76 and are NOT corpus-blocking, but represent design work that should land:

1. **`unify_types` String fallback → AnalyzerError** (Pass 59 M1). Systemic across Unpivot/SetOp/TableFunction. Harden Spark-parity mismatched-type rejection.

2. **Boundary emitter fusion** (Pass 58 OPT-1). Collapse the `contains_unresolved` guard walk into `data_type_to_proto` by making it return `Result`, eliminating the happy-path double schema walk.

3. **Pivot: eager DISTINCT discovery for implicit values** (Pass 60). = Priority 5 above.

4. **Spark-parity CSV escaping** (Pass 62). RFC-4180 quoting for `to_csv`. Corpus witness (json-008) doesn't exercise embedded delimiters, so unblocking is optional.

5. **Spark-parity JSON emission — value-level `to_json` fidelity** (Pass 62). = json-005 above.

6. **`render_function_call` pre-render/early-return waste refactor** (Pass 57 M2 / Pass 58 OPT-1 companion). 7 arms currently pre-render args, then discard on early return. Cold path, style refactor.

7. **`Expression::data_type`/`nullable` recursion memoization** (Pass 57 P1). Analyzer-wide memoization for deep-nested structures. Cold path.

---

## Full remaining failure list (30)

```
cond-004   (schema mismatch — coalesce/Decimal nullability; ambiguous Group A3)
math-010   (Group A1 — Spark REMAINDER_BY_ZERO)
math-011   (Group A1 — Spark DIVIDE_BY_ZERO)
grp-005    (Group B3 — pivot without explicit values)
set-004    (Group A3 — Spark-emulated union-by-name mismatch)
arr-008    (Group A3 — element_at OOB)
arr-012    (Group A3 — arrays_zip duplicate field names)
misc-001   (Group B2 — RelType::Describe)
misc-002   (Group B2 — RelType::Summary)
misc-006   (Group B2 — RelType::Crosstab)
misc-007   (Group B2 — RelType::FreqItems)
chain-002  (Group B3 — same substrate as grp-005)
piv-006    (Group B1 — stack() generator + multi-name-alias)
json-002   (Group B1/C — json_tuple generator)
json-005   (Group C — to_json value fidelity)
json-007   (Group C — from_csv DDL parser port)
hash-001   (Group C — crc32 missing)
intv-001   (Group A2 — PySpark month_day_nano_interval decoder)
intv-002   (Group A2 — PySpark month_interval decoder)
intv-003   (Group A2 — PySpark month_day_nano_interval decoder)
intv-004   (Group D — sql::expr::interval)
intv-005   (Group A2 — PySpark month_day_nano_interval decoder)
intv-006   (Group D — timestampadd MONTH keyword)
inl-001    (Group B1 — inline generator)
inl-002    (Group B1 — inline generator)
parse-003  (Group A3 — Spark IllegalArgumentException on bad format)
samp-002   (Group B2 — RelType::SampleBy)
struc-002  (Group D — Expression::UnresolvedRegex)
struc-006  (Group D — sql::expr::other)
win2-002   (Group C — window(time, interval) tumbling)
```

---

## Diagnostic quick-reference commands

```bash
# Rerun full corpus
cd /workspace && ./tests/scripts/v2-progress.sh

# Cluster remaining failures by root cause
cd /workspace/tests/integration && source venv/bin/activate && \
  python3 -m pytest differential/test_dataframe_corpus_differential.py --tb=short 2>&1 \
    | grep -oE "DuckDB error: [^\"]+|τ: [^\"]+|Binder Error: [^\"]+|Catalog Error: [^\"]+" \
    | sort | uniq -c | sort -rn | head -20

# Get one specific case's full trace
cd /workspace/tests/integration && source venv/bin/activate && \
  python3 -m pytest "differential/test_dataframe_corpus_differential.py::test_case[<case-id>]" \
    -v --tb=long 2>&1 | tail -30

# Kill any leftover servers before rerunning
pkill -f thunderduck-connect-server
```

---

*Report generated 2026-07-03 after session commit `8d232df`.*
