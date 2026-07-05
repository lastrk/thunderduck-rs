# Arrow ↔ DuckDB Zero-Copy Exchange

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**The performance-critical path.**

```
DuckDB query execution
    ↓  Arrow C Data Interface (zero-copy)
arrow::record_batch::RecordBatch
    ↓  arrow_ipc::writer::StreamWriter
Arrow IPC bytes
    ↓  tonic streaming response
PySpark client
```

DuckDB exports Arrow natively via `Connection::query_arrow()` (duckdb-rs high-level API) or `duckdb_query_arrow_array()` (C FFI). The resulting `RecordBatch` objects are serialised to Arrow IPC format and streamed over gRPC. No data is copied between the DuckDB export and the wire.

Default batch size: 8192 rows. Configurable via `THUNDERDUCK_BATCH_SIZE` env var.

---

← [Back to ADR Index](../README.md)
