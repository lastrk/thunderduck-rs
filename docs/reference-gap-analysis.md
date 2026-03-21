# Reference Gap Analysis — Updated Snapshot

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-03-21 (updated after Phase 5 + Section 3 generator correctness gaps)
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

Phases 3, 4, and 5 are now complete. Every item originally classified as **Critical** or
**Important** in the 2026-03-18 analysis has been implemented. This document reflects the current
state: **669 differential tests passing, 0 failing**. Remaining open items are low-priority
generator correctness gaps and optimisations — none cause test failures.

---

## Section 1 — Closed gaps

All items from the original 2026-03-18 analysis, now implemented:

| Item | Closed in | Location |
|---|---|---|
| `LikeExpression` | Phase 3 | `expression/mod.rs:173`, `generator/mod.rs:1398` |
| `SingleRowRelation` | Phase 3 | `logical/mod.rs` |
| `IntervalExpression` | Phase 3 | `expression/mod.rs:174`, `generator/mod.rs:1410` |
| Timezone hardcoded `'UTC'` | Phase 3 | `session.rs` — `detect_timezone()` |
| `preserve_insertion_order=true` | Phase 3 | `session.rs:117` |
| `initcap` macro | Phase 3 | `session.rs:181` |
| Polymorphic function resolution | Phase 2 | `generator/mod.rs` — `_spark_reverse`, `size`, `sort_array` |
| `ExtractValueExpression` | Phase 3 | `expression/mod.rs:176`, `generator/mod.rs:1496` |
| `IsDistinctFromExpression` | Phase 3 | `expression/mod.rs:175`, `generator/mod.rs:1485` |
| `RowConstructorExpression` | Phase 3 | `expression/mod.rs:177`, `generator/mod.rs:1522` |
| `UpdateFieldsExpression` | Phase 4 | `expression/mod.rs`, `generator/mod.rs` |
| `RegexColumnExpression` | Phase 3 | `generator/mod.rs` — `gen_function_call` special-case |
| `FieldAccessExpression` | Phase 3 | via `ExtractValueExpression` |
| Arrow schema fixup | Phase 3 | `arrow_ipc.rs`, `service.rs` schema inference path |
| NADrop / NAFill / NAReplace / Unpivot | Phase 3 | `relation_converter.rs` |
| Describe / Summary | Phase 4 | `relation_converter.rs` |
| Pivot / StatCov / StatCorr / ApproxQuantile | Phase 4 | `relation_converter.rs` |
| StatCrosstab / StatFreqItems / StatSampleBy | Phase 5 | `relation_converter.rs`, `generator/mod.rs`, `logical/mod.rs` |
| NAReplace NULL literal (`IS NULL` vs `= NULL`) | Phase 5 bugfix | `generator/mod.rs` — `gen_na_replace` |
| Empty LocalRelation join schema collapse | Phase 5 bugfix | `logical/mod.rs` — `Join` arm of `infer_schema()` |
| WriteOperation / CTE (WithRelations) | Phase 3/4 | `relation_converter.rs` |
| Distinct column subset | Phase 3/4 | `generator/mod.rs` — `gen_distinct` uses `ROW_NUMBER() OVER (PARTITION BY ...)` |
| GROUPING/GROUPING_ID return type | Section 3 | `generator/mod.rs` — `gen_function_call`; DuckDB INTEGER → CAST to TINYINT/BIGINT |
| DECIMAL SUM/AVG precision | Section 3 | `generator/mod.rs` — `cast_integer_sum`; decimal SUM/AVG with Spark precision rules |
| Union type widening CASTs | Section 3 | `generator/mod.rs` — `gen_union`; emits explicit CASTs when left/right types differ |
| ROLLUP/CUBE NULLS FIRST | Section 3 | `generator/mod.rs` — `gen_sort`; forces `NULLS FIRST` for ROLLUP/CUBE sort orders |

---

## Section 2 — Active bugs

None. All known bugs are resolved. The last two (NAReplace NULL literal and empty
LocalRelation join schema) were fixed in the Phase 5 session (commit `936f229`).

---

## Section 3 — Generator correctness gaps (low priority)

| Gap | File / approx line | Severity | Notes |
|---|---|---|---|
| Auto-alias unaliased expressions | `generator/mod.rs` | Low | **Deferred** — complex expressions in SELECT lack `AS "spark_name"` aliases; column names diverge from Spark in edge cases. Current behaviour passes all 669 differential tests; risk of regression without dedicated test coverage. Address when test gap is closed. |

---

## Section 4 — Missing optimisations (low priority)

| Gap | Notes |
|---|---|
| `generateFlatJoinChainWithMapping` | Rust emits nested subqueries; Java reference builds a flat `FROM t1, t2, t3` with an alias map — avoids extra subquery layers and alias resolution issues |
| `tryGenerateFlatSemiAntiJoin` | Stacked SEMI/ANTI join chains are not flattened; each hop wraps in an EXISTS subquery |
| WithColumns strict-mode CAST | `withColumn` replacement columns are not explicitly CAST to the declared type in strict mode |
| Sample with replacement | `df.sample(withReplacement=True)` silently uses `SYSTEM` sampling; Java reference throws `UnsupportedOperationException` |

---

## Section 5 — Priority summary

| Item | Severity | Status |
|---|---|---|
| Auto-alias complex projections | **Low** | Open (deferred — no test coverage) |
| Flat join chain / flat SEMI/ANTI | **Low** | Open |
| WithColumns strict-mode CAST | **Low** | Open |
| Sample with replacement error | **Low** | Open |
