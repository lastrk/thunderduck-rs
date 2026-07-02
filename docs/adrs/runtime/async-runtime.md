# Async Runtime

> **Status: current — runtime/serving substrate.** An existing decision that applies to *both* transpiler paths (legacy and v2); not superseded by the rearchitecture. ADR index: [`../README.md`](../README.md) · v2 spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: `tokio` (multi-thread scheduler)**

All gRPC I/O, session lifecycle, and result streaming run on tokio. DuckDB operations (inherently blocking) are isolated on dedicated OS threads and communicated with via channels (see ADR-05).

---

← [Back to ADR Index](../README.md)
