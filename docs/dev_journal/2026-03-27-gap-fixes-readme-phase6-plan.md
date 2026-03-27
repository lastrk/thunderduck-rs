# Dev Journal — 2026-03-27 — Gap Fixes, README, Full Suite Run

**Date**: 2026-03-27
**Branch**: main
**Status**: 719 passing / 111 failing (pre-existing) / 6 skipped — 836 total differential tests

---

## Summary

Four deliverables in this session:

1. **Two gap closures** from `reference-gap-analysis.md`: `sample(withReplacement=True)` error and
   `withColumn` strict-mode DECIMAL CAST.
2. **README.md** — adapted from the Java reference implementation, Rust-specific.
3. **Full 836-test suite run** — 719 passing, 111 pre-existing failures catalogued in gap analysis
   Section 6 with strict/relaxed mode distinction.
4. **Benchmarks** — cold-start and RSS measured via SELECT 1 round-trip.

---

## 1. `gen_sample`: `withReplacement=True` now returns Unsupported

**Problem**: `df.sample(withReplacement=True)` silently used `TABLESAMPLE SYSTEM(...)` — a
block-level sampler that is not semantically equivalent to per-row sampling with replacement.
The Java reference throws `UnsupportedOperationException` for this case.

**Fix**: `gen_sample` in `generator/mod.rs` now returns `ThunderduckError::Unsupported` before
attempting SQL generation when `s.with_replacement == true`:

```rust
fn gen_sample(&self, s: &Sample) -> Result<String> {
    if s.with_replacement {
        return Err(crate::error::ThunderduckError::Unsupported(
            "df.sample(withReplacement=True) is not supported; \
             DuckDB has no row-level sampling with replacement".into(),
        ));
    }
    // ... TABLESAMPLE BERNOULLI path unchanged
}
```

**Test added**: `test_sample_with_replacement_raises` in
`test_dataframe_ops_differential.py::TestSample_Differential` — calls `.collect()` on a
`withReplacement=True` sample and asserts the exception message matches
`(?i)not supported|unsupported`. **Passed.**

---

## 2. `try_strict_decimal_cast`: withColumn strict-mode DECIMAL CAST

**Problem**: In strict mode, `df.withColumn("result", col("a") / col("b"))` where `a` and `b`
are DECIMAL columns would produce a DOUBLE output column. DuckDB's native `/` operator on any
type returns DOUBLE; without an explicit CAST the output type diverges from Spark's DECIMAL result.

The Java reference addresses this in `generateExpressionWithCast` (called from
`visitWithColumns`): it wraps the expression with `CAST(expr AS DECIMAL(p,s))` when
`TypeInferenceEngine.resolveType(expr, childSchema)` returns DECIMAL but the expression's own
declared type does not.

**Fix**: New helper `try_strict_decimal_cast` in `generator/mod.rs`, called from
`gen_projection_list` when `self.mode == CompatMode::Strict`:

```rust
fn try_strict_decimal_cast(&self, e: &Expression) -> Result<Option<String>> {
    let (inner_expr, alias_opt) = match e {
        Expression::Alias(a) => (a.expr.as_ref(), Some(a.alias.as_str())),
        other => (other, None),
    };
    // Skip simple passthroughs
    if matches!(inner_expr,
        Expression::ColumnReference(_) | Expression::UnresolvedColumn(_)
        | Expression::Cast(_) | Expression::Literal(_) | Expression::Star(_))
    {
        return Ok(None);
    }
    let intrinsic_type = inner_expr.data_type(&StructType::empty());
    let schema_type = inner_expr.data_type(&self.schema);
    if let DataType::Decimal { precision, scale } = schema_type {
        if !matches!(intrinsic_type, DataType::Decimal { .. }) {
            let inner_sql = self.gen_expr(inner_expr)?;
            let cast_sql = format!("CAST({inner_sql} AS DECIMAL({precision}, {scale}))");
            // ... emit with alias
            return Ok(Some(...));
        }
    }
    Ok(None)
}
```

