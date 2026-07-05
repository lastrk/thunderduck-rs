# v2 Restart Inheritance Checklist

**Purpose.** The morph-track v2 implementation (tag `v2-morph-track-end` at the commit that precedes the v2 code deletion) climbed the corpus from 12/324 → 153/324 across Slices B/C/D. That climb was a debugging arc as much as an emission arc: it surfaced ~10 concrete bugs in the analyzer, the RelationConverter, and the emission arms — bugs that were fixed on the morph track but which the fresh v2 restart could re-discover if the fixes are not deliberately inherited.

**Discipline.** Every item below MUST be present in the v2 restart on day 1 of the relevant slice (Slice A for substrate items, Slice B for analyzer items, Slice C for emission items, Slice D for extension items). If an item is missing, the corpus differential harness will surface it as a red case on the first `tests/scripts/v2-progress.sh` run — burning a `/fix-bug` pipeline pass to re-discover something we already know.

**Provenance.** Every item cites the morph-track commit that landed the fix, so the reviewer can inspect the original diff.

---

## 1. Analyzer / TypeInferenceEngine (v2 `TypeInferenceEngine` and v2 `Expression::nullable`)

These bugs manifested as schema mismatches (wrong type or wrong nullability) at the differential harness. All were **symmetric-omission** cases: a function name existed in one enumeration but was missing from a peer enumeration in the same file. The lesson generalized as `tasks/lessons.md` §"Bug-fix diagnostics — symmetric-omission pattern."

### 1.1 `count_if` aggregate return type and nullability

- **Commit:** `c269ba4` (C.3-3, +2 corpus).
- **Bug:** `TypeInferenceEngine::aggregate_return_type` enumerated the count family (`count | count_distinct | grouping | grouping_id`) but omitted `count_if`; parallel omission in `Expression::FunctionCall::nullable`.
- **Fix shape:** two one-token additions — `type_inference.rs::aggregate_return_type` adds `count_if` returning `Long`; `expression/mod.rs::FunctionCall::nullable` adds `count_if` to the non-nullable literal list.
- **v2 restart action:** v2 `TypeInferenceEngine::aggregate_return_type` must include `count_if` in the count family (`Long` return). v2 `Expression::FunctionCall::nullable` must list `count_if` as non-nullable. Also extend the sibling `aggregate_is_non_nullable` list in the same file.
- **Corpus tests locked by fix:** `agg-020`, `agg2-006`.

### 1.2 `hash` / `xxhash64` / `murmur3` non-nullability

- **Commit:** `c88f04d` (C.3-2, +1 corpus closing `hash-003`).
- **Bug:** Spark's `hash` returns non-nullable INT (seed 42); `xxhash64` returns non-nullable BIGINT. v2 `Expression::FunctionCall::nullable` was marking these nullable via the default fall-through (arg-nullability propagation).
- **Fix shape:** extend `FunctionCall::nullable` non-nullable literal list to include `"hash" | "murmur3" | "xxhash64"` alongside the count family. Bundled `murmur3` in as a Spark synonym.
- **v2 restart action:** v2 `Expression::FunctionCall::nullable` must list `hash`, `murmur3`, `xxhash64` as non-nullable.

### 1.3 `corr` / `covar_samp` / `covar_pop` / `regr_*` aggregate return type

- **Commit:** `0194178` (Slice D Phase 2 analyzer fix).
- **Bug:** `TypeInferenceEngine::aggregate_return_type` had `stddev | stddev_samp | std | stddev_pop | variance | var_samp | var_pop | skewness | kurtosis` in the `→ Double` arm, but omitted `corr`, `covar_samp`, `covar_pop`, `regr_slope`, `regr_r2`, `regr_intercept`, `regr_avgx`, `regr_avgy`, `regr_sxx`, `regr_sxy`, `regr_syy`. Corpus symptom on `agg-012`: schema type mismatch (Integer for integer-column args instead of Double).
- **Fix shape:** extend the `→ Double` arm to include all 11 correlation/covariance/regression names. All 11 are already in `aggregate_is_always_nullable` at the same file, so no nullability fix is needed.
- **v2 restart action:** v2 `TypeInferenceEngine::aggregate_return_type` `→ Double` arm must include the full 11-name family. v2 `aggregate_is_always_nullable` must list all 11 too.

### 1.4 SQL parser aggregate-function classifier

