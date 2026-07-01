# Development Journal

Detailed entries are in [`docs/dev_journal/`](dev_journal/). This file is a chronological index.

---

## 2026-07-01 — v2 Slice B + Slice C.1 + Slice C.2 + Slice D Phase 1 + Slice C.3-4 + Slice C.3-3 + Slice C.3-5

[`dev_journal/2026-07-01-v2-slice-b-analyzer.md`](dev_journal/2026-07-01-v2-slice-b-analyzer.md)

**Slice B**: `CommonAst` grew from unit struct to 15-operator enum + `Punt`. Analyzer substrate:
`TypedAst`, `TypedAttr`, `AnalyzerError`, sealed `HasSchema`, three bounded passes (`resolve`,
`assign_types` with `Union` downward sub-sweep, `derive_nullability`), five input-relation
fixtures + five mini `CommonAst` fixtures, `inference_smoke()`. **INV4** and **INV5** activated.

**Slice C.1** (architect-proposed C.1/C.2 sub-split honored): `lowering.rs`
(29-variant `LogicalPlan → CommonAst` adapter with `Punt`); `emission.rs` grown into hand-written
`dispatch_op` `match` + per-op renderers + `EmittedSql` newtype; `mod.rs` `pub fn generate`
composes `lower → analyze → dispatch`; `service.rs` `TranspilerPath::V2` dispatches with
`is_v2_fallback_eligible` legacy fallback; `error.rs` `V2Lowering`/`V2Analyzer`/`V2Emission`
variants. All six Slice-B mediums (M1-M6) closed. **INV2** and **INV3** activated. Two review
iterations (iteration 1 `NEEDS_CHANGES` — half-declarative `EMISSION_TABLE` scaffolding-without-
interpreter; iteration 2 `APPROVED` — scaffolding deleted). OPT-M1 (`quote_ident` fast path)
applied. `SqlGenerator::gen_expr` remains as a documented C.2 seam.

**Slice C.2** (pass 2 of Slice C): Approach A chosen (hand-written per-variant / per-function
match arms — dead-data lesson applied). `render_expr` became an exhaustive match over all 27
`Expression` variants; `render_function_call` grew ~130 lowercased-name arms hand-copied from
`FunctionRegistry`. `SqlGenerator::gen_expr` seam drained (`use crate::generator::SqlGenerator`
removed; `.with_schema_for_v2(` / `.gen_expr(` / `SqlGenerator::new()` all gone).
`EmissionError::LegacyRenderFailed` removed; new `UnsupportedExpression` / `UnsupportedFunction`
variants fallback-eligible. `spark_return_cast` (projection slot) + `spark_aggregate_return_cast`
(inside `render_aggregate`) handle Spark-parity CASTs. INV3 tightened (8 grep rejections +
26-entry `REQUIRED_RENDERERS`). M5 closed via module-scoped `EMIT_TAP_MUTEX`; M6 closed via
`render_tail` CTE rewrite; UpdateFields walking added; Union / Intersect / Except gained
per-column CAST wrapper. OPT-M2 subsumed by seam drain; OPT-M3 closed via
`plan_has_empty_scan` short-circuit + `BaseTypes` fallback-only doc contract. Two review
iterations: iteration 1 `APPROVED` with 2 CLOSE_NOW Mediums (M1 qualified Star drop, M4 aliased
Div CAST); iteration 2 closed both plus an M2 log correction. Perf `OPTIMIZED` (0 HIGH + 0 MEDIUM);
seam drain silently absorbed OPT-M2 + L1 wins.

**Slice D Phase 1** (ext4 wiring, partially lands Slice D): Two-file diff (`emission.rs` +
`invariants.rs`). 6 scalar arms (`crc32`, `hash`, `xxhash64`, `skewness`, `percentile_approx`,
`median`) + 2 verify-first arms (`kurtosis` → `KURTOSIS_POP`, `count_if` → `COUNT_IF`, both
native pending scoped-differential confirmation at Phase 1 termination). `render_binary`
DECIMAL-div branch + `render_spark_decimal_div` helper mirroring legacy
`gen_strict_decimal_div`. New `spark_aggregate_rewrite` sibling helper rewrites DECIMAL
`SUM`/`AVG`/`mean` to `spark_sum`/`spark_avg` with widened outer CAST.
`extension_targets()` populated with 6-entry ext4 allow-list. **INV6** activated over the
ext4 subset (containment check against `duckdb_functions()` turned green); INV3
`REQUIRED_RENDERERS` extended with the two new helpers. Up-front audit surfaced `md5`,
`sha1`/`sha2`, and stddev family were already wired in Slice C.2, collapsing planned
edit surface from ~14 arms to 8. Review `APPROVED` (5 Mediums: M1 + M5 CLOSE_NOW closed
via iter 2; M2 scoped-differential at Phase 1 termination; M3 + M4 DEFER). Perf
`OPTIMIZED` (5 LOWs all deferred). Slice D as a whole does not terminate here — Phase 2
remains blocked on the ext5 pin.

