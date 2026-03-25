# ADR-20: Testing Strategy

**Unit tests**: Rust `#[test]` in each module — type inference rules, SQL generation for each plan node and expression type, function registry mappings.

**Differential tests**: The Python pytest framework from the Java reference is imported unchanged into `tests/integration/`. The `server_manager.py` is adapted to launch the Rust binary (`target/release/thunderduck-connect-server`) instead of the Java JAR. All 746 differential tests (TPC-H, TPC-DS, joins, aggregations, window functions, etc.) run against the Rust server without modification.

---

← [Back to Architecture Overview](../architecture.md)
