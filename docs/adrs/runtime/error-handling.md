# Error Handling

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: `thiserror` in `core`, `anyhow` in `connect-server`**

```rust
// core/src/error.rs
#[derive(thiserror::Error, Debug)]
pub enum ThunderduckError {
    #[error("SQL generation failed: {0}")]
    SqlGeneration(String),
    #[error("Type inference error: {0}")]
    TypeInference(String),
    #[error("Unsupported operation: {0}")]
    Unsupported(String),
    #[error("DuckDB error: {0}")]
    DuckDb(#[from] duckdb::Error),
    #[error("Arrow error: {0}")]
    Arrow(#[from] arrow::error::ArrowError),
    #[error("Parse error: {0}")]
    Parse(String),
}
```

`ThunderduckError` maps to `tonic::Status` in the gRPC service layer:
- `Unsupported` → `Status::unimplemented`
- `SqlGeneration` / `TypeInference` → `Status::internal`
- `DuckDb` → `Status::internal`

---

← [Back to ADR Index](../README.md)