**Slice C.3-4** (post-Slice-D-Phase-1 halt-and-flag; `/fix-bug` pipeline): the C.3-4
initial-prompt scope named `emission.rs::render_binary` / `render_spark_decimal_div`, but
the diagnostician's multi-hypothesis pass overturned the scope. Actual root cause was
upstream of both transpilers, in `crates/connect-server/src/converter/relation_converter.rs:2513`:
a silent-NULL catch-all in `local_relation_to_values_sql::val()` mapped every unhandled Arrow
type (including `Decimal128`) to the SQL literal `"NULL"`, corrupting every DECIMAL cell in
`createDataFrame` payloads. Fix: added a `Decimal128(p, s)` match arm with a new
`format_decimal128` helper (renders the scaled literal DuckDB requires — the diagnostician's
naive `CAST(<unscaled i128> AS DECIMAL(p,s))` prescription would have hit DuckDB's out-of-range
CAST error) and replaced the catch-all with a loud `Err`. Regression tests: 3 in
`relation_converter.rs` (Decimal128 round-trip; unhandled-type errors; `format_decimal128`
padding/zero/negative/scale-0) plus 1 Div-routing invariant lock in `emission.rs`.
**Progress signal delta: 134 → 149 core_v2 passing (+15)** — far above the +3 minimum
prediction from `type-003/004/005`; the corpus contained many more silently-NULL'd
decimal-payload cases than the halt-and-flag audit had visibility into. Legacy TPC-H 51/51
unregressed. Deferred: M1 (`format_decimal128` negative-scale defense-in-depth), M2 (symmetric
`Decimal256` arm — no corpus case exercises).

**Slice C.3-3** (post-Slice-C.3-4 follow-up; `/fix-bug` pipeline): closes the
`count_if` aggregate-context type + nullability gap that C.3-4's decimal
marshalling fix uncovered as blocking `agg-020` and `agg2-006`. Initial prompt
speculated the `salary > 90000` predicate was routed as Decimal; corpus-first
reading (agg-020 uses Boolean column `active`; agg2-006's comparison result is
Boolean) narrowed to `TypeInferenceEngine::aggregate_return_type` returning the
argument type via a `_` fall-through. Two-file fix: `types/type_inference.rs`
(added `count_if` to the `count | count_distinct => Long` alternation and to
sibling `aggregate_is_non_nullable`) + `expression/mod.rs` (added `count_if`
to `FunctionCall::nullable`'s non-nullable-aggregate literal list —
iteration 2, after iter-1's type-only fix left a newly-visible nullability
mismatch). Symmetric-omission pattern: both files enumerated the count family
and both omitted `count_if`; C.3-3 closes both. 4 regression tests.
**Progress signal delta: 149 → 151 core_v2 passing (+2)** — exactly the two
target unblocks. Legacy TPC-H 51/51 unregressed.

**Slice C.3-5** (post-Slice-C.3-3 follow-up; `/fix-bug` pipeline, **verify-only**):
diagnostician's "rerun first" preflight caught the case-already-green state — `agg-007`
was already GREEN on v2 as of C.3-4 + Slice D Phase 1's composition (Decimal128
`LocalRelation` marshalling + `spark_aggregate_rewrite` routing for DECIMAL SUM/AVG).
No production code change; 2 regression unit tests added to
`crates/core/src/transpiler_v2/emission.rs::tests`
(`sum_of_decimal_routes_through_spark_sum`, `avg_of_decimal_routes_through_spark_avg`)
locking in the extension-routing + widened-DECIMAL-CAST invariant. Both would have
failed against pre-Slice-D-Phase-1 emission. **Progress signal delta: +0** (151 → 151;
`agg-007` was already inside the 151 baseline). Legacy TPC-H 51/51 unregressed.

**Tests**: 276 core + 17 connect-server (+ 2 regression tests from C.3-5) · differential 151/324 core_v2 (v2 path) · 51/51 legacy TPC-H

---

## 2026-04-03 — Array containsNull, HOF Types, CTE Schema Propagation

[`dev_journal/2026-04-03-array-hof-cte-schemas.md`](dev_journal/2026-04-03-array-hof-cte-schemas.md)

