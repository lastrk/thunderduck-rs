# Async Runtime

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: `tokio` (multi-thread scheduler)**

All gRPC I/O, session lifecycle, and result streaming run on tokio. DuckDB operations (inherently blocking) are isolated on dedicated OS threads and communicated with via channels (see ADR-05).

---

← [Back to ADR Index](../README.md)