- **Provenance:** documented but never bit in the corpus at the morph track. Included here as a preventive item for the v2 SparkSQL parser lowering path.
- **Bug potential:** `crates/core/src/parser/sql_converter.rs::is_aggregate_function` (used by SparkSQL parser to detect aggregate context) enumerates aggregate names in uppercase (`COUNT | SUM | AVG | ...`). It currently lists `CORR | COVAR_POP | COVAR_SAMP | KURTOSIS | SKEWNESS | REGR_AVGX | REGR_AVGY | REGR_COUNT | REGR_INTERCEPT | REGR_R2 | REGR_SLOPE | REGR_SXX | REGR_SXY | REGR_SYY | BIT_AND | BIT_OR | BIT_XOR | BOOL_AND | BOOL_OR` — but does NOT include `count_if`. If the SparkSQL parser path is exercised on a query with `count_if(...)`, the aggregate context won't be inferred correctly.
- **v2 restart action:** the v2 SparkSQL parser's aggregate-function classifier must include `count_if`, `try_sum`, `try_avg`, `try_divide` (if any of these appear as aggregates in Spark SQL syntax).

---

## 2. RelationConverter → V2RelationConverter (protobuf → v2 CommonAST directly)

The V2RelationConverter is a fresh peer to the legacy `PlanConverter`/`RelationConverter`, producing v2 CommonAST directly from Spark Connect protobuf. Every bug the legacy converter hit is a bug the V2RelationConverter must avoid by construction.

### 2.1 `Decimal128` marshalling in Arrow-IPC LocalRelation → structured plan

- **Commit:** `960995b` (C.3-4, **+15 corpus**).
- **Bug (legacy):** `crates/connect-server/src/converter/relation_converter.rs::local_relation_to_values_sql::val()` had a silent-NULL catch-all: unknown Arrow types (including `Decimal128`) mapped to the SQL literal `"NULL"`, corrupting every DECIMAL cell in `createDataFrame` payloads across the corpus.
- **Fix shape (legacy):** added a `Decimal128(p, s)` arm using a `format_decimal128` helper (renders the scaled literal DuckDB requires); replaced the catch-all `Ok("NULL")` with a loud `Err` so any future marshalling gap surfaces immediately.
- **v2 restart action:** V2RelationConverter's Arrow-value marshalling must have **exhaustive typed dispatch** for every Arrow type (Int8/Int16/Int32/Int64, Float32/Float64, Decimal128/Decimal256, Utf8/LargeUtf8, Binary/LargeBinary, Boolean, Null, Date32/Date64, Timestamp*, List, Struct, Map, ...). **No catch-all `Ok` fallback** — any unhandled type must be a loud `Err` at construction time. This matches the discipline captured in `tasks/lessons.md` §"No catch-all `Ok` fallbacks in typed dispatch."
- **Note:** legacy uses `format_decimal128` helper to render the scaled literal. V2RelationConverter should NOT synthesize SQL text — instead, it should produce a structured `LocalRelation`/`Values` CommonAST variant carrying the parsed `Decimal128(p, s)` value directly. Emission converts the structured value to SQL, not the converter.

### 2.2 `SqlRelation` shortcut avoidance (16 sites)

- **Provenance:** 2026-07-02 audit (see `tasks/lessons.md` for the categorization; see git log for the corrective SqlRelation-restructure discussion).
- **Bug (legacy):** RelationConverter produces `SqlRelation { sql: String, schema, ... }` at 16 sites — as a shortcut for synthesizing SQL text where a structured plan node would be more principled. Every SqlRelation-wrapped plan was legacy-only; v2 morph-track punted on it.
- **v2 restart action:** V2RelationConverter must NEVER produce a `Sql` opaque variant in CommonAST. Every one of the 16 legacy SqlRelation construction sites must have a structured V2 CommonAST variant:
  - `SetExpr::Values` (`spark.sql("VALUES ...")`) → v2 `LocalRelation` variant (already exists in the v2 CommonAst if seeded from legacy `LogicalPlan::LocalDataRelation`).
  - `TableFactor::Function` (explode, table functions) → v2 `TableFunction` variant (new).
  - `TableFactor::UNNEST` (with ordinality) → v2 `Unnest` variant (new; or fold into `TableFunction`).
  - DataSource Read (parquet/csv/json) → v2 `FileScan { path, format, schema, options }` variant (new).
  - Arrow-IPC `LocalRelation` (`createDataFrame`) → v2 `LocalRelation` variant carrying parsed rows (not synthesized SQL). See item 2.1.
  - Catalog metadata operations (9 sites: `tableExists`, `databaseExists`, `currentDatabase`, `currentCatalog`, `isCached`, `cacheTable`, `uncacheTable`, `functionExists`, `listFunctions`, `getFunction`) → service-layer library calls, NOT plan nodes. The v2 dispatcher handles these outside the CommonAST/emission pipeline entirely.
