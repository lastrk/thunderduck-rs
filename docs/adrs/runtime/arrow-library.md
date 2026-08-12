# Arrow Library

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: `arrow` crate (apache/arrow-rs)**

The `duckdb` crate already depends on `arrow-rs`. Using the same library means DuckDB's Arrow export flows directly into tonic response serialization without a conversion step.

- `arrow::record_batch::RecordBatch` — batch type throughout the pipeline
- `arrow_ipc::writer::StreamWriter` — serializes batches to Arrow IPC for gRPC wire encoding
- `arrow::ffi` — used if we drop to C FFI for DuckDB streaming

`arrow2` is not used: it is less actively maintained and incompatible with `duckdb-rs`.

---

← [Back to ADR Index](../README.md)
