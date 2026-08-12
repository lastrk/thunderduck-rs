# ADR-011 — The Spark Connect `Command` arm is in scope as a separate path with a state-diff oracle

**Status:** Proposed
**Depends on:** ADR-001, ADR-004
**Depended on by:** ADR-012, ADR-017, ADR-018

**Context.** The protocol's top-level `Plan` is `oneof { Relation root; Command command }`. Commands (CreateTable, WriteOperation / saveAsTable / insertInto, CreateView, RegisterFunction, catalog mutations) are side-effecting statements, not query-producing relations. Additionally, statement-rooted raw SQL (ADR-004) routes here after parsing. The relation-focused expression test matrix scoped commands out.

**Decision.** Statement-shaped operations translate to DuckDB DDL/DML via a parallel `emit_command` path, with the same transliterate-don't-optimize (ADR-001) and forced-transliteration (ADR-007) discipline. Statement-rooted SQL from ADR-004's parser is routed here by parse-root. Their differential oracle is **catalog/table state**, not result rows: run on both engines, compare resulting catalog/table state. This query/command distinction remains structural at the runtime boundary: callers submit explicit query, streaming-query, or batch-command requests; the session thread never infers intent from SQL text.

**Consequences.**
- (+) Closes a real gap — commands were silently excluded from the architecture — and gives statement-rooted raw SQL a home.
- (−) Requires a second test harness (state diff) distinct from the expression-result matrix.
- (neutral) Where Spark write semantics (mode handling: overwrite/append/errorIfExists/ignore; partitioning; bucketing) have no DuckDB equivalent, they become forced transliterations or rejection cases.

**Refinement hooks.** Enumerate the supported command surface and the rejection set (raw-SQL handling itself is now resolved by ADR-004). Define the catalog/table-state comparison precisely. Verify the `ExecutePlanResponse` shape for command vs relation results.

---