Lambda schema augmentation for array containsNull + HOF return types (+9 strict tests),
Unpivot nullable propagation, CTE schema propagation for decimal precision (+9 strict tests).
Extension bumped to `duckdb1.5.1-ext2`.

**Tests**: 87 unit · 822 relaxed · 716 strict

---

## 2026-04-02 — CaseWhen, Decimal, Struct Nullable, Code Review, Perf

[`dev_journal/2026-04-02-casewhen-decimal-review.md`](dev_journal/2026-04-02-casewhen-decimal-review.md)

CaseWhen `unify_types()` fix (+178 strict tests), decimal precision fixes (div/mod/AVG/strict
extension functions), struct field nullable resolution (+8 strict tests), two code review passes
(17 findings), 8 performance optimizations. Gap analysis reclassified: Parquet nullable
hypothesis debunked, all remaining failures in type derivation layer.

**Tests**: 84 unit · 822 relaxed · 695 strict

---

## 2026-04-02 — Agent Pipeline, Code Review, Performance Optimizations

[`dev_journal/2026-04-02-agent-pipeline-review-perf.md`](dev_journal/2026-04-02-agent-pipeline-review-perf.md)

Multi-agent pipeline (architect/coder/reviewer/perf) with `/rust-feature` skill. Two code review
passes (17 findings fixed: 2 Critical, 6 High, 9 Medium) and 8 performance optimizations across
hot paths (zero-alloc field lookup, stack-based function registry, `Cow<str>` type mapper, release
profile tuning).

**Tests**: 80 unit tests passing

---

## 2026-03-31 — Phase 6 Wave 2: TPC-DS Q17, Map Key Access, Map Explode, JSON Tuple

[`dev_journal/2026-03-31-phase6-wave2.md`](dev_journal/2026-03-31-phase6-wave2.md)

Q17 flat join chain extension (`plan_contains_user_alias`), DDL double-processing bug fixed
(skip `preprocess_spark_sql` for DDL SqlRelations), map explode column naming (`spark_column_name`
quoted alias stripping), json_tuple pre-parse rewrite. All 111 pre-existing failures now closed.

**Tests**: 825 → 829 passing / 0 reproducible failures / 6 skipped · 836 total

---

## 2026-03-31 — Phase 6 Wave 1: DDL, HOF, Complex Types, String/Collection Functions

[`dev_journal/2026-03-31-phase6-wave1.md`](dev_journal/2026-03-31-phase6-wave1.md)

DDL statements (DROP/CREATE TABLE/VIEW, INSERT, TRUNCATE, ALTER TABLE RENAME COLUMN), VALUES
clause, Lambda/HOF (transform/filter/exists/forall/aggregate), complex type accessors (Subscript,
CompoundFieldAccess), OVERLAY expression. Functions: bit_get fix, collect_list/set, octet_length,
format_number, to_char. Generator: natural flat join for TPC-DS Q25/Q29. Logical: selectExpr
column naming fix. Parallel worktree strategy (WT1+WT2 zero file overlap).

**Tests**: 719 → 825 passing (+106) / 5 failing / 6 skipped · 836 total

---

## 2026-03-27 — Gap Fixes, README, Full Suite Run

[`dev_journal/2026-03-27-gap-fixes-readme-phase6-plan.md`](dev_journal/2026-03-27-gap-fixes-readme-phase6-plan.md)

Two gap closures: `sample(withReplacement=True)` now returns Unsupported error; strict-mode
`withColumn` DECIMAL arithmetic now wrapped with explicit CAST via `try_strict_decimal_cast` in
`gen_projection_list`. README.md added (adapted from Java reference, Rust-specific).
Full 836-test suite run: 719 passing, 111 pre-existing failures catalogued in gap analysis
Section 6. Benchmarks: ~13ms server first output, ~43MB RSS after SELECT 1.

**Tests**: 719 passing / 111 failing (pre-existing) / 6 skipped · 836 total

---

## 2026-03-26 — TPC-DS Full Pass (126/126) + SparkSQL Parser

[`dev_journal/2026-03-26-tpcds-full-pass-sparksql-parser.md`](dev_journal/2026-03-26-tpcds-full-pass-sparksql-parser.md)

SparkSQL parser (`sqlparser-rs` + `SparkDialect` + `SqlConverter`) replacing the preprocessing
path. TPC-DS 126-query differential suite added and fixed to 126/126. Four correctness fixes:
`count(1)` aggregate aliasing, DECIMAL spacing in column names, duplicate column `_1` suffix in
both execute and analyze paths. Total differential: 796 passing, 0 failing.

**Tests**: 670 → 796 passing / 0 failing · 76 unit tests

