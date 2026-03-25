# ADR-06: Logical Plan Representation

**Decision: Rust `enum` — one variant per plan node**

Rust enums are sealed by definition. A `match` on a non-exhaustive enum is a **compile error**. This is strictly stronger than Java's sealed classes which require explicit `permits` and can still fall through to a default case.

```rust
pub enum LogicalPlan {
    // Core relational operators
    Project(Project),
    Filter(Filter),
    Aggregate(Aggregate),
    Join(Join),
    Sort(Sort),
    Limit(Limit),
    Tail(Tail),
    Union(Union),
    Except(Except),
    Intersect(Intersect),
    Distinct(Distinct),
    Sample(Sample),
    // Source relations
    TableScan(TableScan),
    SqlRelation(SqlRelation),       // raw SQL passthrough (spark.sql path)
    LocalRelation(LocalRelation),
    LocalDataRelation(LocalDataRelation),
    RangeRelation(RangeRelation),
    InMemoryRelation(InMemoryRelation),
    SingleRow(SingleRowRelation),
    // Transformations
    WithCte(WithCte),
    WithColumns(WithColumns),
    AliasedRelation(AliasedRelation),
    ToDataFrame(ToDataFrame),
    DropColumns(DropColumns),
    RawDdlStatement(RawDdlStatement),
    // DataFrame API operations
    ShowString(ShowString),
    NADrop(NADrop),
    NAFill(NAFill),
    NAReplace(NAReplace),
    Unpivot(Unpivot),
    Pivot(Pivot),
    // Statistical operations
    StatCov(StatCov),
    StatCorr(StatCorr),
    ApproxQuantile(ApproxQuantile),
    StatCrosstab(StatCrosstab),
    StatFreqItems(StatFreqItems),
    StatSampleBy(StatSampleBy),
    // Summary / describe
    Describe(Describe),
    Summary(Summary),
}
```

Each variant wraps a struct carrying the node's fields. `SqlGenerator` is a set of `match` arms — adding a new variant without handling it is a compile error.

---

← [Back to Architecture Overview](../architecture.md)
