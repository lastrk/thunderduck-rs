# DuckDB Bindings

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: `duckdb` crate with `arrow` feature; drop to `libduckdb-sys` C FFI only if incremental streaming control proves necessary**

The `duckdb` crate provides idiomatic Rust bindings. Its `arrow` feature exposes `Connection::query_arrow()` which drives DuckDB's native Arrow C Data Interface export — the zero-copy path.

**Version pinning**: The DuckDB crate version in `Cargo.toml` **must** exactly match the compiled `thdck_spark_funcs.duckdb_extension` binary version — DuckDB enforces this at `LOAD` time. Currently pinned to the upstream `duckdb-rs` `v1.10505.0` Git revision and rebuilt `v1.5.5` extension binaries. The crates.io archive is not used because it embeds v1.5.4. The architecture additionally sets a floor of **DuckDB ≥ v1.5.3** wherever the Iceberg write path is used (see [ADR-016](../adr-016-version-and-ansi-pins.md)).

If fine-grained streaming control (batch-size, back-pressure) cannot be achieved through the high-level API, we drop down to `libduckdb-sys` and call `duckdb_query_arrow_array()` directly.

---

← [Back to ADR Index](../README.md)