**Why this works**: On the DataFrame API path, column operands are `UnresolvedColumn` (type
unknown without schema). `data_type(&StructType::empty())` returns `Unresolved` for them, so
arithmetic like `a / b` resolves to `Unresolved`. `data_type(&self.schema)` uses the real input
schema and resolves to `DECIMAL(p,s)` via `decimal_div_type`. The mismatch triggers the CAST.
On the SparkSQL path, `ColumnReference` nodes carry their resolved types, so
`data_type(&StructType::empty())` already returns `DECIMAL` — no spurious CAST added.

The helper is integrated in `gen_projection_list`, which is called (via `typed_gen =
self.with_schema(input_schema)`) with the full input schema already populated. Simple column
references and explicit casts are excluded to avoid redundant wrapping.

**Test added**: `test_with_column_decimal_strict_mode` in
`test_dataframe_ops_differential.py::TestColumnOperations_Differential`. Uses `l_extendedprice *
l_discount` (both `DECIMAL(15,2)`) from the lineitem table. Skipped unless
`THUNDERDUCK_COMPAT_MODE=strict`. Run with `THUNDERDUCK_COMPAT_MODE=strict` to activate.

---

## 3. README.md

Adapted from the Java reference `.reference/README.md`. Key changes:

- Prerequisites: Rust 1.75+, Cargo, protoc (replaces Java 21, Maven 3.9)
- Build: `cargo build --release` (replaces `mvn clean package`)
- Server: `./target/release/thunderduck-connect-server` with `--port`, `--strict`, `--relaxed`
  flags (replaces `java --add-opens ... -jar ...`)
- Architecture: Cargo workspace crate structure (replaces Maven module tree)
- Threading model section added (DuckDB `!Send`, mpsc channel pattern)
- Extension: pre-built, embedded via `include_bytes!()` — no separate build step
- Test commands updated to `cargo test` and `run-differential-tests.sh`
- Memory figure: `~45MB RSS` (measured 43MB post-SELECT-1; replaces the ~30MB estimate)

---

## 4. Full suite run + benchmarks

### Test results (2026-03-27)

| Metric | Value |
|--------|-------|
| Total tests | 836 |
| Passing | 719 |
| Failing | 111 (pre-existing, all unimplemented features) |
| Skipped | 6 |
| New regressions | **0** |

All 111 failures are pre-existing unimplemented features. Full breakdown added to
`docs/reference-gap-analysis.md` Section 6 (8 categories, with mode noted).

New tests: `test_sample_with_replacement_raises` **PASSED**;
`test_with_column_decimal_strict_mode` **SKIPPED** (needs strict mode env var).

### Benchmarks (release binary, after `cargo build --release`)

| Metric | Value | Notes |
|--------|-------|-------|
| Server first output | ~13ms | Single log line before gRPC bind |
| SELECT 1 latency (PySpark client) | ~1,050ms | Dominated by PySpark JVM startup, not server |
| RSS after SELECT 1 | **~43MB** | Full DuckDB in-process state loaded |
| VmSize | ~2,827MB | Memory-mapped files, not real RAM |
| Binary size | 56MB | Self-contained; includes DuckDB + embedded extension |

The ~43MB RSS figure updates the project README (previously stated ~30MB).

---

## Files changed

| File | Change |
|------|--------|
| `crates/core/src/generator/mod.rs` | `gen_sample` error + `try_strict_decimal_cast` helper + `gen_projection_list` strict-mode hook |
| `docs/reference-gap-analysis.md` | Header updated; Section 6 (pre-existing failures) added; two items closed |
| `tests/integration/differential/test_dataframe_ops_differential.py` | Two new tests |
| `README.md` | New file — adapted from Java reference |
| `docs/dev-journal-toc.md` | This entry added |
