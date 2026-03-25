# ADR-03: DuckDB Bindings

**Decision: `duckdb` crate with `arrow` feature; drop to `libduckdb-sys` C FFI only if incremental streaming control proves necessary**

The `duckdb` crate provides idiomatic Rust bindings. Its `arrow` feature exposes `Connection::query_arrow()` which drives DuckDB's native Arrow C Data Interface export — the zero-copy path.

**Version pinning**: The DuckDB crate version in `Cargo.toml` **must** exactly match the compiled `thdck_spark_funcs.duckdb_extension` binary version. **Target: DuckDB 1.5.0** (aligned with the `thunderduck-duckdb-extension` v1.5.0 branch).

If fine-grained streaming control (batch-size, back-pressure) cannot be achieved through the high-level API, we drop down to `libduckdb-sys` and call `duckdb_query_arrow_array()` directly.

---

← [Back to Architecture Overview](../architecture.md)
