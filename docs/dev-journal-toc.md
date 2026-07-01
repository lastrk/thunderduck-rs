# Development Journal

Detailed entries are in [`docs/dev_journal/`](dev_journal/). This file is a chronological index.

---

## 2026-07-01 — v2 Slice B + Slice C.1 (Substrate)

[`dev_journal/2026-07-01-v2-slice-b-analyzer.md`](dev_journal/2026-07-01-v2-slice-b-analyzer.md)

**Slice B**: `CommonAst` grew from unit struct to 15-operator enum + `Punt`. Analyzer substrate:
`TypedAst`, `TypedAttr`, `AnalyzerError`, sealed `HasSchema`, three bounded passes (`resolve`,
`assign_types` with `Union` downward sub-sweep, `derive_nullability`), five input-relation
fixtures + five mini `CommonAst` fixtures, `inference_smoke()`. **INV4** and **INV5** activated.

**Slice C.1** (architect-proposed C.1/C.2 sub-split honored; C.2 next pass): `lowering.rs`
(29-variant `LogicalPlan → CommonAst` adapter with `Punt`); `emission.rs` grown into hand-written
`dispatch_op` `match` + per-op renderers + `EmittedSql` newtype; `mod.rs` `pub fn generate`
composes `lower → analyze → dispatch`; `service.rs` `TranspilerPath::V2` dispatches with
`is_v2_fallback_eligible` legacy fallback; `error.rs` `V2Lowering`/`V2Analyzer`/`V2Emission`
variants. All six Slice-B mediums (M1-M6) closed. **INV2** and **INV3** activated. Two review
iterations (iteration 1 `NEEDS_CHANGES` — half-declarative `EMISSION_TABLE` scaffolding-without-
interpreter; iteration 2 `APPROVED` — scaffolding deleted). OPT-M1 (`quote_ident` fast path)
applied. `SqlGenerator::gen_expr` remains as a documented C.2 seam.

**Tests**: 230 core + 14 connect-server · differential unchanged (not re-run)

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
