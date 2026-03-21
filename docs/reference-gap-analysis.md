# Reference Gap Analysis — Updated Snapshot

Verified comparison of the Java reference implementation (`.reference/`) against the Rust port
(`crates/core/`). All findings are confirmed against actual source files.

**Date**: 2026-03-21
**Reference**: 210 Java source files, 4091-line `SQLGenerator.java`, 1776-line `FunctionRegistry.java`

Phases 3 and 4 are now complete. Every item originally classified as **Critical** or **Important**
in the 2026-03-18 analysis has been implemented. This document reflects the current state:
665 differential tests passing, 4 failing. The 4 failures break down as 3 unimplemented Phase 5
stat features and 1 pre-existing empty-relation join bug.

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
| WriteOperation / CTE (WithRelations) | Phase 3/4 | `relation_converter.rs` |

---

## Section 2 — Active bugs (open, affect test pass rate)

### Bug 1 — NAReplace NULL handling

- **File**: `crates/core/src/generator/mod.rs` ~line 752
- **Problem**: `gen_na_replace` emits `WHEN col = NULL THEN val` — always false in SQL; the `= NULL` comparison never matches any row
- **Fix needed**: detect NULL old-value literal and emit `WHEN col IS NULL THEN val` instead
- **Severity**: **High** — `df.replace(None, val)` silently no-ops; no test failure yet but correctness is broken

### Bug 2 — Empty LocalRelation join produces wrong column count

- **Failing test**: `test_join_empty_with_non_empty` (expects 5 columns, gets 2)
- **File**: `crates/core/src/generator/mod.rs` ~line 508 `gen_local_relation`
- **Root cause**: needs investigation — likely VALUES clause schema collapse in DuckDB for empty relations; the empty side loses its schema and the join output schema narrows to the non-empty side only
- **Severity**: **High** — 1 failing differential test; pre-existing regression

---

## Section 3 — Phase 5 unimplemented features (3 failing differential tests)

| Feature | Proto RelType | Failing test |
|---|---|---|
| `df.stat.crosstab()` | `StatCrosstab` | `test_crosstab_basic` |
| `df.stat.freqItems()` | `StatFreqItems` | `test_freqitems_basic` |
| `df.stat.sampleBy()` | `StatSampleBy` | `test_sampleby_preserves_schema` |

Each requires: new `LogicalPlan` variant, `gen_*` method in `generator/mod.rs`, and
`convert_*` handler in `crates/connect-server/src/converter/relation_converter.rs`.

---

## Section 4 — Generator correctness gaps (medium priority)

| Gap | File / approx line | Severity | Notes |
|---|---|---|---|
| Union type widening | `generator/mod.rs` ~line 431 | Medium | Schema inferrer widens types; generator does not emit CASTs for mixed INT/BIGINT columns across UNION branches |
| ROLLUP/CUBE NULLS FIRST | `generator/mod.rs` ~line 380 | Medium | Sort over ROLLUP should force `NULLS FIRST` on grouping columns; currently omitted |
| DECIMAL SUM/AVG precision | `generator/mod.rs` ~line 1667 | Medium | `cast_integer_sum` handles integer SUM only; decimal SUM/AVG precision and scale rules not implemented |
| GROUPING/GROUPING_ID return type | `functions/mod.rs` | Medium | Not in registry; DuckDB returns INTEGER, Spark returns TINYINT (GROUPING) / BIGINT (GROUPING_ID) |
| Auto-alias unaliased expressions | `generator/mod.rs` | Low | Complex expressions in SELECT lack `AS "spark_name"` aliases; column names diverge from Spark |
| Distinct column subset | `generator/mod.rs` ~line 452 | Low | Should use `ROW_NUMBER() OVER (PARTITION BY ...)` for subset-distinct; currently falls back to plain DISTINCT |

---

## Section 5 — Missing optimisations (low priority)

| Gap | Notes |
|---|---|
| `generateFlatJoinChainWithMapping` | Rust emits nested subqueries; Java reference builds a flat `FROM t1, t2, t3` with an alias map — avoids extra subquery layers and alias resolution issues |
| `tryGenerateFlatSemiAntiJoin` | Stacked SEMI/ANTI join chains are not flattened; each hop wraps in an EXISTS subquery |
| WithColumns strict-mode CAST | `withColumn` replacement columns are not explicitly CAST to the declared type in strict mode |
| Sample with replacement | `df.sample(withReplacement=True)` silently uses `SYSTEM` sampling; Java reference throws `UnsupportedOperationException` |

---

## Section 6 — Priority summary

| Item | Severity | Status |
|---|---|---|
| NAReplace NULL (`IS NULL` vs `= NULL`) | **High** | Bug — open |
| Empty LocalRelation join schema | **High** | Bug — open |
| `StatCrosstab` / `StatFreqItems` / `StatSampleBy` | **High** | Phase 5 — planned |
| Union type widening (generator CASTs) | **Medium** | Open |
| ROLLUP/CUBE NULLS FIRST | **Medium** | Open |
| DECIMAL SUM/AVG precision | **Medium** | Open |
| GROUPING/GROUPING_ID return type | **Medium** | Open |
| Auto-alias complex projections | **Low** | Open |
| Flat join chain / flat SEMI/ANTI | **Low** | Open |
| WithColumns strict-mode CAST | **Low** | Open |
| Sample with replacement error | **Low** | Open |
