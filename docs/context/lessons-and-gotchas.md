# Lessons & Gotchas

Review before working on SQL generation, expression handling, threading, extension integration, or aggregate paths.

## `to_sql()` vs `Display`

**Gotcha**: Expression rendering MUST use `to_sql()`, not `Display` / `Debug`. The `Display` impl is for debug logging only.

**Past bug**: In the Java reference, `FunctionCall.toSQL()` called `Expression::toString` on arguments instead of `Expression::toSQL`, producing incorrect SQL for complex argument expressions (nested functions, casts). This is a recurring bug class — the Rust port must not repeat it.

**Rule**: Any code that converts an `Expression` to a SQL string must call `to_sql()`, never `format!("{}", expr)` or `format!("{:?}", expr)`.

## `duckdb::Connection` is `!Send + !Sync`

**Gotcha**: `duckdb::Connection` cannot cross thread boundaries and cannot be held across `.await` points. Attempting either is a compile error or — worse — a logic bug if smuggled through a wrapper.

**Rule**: Each session owns its `Connection` on a dedicated `std::thread`. The async gRPC handler communicates with the session thread via `tokio::sync::mpsc::Sender<SessionCommand>` and receives results via `tokio::sync::oneshot::Sender<SessionResult>`. Never wrap `Connection` in `Arc<Mutex<…>>` and call it from a tokio task — use the channel model.

## Composite Aggregate Expressions

**Gotcha**: When aggregate expressions contain non-`FunctionCall` variants (e.g., `Binary` wrapping `FunctionCall`s), they can be silently dropped if `RelationConverter::convert_aggregate()` falls through to a default `_` arm.

**Past bug**: The Java reference's `else` branch dropped non-`FunctionCall` expressions inside aggregates.

**Rule**: Both `gen_aggregate()` and `RelationConverter::convert_aggregate()` must handle every expression variant that can appear inside an aggregate. Default `_` arms must explicitly error or warn — never silently drop.

## Semi/Anti Join Dual Path

**Gotcha**: `gen_join()` emits native DuckDB `SEMI JOIN` / `ANTI JOIN`. The flat-chain rendering branch inside `gen_join()` must break at SEMI/ANTI boundaries — folding the chain across a semi/anti would change the tree shape and reorder filtering semantics.

**Rule**: When fixing join SQL generation, always check **both** the primary `gen_join()` body and the flat-chain branch inside it. A change in one without the other is a partial fix.

## DuckDB SEMI JOIN Syntax

**Gotcha**: DuckDB uses `SEMI JOIN` and `ANTI JOIN` (without the `LEFT` prefix). `LEFT SEMI JOIN` is a parser error.

**Rule**: When emitting semi/anti joins, never prefix with `LEFT`.

## Extension Version Pinning

**Gotcha**: The `.duckdb_extension` binary's embedded DuckDB version must exactly match the `duckdb` crate version in `Cargo.toml`. Mismatch causes runtime load failures with cryptic ABI errors.

**Rule**: Currently pinned to the `ext4` release (multi-version: pulls the `v1.5.1` binaries to match the `duckdb` crate at `1.10501.0`). When bumping either the crate or the extension, bump both in lockstep and re-run `cargo build --release --features bundled-extension` to confirm.

## HUGEINT Overflow on Integer SUM

**Gotcha**: DuckDB `SUM()` of integer columns returns `HUGEINT` (`i128`). Spark returns `BIGINT` (`i64`).

**Rule**: SQL generation must emit an explicit `CAST(... AS BIGINT)` for integer `SUM` to preserve Spark parity. Differential tests catch this — but it's easy to miss when adding a new aggregate path.

## Schema Inference vs DESCRIBE

**Gotcha**: Falling back to `DESCRIBE` queries against DuckDB to learn a plan's schema is slow and round-trips through SQL.

**Rule**: Prefer `plan.infer_schema()` for schema analysis. Only issue `DESCRIBE` when plan-level inference is genuinely impossible (e.g., a `RawSql` node with no upstream type info).
