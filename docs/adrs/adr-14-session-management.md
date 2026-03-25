# ADR-14: Session Management

**Decision: `DashMap<String, Arc<SessionHandle>>` for concurrent access; one OS thread per session**

```rust
pub struct SessionManager {
    sessions: DashMap<String, Arc<SessionHandle>>,
}

pub struct SessionHandle {
    session_id: String,
    /// Channel to the session's dedicated OS thread
    cmd_tx: mpsc::Sender<SessionCommand>,
    /// Cached view schemas (written when temp views are created)
    view_schemas: Arc<RwLock<HashMap<String, StructType>>>,
}
```

Session isolation: each session creates a named in-memory DuckDB database (`duckdb:///:memory:<session_id_sanitised>`), ensuring temp views and state don't bleed between sessions.

Session replacement (idle session replaced by a new client with a different session ID) is handled by dropping the old `SessionHandle`, which closes the `mpsc::Sender`, causing the session thread to exit.

---

← [Back to Architecture Overview](../architecture.md)
