# ADR-11: Protobuf Plan Conversion

**Decision: Two-module converter mirroring the Java `RelationConverter` + `ExpressionConverter`**

- `relation_converter.rs` — converts prost-generated Spark Connect `Relation` to `LogicalPlan`
- `expression_converter.rs` — converts prost-generated `Expression` to our `Expression` enum
- `plan_converter.rs` — entry point, orchestrates both

Input: prost-generated types from Spark Connect protos.
Output: our typed `LogicalPlan` / `Expression` trees.

---

← [Back to Architecture Overview](../architecture.md)