- **Anti-pattern to reject:** if any V2RelationConverter arm produces synthesized SQL text (a `String` field carrying `"SELECT ..."`), stop and rewrite it as a structured CommonAST variant.

### 2.3 Plan-ID qualifier encoding

- **Provenance:** 2026-07-02 audit of Spark plan_id mechanism (see git log for the plan_id research findings).
- **Bug (legacy):** RelationConverter converts Spark Connect protobuf `attr.plan_id` fields into synthetic `__plan_id_{N}__` qualifiers on `UnresolvedColumn`, then wraps ambiguous join sides in `__td_jl_{N}__` / `__td_jr_{N}__` subquery aliases. This works but is a stringly-typed encoding.
- **v2 restart action:** V2RelationConverter should represent plan_id as a **first-class field** on the v2 CommonAST `Join` variant (`left_plan_ids: Vec<i64>`, `right_plan_ids: Vec<i64>`) and on v2 `Expression::UnresolvedColumn` (`plan_id: Option<i64>`), NOT as a stringly-encoded qualifier. Emission then uses the structured plan_ids to decide subquery aliasing. This eliminates the "parse `__plan_id_*` qualifier back out" logic legacy has to do at emission time.

---

## 3. Emission arm bugs (v2 restart must not re-introduce)

Two shape-correction fixes landed as "dormant v2 fixes" on the morph track (the arms were correct in v2 emission but the corpus routed through legacy fallback due to substrate coupling). Under the restart architecture, v2 arms are the ONLY path — so these corrections must be present on day 1.

### 3.1 `sha` / `sha1` / `sha2` arg-stripping

