# Protobuf Plan Conversion (legacy path)

> **Status: existing implementation — runs behind `--transpiler legacy` (the default).** The authoritative v2 architecture supersedes this file where they conflict; the two paths coexist, so do not delete the legacy path to make room for v2. This file's v2 successor is listed in the legacy→v2 map in [`../README.md`](../README.md); the v2 spine is [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: Two-module converter mirroring the Java `RelationConverter` + `ExpressionConverter`**

- `relation_converter.rs` — converts prost-generated Spark Connect `Relation` to `LogicalPlan`
- `expression_converter.rs` — converts prost-generated `Expression` to our `Expression` enum
- `plan_converter.rs` — entry point, orchestrates both

Input: prost-generated types from Spark Connect protos.
Output: our typed `LogicalPlan` / `Expression` trees.

---

← [Back to ADR Index](../README.md)
