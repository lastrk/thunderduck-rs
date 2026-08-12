# ADR-003 — The intermediate representation is a proto-inspired common AST, extended incrementally, not full Catalyst LogicalPlan

> **Generator amendment (2026-08-09):** row-generating functions use a
> structured `Generator` value and a unary `Generate` operator. A generator in
> a SELECT projection is an unresolved, projection-only marker until analysis
> normalizes it to `Project` over `Generate`; it is never a scalar
> `FunctionCall`. SQL `LATERAL VIEW`, generator table syntax, and Spark Connect
> projections converge on the same operator. `Generate` appends its resolved
> output attributes to its child's output, applies an optional qualifier, and
> marks every generated attribute nullable when `outer` is set. Layer B emits
> the required DuckDB row-expansion shape from the structured kind.

**Status:** Proposed
**Depends on:** ADR-000, ADR-002
**Depended on by:** ADR-004, ADR-005, ADR-009, ADR-026

**Context.** Both front-ends — the Spark Connect proto deserializer and the SparkSQL parser (ADR-004) — must lower into a single representation that the analyzer (ADR-005/006) types and the emitter (ADR-009) translates. Two anchor facts shape the choice. First, in Spark itself the SQL parser and the DataFrame API converge on the same unresolved Catalyst `LogicalPlan`, so a common representation is known to exist. Second, SQL can express constructs the DataFrame/Connect-proto surface cannot (CTEs, `GROUPING SETS`/`CUBE`/`ROLLUP` in arbitrary combinations, `LATERAL VIEW`, `PIVOT`/`UNPIVOT`, recursive CTEs, richer window frames), so a strictly-proto-shaped AST is too small for SQL. But full Catalyst LogicalPlan is far larger than needed and carries resolution machinery ADR-002 delegates.

**Decision.** The target IR is the existing **Spark-Connect-proto-inspired AST**, **extended incrementally** to accommodate SQL-driven constructs, but deliberately kept short of a full Catalyst LogicalPlan reimplementation. Extension is bounded by a rule: **add a node type only when (a) SQL in the supported surface actually produces it AND (b) it is not expressible by composing existing nodes.** Constructs that fail (b) are represented with existing nodes (e.g. non-recursive CTEs as named/inlined sub-plans) or desugared in the retained B layer (ADR-007) when the desugaring is expressibility-forced (e.g. `GROUPING SETS` → `UNION ALL` of grouped aggregates if DuckDB lacks native support). Genuinely new, irreducible constructs (e.g. recursive CTEs, `LATERAL VIEW`/`explode` shape) get new nodes.

**Consequences.**
- (+) Reuses Spark's own proof that one representation suffices for both surfaces, without paying for full Catalyst.
- (+) The bounded-extension rule gives the IR a stopping condition, so "extend toward Catalyst" cannot drift into rebuilding the LogicalPlan the positioning (ADR-000) avoids.
- (+) SQL constructs that map to *existing* node types inherit the emission and inference rules for free — once both front-ends produce the same node, τ cannot tell SQL-origin from DataFrame-origin (the convergence guarantee).
- (−) Each genuinely new node added for SQL needs new emission-table coverage (ADR-009), possibly a new schema-derivation rule in the analyzer (ADR-005/006), and is subject to the LB5 expressiveness bet.
- (−) The convergence guarantee holds only if both front-ends normalize to the *same* node for the same semantics — a new obligation (Invariant INV7), since thunderduck has two front-ends where Spark has one shared `AstBuilder`.

**Refinement hooks.** Produce the empirical histogram: parse a SQL corpus (DuckDB SLT, Spark SQL test inputs, real workloads) with the existing SparkSQL parser and bucket results by "node types the current AST lacks." That histogram is the sized scope of the extension (likely a small high-frequency head — CTEs, set-op modifiers — plus a long rare tail to support or reject case by case). Decide per rare construct: support (new node), desugar (B layer), or reject (ADR-004 rejection path).

**Substrate split — behavior-carrying types vs pure value types.** CommonAST's expression-payload type is `crate::transpiler_v2::expression::Expression`, owned by τ. Value-level types (`DataType`, `StructType`, `StructField`) live in `crate::types::*` and are used verbatim by τ: they carry no behavior, so duplicating them would only risk conversion bugs at the boundary. This split is the reference for INV10 (§CV.5): behavior-carrying types are v2's; value types are shared.

**Plan-boundary metadata (ADR-026).** `CommonAst` also preserves each Connect node's optional `plan_id`. SparkSQL nodes carry `None`. A tagged node may be represented by its operator or a transparent boundary node, but it may not be collapsed or combined with another ID.

---