- **Commit:** `c88f04d` (C.3-1, dormant v2 fix — the arm is correct; corpus didn't move on the morph track because it routed through legacy).
- **Bug:** DuckDB's `SHA256` is a single-arg function; Spark's `sha1(col)` / `sha2(col, bits)` pass multiple args. The morph-track fix in `emission.rs::render_function_call` drops args beyond arg 0 for these three names.
- **v2 restart action:** the v2 `sha`/`sha1`/`sha2` arms in `render_function_call` must emit `SHA256(arg_0)` — no additional args passed through.
- **Corpus tests unlocked:** `hash-002` (was dormant on morph track; is a straight green case in the restart).

### 3.2 `percentile_approx` FLOAT CAST for quantile arg

- **Commit:** `797893e` (C.3-6b, dormant v2 fix).
- **Bug:** DuckDB's `approx_quantile(X, pct)` overloads all require the quantile position to be `FLOAT`; DuckDB 1.5.x has no `(X, DOUBLE)` overload. Spark's `percentile_approx(col, pct)` accepts a Double `pct`. The morph-track fix wraps the quantile arg in `CAST(... AS FLOAT)`.
- **v2 restart action:** v2 `percentile_approx` arm must emit `approx_quantile({arg_0}, CAST({arg_1} AS FLOAT))`.
- **Corpus tests unlocked:** `agg-013` (was dormant on morph track).

---

## 4. Extension arm wiring (specific function-name mappings)

The Slice D Phase 2 landing settled the ext6-set + native-parity routing for 10 functions the legacy `FunctionRegistry` did not name-map. These mappings must be present in the v2 emission table on day 1 of Slice D (extension dispatch).

### 4.1 ext6-provided extension arms (3)

- `try_divide(x, y)` → `spark_try_divide(x, y)` — extension.
- `try_sum(x)` → `spark_try_sum(x)` — extension. Unconditional pass-through (not aggregate-rewrite; unlike `sum`/`spark_sum` which reshape for DECIMAL precision widening, `try_sum` routes directly).
- `try_avg(x)` → `spark_try_avg(x)` — extension. Same shape.

**`extension_targets()` allow-list** (v2 restart Slice D deliverable) must include: `spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`, `spark_decimal_div`, `spark_try_divide`, `spark_try_sum`, `spark_try_avg` (9 entries; INV6 checks containment against `duckdb_functions()` in a loaded ext6 session).

### 4.2 Native DuckDB parity arms (7)

- `try_cast(expr AS ty)` → native `TRY_CAST(expr AS ty)` — routed via the `Cast::try_cast` flag on the v2 `Expression::Cast` variant (not through `render_function_call`).
- `corr(y, x)` → native `CORR(y, x)`.
- `covar_samp(y, x)` → native `COVAR_SAMP(y, x)`.
- `regr_slope(y, x)` → native `REGR_SLOPE(y, x)`. **Direction-sensitive**: arg 0 is `y` (dependent), arg 1 is `x` (independent). Spark and DuckDB agree on convention.
- `regr_r2(y, x)` → native `REGR_R2(y, x)`. Same convention.
- `kurtosis(x)` → native `KURTOSIS_POP(x)`. **Population/excess Fisher-definition — NOT DuckDB's sample-based `KURTOSIS`**. Failing to route to `KURTOSIS_POP` produces silent numerical divergence.
- `count_if(pred)` → native `COUNT_IF(pred)`.

### 4.3 Verify-native-first discipline

- **Provenance:** `tasks/lessons.md` §"Extension-spec discipline" (2026-07-02 lesson).
- **Rule:** before writing a `spark_<fn>` extension spec, run `SELECT * FROM duckdb_functions() WHERE function_name = '<fn>'` in a live DuckDB session (with the pinned extension loaded) and check whether the native function's value+type behavior matches Spark for the corpus test range. Only spec an extension arm when the native path demonstrably diverges. This is ADR-010's cast-preferred discipline applied at spec-drafting time; it prevented 7-of-10 wasted specs on the morph track's ext5 planning.

### 4.4 `extension_targets()` is not authoritative for what the extension provides

- **Provenance:** `tasks/lessons.md` §"Extension-spec discipline" (same session).
- **Rule:** the allow-list check (INV6) verifies `extension_targets() ⊆ duckdb_functions()`. It does NOT check the reverse (extensions can register functions the allow-list hasn't caught up with). For "does the extension provide function X" runtime questions, query `duckdb_functions()` on a live session, not the source-side allow-list.

---

## 5. Design patterns to honor

These are architectural patterns from the morph track that transfer as design confidence (not as code). They should be present in the restart by construction, not by rediscovery.

### 5.1 `spark_return_cast` vs `spark_aggregate_return_cast` separation

Two distinct call sites for Spark-parity return-type CASTs. The morph-track lesson: sharing a single helper across both would double-cast aggregate output.

- **`spark_return_cast`** — applied at projection-slot level in `render_projection_slot`. Handles cases like int/int Div → DOUBLE (including the aliased-Div case). Applies to scalar expressions.
- **`spark_aggregate_return_cast`** — applied inside `render_aggregate` for the aggregate's return-type. Handles integer SUM/AVG return-type widening.
- **v2 restart action:** keep them separate. One helper per call site; do NOT unify.

### 5.2 INV6 activation shape (allow-list + `duckdb_functions()` check)

- Slice D deliverable: `extension_targets()` returns `&'static [&'static str]` of expected extension function names; a unit test opens a loaded ext6 session and asserts `extension_targets() ⊆ duckdb_functions()`.
- Pre-existing devcontainer issue: the unit test currently fails in this repo's devcontainer with a libduckdb v1.5.1 vs ext6 v1.5.4 runtime mismatch. Track separately from the correctness of the check.

### 5.3 `EMIT_TAP` isolation via `EMIT_TAP_MUTEX`

- INV2's dispatch-is-only-writer companion uses a counting `EMIT_TAP` global. Tests that touch the tap must be serialized via a module-level `Mutex` to prevent parallel-test flake.
- v2 restart action: Slice C's INV2 activation must include the `EMIT_TAP_MUTEX` from day 1.

### 5.4 `render_tail` CTE rewrite

- Morph-track M6 review finding + fix: `render_tail` embeds `child_sql` twice unless rewritten as a CTE.
- v2 restart action: v2 `render_tail` uses `WITH __td_child AS ({child_sql}) SELECT * EXCLUDE (__td_row_num__) FROM (SELECT *, ROW_NUMBER() ... FROM __td_child) WHERE __td_row_num__ > (SELECT COUNT(*) FROM __td_child) - {n_sql}`.

### 5.5 `plan_has_empty_scan` short-circuit for BaseTypes

- Morph-track OPT-M3 landing: `service.rs::build_base_types_from_plan` short-circuits and returns an empty overlay if no `TableScan` in the plan has an empty `schema`. Avoids unconditional session-catalog walks on every request.
- v2 restart action: the v2 dispatch site (protobuf boundary, `service.rs`) should apply the same short-circuit when constructing the BaseTypes overlay for the analyzer.

### 5.6 `quote_ident` no-quote fast path

- OPT-M1: `quote_ident("plain_ident")` returns the input unchanged (no quotes) when the identifier is a valid unquoted identifier. Saves allocations on the hot path.
- v2 restart action: v2 `quote_ident` must have the same fast path.

### 5.7 DECIMAL SUM/AVG precision-widening via `spark_aggregate_rewrite`

- Slice D Phase 1 substrate: `spark_aggregate_rewrite(func, schema)` inspects `sum`/`avg`/`sum_distinct` FunctionCalls on DECIMAL args, rewrites the name to `spark_sum`/`spark_avg`, and computes the widened DECIMAL return-type cast target using Spark's precision-inflation formulas (`p+10, min(38)` for sum; `p+4, s+4, min(18,p)` for avg).
- v2 restart action: v2 must have an equivalent `spark_aggregate_rewrite` helper. `try_sum`/`try_avg` do NOT route through this rewrite (unconditional pass-through per item 4.1).

---

## 6. Discipline anchors (from `tasks/lessons.md`)

These are meta-rules from the morph-track debugging arc that shape how the restart should proceed, but which don't map to a specific code change.

1. **Symmetric-omission check** — for every new aggregate function added, check both `TypeInferenceEngine::aggregate_return_type` AND `Expression::FunctionCall::nullable` AND `aggregate_is_always_nullable` (or the v2 equivalents). Same rule for `is_aggregate_function` in the SQL parser path.
2. **Corpus-first reading beats prompt speculation** — before scoping a fix, verify the case's actual PySpark expression in `dataframe_corpus.py` rather than reasoning from the prompt.
3. **Rerun-first preflight** — before diagnostic scoping, run the reproducer to establish current red/green state. Morph-track precedent: C.3-5 collapsed to verify-only after preflight showed the case was already green.
4. **Loud-fail over silent NULL** — `crates/connect-server/src/converter/`-scope rule: no catch-all `Ok` fallbacks for typed dispatch. Every unhandled type surfaces as a loud error. The C.3-4 +15 delta was the empirical validation.
5. **Verify-native-first before speccing extensions** — see §4.3.
6. **`extension_targets()` allow-list is not authoritative** — see §4.4.

---

## 7. Restart baseline assumptions

- **Legacy path unchanged.** All legacy fixes (C.3-3 count_if in legacy TypeInferenceEngine, C.3-4 Decimal128 marshalling in legacy RelationConverter, Slice D Phase 2 corr/covar_samp/regr_* additions in legacy TypeInferenceEngine) stay in the legacy tree. The restart deletes only `crates/core/src/transpiler_v2/`.
- **Baseline core_v2:** 12/324 at Slice A start. Legacy TPC-H stays 51/51 throughout.
- **INV1–INV10:** all stubbed in `crates/core/src/transpiler_v2/invariants.rs` at Slice A end. Marker convention per §CV.5.1 (`TODO INV<N>:` for current slice; `DEFER INV<N> → <slice>:` for future slice reassignment).
- **Ext6 pin.** `crates/core/build.rs:33` stays pinned to `ext6`. Extension binary cached under `extensions/ext6/`.
- **Differential harness.** `tests/scripts/run-differential-tests.sh` + `tests/scripts/v2-progress.sh` unchanged.

---

## 8. Verification

After each slice lands, before termination, verify against this checklist:

- **Slice A:** items 2.1 (Decimal128 exhaustive typed dispatch), 2.2 (no SqlRelation shortcut, structured CommonAST for all 6 legacy shortcut categories), 2.3 (plan_id as first-class field), and the discipline anchors (§6).
- **Slice B:** items 1.1 (count_if), 1.2 (hash/xxhash64/murmur3), 1.3 (corr/covar/regr → Double family), 1.4 (aggregate classifier includes count_if + try_*).
- **Slice C:** items 3.1 (sha arg-strip), 3.2 (percentile_approx FLOAT CAST), 5.1 (spark_return_cast separation), 5.3 (EMIT_TAP_MUTEX), 5.4 (render_tail CTE), 5.5 (plan_has_empty_scan), 5.6 (quote_ident fast path).
- **Slice D:** items 4.1 (ext6 arms + `extension_targets()` 9 entries), 4.2 (native parity arms), 4.3 (verify-native-first), 4.4 (allow-list not authoritative), 5.2 (INV6 activation), 5.7 (spark_aggregate_rewrite).

A slice's Pass 1 architect MUST cite this checklist and confirm each applicable item is scoped into the slice's plan. A slice's reviewer MUST verify each applicable item is present in the implementation before returning APPROVED.

---

## Provenance summary

The checklist was assembled from morph-track work between 2026-06-30 and 2026-07-02, spanning commits `9285ba8` (Slice D Phase 1 halt-and-flag) through `0194178` (Slice D Phase 2 landing). See the archived iteration logs in git history for the full debugging arc; see `docs/dev_journal/2026-07-01-*.md` and `docs/dev_journal/2026-07-02-*.md` (once written) for the chronological narrative.
