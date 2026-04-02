# Dev Journal — 2026-04-02 — CaseWhen Type Inference, Decimal Precision, Code Review

## Summary

Three major improvements: (1) CaseWhen type inference fix resolving +178 strict-mode tests,
(2) decimal precision/scale fixes for division, modulo, and AVG, (3) two full code review
passes with 17 correctness/safety findings fixed and 8 performance optimizations applied.

---

## CaseWhen Type Inference Fix (+178 strict mode tests)

**Problem**: `CaseWhen` called `promote_numeric()` for ALL branch types. The catch-all
`_ => Double` arm caused non-numeric branches (String, Boolean) to incorrectly infer as
`DoubleType`.

**Fix**: Added `unify_types()` to `TypeInferenceEngine` — a general Spark-compatible type
unification function matching `TypeCoercion.findTightestCommonType`:
- Null as bottom type (yields other type)
- Unresolved → prefer concrete type
- Both numeric → delegate to `promote_numeric()`
- Boolean + numeric → numeric (Spark coercion)
- Date + Timestamp → Timestamp
- Incompatible → String (Spark's implicit widening)

Additionally, CaseWhen now skips untyped NULL literals (Null/Unresolved data type) per Spark
semantics — prevents `CASE WHEN cond THEN decimal_col ELSE NULL END` from inflating precision.
Falls back to `DataType::String` when all branches are untyped NULL.

Union `infer_schema()` and generator both updated to use `unify_types()`, removing the
previous inline `promote_numeric` + non-numeric guard workaround.

**Result**: Strict mode 508 → 686 passed (+178).

---

## Decimal Precision/Scale Fixes (+1 strict mode test)

Six gaps closed between Rust and Java reference:

| Gap | Fix |
|-----|-----|
| D1: `decimal_div_type()` never called | Wired into Div arm of `data_type()` |
| D2: No decimal modulo handling | Added `decimal_mod_type()` with `min(int_digits)` formula |
| D3: AVG scale cap 38 instead of 18 | Changed to `min(min(s+4, 18), precision)` |
| D4: `integral_to_decimal` private | Made `pub` for cross-module use |
| D5: Strict division uses plain `/` | Added `spark_decimal_div()` in `gen_binary()` |
| D6: Strict SUM uses native sum+CAST | Changed to `spark_sum()` extension function |

Most decimal-failing tests also have nullable mismatches, so precision fix alone only flips +1
test. The precision logic is correct for when nullable issues are resolved.

---

## Code Review Findings Fixed (17 total)

### Round 1 (12 findings)
- **C1**: Integer overflow in interval decomposition (`unsigned_abs as i64`)
- **C2**: Unchecked index `a.aggregates[*idx]` panics
- **H1**: `.unwrap()` in expression converter
- **H2**: SQL injection via `$TZ` env var
- **H3**: Silent error swallowing in column rename
- **H4**: Silent DDL failure
- **M1-M6**: Duplicated trait, unquoted identifiers, inject_distinct precondition,
  escaped quotes, iterator patterns, safe integer conversion

### Round 2 (5 findings)
- **H1**: Unquoted field names in struct literals
- **H2**: Timezone whitelist validation (alphanumeric + `/_-+: `)
- **M1-M3**: Saturating arithmetic, safe i32/u8 casts

---

## Performance Optimizations (8 applied)

| OPT | Change |
|-----|--------|
| OPT-1 | `field_by_name`: `to_lowercase()` → `eq_ignore_ascii_case()` |
| OPT-2 | `quote_ident`: fast path skips `replace()` when no `"` |
| OPT-3 | Function registry: stack-based ASCII lowercase buffer |
| OPT-4 | `TypeMapper::to_duckdb`: returns `Cow<'static, str>` |
| OPT-5 | `TD_DEBUG_SQL` cached in `LazyLock<bool>` |
| OPT-7 | Arrow IPC buffer pre-allocated |
| OPT-9 | Release profile: `codegen-units = 1`, `opt-level = 3` |
| OPT-10 | `run_query` keyword check via prefix `eq_ignore_ascii_case` |

---

## Gap Analysis Update

Strict mode failures reclassified after investigation showed the original 8.1 hypothesis
(Parquet REQUIRED/OPTIONAL metadata) was wrong — Spark calls `asNullable` on Parquet reads,
making all source columns nullable (same as DuckDB). All 148 remaining failures are in the
type derivation layer: struct nullable (35-40), column nullable propagation (30-35),
array containsNull (25-30), HOF result types (15-20), math function nullable (10-15),
map construction (5-10).

---

## Test Status

- **Unit tests**: 84 passing (3 new decimal tests, 1 new unify_types test)
- **Relaxed mode**: 822 passing, 8 pre-existing failures (6 map + Q40 + Q66), 6 skipped
- **Strict mode**: 687 passing, 148 failed, 1 skipped
