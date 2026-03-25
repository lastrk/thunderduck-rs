# Dev Journal — 2026-03-25 — Build Fixes + Spark SQL Backtick Compatibility

**Date**: 2026-03-25
**Branch**: main
**Status**: Maintenance — 670 differential tests passing, 0 failing (unchanged)

---

## Summary

Three targeted fixes: cross-platform protobuf compilation (macOS build was broken), a deprecation
warning on the `SqlCommand.sql` field, and DuckDB's rejection of Spark SQL backtick-quoted
identifiers in the raw SQL path.

No new features. No differential test regressions.

---

## 1. macOS protobuf build fix — `crates/connect-server/`

**Problem**: `cargo build` on macOS failed during protobuf codegen:

```
google/protobuf/any.proto: File not found.
spark/connect/expressions.proto: Import "google/protobuf/any.proto" was not found or had errors.
```

**Root cause**: `prost-build 0.13` (used by `tonic-build 0.12`) removed its bundled copy of the
Google well-known type proto files (`google/protobuf/any.proto`, `google/protobuf/timestamp.proto`,
etc.). Earlier versions bundled them; 0.13 expects either the system `protoc` to have them in its
include path, or a vendored binary to supply them.

On macOS with a Homebrew-installed `protoc`, the well-known types are at
`/opt/homebrew/include/google/protobuf/` but `prost-build` does not automatically add that to
protoc's include path, so the build fails. The Linux devcontainer was unaffected because `protoc`
was installed with includes on a path prost-build already searches.

**Fix**: Added `protoc-bin-vendored = "3"` to `[build-dependencies]` in
`crates/connect-server/Cargo.toml`. In `build.rs`, set the `PROTOC` environment variable to the
vendored binary before `tonic_build::configure().compile_protos(...)`:

```rust
if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
    std::env::set_var("PROTOC", protoc);
}
```

`protoc-bin-vendored` ships `protoc 3.19.4` as a self-contained binary. At this version, the
Google well-known types are compiled into the `protoc` binary itself — no `-I` flag needed.
The `if let Ok(...)` guard means CI and Linux builds that already have a working `protoc` in
`PATH` are unaffected (the vendored path is preferred, not forced as an error).

**Files changed**:
- `crates/connect-server/Cargo.toml` — `protoc-bin-vendored = "3"` under `[build-dependencies]`
- `crates/connect-server/build.rs` — `PROTOC` env var set at top of `main()`

---

## 2. `SqlCommand.sql` deprecation warning — `crates/connect-server/src/service.rs`

**Problem**: The Spark Connect proto marks `SqlCommand.sql` (field 1) as `[deprecated=true]`,
along with all other text-based fields (`args`, `pos_args`, `named_arguments`, `pos_arguments`).
The prost-generated Rust struct reflects this with `#[deprecated]`, so the compiler emits a
`use of deprecated field` warning at the two use-sites in `service.rs`.

**Context**: The `sql` field is the legacy path for `spark.sql()` in older PySpark clients.
PySpark 4.x sends `spark.sql()` via the `input` relation field (a parsed `SQL` relation) — the
preferred non-deprecated path. Our `else` branch falls back to `sql` for backward compatibility
and cannot be removed.

**Fix**: Added `#[allow(deprecated)]` at the inner `let text = sql_cmd.sql.clone();` expression,
with a comment explaining why the deprecated field is intentionally accessed. Restructured the
`else if !sql_cmd.sql.is_empty()` / `else` chain into a single `else` block with an inline
`is_empty()` guard, which is cleaner and localises the `#[allow]` to exactly the one statement
that needs it.

**Files changed**:
- `crates/connect-server/src/service.rs` — lines ~319–329

---

## 3. Backtick identifier support in raw SQL path

**Problem**: `warmup_tables()` in `playground/util.py` constructs Spark SQL with backtick-quoted
column names:

```python
exprs = ", ".join(f"count(`{c}`)" for c in cols)
spark.sql(f"SELECT {exprs} FROM {table}").collect()
```

Spark SQL uses backticks as the identifier quoting character (MySQL-style). DuckDB uses double
quotes (ANSI SQL). The generated SQL `SELECT count(\`l_orderkey\`), ...` was passed verbatim to
DuckDB, which rejected it:

```
Catalog Error: Scalar Function with name `__postfix does not exist!
```

DuckDB was misinterpreting the backtick as part of function-call syntax.

**Root cause**: The `spark.sql()` path sends the SQL as a `SQL` relation proto, which
`RelationConverter::convert_sql()` wraps in a `SqlRelation` without any parsing. The raw SQL
string is then processed by `preprocess_spark_sql()` before being sent to DuckDB. That function
handled many Spark-to-DuckDB dialect differences but not backtick quoting.

**Fix**: Added `rewrite_backtick_identifiers(sql: &str) -> String` in `generator/mod.rs` as
Phase 0 of `preprocess_spark_sql`. The function scans the SQL character-by-character using
`Peekable<Chars>`:

- **Single-quoted string literals** (`'...'`): scanned and copied unchanged, including any
  backtick characters inside them (e.g., `SELECT 'hello \`world\`'`).
- **Already double-quoted identifiers** (`"..."`): copied unchanged to avoid double-processing.
- **Backtick-quoted identifiers** (`` `...` ``): the opening backtick is replaced with `"`, the
  closing backtick with `"`, and any embedded `"` characters are escaped as `""`.

This correctly handles all Spark SQL identifier quoting idioms including reserved-word escaping
(e.g., `` `from` ``, `` `order` ``) and multi-word column names.

**Unit test** added: `generator::tests::backtick_identifiers_rewritten_to_double_quote` — covers
plain identifiers, multiple identifiers in one expression, backticks inside string literals (must
not convert), and already-double-quoted identifiers (must pass through unchanged).

**Files changed**:
- `crates/core/src/generator/mod.rs` — `rewrite_backtick_identifiers()` function + Phase 0 call
  in `preprocess_spark_sql()`; unit test

**Note**: This fix also covers any other `spark.sql()` queries that use backtick quoting for
reserved words or special column names — a common pattern in Spark SQL workloads.

---

## Test status

- Core unit tests: **76/76** passing (1 new test added)
- Differential tests: **670/670** passing (unchanged)
- macOS release build: clean (0 warnings, 0 errors)
