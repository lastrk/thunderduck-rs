# Logical Plan Representation (SUPERSEDED)

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as a historical reference to the pre-τ architecture. ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

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

← [Back to ADR Index](../README.md)
