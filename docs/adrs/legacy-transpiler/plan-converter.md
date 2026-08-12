# Protobuf Plan Conversion (SUPERSEDED)

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as historical reference only. Active ADR index: [`../README.md`](../README.md).

**Decision: Two-module converter mirroring the Java `RelationConverter` + `ExpressionConverter`**

- `relation_converter.rs` — converts prost-generated Spark Connect `Relation` to `LogicalPlan`
- `expression_converter.rs` — converts prost-generated `Expression` to our `Expression` enum
- `plan_converter.rs` — entry point, orchestrates both

Input: prost-generated types from Spark Connect protos.
Output: our typed `LogicalPlan` / `Expression` trees.

---

← [Back to ADR Index](../README.md)
