# ADR-02: Async Runtime

**Decision: `tokio` (multi-thread scheduler)**

All gRPC I/O, session lifecycle, and result streaming run on tokio. DuckDB operations (inherently blocking) are isolated on dedicated OS threads and communicated with via channels (see ADR-05).

---

← [Back to Architecture Overview](../architecture.md)