---

## 2026-03-25 — Build Fixes + Spark SQL Backtick Compatibility

[`dev_journal/2026-03-25-build-fixes-and-sql-compat.md`](dev_journal/2026-03-25-build-fixes-and-sql-compat.md)

macOS protobuf build fix (`protoc-bin-vendored`), deprecated `SqlCommand.sql` warning fix, and
backtick-to-double-quote rewrite in the raw SQL preprocessing path.

**Tests**: 670 passing / 0 failing · 76 unit tests

---

## 2026-03-21 — Section 3 Generator Correctness Gaps

[`dev_journal/2026-03-21-reference-gap-analysis-rewrite.md`](dev_journal/2026-03-21-reference-gap-analysis-rewrite.md)

Closed four medium-priority generator correctness gaps from `reference-gap-analysis.md` Section 3:
GROUPING/GROUPING_ID return type CASTs, DECIMAL SUM/AVG precision rules, Union type widening
CASTs, ROLLUP/CUBE NULLS FIRST sort order. Auto-alias complex projections deferred (no test
coverage).

**Tests**: 669 → 670 passing / 0 failing · 75 unit tests

---

## 2026-03-21 — Phase 5: StatCrosstab, StatFreqItems, StatSampleBy

[`dev_journal/2026-03-21-phase5-stat-features.md`](dev_journal/2026-03-21-phase5-stat-features.md)

Implemented the final three Phase 5 statistical plan nodes (`StatCrosstab`, `StatFreqItems`,
`StatSampleBy`). Fixed `NAReplace` NULL literal bug (`= NULL` → `IS NULL`) and empty
`LocalRelation` join schema collapse. Reached 670/670 differential tests passing.

**Tests**: 666 → 670 passing / 0 failing

---

## 2026-03-21 — Schema Inference Fixes + Generator Correctness

[`dev_journal/2026-03-21-schema-inference-and-generator-fixes.md`](dev_journal/2026-03-21-schema-inference-and-generator-fixes.md)

Eight `infer_schema()` correctness gaps closed (join outer nullability, USING column ordering,
AliasedRelation column aliases, Union type widening, ROLLUP/CUBE nullability, `spark_column_name`
for unaliased expressions, `ToDataFrame` extra-name handling). Generator fixes: `extract_filters`
for filter-stack subquery wrapping, SEMI/ANTI qualifier stripping, USING join column reordering.
`Describe` and `Summary` plan variants added.

**Tests**: 665 passing / 4 failing (pre-existing) → closing toward 670

---

## 2026-03-20 — Phase 4 Progress

[`dev_journal/2026-03-20-phase4-updates.md`](dev_journal/2026-03-20-phase4-updates.md)

Large batch of Phase 4 gap closures: `Pivot`, `StatCov`, `StatCorr`, `ApproxQuantile`, complex
literals, `SchemaInferrer`, `WriteOperation`, join plan_id qualification, window frame
boundaries, `unionByName`, `parse_type_str`, session macros, Arrow IPC zero-row fix, CLI flags.

---

## 2026-03-18 — Phase 4: Differential Tests + NA Operations + Unpivot + WriteOperation

[`dev_journal/dev-journal-phase4.md`](dev_journal/dev-journal-phase4.md)

`SchemaInferrer` (DuckDB probe), NA operations (`NADrop`, `NAFill`, `NAReplace`), `Unpivot`
(DuckDB native), `WriteOperation`, differential test infrastructure (PySpark vs Thunderduck).

---

## 2026-03-18 — Phase 3: gRPC Server + Protobuf Converter

[`dev_journal/dev-journal-phase3.md`](dev_journal/dev-journal-phase3.md)

`connect-server` crate: tonic service, `RelationConverter`, `ExpressionConverter`,
`SparkConnectService`, session routing, Arrow IPC streaming, smoke test passing.

---

## 2026-03-18 — Phase 2: DuckDB Runtime + Arrow Streaming

[`dev_journal/2026-03-18-phase2-complete.md`](dev_journal/2026-03-18-phase2-complete.md)

`DuckDbSession` on a dedicated OS thread, `SessionManager`, Arrow streaming, extension loading,
`CompatMode`, DuckDB configuration, integration test passing.

---

## 2026-03-18 — Phase 1: Core Types + SQL Generation

[`dev_journal/2026-03-18-phase1-complete.md`](dev_journal/2026-03-18-phase1-complete.md)

`DataType`, `Expression` (21 variants), `LogicalPlan` (29 variants), `TypeInferenceEngine`,
`SqlGenerator`, `FunctionRegistry` (500+ mappings). All unit tests passing.
