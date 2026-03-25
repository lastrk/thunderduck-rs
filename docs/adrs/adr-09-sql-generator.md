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

**Join dual-path rule (inherited from the Java reference)**:
- `gen_join()`: primary path, converts SEMI/ANTI to `EXISTS` subqueries.
- `generate_flat_join_chain()`: optimised flat chain path, **must break at SEMI/ANTI joins** (does not do EXISTS conversion).
- When modifying join SQL generation, **always check both paths**.

**Aggregate path**: single canonical path through `gen_aggregate()` — no dual-path issue.

**Filter stack handling**: `extract_filters(plan)` peels all stacked `Filter` nodes off the top of
a plan subtree, returning the base plan + collected conditions. Call this at the start of
`gen_project`, `gen_aggregate`, and `gen_filter` to avoid double-wrapping in subqueries.

---

← [Back to Architecture Overview](../architecture.md)
