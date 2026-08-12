# DuckDB Threading Model

> **Status: current — runtime/serving substrate.** Applies to τ (`crates/core/src/transpiler_v2/`). Active ADR index: [`../README.md`](../README.md).

**Decision: Dedicated OS thread per session with `tokio::sync::mpsc` channel communication**

`duckdb::Connection` is `!Send + !Sync`. It cannot be moved across thread boundaries or held across `.await` points in async code. The solution:

```
tokio async task (gRPC handler)
    │  sends QueryRequest via mpsc::Sender<SessionCommand>
    ▼
Session thread  (std::thread, owns Connection for its lifetime)
    │  executes query, collects Arrow batches
    │  sends results back via oneshot::Sender<SessionResult>
    ▼
tokio async task
    │  streams Arrow batches over gRPC response
```

Properties of this design:
- Each session's `Connection` is created on the session thread and never leaves it — fully safe.
- DuckDB uses its own internal thread pool for query parallelism; the session thread is just a dispatcher.
- Execution serialization per session (one query at a time) is the natural consequence of a single-receiver channel.
- Session teardown is clean: dropping the `mpsc::Sender` causes the session thread to exit its receive loop and drop the `Connection`.

**Rejected alternative**: `tokio::task::spawn_blocking` — `Connection` is `!Send`, so it cannot be moved into the closure.

---

← [Back to ADR Index](../README.md)
