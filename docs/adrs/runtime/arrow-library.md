# Arrow Library

> **Status: current — runtime/serving substrate.** An existing decision that applies to *both* transpiler paths (legacy and v2); not superseded by the rearchitecture. ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: `arrow` crate (apache/arrow-rs)**

The `duckdb` crate already depends on `arrow-rs`. Using the same library means DuckDB's Arrow export flows directly into tonic response serialization without a conversion step.

- `arrow::record_batch::RecordBatch` — batch type throughout the pipeline
- `arrow_ipc::writer::StreamWriter` — serializes batches to Arrow IPC for gRPC wire encoding
- `arrow::ffi` — used if we drop to C FFI for DuckDB streaming

`arrow2` is not used: it is less actively maintained and incompatible with `duckdb-rs`.

---

← [Back to ADR Index](../README.md)
