# DuckDB Bindings

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: `duckdb` crate with `arrow` feature; drop to `libduckdb-sys` C FFI only if incremental streaming control proves necessary**

The `duckdb` crate provides idiomatic Rust bindings. Its `arrow` feature exposes `Connection::query_arrow()` which drives DuckDB's native Arrow C Data Interface export — the zero-copy path.

**Version pinning**: The DuckDB crate version in `Cargo.toml` **must** exactly match the compiled `thdck_spark_funcs.duckdb_extension` binary version — DuckDB enforces this at `LOAD` time. Currently pinned to the `ext6` extension release (multi-version — pulls the `v1.5.4` binaries to match the `duckdb` crate at `1.10504.0`). The rearchitecture additionally sets a floor of **DuckDB ≥ v1.5.3** wherever the Iceberg write path is used (see [rearchitect ADR-016](../../thunderduck-rearchitect-ADRs.md)).

If fine-grained streaming control (batch-size, back-pressure) cannot be achieved through the high-level API, we drop down to `libduckdb-sys` and call `duckdb_query_arrow_array()` directly.

---

← [Back to ADR Index](../README.md)
