# ADR-09: SQL Generator

**Decision: `SqlGenerator` struct with `match`-based dispatch**

```rust
pub struct SqlGenerator {
    alias_counter: u32,
    subquery_depth: u32,
}

impl SqlGenerator {
    pub fn generate(&self, plan: &LogicalPlan) -> Result<String> {
        match plan {
            LogicalPlan::Project(p)    => self.gen_project(p),
            LogicalPlan::Filter(f)     => self.gen_filter(f),
            LogicalPlan::Aggregate(a)  => self.gen_aggregate(a),
            LogicalPlan::Join(j)       => self.gen_join(j),
            // ... exhaustive — compiler enforces completeness
        }
    }
}
```

Internal helpers follow the `gen_*` naming convention (`gen_project`, `gen_filter`, `gen_join`, etc.).

**Join rendering rule**:
- `gen_join()`: emits DuckDB's native `SEMI JOIN` / `ANTI JOIN` syntax directly. The Rust port does **not** convert semi/anti joins to `EXISTS` subqueries (a departure from the Java reference, which had to do so for compatibility with a SQL dialect that lacked native SEMI/ANTI).
- Flat-chain rendering inside `gen_join()` (the "natural flat join" branch) **must break at SEMI/ANTI joins** — folding the chain across a semi/anti boundary would change the tree shape and reorder filtering semantics.
- When modifying join SQL generation, **always check both branches** of `gen_join()` (the wrapped-subquery path and the natural-flat-join path).

**Aggregate path**: single canonical path through `gen_aggregate()` — no dual-path issue.

**Filter stack handling**: `extract_filters(plan)` peels all stacked `Filter` nodes off the top of
a plan subtree, returning the base plan + collected conditions. Call this at the start of
`gen_project`, `gen_aggregate`, and `gen_filter` to avoid double-wrapping in subqueries.

---

← [Back to Architecture Overview](../architecture.md)
