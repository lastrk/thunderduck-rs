# ADR-15: Compatibility Modes

**Decision: Mirror the Java strict/relaxed/auto model**

```rust
pub enum CompatMode { Strict, Relaxed, Auto }
```

- **Relaxed** (default): vanilla DuckDB functions, ~85% Spark parity, no extension required.
- **Strict**: `thdck_spark_funcs` extension loaded, exact Spark numeric semantics, ~100% parity.
- **Auto**: strict if extension available, relaxed otherwise.

CLI flags: `--strict`, `--relaxed`. Environment variable: `THUNDERDUCK_COMPAT_MODE=strict|relaxed|auto`.

---

← [Back to Architecture Overview](../architecture.md)
