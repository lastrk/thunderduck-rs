# Thunderduck Architecture Decision Records

**Subject:** Spark Connect → DuckDB SQL transliterator (`τ`), its front-ends, its analyzer, and its test architecture.
**Status of set:** Proposed — for ratification before / alongside reimplementation.
**Reference Spark version:** 4.1.1.

This document records the architectural decisions that emerged from design discussion. Each ADR is self-contained for individual review and carries explicit `Depends on` / `Depended on by` links. The ADRs are ordered as a logical narrative: ADR-000 establishes the product premise that selects the whole approach; ADR-001–002 state what τ is and what it delegates; ADR-003–004 define the intermediate representation and how both front-ends populate it; ADR-005–006 define the analyzer that resolves and types it; ADR-007–010 define how it is transformed and emitted; ADR-011–013 cover commands, the catalog, and external/lakehouse reads; ADR-014–016 are the testing architecture the rest makes possible; ADR-017–019 add the per-format write paths (Delta append, UC-managed Iceberg) and the end-to-end I/O contract; ADR-020 pins the emission target (strict-only, extension mandatory); ADR-021 pins the substrate boundary (τ owns its protobuf converter, Expression, TypeInferenceEngine); ADR-022 pins the runtime position (τ is the only path; no fallback; two error categories).

The **Cross-Validation** section at the end provides the dependency matrix, the tension points where decisions pull against each other, the load-bearing assumptions whose failure would cascade, and the cross-cutting invariants any refinement must preserve.

How to use this document: review each ADR on its own using its *Refinement hooks*; then run the *Cross-Validation* section to check that a change to one ADR doesn't silently violate another. The decisions are not independent — §CV makes the coupling explicit.

---

## ADR-000 — Positioning: instant-start, single-node, vertically-scaled, no-JVM DuckDB-backed Spark replacement

**Status:** Proposed
**Depends on:** — (premise)
**Depended on by:** ADR-001, ADR-002, ADR-003, ADR-004, ADR-005, ADR-006, ADR-013 (it is the premise that selects their shape)

**Context.** thunderduck-rs can occupy a specific niche: a drop-in Spark Connect server that, for workloads that fit on one large multi-core machine, replaces Spark without the JVM and without the Spark runtime — starting instantly, scaling vertically, and fully exploiting DuckDB's advanced vectorized execution engine. This positioning is not incidental; it *selects the entire implementation strategy*, because it rules out anything that puts a JVM or the Spark runtime in the serving path.

**Decision.** thunderduck is built as a **single-node, vertically-scaled, instant-start, no-JVM** Spark Connect server backed by DuckDB's vectorized engine. Of the three implementation strategies considered (below), this selects **Alternative 2: reimplement the necessary front-end and analyzer slice in Rust, target a Rust IR, and translate to DuckDB SQL**, which is the state thunderduck-rs is already in.

**Alternatives considered (and rejected).**
- *Alternative 1 — embed a minimal Spark front-end (SparkSQL parser + Catalyst analyzer) on the JVM, parse to a real Catalyst plan, translate Catalyst → DuckDB SQL in Java.* Tempting on pure-correctness grounds: it deletes the analyzer-reimplementation problem entirely (you get Spark's real type inference and nullability for free, and SQL/DataFrame provably converge on one representation). **Rejected** because it puts a Spark-Catalyst-bearing JVM in the serving path, violating the instant-start / no-JVM / no-Spark-runtime premise at the root; it also couples hard to non-public Catalyst internals across versions. The correctness win does not survive the positioning constraint.
- *Alternative 2 — reimplement a minimal subset in Rust (SparkSQL parser + the divergent analyzer slice), target a Rust IR, translate IR → DuckDB SQL.* **Chosen.** Maximum control, minimum runtime footprint, pure Rust, in-process DuckDB, instant start. Cost: thunderduck owns a parser and a faithful-to-Spark type/nullability analyzer (the hardest components), validated forever against Spark — accepted as the price of the positioning.
- *Alternative 3 — hook DuckDB into Spark as a native execution backend (Gluten/Comet-style), offloading Spark's physical operators.* **Rejected** as a different product entirely: it keeps the full Spark runtime (Spark does parse/analyze/optimize down to a physical plan), discards the "DuckDB SQL as the target, DuckDB as the optimizer" thesis, and targets DuckDB's execution internals (which DuckDB is not architected to expose the way Velox is). Maximizes Spark fidelity at the total cost of the lightweight, no-JVM positioning.

**Non-goals (scope fences this premise establishes).** No distributed / multi-node execution; no shuffle across machines; no JVM in the serving path; no Spark runtime dependency at serving time; no RDD-style or low-level Spark APIs. Future "does feature X fit?" questions resolve by appeal to this ADR: X must serve the single-node, vertical, instant-start, no-JVM goal.

**Consequences.**
- (+) Selects Alternative 2 decisively and records *why* the more correctness-convenient Alternative 1 is nonetheless rejected, so it need not be relitigated.
- (+) Gives every downstream ADR a premise to appeal to; in particular it is the reason thunderduck *owns* the analyzer slice (ADR-005/006) rather than embedding Spark's.
- (−) Commits thunderduck to reimplementing Spark's type/nullability semantics in Rust (ADR-005/006) — the hardest, highest-risk work — because the cheap alternative (real Catalyst on the JVM) is positioning-incompatible.
- (neutral) The niche is explicitly "fits on one big machine"; workloads that genuinely need distributed execution are out of scope, not a failure.

**Refinement hooks.** Confirm the single-node ceiling is acceptable for the target workloads. If a JVM in the serving path ever becomes acceptable, Alternative 1 should be reconsidered, since it would delete ADR-005/006's entire reimplementation burden. Confirm there is no requirement that forces distributed execution.

---

## ADR-001 — τ is a transliterator, not an optimizer

**Status:** Proposed
**Depends on:** ADR-000
**Depended on by:** ADR-007, ADR-008, ADR-011, ADR-015

**Context.** The same query result can be produced by many query plans. A translator could either emit the plan as given and let the engine optimize, or perform its own cost-driven rewrites. thunderduck sits in front of DuckDB, whose optimizer is assumed competent (ADR-014 assumption), and the positioning (ADR-000) is to lean on DuckDB's engine rather than rebuild planning.

**Decision.** τ performs no cost-driven transformations: no predicate pushdown, no join reordering, no constant folding, no common-subexpression elimination, no decorrelation-for-efficiency. It changes operator structure when forced by expressibility (a Spark DataFrame operator has no direct DuckDB surface form), and it *may* additionally perform **result-irrelevant cosmetic simplifications** — transformations that produce strictly less SQL (fewer nodes; no operator reordering; no change to what DuckDB's optimizer can subsequently see) and that DuckDB would discard anyway. All cost-driven optimization is delegated to DuckDB.

So the permitted transformations fall in three categories: *expressibility-forced* (required), *cosmetic* (optional, result-irrelevant, strictly-reducing, syntactic), and *enumerated correctness-forcing carve-outs* (rare; see Refinement hooks). Everything cost-motivated is forbidden.

**Guardrail for cosmetic simplifications.** A cosmetic simplification must be (a) provably result-irrelevant, (b) strictly node-reducing, (c) purely *syntactic* — operating on the SQL/plan shape without consulting cost or statistics — and (d) non-reordering (it may not move an operator relative to another, since that is the optimizer's job). Qualifying examples: nested-alias elimination, collapsing `SELECT *` over `SELECT *`, redundant-parenthesis removal. Disqualifying: anything that relocates an operator (predicate pushdown shapes, projection pruning that changes what reaches the optimizer).

**Consequences.**
- (+) Reduces τ to a near-mechanical mapping whose decisions are mostly node-local, making them enumerable and testable (ADR-009, ADR-014).
- (+) Cosmetic simplification improves emitted-SQL readability and snapshot-test stability without affecting results.
- (+) The engine assumed correct does the cost-driven work it is best at.
- (−) Emitted SQL may still be naïve in shape; correctness depends on DuckDB's optimizer actually handling it well (acceptable under the ADR-014 assumption).
- (−) The bright line is now three clauses rather than one (forbidden cost-driven; permitted cosmetic; enumerated carve-outs), so the cosmetic set and the carve-out set must each be enumerable and reviewed, or ADR-001 erodes silently.
- (neutral) Forces a discipline: every structural transformation must be justified as expressibility-forced, result-irrelevant-cosmetic, or a recorded carve-out — never "this is faster."

**Refinement hooks.** A narrow **carve-out** is permitted for transformations that are *correctness-forcing yet optimization-shaped* — where DuckDB produces a wrong-vs-Spark *result* (not merely a slower one) without the transformation. Each carve-out requires: (a) a written justification that it is necessary for correctness, (b) confirmation that no expressibility-forced or cosmetic framing achieves the same end, and (c) recording as a *named exception in this ADR*, so the carve-out set is enumerable and reviewable (the same discipline as the Option-C escape hatches in ADR-007). If the carve-out set grows past a handful, that is a signal the transliterator framing is wrong and this ADR should be revisited. (See Tension T2.) As the problem structure becomes better understood, expect this ADR to be revised.

*Carve-out register (currently empty):* — none.

---

## ADR-002 — Emit-level delegation: own only the slice where Spark diverges from DuckDB

**Status:** Proposed
**Depends on:** ADR-000, the DuckDB-correctness assumption (formalized as LB2 in §CV)
**Depended on by:** ADR-003, ADR-005, ADR-007, ADR-013

**Context.** Resolution work (name binding, scope resolution, star expansion, column-to-table association, type inference, nullability) could be reimplemented in thunderduck or delegated to DuckDB's binder. DuckDB's binder is correct for dialect-agnostic SQL semantics; its *type* and *nullability* answers are not Spark's. The positioning (ADR-000) pushes toward delegating whatever DuckDB already does correctly, to keep the owned surface minimal.

**Decision.** thunderduck delegates structural resolution to DuckDB at the *emission* level — it emits `SELECT *`, emits unqualified/qualified names, and lets DuckDB's binder expand and resolve — and reimplements only the slice where Spark's semantics diverge observably from DuckDB's. That divergent slice is type inference and nullability (ADR-005).

**Consequences.**
- (+) Every piece of resolution thunderduck doesn't do is a piece that can't diverge and needs no maintenance.
- (+) Minimal analyzer surface: thunderduck reimplements the *smallest* slice that achieves parity — and justifies keeping the IR (ADR-003) short of full Catalyst, since the resolution machinery a full LogicalPlan would carry is delegated.
- (−) The delegation boundary is clean only where the delegated (structural) result is not an *input* to the owned (semantic) computation — and type inference *does* depend on resolved structure (Tension T1).
- (neutral) "Wherever DuckDB's semantics already match" is an assumption that must be validated empirically, not asserted (Load-bearing LB3, validated via ADR-015's AnalyzePlan diff).

**Refinement hooks.** The exact membership of "the divergent slice" is the keystone of the whole architecture (LB1). It is assumed to be {type inference, nullability}; if structural resolution also diverges for some construct, this boundary moves and ADR-005's scope grows.

**Delegation boundary at v2's edge.** Where v2 does not delegate (owns) and does not implement, τ produces a typed error rather than emit partial or synthetic SQL. Two categories: **Spark-emulated errors** (Spark itself would reject the input; τ matches Spark's error semantics) and **Thunderduck-boundary errors** (Spark accepts the input but τ has not implemented it — honest "not implemented in Thunderduck," where Thunderduck-specificity leaks through the Spark Connect facade deliberately). See ADR-022 for the full contract.

---

## ADR-003 — The intermediate representation is a proto-inspired common AST, extended incrementally, not full Catalyst LogicalPlan

**Status:** Proposed
**Depends on:** ADR-000, ADR-002
**Depended on by:** ADR-004, ADR-005, ADR-009

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

---

## ADR-004 — SQL and DataFrame both lower to the common AST; relation-vs-command is decided by parse-tree root

**Status:** Proposed
**Depends on:** ADR-000, ADR-003
**Depended on by:** ADR-005, ADR-011, ADR-015
**Resolves:** OQ-1 (raw-SQL handling)

**Context.** `spark.sql("…")` is a hard requirement. The Spark Connect client is thin: it ships the raw SQL string verbatim inside a `SqlCommand` (plus parameters); parsing and analysis are server-side. Critically, the wire message does **not** indicate whether the SQL is a query (relation) or a side-effecting statement (command) — the client cannot know without parsing. A prior approach of bashing SparkSQL into DuckDB SQL with regex/string fixups (OQ-1 option c) was tried and failed (an unmaintainable, ever-growing regex chasing dialect mismatches).

**Decision.** thunderduck parses SparkSQL with its own Rust parser and **lowers it into the same common AST** (ADR-003) that the Connect-proto deserializer produces. Raw SQL therefore flows through the *same* analyzer (ADR-005/006) and the *same* τ emission (ADR-009) as DataFrame plans — one translation path, two front-ends. The **relation-vs-command discrimination is computed from the parse-tree root**, exactly as Spark does: a query/SELECT-rooted statement routes into the relation path; a DDL/DML/catalog-statement root routes into the command path (ADR-011, with its catalog-state oracle). This closes OQ-1 in favour of option (b), and explicitly rejects option (a) reject-raw-SQL (incompatible with the `spark.sql` requirement) and option (c) string-bashing (tried, failed).

**Consequences.**
- (+) Raw-SQL `SELECT`s get the *same* Spark-parity guarantees (ADR-005/006 type inference, ADR-009/010 emission + extensions) as DataFrame `SELECT`s, because both become the same analyzed AST.
- (+) Eliminates the "two translation paths" risk: there is one τ; the SQL parser is just a second front-end feeding the common AST.
- (+) The discriminator is principled (parse-root, mirroring Spark) rather than a fragile heuristic; the wire's silence on relation-vs-command is handled the only correct way.
- (−) thunderduck owns a SparkSQL parser whose fidelity to Spark's grammar (precedence, identifier rules, implicit behaviours) must be maintained and validated forever against Spark — a substantial, version-sensitive component (consistent with the ADR-000 cost).
- (−) Reinforces INV7 (both front-ends must normalize to the same AST node for the same meaning), now the load-bearing soundness condition for the whole common-AST approach.

**Refinement hooks.** Confirm the SparkSQL parser's coverage against the supported surface (it already exists in thunderduck-rs; size the gap via ADR-003's histogram). Define the parse-root → relation/command routing table. Verify the `ExecutePlanResponse` plumbing for how the server returns a relation result vs a command result (the response-field detail flagged during design). Validate front-end agreement (INV7) via the AnalyzePlan schema diff (ADR-015): parse SQL in thunderduck, send the same string to reference Spark, compare resolved schemas.

**Two v2 front-ends, one CommonAST.** τ has two front-ends: `V2RelationConverter` (Spark Connect protobuf → CommonAST) for DataFrame calls, and the v2 SparkSQL lowering (raw SQL text via `spark.sql(...)` → CommonAST) for statement-shaped calls. Both produce the same CommonAST for a given semantic input. Agreement between them is transitive from each front-end's independent Spark-parity validation via ADR-015's differential oracle (no per-run invariant check is imposed).

---

## ADR-005 — thunderduck owns Spark type and nullability inference (the divergent slice), as a schema-threading analysis over the common AST

> **Amendment (2026-07-12, ADR-024):** the threaded analyzer schema is now the τ-owned `ResolvedSchema(Vec<Attribute>)` — each attribute carries its resolved type, nullability, stable `ExprId`, and source-qualifier lineage. `StructType` remains the wire/value type, produced at τ's public entry points.

**Status:** Proposed
**Depends on:** ADR-000, ADR-002 (defines the boundary), ADR-003 (the IR it annotates), ADR-004 (must serve both front-ends), ADR-012 (catalog seed)
**Depended on by:** ADR-006, ADR-007, ADR-009, ADR-010, ADR-012, ADR-014, ADR-015, ADR-017, ADR-018, ADR-023

**Context.** Every dispatch decision in τ keys on resolved Spark types (ADR-009). The common AST (ADR-003) arrives unresolved (`UnresolvedAttribute`, `UnresolvedRelation`) from both front-ends; types and nullability live in the catalog and in Spark's analyzer rules, not in the AST. DuckDB's native inference gives DuckDB types, which diverge from Spark. Per ADR-000, embedding real Catalyst (which would supply correct types) is rejected, so thunderduck must reimplement this slice.

**Decision.** thunderduck implements a schema-threading analysis `infer : (CommonAST, BaseTypes) → TypedAST` that propagates a Spark-typed schema through every operator, so operand types and nullability are known at every expression node, for plans from *either* front-end. Two named sub-units carry the Spark-specific weight: the **type-coercion lattice** (implicit casts, least-common-type, decimal precision/scale propagation) and the **nullability derivation** (outer-join null-extension, CASE/COALESCE, aggregate nullability). The pass *knows* the schema everywhere (it internally resolves references and expands `*` for type-tracking) even though it *emits* delegated structure (ADR-002). This ADR fixes the *scope* (what thunderduck owns); ADR-006 fixes the *structure* (how the analysis runs).

**Consequences.**
- (+) Makes the emission table correct: dispatch keys on Spark-accurate types. This is the precondition for everything downstream — it is foundational correctness, not a feature.
- (+) Confines the reimplemented analyzer to the minimal divergent slice (ADR-002), and serves SQL and DataFrame uniformly because both are the same common AST (ADR-003/004).
- (−) This is the largest and most correctness-critical component, and it must match *Spark's* analyzer specifically, not merely be internally consistent.
- (−) Per-operator schema-derivation rules (esp. outer-join nullability rewrite and aggregate nullability) are subtle and diverge from a naïve "just track types" implementation.
- (neutral) Commits to emit-level delegation, analysis-level ownership. The internal resolver/star-expander exists for schema derivation only and must never be removed on the grounds that resolution/star-expansion is delegated (Invariant INV5).

**Refinement hooks.** Decompose the coercion lattice and the nullability derivation as separately-testable named units. Validate against ground truth (ADR-015's AnalyzePlan schema diff) *before* the emitter has to be correct in concert. Highest-risk paths: a typed expression above a delegated, unexpanded structural construct (e.g. `amount * 1.1` over a starred join) — the pass must thread `amount`'s Spark type through internally. Extraction of the coercion/decimal/nullability rules from Spark sources may be LLM-accelerated, but no rule enters thunderduck until the AnalyzePlan diff is green for it (this is INV4 applied to rule provenance).

**Symmetric-omission discipline.** The analyzer's function-name enumerations must be kept internally consistent: any function name added to `aggregate_return_type` must also appear (or be justified absent) in `aggregate_is_non_nullable` / `aggregate_is_always_nullable`, and vice versa. Same rule applies for the SQL parser's `is_aggregate_function` classifier. Missing entries in one enumeration but present in another produce silent wrong types or nullability. This is a codified design constraint, not a lint — reviewers of any change touching these tables must confirm parallel updates.

---

## ADR-006 — The analyzer is a bounded sequence of coordinated passes, not an iterate-to-fixed-point engine

> **Amendment (2026-07-12, ADR-024):** the resolve pass additionally mints/propagates attribute `ExprId`s and stamps them onto resolved references alongside ordinals. The 0/1/2+ match-count error semantics are unchanged.

**Status:** Proposed
**Depends on:** ADR-000, ADR-005
**Depended on by:** ADR-007, ADR-023

**Context.** Spark's Catalyst analyzer applies rules in `FixedPoint` batches — it re-runs each batch until the tree stops changing — because Catalyst's rules are deliberately small, self-contained, and uncoordinated, so global behavior emerges only from iteration, and because the optimizer keeps mutating the tree (requiring re-analysis). thunderduck does **neither** of those things: it does not optimize (ADR-001), so there is no optimization-churn re-analysis; and it can write analysis as a single coordinated pass rather than uncoordinated re-scanned rules. The question is therefore which of Catalyst's fixed-points are *essential to the analysis* versus *artifacts of Catalyst's architecture*.

**Decision.** thunderduck implements the analyzer (ADR-005's `infer`) as a **bounded, known sequence of coordinated passes**, not an open-ended iterate-to-fixed-point loop. Most of Catalyst's analyzer fixed-points are recoverable by a single coordinated **bottom-up** pass that computes resolution, function-binding, type coercion, and nullability *together* in dependency order, because those facts flow strictly **upward** (leaf-to-root) — where information flows one way through a DAG, a topologically-ordered single pass computes the fixed-point directly without iteration. The exceptions are analyses whose information does **not** flow purely upward, which require explicit extra passes or staged ordering (a small, fixed, a-priori-known number — never an unbounded loop): notably **set-operation type widening** (`UNION`/`INTERSECT`/`EXCEPT` least-common-type must be computed across sibling branches then pushed *back down* into both branches' projections), **correlated-subquery scoping** (the inner plan resolves against the *outer* scope, an outside-in dependency requiring outer-before-inner staging), and **aggregate/HAVING/ORDER-BY-over-alias** (sideways dependency on a sibling clause's resolved aggregate/alias). The pass structure replicates each rule's required **information-flow direction** (up for most; down for set-op widening; outside-in for correlation), which is the semantically essential part; Catalyst's iteration *mechanism* is not replicated.

**Consequences.**
- (+) A bounded, coordinated-pass analyzer is far simpler and more analyzable than Catalyst's iterate-to-fixed-point engine, and the number of passes is known a priori, not "run until nothing changes."
- (+) Eliminating optimization-churn re-analysis (ADR-001) removes an entire class of Catalyst's iteration reasons that simply do not apply to thunderduck.
- (−) Getting a rule's information-flow direction wrong (treating a set-op-widening or correlated case as if it were upward-only) produces wrong types/nullability — a real bug class.
- (−) Set-op widening and correlated scoping genuinely require more than one sweep; the analyzer is "single coordinated pass" only for the upward-flowing majority, with explicit staged passes for the non-upward minority.
- (neutral) When extracting Spark's rules (ADR-005), the rule *content* (formulas, least-common-type table, nullability derivations) transfers, but Catalyst's iteration *mechanism* is deliberately not transferred; what must be preserved is the flow direction each rule needs.

**Refinement hooks.** Enumerate the pass sequence explicitly (e.g. CTE/substitution staging → unified resolve-and-type bottom-up pass → set-operation widening down-propagation → correlated-subquery outer-first staging). For each analysis rule, classify its information-flow direction (up / down / outside-in) and confirm against the actual Spark 4.1.1 rule set whether any rule beyond the named three is non-upward. The AnalyzePlan differential (ADR-015) is the backstop: a divergence localizes the rule whose flow direction was modelled wrongly.

**Set-op widened schema wins at emission time.** The downward set-op sub-sweep (UNION / INTERSECT / EXCEPT) produces a widened schema on the set-op node. At emission time, that widened schema wins over any child projection's declared cast target. Concretely, if one child projection declares `CAST(a AS DECIMAL(5,0))` and its sibling declares `CAST(b AS DECIMAL(10,2))`, the parent's widened schema is `DECIMAL(10,2)` and the emitter wraps each child's projected column in `CAST(... AS DECIMAL(10,2))` regardless of the child's declared cast target. This is not a "clean-up" cast; it is the load-bearing rule for set-op parity with Spark, whose analyzer computes the widened set-op type before the child projections' types are pinned. Codified: the analyzer's sub-sweep computes the widened schema; the emitter's `render_union` / `render_intersect` / `render_except` applies a per-column CAST wrapper from that widened schema. Neither pass may defer the CAST to the other — the analyzer does not rewrite child projections in place; the emitter reads the widened schema on the parent, not the child-declared cast.

**Ordinal reference resolution (ADR-023).** The resolve pass performs name resolution exactly once, validator-style — a scope maps each (qualified or bare) name to (input, ordinal) with match count 0 → `UnknownColumn`, 1 → bound, 2+ → `AmbiguousColumn` — and downstream passes and emission reference columns by ordinal; qualifiers are regenerated at emission against the then-current alias, never carried as analysis facts.

---

## ADR-007 — τ structured as three layers A / B / C; B is retained but currently minimal

**Status:** Proposed
**Depends on:** ADR-001, ADR-002, ADR-005, ADR-006, ADR-008
**Depended on by:** ADR-009

**Context.** Translation decisions vary in how much context they need. Some are functions of the node alone; some depend on resolved facts that can be pushed into the node; a few depend on environment state or surrounding structure. A prior framing held that the no-optimization constraint (ADR-001) collapses the tree-rewrite layer entirely. That collapse is *constraint-dependent, not proven* — there is no mathematical or other guarantee that no genuinely structural transformation will ever be needed (and ADR-003's SQL-driven desugarings, e.g. `GROUPING SETS`, are exactly such cases).

**Decision.** τ is structured as three layers. **A (resolve/annotate)** is the analysis of ADR-005/006, pushing decision-relevant facts (Spark types, nullability, correlation level) into each node so emission decisions become node-local. **B (tree-rewrite / forced transliteration)** is *retained as an explicit layer* even though it is currently empty of cost-driven rules and minimal in forced ones: it contains only expressibility-forced operator transliterations (no correlation rewrite per ADR-008, no optimizations per ADR-001), most of which are operator-identity-triggered and could fold into the flat emission table — but the layer is kept because (a) its emptiness is constraint-dependent not proven, and (b) it is the designated home for genuinely *structural* forced transliterations (consume a subtree, emit a multi-node SQL shape, e.g. `explode` → `UNNEST`, or SQL-driven desugarings per ADR-003) and for any ADR-001 correctness-forcing carve-outs. **C (escape hatch)** is a small, labeled, instrumented set of decisions depending on environment state not carried in the node pattern (e.g. session timezone driving a timestamp cast).

**Consequences.**
- (+) Retaining B preserves the architectural seam at near-zero cost (an empty rule slice), so a future genuinely-structural transliteration (including SQL desugarings) has a home that is not the flat emission table.
- (+) B is stable and slow-growing (bounded by Spark's operator inventory, not by the expression surface), so it stays as hand-written rules rather than a data-driven pattern DSL — the deduplication and frequency arguments for a pattern DSL do not apply at its size.
- (+) B is the single designated container for structural forced transliterations, SQL-driven desugarings, and ADR-001 carve-outs — keeping them out of the flat table where they would break its node-local audit story.
- (−) C is opaque to audit and not auto-coverable by directed synthesis; covering a C decision needs a hand-written witness.
- (neutral) The count of C entries — and now also the count of non-empty B rules — is a metric to watch: growth signals more non-local coupling than the model assumes, signalling design drift.

**Refinement hooks.** Confirm any B entry is either expressibility-forced (operator-identity-triggered, foldable into the table) or a genuinely structural forced transliteration / SQL desugaring (stays in the rule list). Keep C minimal and counted. Decide whether the operator-identity-triggered B entries live in the emission table via a richer `Emission` variant or in the B rule list; the genuinely structural ones (multi-node output) belong in the rule list regardless. Revisit if the B rule count grows past a handful.

---

## ADR-008 — Correlated subqueries are emitted directly as DuckDB correlated subqueries

**Status:** Proposed
**Depends on:** ADR-001
**Depended on by:** ADR-007

**Context.** Spark correlated subqueries could be emitted verbatim or rewritten (e.g. to lateral joins / EXISTS). DuckDB supports correlated subqueries directly.

**Decision.** τ emits Spark's correlated subquery structure verbatim and lets DuckDB handle it. No rewrite to lateral.

**Consequences.**
- (+) Consistent with ADR-001: rewriting to lateral would be a restructuring DuckDB does not require, hence forbidden.
- (+) The correlation non-locality stops being a *transformation* concern entirely.
- (neutral) Correlation remains an *annotation* concern: the analyzer (ADR-005/006) must know a reference is correlated to emit the right qualified name and must stage outer-before-inner resolution (ADR-006) — but it produces no rewrite. This is the concrete case that proves ADR-001's discipline.

**Refinement hooks.** Confirm DuckDB's correlated-subquery support covers every correlation shape Spark produces. If some shape is not expressible, *that specific shape* becomes a forced transliteration (ADR-007 B), and only that shape — not correlation in general.

---

## ADR-009 — The emission table is declarative data, and is both the input-grammar backbone and the coverage denominator; compiled dispatch

**Status:** Proposed
**Depends on:** ADR-003, ADR-005, ADR-007
**Depended on by:** ADR-014, ADR-015

**Context.** τ's translation decisions could be opaque control flow or declarative data. Separately, the test harness needs both a generation grammar and a coverage denominator. Generating from the Spark Connect *proto* grammar over-produces (the proto is far more permissive than the DataFrame client); generating from the *API signature* gives realistic, well-typed-by-construction inputs.

**Decision.** Decision sites are inspectable values keyed on `(spark_op, resolved_operand_types, mode, nullability)`, with the emission target (native DuckDB op, native-with-casts, or extension function) as data. This table — aligned with / derived from the supported Spark DataFrame API signature — is simultaneously the backbone of the input grammar and the coverage denominator. The table dispatches on common-AST node types (ADR-003), so it serves SQL- and DataFrame-origin plans identically.

Production dispatch uses the **compiled approach**: a build-time procedural macro (or `build.rs` codegen) consumes the declarative `&[DecisionSite]` table and emits the dispatch function as generated source, so the table is the input and the compiled control flow is the output and the two cannot drift. The **interpreted approach** (the table walked and matched at runtime) is recorded as a considered alternative and retained as a **fallback** to be adopted only if the compiled approach fails for functional reasons (e.g. a guard form that cannot be expressed in codegen) or non-functional ones (e.g. unacceptable build-time cost, macro-maintenance burden, or tooling friction). A switch to the fallback is a deliberate, recorded decision, not a silent drift, and it re-incurs the drift risk the compiled approach removes (see Alternatives).

**Consequences.**
- (+) A single artifact defines what is generated for testing and what coverage is measured against, making 100% decision coverage *reachable* rather than aspirational, and making "support a new function" an additive row rather than a control-flow edit.
- (+) Directed witness synthesis can read a site's guard and generate a covering input (for `Pat` sites; not for C escape hatches).
- (+) Compiled dispatch makes the table the sole determinant of runtime behavior by construction, and lets the guard-overlap invariant run at macro-expansion time — an ambiguous table becomes a *compile error*, not a test that must be remembered. INV6 (extension-target existence) can likewise be enforced at compile time.
- (−) Guard ambiguity (two patterns matching one node) needs an explicit resolution order and a mandatory non-overlap check — provided here at compile time, but the resolution-order policy still has to be defined.
- (−) The compiled route assumes guards are codegen-able, which holds for `Pat` but **not** for the Option-C `Dynamic` escape hatches — those stay hand-written and instrumented, consistent with their already being outside the auditable-table story.
- (−) Adds build-time machinery (a proc-macro or `build.rs`) with its own maintenance surface; if that surface proves too costly, the interpreted fallback exists, at the price of re-incurring drift risk.

**Alternatives considered.**
*Interpreted dispatch (the table matched at runtime) — considered, not chosen; retained as fallback.* The reason it is not the default is drift risk that survives unit-testing: with a runtime interpreter, the table and the *dispatch semantics* are two separate things that must agree, and unit tests check the table's *outputs*, not that the table is the *sole determinant* of dispatch. Under deadline, someone adds a special case as an `if` *wrapping* the table lookup; production behavior now diverges from "what the table says" while the table-exercising unit tests still pass, because they exercise the table in isolation rather than the wrapped dispatch. The drift is between the table-as-audited-artifact and the actual runtime decision, and it is invisible to tests that treat the table as ground truth. The compiled approach removes this gap by construction; the interpreted approach reintroduces it, which is why it is fallback-only.
*Lighter-weight cross-check (hand-written dispatch + an agreement test) — considered.* Keep hand-written dispatch but add a test asserting it agrees with the table over an exhaustive enumeration of `(op × type-class × mode)`. Recovers most of the compiled approach's safety without a macro, at the cost of the enumeration's completeness being a manual responsibility. Available as an intermediate option if the full macro is deemed too heavy but the interpreted fallback's drift risk is unacceptable.

**Refinement hooks.** Decide the codegen mechanism (proc-macro vs `build.rs`). Define the guard resolution-order policy (specificity vs declared priority) and make the non-overlap check a compile-time error. Confirm `Dynamic` (Option-C) sites are handled outside codegen and remain instrumented. Establish the trigger conditions under which the interpreted fallback would be adopted, so that switch remains a recorded decision rather than an ad-hoc one.

**Dispatch shape: hand-written `match` arms over the analyzed AST discriminant** (and, for `FunctionCall`, over the lowercased function name). Arms are trivial 3-to-5-line format strings; total arm count is bounded (~130 scalar-function arms + ~15 operator arms + ~10 aggregate arms). LLVM lowers the case-insensitive name lookup to length-bucketed `memcmp`; every arm is inspectable at review time. INV3's coverage anchor is the set of `render_*` helper names, greppable from source. Compiled-dispatch codegen (proc-macro / `build.rs`) is a considered-and-rejected alternative at this scope; a future need for it would reopen this ADR.

**Anti-pattern: declarative row data structures without a live interpreter.** Rows that no runtime dispatch consults are dead data; they inflate INV3's coverage denominator with false coverage and drift silently from the `match`-based dispatch. Full Approach B (rich declarative table + interpreter) is a legitimate alternative *if* both land together; half-declarative is worse than either fully hand-written or fully interpreted.

---

## ADR-010 — Extension functions are a minimal gap-filler for Spark/DuckDB divergence, implemented in the C++ extension project

**Status:** Proposed
**Depends on:** ADR-005
**Depended on by:** ADR-015, ADR-016

**Context.** Most Spark expressions translate directly to native DuckDB ops. A minority cannot, for one of two reasons. Either DuckDB's native behavior diverges from Spark in the *value* it computes — rounding mode, decimal arithmetic, ANSI overflow, specific date/string semantics — or DuckDB diverges in the *result type* it returns, e.g. decimal-divided-by-decimal returns `double` in DuckDB but a `decimal` (with Spark's precision/scale) in Spark. Or DuckDB lacks the operator entirely.

**Decision.** τ uses thunderduck-provided extension functions *only* where direct translation cannot match Spark — predominantly numerical semantics, return-type divergences, and nullability. They are emission *outcomes* for a small subset of dispatch cells, not the coverage scope. The extension functions are **implemented in C++ as part of the [`thunderduck-duckdb-extension`](https://github.com/lastrk/thunderduck-duckdb-extension) DuckDB extension project**, a separate build artifact loaded into DuckDB; τ's emission of an `Extension(name)` call is correct only if the corresponding C++ function exists, is loaded, and faithfully implements Spark semantics.

**Consequences.**
- (+) Keeps the bespoke Spark-reimplementation surface as small as possible — extension functions are the highest-risk-per-cell component, so minimizing them minimizes risk.
- (+) Each extension function exists for a specific semantic mismatch; the *why* annotation names the edge values that matter for testing it (ADR-015).
- (+) Return-type divergences are handled *jointly* by inference (ADR-005 must infer the Spark result type — e.g. that decimal÷decimal is decimal with Spark's precision formula) and emission (ADR-010 must emit a function that produces both the right value and that Spark type), tightening the ADR-005 ↔ ADR-010 coupling (Tension T3).
- (−) Extension functions are bespoke reimplementations of Spark semantics and must be covered across their full input edge-value set, with the same differential validation as everything else (edge ADR-010 → ADR-015).
- (−) Introduces a new external build/version dependency: the C++ extension's exported function set and the dispatch table's `Extension(...)` targets must agree (INV6), and version coordination now spans three artifacts — Spark 4.1.1, the dispatch table, and the C++ extension (edge ADR-010 → ADR-016).
- (neutral) Coverage is over the *whole* translation surface; extensions are a minority of the emission outcomes within it.

**Refinement hooks.** The boundary between "a cast fixes the mismatch (stay native)" and "this needs an extension function" is a sub-decision living between ADR-005 and ADR-010 (Tension T3): prefer casts where a cast sequence reproduces Spark semantics *exactly* (both value and type); use an extension function only where no cast sequence does — which is exactly the return-type-divergence case, since a cast on a wrong-typed native result may not recover Spark's value. Document, per divergence, which mechanism is used and why; annotate every extension function with the mismatch it addresses. Define how the C++ extension's function set is kept in lockstep with the dispatch table's `Extension(...)` targets (INV6) and how the extension's behavior is differentially validated against Spark.

---

## ADR-011 — The Spark Connect `Command` arm is in scope as a separate path with a state-diff oracle

**Status:** Proposed
**Depends on:** ADR-001, ADR-004
**Depended on by:** ADR-012, ADR-017, ADR-018

**Context.** The protocol's top-level `Plan` is `oneof { Relation root; Command command }`. Commands (CreateTable, WriteOperation / saveAsTable / insertInto, CreateView, RegisterFunction, catalog mutations) are side-effecting statements, not query-producing relations. Additionally, statement-rooted raw SQL (ADR-004) routes here after parsing. The relation-focused expression test matrix scoped commands out.

**Decision.** Statement-shaped operations translate to DuckDB DDL/DML via a parallel `emit_command` path, with the same transliterate-don't-optimize (ADR-001) and forced-transliteration (ADR-007) discipline. Statement-rooted SQL from ADR-004's parser is routed here by parse-root. Their differential oracle is **catalog/table state**, not result rows: run on both engines, compare resulting catalog/table state.

**Consequences.**
- (+) Closes a real gap — commands were silently excluded from the architecture — and gives statement-rooted raw SQL a home.
- (−) Requires a second test harness (state diff) distinct from the expression-result matrix.
- (neutral) Where Spark write semantics (mode handling: overwrite/append/errorIfExists/ignore; partitioning; bucketing) have no DuckDB equivalent, they become forced transliterations or rejection cases.

**Refinement hooks.** Enumerate the supported command surface and the rejection set (raw-SQL handling itself is now resolved by ADR-004). Define the catalog/table-state comparison precisely. Verify the `ExecutePlanResponse` shape for command vs relation results.

---

## ADR-012 — thunderduck owns a narrow catalog overlay; commands write it, resolution reads it

**Status:** Proposed
**Depends on:** ADR-011 (writers), ADR-005 (reader/seed) — see the ADR-005/012 cycle in §CV
**Depended on by:** ADR-005, ADR-013, ADR-015

**Context.** Resolution (ADR-005) needs Spark types of base columns to seed inference. Commands (ADR-011) mutate what relations later resolve against. A full Spark shadow catalog is more than needed, since DuckDB's catalog handles existence and binding.

**Decision.** thunderduck maintains a *narrow* overlay of Spark types on base relations, consulted by the inference pass and updated by commands. It is shared state between the command arm and the relation arm. DuckDB's catalog continues to handle existence and binding; the overlay carries only the Spark-semantics facts the divergent slice needs.

The overlay additionally records, per base relation, its **access provenance** — `native` (a DuckDB-store table), `path-scan` (a Hive-Parquet / bare Delta or Iceberg path), or `attached-catalog` (an Iceberg-REST / Unity-Catalog attachment) — and, for the non-native cases, the **format** (Parquet / Delta / Iceberg / UC). Provenance is load-bearing because it determines *both* the emission surface (ADR-013) and the **type-source**: for `native` relations the overlay maps DuckDB catalog types → Spark types; for format-backed relations the source of truth is the *format's own* schema (Iceberg and Delta each have their own type systems), which should be mapped **directly** to the Spark type Spark would assign when reading that table, not laundered through a DuckDB-type intermediate that may not round-trip precision/timezone/nullability faithfully.

**Consequences.**
- (+) Minimality again — own only the catalog facts the divergent slice requires.
- (−) This overlay is the seam where the Spark-type ↔ DuckDB-type mapping lives and where command/relation synchronization must be kept consistent; the most insidious resolution-divergence bugs will hide here.
- (−) Format-backed relations introduce a *second* type-source (the format schema) and a direct format-type → Spark-type mapping distinct from the DuckDB-type mapping — more mapping tables to maintain and validate.
- (neutral) Establishes the differential-harness precondition: identical Spark base-column types pinned on both engines (ADR-015).

**Refinement hooks.** Decide whether the overlay is materialized alongside DuckDB's catalog or computed by mapping DuckDB types back to Spark types on read. Define the Spark↔DuckDB type mapping table explicitly, and separately the direct Iceberg-type→Spark-type and Delta-type→Spark-type mappings for format-backed relations (ADR-013). Specify how commands keep overlay and DuckDB catalog in sync. Record access provenance + format per relation.

**`BaseTypes` overlay is fallback-only for the analyzer's read side.** Rule: `resolve_table_scan` reads the analyzed AST's own `TableScan.schema` field first; the `BaseTypes` overlay is consulted only when the AST-carried schema is unresolved (empty or all-`Unresolved`). Concretely: `resolve_table_scan(&TableScan, &BaseTypes) -> Result<Schema>` returns AST schema if non-empty; else BaseTypes lookup by name; else `AnalyzerError::UnknownTable`. Request-handler seeding of `BaseTypes` short-circuits — if no `TableScan` in the plan has an empty `schema`, `build_base_types_from_plan` returns an empty overlay without walking the session catalog. Rationale: keeps the analyzer's tree-local reasoning intact (schemas travel with nodes) while still supporting deferred session-catalog resolution for scans the front-end couldn't schema-thread. The command-arm write contract (this ADR's core) is unaffected — the rule scopes only the analyzer's read side.

---

## ADR-013 — External / lakehouse tables (Hive-Parquet, Delta, Iceberg on S3; Unity Catalog) are read by delegating to DuckDB's storage extensions; read-only for this iteration

**Status:** Proposed
**Depends on:** ADR-000, ADR-002, ADR-012
**Depended on by:** ADR-015, ADR-017, ADR-018, ADR-019

**Context.** thunderduck must read tables that do not live in DuckDB's native store: Hive-partitioned Parquet on S3, Delta tables on S3, Iceberg tables on S3, and Unity-Catalog-managed tables. DuckDB already provides mature reader extensions for all of these (`parquet`, `httpfs`/`s3`, `delta`, `iceberg`, and `uc_catalog`), including Unity Catalog support and Databricks catalog-managed-commit handling on the Delta path. Two facts shape the decision. First, per ADR-000/ADR-002, anything DuckDB already does correctly should be delegated, not rebuilt — and these readers, transaction-log handling, and catalog protocols are exactly that. Second, the four targets split into two structurally different access modes, which the overlay (ADR-012) must model. (A prior pass-through-string approach for SQL was abandoned for the regex-monster reasons recorded in ADR-004; that is unrelated here — external tables are reached through structured scans/attachments, not string rewriting.)

**Decision.** thunderduck reads external/lakehouse tables by **emitting the corresponding DuckDB storage-extension surface and delegating all reading, metadata, and catalog-protocol work to those extensions**. It owns only (a) translating a Spark table/catalog reference + storage descriptor + credentials into the correct DuckDB invocation, and (b) reconciling the resolved schema into Spark types in the overlay (ADR-012). It **never** implements a reader, a transaction log, or a catalog protocol (INV8). Unity Catalog specifically is delegated, and routes by table format: **UC→Delta via the `uc_catalog` extension; UC→Iceberg via the Iceberg REST endpoint** (`ATTACH … TYPE iceberg`). **Writes are out of scope for this iteration** and are deferred per-format (see OQ-2); external tables here are read relations only, so they flow through the normal relation path and τ, *not* the command arm (ADR-011).

The two access modes are the central structural content:
- **Path-based (schema-on-read).** Hive-Parquet (`read_parquet(glob, hive_partitioning=true)` over an `s3` secret) and bare Iceberg/Delta paths (`iceberg_scan(...)` / `delta_scan(...)`, or `ATTACH … TYPE delta`). There is no catalog; the schema is discovered from Parquet footers / format metadata plus partition-column inference from the paths. thunderduck must drive a schema-discovery step, and **partition-column typing is the exposed divergence surface** (Spark's partition-type inference has its own rules).
- **Catalog-attached.** Iceberg REST / S3 Tables / Glue / UC-Iceberg (`ATTACH … TYPE iceberg`) and UC-Delta (`uc_catalog`). Namespaces are exposed as schemas and the schema is resolved by the extension; thunderduck consults a real catalog rather than discovering schema from files.

**Consequences.**
- (+) A clean instance of ADR-002's delegation applied to storage: thunderduck adds no reader/format/protocol code, only reference-and-credential translation plus type reconciliation.
- (+) Reads are precisely where DuckDB's lakehouse support is mature, so the delegated surface is solid for this iteration.
- (+) Read-only keeps ADR-011 untouched — external reads are relations through τ, not `emit_command`.
- (−) The type-source seam (ADR-012): the Spark type for a format-backed column must come from the *format's* schema mapped directly to Spark, not from DuckDB's reported scan type, or precision/timezone/nullability can diverge (LB7).
- (−) Version-coordination surface grows: thunderduck is now implicitly sensitive to specific DuckDB storage-extension versions, whose lakehouse behaviour evolves release-to-release (edge to ADR-016).
- (−) Bound to LB7 (DuckDB's readers must be schema- and value-faithful to Spark on the same table) — a bet partly outside thunderduck's control.

**Refinement hooks.** Define, per format, the mapping table from (Spark reference + storage descriptor) → DuckDB invocation (`read_parquet` glob / `iceberg_scan` / `delta_scan` / `ATTACH TYPE iceberg` / `uc_catalog`). Define credential/secret translation (Spark S3/credential config → DuckDB `CREATE SECRET (TYPE s3, …)` or the catalog OAuth2 secret). Define the direct Iceberg-type→Spark-type and Delta-type→Spark-type mappings (ADR-012), validated by reading the same table in reference Spark and diffing the resolved schema (ADR-015). Record access provenance + format per relation in the overlay. Pin/track the DuckDB extension versions as part of ADR-016. Write paths are deferred to OQ-2 and will each become their own ADR.

---

## ADR-014 — Two decision spaces to test; three failure-attribution buckets

**Status:** Proposed
**Depends on:** ADR-005, ADR-009, ADR-016, the DuckDB-correctness assumption (LB2)
**Depended on by:** ADR-015

**Context.** A Spark-vs-thunderduck result divergence could originate in resolution, in translation, or in DuckDB execution. A result-level diff alone cannot localize which.

**Decision.** There are two distinct decision spaces, each instrumented with its own coverage claim: the **translation decision space** (the emission table, ADR-009) and the **resolution decision space** (Spark's type/nullability inference rules, ADR-005/006). Failure attribution has three buckets: resolver bug, translator bug, or — excluded by assumption — DuckDB execution. DuckDB is assumed correct on valid SQL (the assumption underlying ADR-002).

**Consequences.**
- (+) Validating the two decision spaces separately makes failures attributable rather than ambiguous.
- (+) The resolution decision space is small and tractable (Spark's type/nullability rules, not all of Catalyst).
- (−) Explicit scope limitation: the suite will not isolate a correct-SQL / DuckDB-mis-execution case; such a case surfaces as a diff but is misattributed unless triaged.
- (neutral) Triage branches on attribution first, supported by emitted-SQL capture (ADR-015).

**Refinement hooks.** Define the resolution-decision instrumentation (which inference rule fired). Decide how much trust to place in DuckDB's own test suite for the excluded bucket. Specify the triage decision tree.

**Seam-and-drain pattern for cross-cuts.** A pass may deliberately keep a specific cross-cut to an upstream unimplemented feature as an acknowledged seam **iff the next pass's core deliverable is to drain that seam**. Constraints: (a) the seam MUST be marked in source with a `TODO:` comment naming the drain; (b) any invariant relaxed to permit the seam MUST be tightened back to full strength when the drain completes; (c) an unnamed drain is contamination, not a seam. There is no cross-implementation seam because there is no other implementation (ADR-022).

**INV3 + INV10 bracket τ's substrate boundary.** INV3 (the *emission-side* single-source-of-truth rule) and INV10 (the *input-side* barrier: τ imports only value-level types from outside its own module tree) together enforce that τ's substrate stays clean. See §CV.5 for the grep checks.

---

## ADR-015 — Differential oracle against reference Spark; all variation-suppression is test-side; inference validated in isolation via AnalyzePlan

**Status:** Proposed
**Depends on:** ADR-001, ADR-004, ADR-005, ADR-009, ADR-010, ADR-012, ADR-013, ADR-014, ADR-016, ADR-017, ADR-018
**Depended on by:** —

**Context.** The oracle is differential against Apache Spark. Equivalence/variation-suppression could be done in production τ or in the test harness. Inference (ADR-005/006) needs an independent validation oracle so that translation-test failures are attributable (ADR-014).

**Decision.** Generate a plan, serialize once, send identical bytes to both engines, canonicalize *results* (row order, float ULP, NaN/NULL, decimal precision, UTC, collation, map-key order), diff. Variation suppression and equivalence reduction happen test-side at generation time and in result canonicalization — never in production τ. Additionally, the resolution layer is validated *in isolation* against Spark Connect's `AnalyzePlan` RPC, which returns ground-truth resolved schemas; thunderduck's inferred schema is diffed column-by-column (type and nullability) *before* the inference pass has to be correct in concert with the emitter. The AnalyzePlan diff also validates front-end agreement (INV7): the same SQL string parsed by thunderduck and sent to Spark should yield matching resolved schemas.

**Consequences.**
- (+) Validating inference separately collapses the attribution problem: with inference independently confirmed, a result divergence is a translation bug, not a possible upstream type bug.
- (+) The AnalyzePlan diff is cheaper and more localizing than the result diff (no DuckDB execution in the loop) and doubles as the *implementation oracle* for ADR-005/006 and the *front-end-agreement* check for ADR-003/004.
- (+) Identical-bytes-to-both-engines (parity-via-identical-bytes) falls out of serialize-once-send-twice (Invariant INV1).
- (−) Requires a reference Spark 4.1.1 instance and a catalog fixture pinning identical base-column Spark types on both engines.
- (neutral) Canonicalization is test-side because any normalization baked into production τ would itself be a divergence from Spark — this is why the production-canonicalizer idea was rejected during design.

**Refinement hooks.** Specify the result-canonicalization rules precisely (the float/NaN/decimal/collation/map handling). Build the AnalyzePlan schema-diff first, as validation harness, inference implementation oracle (ADR-005/006), and front-end-agreement check (INV7). Define the catalog fixture and how it is established identically on both engines. Implement the tri-state error comparison mandated by ADR-016: on any given case, the oracle accepts **both-succeed** (compare rows) OR **both-throw-with-matching-Spark-error-class** as a PASS; anything else is a divergence. This makes ANSI-throw cases (e.g. `a / 0`, `element_at(empty, 1)`) first-class corpus witnesses of τ's error emulation rather than blanket-failures.

**The oracle is the parity contract.** ADR-015's differential oracle (AnalyzePlan schema diff + result differential) validates τ against **reference Spark**. Correctness is a test-time property enforced by the oracle, not a compile-time property inherited from any substrate. LB9 (§CV.4) is the load-bearing form of this stance.

---

## ADR-016 — Pinned reference version and ANSI-mode configuration; coverage claims are version-and-config-scoped

**Status:** Proposed
**Depends on:** ADR-010
**Depended on by:** ADR-014, ADR-015, ADR-017, ADR-018, ADR-022

**Context.** Spark's semantics (type coercion, nullability, function behavior, SQL grammar) evolve across versions. They also branch on runtime configuration — most critically `spark.sql.ansi.enabled`, which selects between two materially different execution semantics for arithmetic, array indexing, cast overflow, string-to-number parsing, and several other primitives. Coverage claims and inference fidelity are only meaningful against a fixed semantics *and* a fixed config profile.

**Decision.** Everything is pegged to Apache Spark 4.1.1 **running in ANSI SQL mode** (`spark.sql.ansi.enabled=true`, the Spark 4.x default). Under ANSI mode:
- Division / remainder by zero raises `SparkArithmeticException [DIVIDE_BY_ZERO]` / `[REMAINDER_BY_ZERO]` instead of returning NULL.
- `element_at` / array subscript on out-of-bounds indices raises `SparkArrayIndexOutOfBoundsException [INVALID_ARRAY_INDEX_IN_ELEMENT_AT]` instead of returning NULL.
- Numeric-cast overflow raises `SparkNumberFormatException [CAST_INVALID_INPUT]` instead of returning NULL / silently truncating.
- `to_number` on a format-input mismatch raises `SparkIllegalArgumentException [INVALID_FORMAT.MISMATCH_INPUT]`.
- Interval-related conversions surface their strict Spark types (`YearMonthInterval`, `DayTimeInterval`) rather than being coerced to a permissive representation.

Callers who want the non-ANSI semantics use Spark's opt-in `try_*` families (`try_divide`, `try_mod`, `try_element_at`, `try_cast`, `try_to_number`, …) or `NULLIF(x, 0)` guards — those are explicit, τ-emittable, and NULL-returning. **τ matches ANSI Spark by default and matches `try_*` when the caller wrote `try_*`.** τ MUST NOT silently rewrite an ANSI arithmetic path to a NULL-returning wrapper.

The pinned-artifact set also includes DuckDB and its extensions, with a floor of **DuckDB ≥ v1.5.3** where the Iceberg write path (ADR-018) is used (required for MERGE, ALTER, and Iceberg v3 deletion vectors). A version *or config* bump re-derives the coverage denominators (ADR-009, ADR-014) and re-runs both suites (ADR-015). The pinning policy is bump-and-re-run, with the pinned version and `spark.sql.ansi.enabled=true` both hard CI checks against the Spark image under test.

**Error emulation contract (interaction with ADR-022).** When ANSI semantics call for a strict-throw and τ's emission delegates to DuckDB, τ MUST surface the failure as a Spark-emulated error carrying Spark's error-class code (e.g. `DIVIDE_BY_ZERO`, `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`), not as an opaque `SparkConnectGrpcException` wrapping a DuckDB error string. DuckDB's engine-level throws MUST be caught in the runtime layer and re-wrapped with Spark's error taxonomy before crossing the wire. This is what makes the differential oracle (ADR-015) able to compare error paths symmetrically — "both errored with matching class" is a legitimate PASS mode; "τ errored with a different class" is a divergence.

**Consequences.**
- (+) Coverage claims and parity guarantees are precise and auditable against a definite semantics *and* a definite runtime config.
- (+) The ANSI pin eliminates a whole category of ambiguity: `a / 0` has exactly one correct answer (throw with `DIVIDE_BY_ZERO`), not two.
- (+) The differential oracle (ADR-015) becomes tri-state on any given case: **both-succeed** (compare rows), **both-throw with matching error class** (PASS), or **anything else** (fail). The harness must implement this tri-state — see ADR-015 refinement hooks.
- (−) A version *or* config bump is a planned, multi-day effort (regenerate denominators, re-run suites, reconcile any new divergences in coercion/nullability/SQL-grammar/error-class), not a silent change.
- (−) Re-wrapping DuckDB engine errors as Spark-classed errors is a real runtime-layer responsibility; τ owns the mapping table `DuckDB error kind → Spark error class` (arithmetic, array-index, cast, format, decimal-overflow).
- (neutral) A `SparkConfig { version: "4.1.1", ansi_enabled: true, ... }` constant documents the contract and preserves room for config-conditional behavior if ever needed (default: none; all unconditional on the pinned config).

**Non-goals.** Non-ANSI mode is out of scope. If a user needs `spark.sql.ansi.enabled=false` semantics, they call `try_*` explicitly. τ does not offer a "relaxed mode" runtime switch — parallel to ADR-020's elimination of "relaxed extension mode."

**Refinement hooks.**
- Establish the version-and-config-bump runbook.
- Enumerate the `DuckDB error kind → Spark error class` mapping table exhaustively (arithmetic, array-index, numeric cast, decimal overflow, string-to-number format, JSON parse). This is a τ runtime-layer artifact and should be locked with unit tests plus corpus witnesses (the ANSI-throw cases: math-010, math-011, arr-008, parse-003, cast overflow variants, …).
- Decide whether any transformation ever needs to be version-conditional or config-conditional (default: none; all unconditional on the pinned config).
- Update the differential harness (ADR-015) to implement the tri-state error comparison: both-succeed / both-throw-matching-class / divergence.

---

## ADR-017 — Delta-table writes are scoped to append-into-existing via DuckDB's Delta extension; DELETE / MERGE / overwrite / create are rejected pending DuckDB support

**Status:** Proposed
**Depends on:** ADR-005, ADR-011, ADR-013, ADR-016
**Depended on by:** ADR-015

*(This is the first of the per-format external-table write specializations promised by OQ-2. It hangs off the command arm (ADR-011) and the external-table delegation (ADR-013); it is a leaf consequence, not part of the spine.)*

**Context.** DuckDB's Delta extension (write support graduated from experimental in v1.5.2) currently provides read plus *limited write*: a **blind `INSERT`/append** into an *attached* Delta table, atomic per transaction (multiple `INSERT`s inside one `BEGIN`/`COMMIT` become a single Delta version), with an optional idempotent-append API (`delta_set_transaction_version`) giving exactly-once retries via compare-and-swap on a per-application version. It does **not** yet support `UPDATE`, `MERGE`, or `DELETE`; table-creation DDL is officially future work (a `CREATE TABLE`-on-attached-schema path appears in some third-party examples but is unconfirmed and version-fragile). Writes require attaching the table (`ATTACH … TYPE delta`); the path-based `delta_scan` is read-only. Per INV8 thunderduck delegates the writer and does not implement Delta-protocol write logic itself, so thunderduck's Delta write surface is exactly DuckDB's.

**Decision.** For this iteration thunderduck supports exactly one Delta write path: **`INSERT` / append into a pre-existing, attached Delta table**. A single Spark write *action* maps to a single DuckDB transaction so that it produces one Delta version (matching Spark's one-commit-per-write). The source rows come from a τ-translated relation, so the emitted command is `INSERT INTO ⟨attached Delta table⟩ ⟨τ(source relation)⟩` — composing the existing relation path (τ) with the command arm (`emit_command`, ADR-011). Every other Delta write operation is **rejected with a diagnostic that names the limitation as DuckDB-version-gated, not a thunderduck limitation**, each with an explicit revisit-trigger:
- `DELETE FROM`, `MERGE INTO`, `UPDATE` → rejected; revisit when the Delta extension ships delete/update/merge.
- `mode("overwrite")` → rejected; faithful overwrite needs delete/truncate, which DuckDB-Delta lacks; revisit with delete support.
- CTAS / create-on-write, and the `errorIfExists` / `ignore` create-semantics → out of scope; DuckDB-Delta table-creation DDL is future work, so **a writable Delta table must pre-exist**; revisit when DDL is confirmed in a pinned build.

A writable Delta relation must have attached provenance (ADR-013); a path-scan Delta relation is read-only (INV9).

**Consequences.**
- (+) Delivers the highest-value actually-supported Delta write (append) now, composed cleanly from the existing τ relation path and `emit_command` rather than new machinery.
- (+) One-action → one-transaction → one-version matches Spark's commit semantics; the idempotent-append API is available if retry-safety is wanted.
- (+) Rejections are precise and DuckDB-gated, so the supported set lifts automatically as DuckDB's Delta writer matures, and the diagnostics tell users *why* a write is refused and *when* to expect it.
- (−) Append-only is a real functional gap versus Spark: no overwrite, delete, merge, or CTAS into Delta — many Spark Delta write workloads will not run until DuckDB catches up. This is an honest scope limit, surfaced as a typed rejection, not silent.
- (−) Insert-time type handling must reproduce **Spark's** store-assignment casting and column-resolution (by-name vs by-position), a write-direction instance of ADR-005 — not DuckDB's native `INSERT` casting.
- (−) Carries write-direction fidelity risk (LB8): DuckDB's Delta write surface is young, including a noted delta-kernel-rs OneLake regression.

**Refinement hooks.** Define the Spark-write-mode → action mapping (append supported; all others → typed rejection with revisit-trigger). Define `INSERT` column resolution to match Spark: `insertInto` is by-position, `saveAsTable` / `DataFrameWriterV2` by-name, with Spark's store-assignment casts — reproduce Spark's, validated via the oracle. Decide whether to use `delta_set_transaction_version` for retry-safety (note: Spark structured-streaming `txnAppId`/`txnVersion` exactly-once is out of scope here; batch append is one plain commit). Specify the state-diff oracle for append: logical read-after-write row comparison plus Delta version/log lineage, through both engines. Pin the Delta extension version (ADR-016) and track the delete/update/merge/DDL roadmap to lift the rejections one at a time.

---

## ADR-018 — Iceberg writes target Databricks UC-managed Iceberg through the attached Iceberg REST endpoint; CTAS / INSERT / DELETE / MERGE, subject to the UC endpoint's constraints

**Status:** Proposed (verified June 2026 against DuckDB v1.5.3 + Databricks Managed Iceberg GA; residual *per-table* conditions noted below)
**Depends on:** ADR-005, ADR-011, ADR-013, ADR-016
**Depended on by:** ADR-015, ADR-019

*(Second per-format external-table write specialization from OQ-2, sibling to ADR-017. Hangs off the command arm (ADR-011) and external-table delegation (ADR-013); a leaf consequence, not spine.)*

**Context.** The write target is **Databricks UC-managed native Iceberg** (GA since May 2026, Databricks Runtime 16.4 LTS+): tables UC manages and exposes through the Iceberg REST Catalog (IRC) API at `/api/2.1/unity-catalog/iceberg-rest`, writable *as Iceberg*, and separately exposed *read-as-Delta via UniForm* (the intended downstream read pattern). Verified facts (June 2026): the IRC has **read, write, and create** access for Iceberg clients to managed Iceberg tables; Databricks explicitly lists **DuckDB** among supported external engines; and credential vending issues **read-and-write-scoped** storage credentials for managed Iceberg (more than managed Delta, which is read-only via vending for Delta clients). DuckDB's `iceberg` extension (v1.5.3) supports the full single-table write set against an attached REST catalog — CTAS, INSERT, UPDATE, DELETE, MERGE, ALTER — committing each write as a new Iceberg snapshot, with DELETE/MERGE using merge-on-read positional deletes (binary deletion vectors on v3 tables). The UC endpoint is a *restricted* REST implementation: it does not support staged creates, rejects the multi-table transactions/commit endpoint, and manages its own metadata and storage cleanup — DuckDB's documented attach for it uses `STAGE_CREATE_TABLES false`, `DISABLE_MULTI_TABLE_COMMIT true`, `SKIP_CREATE_TABLE_METADATA_UPDATES true`, `REMOVE_FILES_ON_DELETE false`. UC-managed Iceberg *writes* go through this IRC path with native Iceberg writers — **not** through `uc_catalog` (which is Delta-centric, ADR-017). Per INV8 thunderduck delegates the writer; per INV9 writes route through the attachment, never a path-scan — and UC independently forbids path-based access to managed tables, reinforcing INV9.

**Decision.** thunderduck writes to UC-managed Iceberg by **emitting single-table Iceberg DML/DDL against the attached UC Iceberg REST catalog**, mapping each Spark write action to a single Iceberg snapshot commit. The supported operations are **CTAS, INSERT (append), DELETE, and MERGE** — all single-table, consistent with `DISABLE_MULTI_TABLE_COMMIT` (cross-table atomic writes are not available). DELETE and MERGE use DuckDB's merge-on-read positional deletes. The UC-specific attach configuration (the flags above) and **write-scoped vended credentials** (`ACCESS_DELEGATION_MODE 'vended_credentials'`, PAT or OAuth) are part of ADR-013's reference/secret mapping. **CTAS is supported** via a non-staged create (`STAGE_CREATE_TABLES false`), so unlike the Delta path (ADR-017) the target table need **not** pre-exist — though external CTAS cannot define generated/default/constraint columns, and UC blocks commits of duplicate data files. `REMOVE_FILES_ON_DELETE false` means DELETE/MERGE record positional deletes but do **not** physically remove data files — UC's own maintenance reclaims storage. DELETE/UPDATE/MERGE require the target table to be **merge-on-read** (`write.update.mode`/`write.delete.mode` = `merge-on-read`); UC managed Iceberg v3 uses deletion vectors (merge-on-read) by default, so this holds for standard tables, but a copy-on-write-configured table makes those operations a typed rejection. Where any residual condition (below) fails, the affected operation becomes a typed rejection with a revisit-trigger, exactly as ADR-017.

**Consequences.**
- (+) A much richer write surface than Delta (ADR-017): CTAS/INSERT/DELETE/MERGE rather than append-only, because DuckDB's Iceberg writer is far ahead of its Delta writer.
- (+) One action → one snapshot commit matches Spark's commit semantics; reuses the relation path (τ) as the source of INSERT/CTAS/MERGE.
- (+) Reuses ADR-013's attach/credential machinery — the UC constraints are *configuration*, not new code.
- (+) **CTAS works without pre-creation** (non-staged create), a concrete advantage over the Delta path (ADR-017), which requires the table to pre-exist.
- (−) Single-table only — no cross-table atomic commit per the UC endpoint (`DISABLE_MULTI_TABLE_COMMIT`); fine for the four target operations, which are all single-table.
- (−) Merge-on-read positional deletes mean the state-diff oracle must compare at *logical-row* level (read-after-write), not physical files; DELETE does not reclaim storage (UC does).
- (−) **UniForm read-after-write lag (directly relevant to the intended pattern).** Since the design is write-as-Iceberg then read-as-Delta-via-UniForm, and UniForm's Iceberg→Delta metadata generation is asynchronous, downstream Delta readers may observe a just-committed Iceberg snapshot only after UC regenerates UniForm metadata. thunderduck's own read-after-write must read the table **as Iceberg via the same attachment** to see its writes immediately; downstream Delta-via-UniForm consumers inherit UC's eventual consistency. This is a Databricks-side property, not a thunderduck bug, but it must be documented.
- (−) Insert/merge type handling must reproduce Spark's store-assignment casting and column resolution (ADR-005, write-direction), and MERGE must reproduce Spark's clause semantics.
- (−) Write-direction fidelity risk (LB8), now spanning Iceberg as well as Delta.

**Verified (June 2026).** The three drafting-time gates are resolved favorably:
- *External-engine write* — **yes.** UC's IRC has read/write/create for Iceberg clients; Databricks names DuckDB explicitly; Managed Iceberg is GA. Writes are not reserved to Databricks compute.
- *Write-scoped credential vending* — **yes.** Vending grants read-and-write credentials for managed Iceberg, gated on enabling external data access on the metastore and granting the principal `EXTERNAL USE SCHEMA` on the schema (PAT or OAuth; OAuth M2M for >1h token refresh).
- *CTAS without staged create* — **yes.** IRC has create access and DuckDB's UC attach disables staged creates, so CTAS succeeds and the table need not pre-exist.

**Residual conditions (per-table / setup, to confirm in the target workspace).**
- **merge-on-read mode:** DELETE/UPDATE/MERGE fail on a table whose `write.update.mode`/`write.delete.mode` ≠ `merge-on-read`. UC managed Iceberg v3 (deletion vectors) is merge-on-read by default; a copy-on-write-configured table is the exception → typed rejection.
- **partition transforms:** DuckDB v1.5.3 supports update/delete on unpartitioned tables and `bucket()`/`truncate()`-partitioned tables; other transforms may not — confirm the target tables' physical partitioning (UC Liquid Clustering / Predictive Optimization layout) does not block mutations.
- **external-client limits on managed tables:** ALTER TABLE, table-property updates, and maintenance (OPTIMIZE/VACUUM/ANALYZE) are not supported on managed tables from external clients (UC performs maintenance itself) — so thunderduck must not promise ALTER on UC managed Iceberg; CTAS cannot define generated/default/constraint columns.
- **governance + version prerequisites:** metastore external-data-access enabled, `EXTERNAL USE SCHEMA` granted; DuckDB **≥ v1.5.3** (for MERGE, ALTER, and v3 deletion vectors) — tighten ADR-016's pin accordingly.

**Refinement hooks.** Define the Spark-write-op → Iceberg-DML mapping (`saveAsTable`/CTAS, `insertInto`/append, `DELETE`, `MERGE`). Reproduce Spark MERGE semantics — `WHEN MATCHED` / `WHEN NOT MATCHED` / `WHEN NOT MATCHED BY SOURCE` and the default error-on-multiple-match — and confirm DuckDB MERGE covers the clauses Spark emits; flag any unsupported clause as a typed rejection. Reproduce Spark's INSERT store-assignment casts and by-name/by-position resolution (ADR-005). Specify the state-diff oracle: logical read-after-write row comparison plus Iceberg snapshot lineage, reading back **as Iceberg** through both engines (avoid UniForm-Delta in the oracle to dodge metadata lag). Encode the UC attach flags + vended-write-credentials in ADR-013's mapping. Add a preflight check for the per-table residuals: confirm `write.*.mode = merge-on-read` and a DuckDB-supported partition layout before allowing DELETE/MERGE, else typed rejection. Set ADR-016's pin to DuckDB ≥ v1.5.3 for this path.

---

## ADR-019 — Lakehouse I/O contract: read inputs in their native format and write results as Iceberg, both via Unity Catalog — each format on its DuckDB-strong side, no cross-format single-table access

**Status:** Proposed (composes the verified ADR-013 read path and ADR-018 write path; introduces no new mechanism)
**Depends on:** ADR-013, ADR-018
**Depended on by:** —

*(A capstone composition over the lakehouse read/write ADRs, resolving the format-direction tension surfaced while validating UniForm. It assigns roles to mechanisms ADR-013 and ADR-018 already define.)*

**Context.** Two facts established earlier pull in opposite directions for any single dual-format table, and this ADR avoids the collision by construction. (1) DuckDB's Delta and Iceberg extensions are at different maturity points, asymmetrically *by direction*: delta-kernel-rs is the stronger Delta **reader** (handles deletion vectors and column mapping on read — it is the workaround used where the older Delta reader fails), while DuckDB's Delta **writer** is append-only and immature (ADR-017); conversely DuckDB's native `iceberg` extension is a mature **writer** (CTAS/INSERT/DELETE/MERGE via an attached REST catalog, ADR-018). (2) UC UniForm is one-directional as documented — it generates Iceberg read metadata *from* a Delta table (Delta-primary → Iceberg-read); the reverse, a managed Iceberg table read transparently *as* Delta, is not documented. A design that makes one table serve both formats therefore hits either the Delta-write immaturity or the missing Iceberg→Delta-read direction.

**Decision.** thunderduck's lakehouse I/O contract assigns each format to its DuckDB-strong side, on *distinct* tables:
- **Read path:** thunderduck reads each input table via Unity Catalog **in its native format**, selected from UC's catalog metadata — because thunderduck is an *external open-API client*, and UC's open APIs are format-specific: managed Delta is served to Delta clients via the Unity REST API / Delta path, managed Iceberg to Iceberg clients via the IRC. Managed Delta inputs (the Databricks default) take the Delta read path — attach UC via the `unity_catalog`/`uc_catalog` extension, read **by name** (path-based access to UC-managed tables is forbidden), **read-only** vended credentials; delta-kernel-rs handles deletion vectors and column mapping, and DuckDB transparently uses Catalog-Coordinated Commits (CCv2) where required. Managed *Iceberg* inputs (e.g. tables written by an upstream Flink Iceberg connector) are read **as Iceberg via the IRC** — directly, not "as Delta." *Note the external-client distinction:* Databricks-native Spark is format-transparent and presents every managed table uniformly (so "looked up via UC ⇒ seen as Delta" holds there); that is a native-engine behavior and does **not** extend to external open-API clients like thunderduck. If literal uniform-read-as-Delta is required for an external client, UC Compatibility Mode is the documented fallback — but its cross-format semantics and path-based caveats must be verified, and it is strictly less clean than reading Iceberg as Iceberg.
- **Write path:** results are written as **UC-managed Iceberg** via the attached Iceberg REST catalog (ADR-018), with **read-write** vended credentials.
- **Deliberately excluded:** writing Delta from thunderduck (DuckDB Delta writes are append-only/immature, ADR-017), and any reliance on cross-format single-table access in either UniForm direction.

Because inputs and outputs are *different tables in different formats*, no single table must serve two formats, so the UniForm-direction limitation never arises.

**Consequences.**
- (+) Each format runs on its DuckDB-strong side — the stronger Delta *reader* and the mature Iceberg *writer* — sidestepping both the Delta-write immaturity and the undocumented Iceberg→Delta-read direction.
- (+) The credential model matches exactly: UC vends **read-only** credentials for managed Delta and **read-write** for managed Iceberg — precisely the access this contract needs on each side.
- (+) No UniForm dependency: no asynchronous metadata-generation lag, no IcebergCompatV3 external-access wall, no manual metadata-refresh step.
- (+) Reading each input in its native format means Flink-written Iceberg inputs are read **directly as Iceberg** via the IRC — no reverse-UniForm metadata generation, no async lag, and no IcebergCompatV3 external-access wall. thunderduck is itself an Iceberg-capable consumer, so a downstream that "sees Delta" (Databricks Spark) and thunderduck (sees Iceberg) read the same files without conflict.
- (−) thunderduck reads **both** formats (Delta-native via the Delta path, Iceberg-native via the IRC), selected per-table from UC metadata; it does not get the single-format uniformity that Databricks-native Spark enjoys. This is a capability requirement, not a blocker — both paths are supported (ADR-013).
- (−) Liquid-clustered Delta inputs: **correctness is structurally safe.** Delta's protocol versioning is fail-closed — a reader lacking a required feature refuses the table, never returns partial rows. On Databricks these tables are reader-version 3 (from bundled deletion vectors / row tracking / V2 checkpoints, not clustering itself), which delta-kernel-rs supports; so the realistic outcome is correct reads with clustering unused for pruning (accepted degraded-pruning), and the worst case is a hard refusal (safe/unavailable), never silent data loss. Residual depends only on the pinned delta-kernel honoring the protocol gate (LB7).
- (−) Cross-engine write isolation: if Databricks or another writer also writes the same Iceberg outputs, ADR-018's single-table / no-multi-table-commit constraints and snapshot-version isolation apply; UC's duplicate-file blocking guards the commit.

**Refinement hooks.** Define per-format read attach within ADR-013's mapping: managed Delta via the `unity_catalog`/`uc_catalog` extension (by-name, read-only vended credentials, CCv2 transparent), managed Iceberg via the IRC (read). Resolve the table's format from UC catalog metadata to choose the path. Add an input preflight that surfaces any table whose reader-feature set outruns the pinned delta-kernel as an explicit hard error (never a silent read), accepting degraded pruning where the table is readable. If uniform read-as-Delta for external clients is ever required, evaluate Compatibility Mode (verify cross-format semantics + path-based caveats). The write path and its residuals are ADR-018's. End-to-end validation composes ADR-013's read check with ADR-018's write check against reference Spark (ADR-015). Rests on LB7 (Delta/Iceberg read fidelity) and LB8 (Iceberg write fidelity).

---

## ADR-020 — Strict-only target: the `thdck_spark_funcs` extension is mandatory; "relaxed mode" is eliminated

**Status:** Proposed
**Depends on:** ADR-000, ADR-001, ADR-010
**Depended on by:** ADR-009, ADR-015 (each is simplified by the single target this ADR fixes)

**Context.** The existing implementation ships two compatibility modes. *Relaxed* (the default) emits to vanilla DuckDB and accepts a documented gap to Spark — sample-vs-population semantics for kurtosis/skewness, DuckDB-typed return widths for `SUM`/`AVG` on integers and decimals, DuckDB decimal-division semantics, and a runtime error for `hash` / `xxhash64` because no vanilla form matches Spark. *Strict* loads the `thdck_spark_funcs` DuckDB extension and routes each of those Spark-divergent functions through it, restoring Spark semantics. The two-mode design predates the rearchitecture: it bought platform flexibility (the extension was optional, and unsupported targets could still run *something*) and let the project ship before the extension binary was production-ready.

That positioning has shifted. ADR-010 already classifies the extension functions as the *principled* answer to Spark/DuckDB divergence — a minimal gap-filler, emitted by a small subset of dispatch cells in ADR-009's table, validated as part of the same differential surface (ADR-015) as everything else. With ADR-010 in place, a second emission target is no longer carrying its weight. It costs: every divergent function carries two implementations and two differential-suite passes, the dispatch tables and code branches inside τ encode a `CompatMode`, and "relaxed-only" defects (silently wrong values, surface-level overflows) live in the *default* mode where most users land. Worse, each future Spark/DuckDB gap re-litigates ADR-010 — "do we add this to the extension, or document a relaxed gap?" — at the moment a single answer is what ADR-010 was meant to settle.

**Decision.** thunderduck has **one** emission target: Spark parity, with the `thdck_spark_funcs` extension loaded. There is no `CompatMode`, no `RuntimeCompatMode`, no `--relaxed` flag, no `THUNDERDUCK_COMPAT_MODE` selection. The extension binary is downloaded by `build.rs` unconditionally, embedded into the server via `include_bytes!`, and **loaded at every session's startup**; failure to load is a hard startup error, not a fallback. ADR-009's dispatch table emits one form per Spark expression; where that form is an `Extension(name)` call (ADR-010), the extension is guaranteed present by construction. The previously-relaxed-only function arms (`hash`, `xxhash64`, kurtosis-as-sample, skewness-as-sample, integer/decimal SUM and AVG return widths, DuckDB decimal division) are deleted; every emission goes through the Spark-faithful path.

**Alternatives considered (and rejected).**
- *Keep both modes, document strict as preferred.* **Rejected.** Documentation does not eliminate the maintenance cost: the relaxed code paths still exist, the registry still branches, the differential suite still must defend two emission targets, and "passes in relaxed" remains a real failure mode wherever a user lands in the default. Every gap discovered after this point would still force a two-arm decision.
- *Make strict the default, keep relaxed available behind a flag.* **Rejected** for the same reason — the maintenance cost is identical and the flag becomes an escape hatch for "I'll just turn relaxed on for this query," which re-creates the silent-divergence failure mode the rearchitecture is meant to remove.
- *Strict-only with the extension mandatory.* **Chosen.** Single emission target absorbs every Spark/DuckDB divergence through the mechanism ADR-010 already endorses. The dispatch table (ADR-009) has one row per construct, the analyzer's type and nullability conclusions (ADR-005) flow to one emission, and the oracle (ADR-015) has one configuration to defend.

**Consequences.**
- (+) τ has one emission target. ADR-009's table simplifies (no per-cell strict/relaxed split), and ADR-015's differential oracle has one configuration to defend against reference Spark — the "skip in relaxed" / "skip in strict" markers go away.
- (+) The `CompatMode` / `RuntimeCompatMode` types, the `mode` parameter threaded through `SqlGenerator` and the function registry, and the `bundled-extension` Cargo feature gate all disappear from the codebase. The mode-resolution code in session startup collapses to "load the extension, fail if you can't."
- (+) The full Spark function surface (notably `hash` and `xxhash64`, which previously errored on the default path) becomes uniformly available. No per-function "requires strict mode" disclaimer.
- (−) The build now requires network access to download the extension binary on first build (cached under `extensions/<release>/` after that). Unsupported host platforms become unsupported *builds*, not degraded runs — the binary's platform set (linux/x86_64, linux/aarch64, macos/x86_64, macos/aarch64 at the `ext4` release) is now a hard precondition.
- (−) The upstream extension repository (`nubank/thunderduck-duckdb-extension`) becomes a hard build dependency. An outage there during a fresh build is a build failure. The cache mitigates this for established workstations and CI runners that have built at least once.
- (neutral) The CLI keeps `--strict` as a deprecated no-op (with a one-line `tracing::warn!`) so existing scripts and Helm-style invocations do not break the day this lands; `--relaxed` and `THUNDERDUCK_COMPAT_MODE=relaxed` are rejected at startup with a clear message pointing at this ADR.

**Refinement hooks.** When a new Spark/DuckDB divergence is found, the workflow is unambiguous: add a function to the extension (or extend an existing one), add the dispatch row in τ, validate via ADR-015 — the "should we just document a relaxed gap?" question does not arise. The extension's exported function set must be kept in lockstep with τ's dispatch (this is ADR-010's INV6, sharpened: every `Extension(name)` cell now has exactly one binary it must agree with, not two configurations to validate). If a host platform without an extension binary ever becomes a requirement, this ADR is revisited rather than worked around at runtime — the answer is "ship the binary for that platform," not "re-introduce a vanilla fallback."

---

## ADR-021 — τ owns its substrate: protobuf converter, Expression, TypeInferenceEngine

**Status:** Proposed
**Depends on:** ADR-002 (delegation boundary), ADR-003 (common AST), ADR-004 (front-end convergence), ADR-005 (owned inference), ADR-014 (two decision spaces), ADR-015 (differential oracle)
**Depended on by:** every implementation slice; ADR-022 (τ is the only path — runtime-position companion); ADR-023 (the ordinal reference model lives in the τ-owned `TypedOp`/`RelScope` substrate)

**Context.** τ must own its substrate — the protobuf-to-CommonAST converter, the `Expression` payload in CommonAST, and the `TypeInferenceEngine`. If τ consumed an upstream plan type produced by a converter it does not own, τ inherits every quirk of that converter: synthesized-SQL shortcut shapes for structured operations (VALUES, table functions, file scans, Arrow-IPC LocalRelation), stringly-typed qualifier encodings (`__plan_id_{N}__`), silent-NULL fallbacks in Arrow value marshalling. If τ delegated type/nullability calls to an upstream inference engine, symmetric-omission gaps in that engine (a function present in `aggregate_return_type` but missing from `aggregate_is_nullable`) transit silently — τ arms can be individually correct yet the corpus stays red because the input's schema or nullability was wrong before the arm ran. Substrate ownership is the design lever that makes τ's correctness a τ-local concern.

**Decision.** τ owns its substrate from the protobuf boundary onward.

1. **Protobuf conversion.** τ's `V2RelationConverter` (in `crates/connect-server/src/converter/`) produces CommonAST directly from Spark Connect protobuf. The converter is exhaustive over the proto surface τ targets; un-handled proto messages surface as Thunderduck-boundary errors per ADR-022, not as silent shortcut shapes. Structured operations (`Values`, `LocalRelation` from Arrow-IPC, `TableFunction`/`Unnest`, `FileScan`, `Join`) get first-class CommonAST variants — no opaque SQL-string variants. Plan-ID is a first-class field on `Join` and `UnresolvedColumn` — not a stringly-typed qualifier baked into a name.

2. **Expression payload.** CommonAST's expression-payload type is `crate::transpiler_v2::expression::Expression`, owned by τ.

3. **Type inference.** `crate::transpiler_v2::type_inference::TypeInferenceEngine` is τ's, owned by τ, validated against reference Spark by ADR-015's differential oracle. Symmetric-omission discipline (see the refinement on ADR-005) governs internal consistency across the engine's function-name enumerations.

4. **Value-level types are shared.** `DataType`, `StructType`, `StructField` live in `crate::types::*` and are used verbatim by τ: pure value types (no behavior). Duplicating them would only risk conversion bugs at the boundary without buying substrate ownership.

**Alternatives considered (and rejected).**
- *Consume an upstream `LogicalPlan` produced by a converter τ does not own; adapt via a lowering layer.* Every converter shortcut (synthesized SQL, plan-ID-encoded qualifiers, silent-NULL Arrow gaps) transits into τ unfixably from τ's side. τ arms can be correct yet the corpus stays red because the input shape prevents the arm from firing on the interesting case.
- *Share `Expression` and `TypeInferenceEngine` upstream; isolate only the protobuf converter.* Keeps τ unable to refine its expression surface locally; upstream engine bugs transit into τ; the analyzer's symmetric-omission discipline can only be enforced in one place.

**Consequences.**
- (+) τ's correctness is a τ-local concern — no external substrate to coordinate with.
- (+) τ refines its `Expression` surface freely; τ owns the `TypeInferenceEngine` roster and can close symmetric-omission gaps at the point of discovery.
- (+) The protobuf converter emits structured CommonAST for every construct — no `Sql` opaque variant, no shortcut shape leaking into τ.
- (+) INV10 (§CV.5) enforces the input-side substrate boundary during migration; INV3 enforces the emission-side substrate boundary permanently.
- (−) Substantial code: `V2RelationConverter` (proto-surface exhaustive dispatch), τ `Expression` enum, τ `TypeInferenceEngine`, τ SparkSQL front-end. Bounded by the τ-targeted proto surface and by ADR-003's incremental-extension rule.

**Refinement hooks.**
- **V2RelationConverter's proto surface** must be exhaustive over the Spark Connect proto set τ targets. Un-handled shapes surface as Thunderduck-boundary errors, never as silent shortcut shapes.
- **Value-level type boundary.** `use crate::types::{DataType, StructType, StructField}` is the only permitted import from τ into the shared-value-types module; INV10's grep enforces this.

---

## ADR-022 — τ is the only path; two error categories

**Status:** Proposed
**Depends on:** ADR-000 (no-JVM premise), ADR-002 (delegation boundary), ADR-021 (substrate ownership)
**Depended on by:** every implementation slice; ADR-023 (qualified-reference Spark-emulated errors)

**Context.** τ is the transpiler. When τ does not implement a Spark input, τ says so — it produces a typed error to the caller, not a partial or synthetic SQL string, and not a route to some other execution path. The correctness contract is Spark parity or a named limitation; there is no third choice ("silently different"). This ADR pins that contract.

**Decision.** τ is the only production path. All Spark Connect requests flow to τ; τ's output is the response. If τ cannot handle the input, τ returns a typed error that surfaces directly to the client. Errors fall into exactly two categories:

1. **Spark-emulated errors.** Inputs Spark itself would reject — malformed queries, unknown columns, ambiguous references, type mismatches, aggregate-context violations. τ emulates Spark's error semantics: same error class, same message shape where practical, same failure mode. The Spark Connect facade is preserved — a Spark client sees the same errors it would see against reference Spark. Concretely: `AnalyzerError::AmbiguousColumn` / `UnknownColumn` / `UnknownTable` / `TypeMismatch` and future Spark-equivalent variants.

2. **Thunderduck-boundary errors.** Inputs Spark accepts but τ has not implemented, or that τ cannot correctly transliterate to DuckDB. Distinct error class: `AnalyzerError::PuntedOperator`, `EmissionError::UnsupportedOp` / `UnsupportedExpression` / `UnsupportedFunction`. The message is honest — "this operator is not implemented in Thunderduck," not "an internal error." This is the only place Thunderduck-specificity leaks through the Spark Connect facade, and the leak is deliberate and named.

Neither category triggers a runtime fallback. Both surface directly to the client. A slice's Pass 1 architect designing a new error variant classifies it into one of the two categories at design time; a reviewer verifies the classification.

**Alternatives considered (and rejected).**
- *Fall back to an alternate implementation when τ does not implement an input.* Adds runtime dispatch machinery, corpus-attribution instrumentation, and cross-implementation coordination. The alternate implementation drifts because nobody actively maintains it while τ climbs. Silent-divergence failures become indistinguishable from "green via τ" in the aggregate progress signal.
- *Two paths behind a build-time feature flag.* Same coordination cost as runtime dispatch; adds "which flag was set when I built this artifact?" as a surface bug.

**Consequences.**
- (+) Progress signal is unambiguous: every corpus-green case is τ's own success; every red case is a τ bug or an unimplemented feature.
- (+) No fallback machinery — no eligibility predicate, no attribution instrumentation, no runtime env-var routing.
- (+) The Spark Connect facade is honest about where the Spark emulation ends: Thunderduck-boundary errors say so.
- (−) The DataFrame corpus (`tests/scripts/v2-progress.sh`, 324 cases) is the fitness function while τ grows; TPC-H is temporarily red until τ covers its query surface.
- (−) Any non-τ source that remains as reference material does not compile as a service backend and is not exercised by tests. INV10's grep barrier (§CV.5) forbids τ from importing behavior-carrying types across the boundary.

**Refinement hooks.**
- **Timing of non-τ source cleanup** is a scheduling decision, not an ADR one — delete once no test, no CI job, and no build step references it. Incremental per-slice deletion is permitted.
- **Boundary-error precision.** Over time, the set of `Unsupported*` variants shrinks as τ grows. A future ADR (post-corpus-green) may enumerate the residual *permanent* unsupported set — inputs Spark accepts that τ will never support (e.g. distributed-only operators). Until then, boundary errors are pragmatic — "not yet" is a valid reason.
- **Qualified-reference emulation is produced by ADR-023's ordinal resolver.** The Spark-emulated category-1 errors for qualified references — `AmbiguousColumn` when a qualifier binds 2+ candidates, `UnknownColumn` when it binds none — fall out of the resolve-time 0/1/2+ match count of ADR-023's ordinal resolution; without it, those inputs ride the permissive name-only fallback into exactly the forbidden silent-divergence / opaque-error modes.

**Carve-out register (for permanent Thunderduck-boundary errors).** *Currently empty.* When a boundary error is deemed permanent (Thunderduck will not implement this feature), it is recorded here with a written justification. Non-permanent "not yet" boundary errors are the ADR-022 category-2 default (surfaced to the caller with an honest `Unsupported*` reason) — they are not tracked here.

---

## ADR-023 — τ resolves column references to (source-qualifier lineage, ordinal) at analysis time; qualifiers are regenerated at emission

**Status:** Superseded by ADR-024 (2026-07-12). ADR-023's decision *outcomes* — F8 (created alias inherits ∅ lineage), F10 (projected-through keeps the qualifier), F11 (ambiguity by match count, incl. plan_id), verbatim duplicate names on the wire, emission-time qualifier regeneration, remaps only at fixed structural points — are carried forward verbatim as ADR-024 must-preserve constraints. What is superseded is the *representation*: the positional `ordinal` as the identity surrogate and the `RelScope.source_quals` parallel-vector lineage are replaced by identity and lineage stored on the schema attribute itself. ADR-023 was Proposed and partially built (tiers 1+2) at supersession — a representation swap before ratification, not a rip-out of shipped design.
**Depends on:** ADR-005 (schema-threading analysis), ADR-006 (single resolve pass), ADR-021 (τ owns `TypedOp`/`RelScope`), ADR-022 (two error categories — the resolver produces the qualified-reference Spark-emulated errors)
**Depended on by:** the qualified-resolution slices (F8/F10/F11 closure); retirement of the emission strip machinery (`strip_stranded_qualifiers`, the F5/F9/F12 wrap rewrites, `exprs_visible_in` exemptions, F14 walkers, the `__td_jl`/`__td_jr` synthetic-alias machinery)

**Context.** τ carries string qualifiers (`e.name`, `__td_jl.col`) from the analyzer into emitted SQL. When `SelectBlock::wrap` buries a child under `(…) AS __td_sub`, a carried qualifier naming a pre-wrap alias is **stranded** — the alias no longer exists at that level. This one class consumed findings F5/F9/F12/item-3 (each patched case-by-case at the emission boundary) and leaves the F8/F10/F11 cluster the string machinery cannot cleanly solve. Two references make the point, both `e.<col>` with `e` binding no local scope yet demanding opposite outcomes: **F10** `df.alias('e').select('e.dept_id','e.name').…filter(e.name=='x')` — Spark **SUCCEEDS** (the projected-through column keeps `e` via Spark's attribute lineage); **F8** `df.alias('e').select(col('dept_id').alias('k')).filter(e.k==101)` — Spark **ERRORS** (`k` is a *created* alias, inherits no qualifier). The correlated LATERAL case tbl-005 (`… WHERE e2.dept_id <=> e.dept_id`) presents the same shape a third time with a **live outer** qualifier that must resolve outward.

**The model.** Two ideas combine. Apache Calcite (studied at tip `e15c395`) never carries string qualifiers into its algebra: columns are **ordinals** (`rex/RexInputRef`), name→ordinal resolution happens once in the validator (`DelegatingScope.fullyQualify`: 0→not-found / 1→bound / 2+→ambiguous), and qualifiers are **regenerated at emission** (`SqlImplementor.AliasContext.field(int)`) against whatever alias wraps the child *now* — so a wrap can never strand. That gives strand-elimination, F11 (ambiguity by match count), and correlation. But Calcite is pure-SQL, where a SELECT drops its FROM aliases; **Spark's DataFrame attribute-lineage** keeps a qualifier usable on a projected-through output column — the extra fact that separates F10 (succeed) from F8 (error). So τ carries two derived per-node facts, both computed structurally in `RelScope::of` like the schema/scope (never hand-maintained, never carried to emission):
- **ordinal** — the column's position in the producing node's output. Eliminates stranding (emission renders position *k* against the current wrapper's alias) and gives ambiguity by match count.
- **source-qualifier lineage** (`RelScope::source_quals`) — per output column, the set of relation qualifiers it inherits. A passthrough `ColumnReference` inherits its source column's set; an `Alias`/computed column inherits ∅. This is Spark's attribute lineage — what distinguishes projected-through from created.

**Decision.** Resolve every reference **once, at analysis time**, to `(source-qualifier lineage, ordinal)`, and synthesize qualifiers at emission from the current block's alias; a string qualifier is never a long-lived carried fact. Resolving `q.name`:
- `q` binds exactly one local scope → bind (ordinal).
- `q` binds **2+** ranges → `AmbiguousColumn` (**F11**; includes a DataFrame `plan_id` binding both sides of one join).
- `q` binds **no** local scope → output column's `source_quals` contains `q` ⇒ projected-through: resolve by ordinal, drop `q` at emission (**F10**, and USING joins whose keys carry both source qualifiers) · else `q` bound in an **OUTER** scope ⇒ correlated: keep `q` qualified for DuckDB's outward binding (**tbl-005**, the `sq-*` cluster) · else ⇒ `UnknownColumn` (**F8**, typos).

`plan_id` (the DataFrame reference key, ADR-021: `UnresolvedColumn.plan_id`, `Join.left/right_plan_ids`) resolves through the same scope — (plan_id, name)→(input, ordinal); 2+ ranges → ambiguous; a nested same-side plan_id is one binding (outermost wins); a plan_id star is an ordinal range. Emitted-SQL names (subquery aliases, struct CAST fields) are **uniquified** for reference safety, but the analyzer's `resolved_schema` and the Arrow wire schema keep Spark's names **verbatim, duplicates included** (ADR-005 parity: `df.select(df1.a, df2.a)` stays two `a`s; wire dedup only per PySpark's own `_deduplicate_field_names`). Column-set operators (`drop`/`withColumnRenamed`/`withColumns`) keep Spark all-matches semantics (a name → set of ordinals; 0 = no-op); lambda parameters bind to their binder. τ adopts Calcite's reference *model*, not its pipeline — no optimizer (ADR-001), so ordinal remaps occur only at fixed structural points (join concat, USING output contraction, set-ops, `SelectBlock` merge), never as tree-rewrite shuttles.

**Alternatives considered (and rejected).**
- *Pure ordinal, no lineage.* Errors on F8 correctly but on F10 too, and regresses USING joins — ordinals alone cannot tell projected-through from created once `q` binds no local scope.
- *Pure emission-side strip/rewrite* (today's mechanism). `strip_stranded_qualifiers` cannot distinguish a dead-alias local ref from a live correlated one (tbl-005's `e.dept_id` is string-identical at emission to F10's dead alias) — it silently rebinds the correlation; and every new wrap site needs its own rewrite.
- *Qualifier strings threaded ad-hoc and carried into emission.* Right information, wrong shape — carried strings still strand at wrap boundaries. Lineage must be a derived per-node fact consulted at *resolution*, not carried to emission.

**Consequences.**
- (+) Strand class eliminated at the root; F8/F10/F11 correct by construction; qualified-reference errors (ADR-022 cat-1) fall out of the resolver's match count.
- (+) Retires machinery rather than growing it: `strip_stranded_qualifiers`, the F5/F9/F12 wrap rewrites, `exprs_visible_in` exemptions, F14's walkers, and the `__td_jl`/`__td_jr` synthetic-alias machinery all exist only to chase carried strings.
- (−) A migration touching `TypedOp`/`RelScope`/the resolver/emission; the per-operator ordinal + lineage derivation must be Spark-exact — a silent wrong-column is the hazard, and ADR-014's differential oracle is the gate.

**Refinement hooks / adoption sequence** (each corpus-gated, zero-regression): substrate-name uniquify → emission ordinal shim → carry the ordinal → ambiguity (F11) + Spark-emulated error surfacing → add `source_quals` lineage → resolver consults lineage (flip F8/F10, preserve USING + correlation) → retire the strip machinery. Evidence gate: the four deferred witnesses (`filt-018`/`filt-019`, `join-023`, `jn-024`) flip; the ten strand witnesses (`join-018..022`, `cx-015/016`, `jn-023`, `agg-025`, `proj-016`) stay green; corpora show zero regressions.

---

## ADR-024 — τ stores attribute identity in the resolved schema; references bind to attributes, not positions

**Status:** Proposed (implemented 2026-07-12 as N9 increments 1–3; see `tasks/v2-canonicalization-invariants.md`)
**Depends on:** ADR-005 (schema-threading analysis — see its amendment note), ADR-006 (single resolve pass — see its amendment note), ADR-021 (τ owns its substrate), ADR-022 (two error categories)
**Supersedes:** ADR-023 (representation only; its decision outcomes are must-preserve constraints here)
**Depended on by:** N10 (bind-by-id emission: unique id-derived aliases retire the positional duplicate-name machinery — `bare_dup_slot` (N10-lite stage 1 rename of `bare_dup_ordinal`) and `requalify_column_ref` (N10-lite stage 2) are both id-keyed as of 2026-07-13; `output_uniquified`, `wrap_reprojected` remain future N10 work)

**Context.** ADR-023 correctly moved τ off carried string qualifiers, but its representation encoded one fact three ways: `ColumnReference.ordinal` (position as an identity surrogate), `RelScope.source_quals` (an ordinal-indexed parallel vector for lineage, with a hand-maintained `len == schema.len()` invariant and a mirror derivation kept in lockstep), and a structural `semantic_eq` whose ordinal re-walk (`ordinals_compatible`) existed only because `ColumnReference::eq` deliberately excludes `ordinal`. A first attempt to add Spark-style `exprId`s was **aborted** (2026-07-12, `tasks/v2-attribute-identity-unification.md`): deriving ids in `RelScope` re-mints them on every `TypedAst::new` re-derivation — and the sort-rebind path re-stamps mutated Aggregate/Project nodes, so derived ids go stale exactly where identity is consumed. The sound design requires identity to be **stored state that moves by value**.

**Decision.** τ's analyzer output schema is a τ-owned `ResolvedSchema(Vec<Attribute>)` where each `Attribute { name, data_type, nullable, expr_id, source_quals }` carries a stable identity (`ExprId`: globally-unique `AtomicU64` mint — Spark `NamedExpression.curId` precedent) and its own source-qualifier lineage. Identity is minted exactly once — at leaf sources and at computed/aliased output entries — and thereafter moves **by value**: passthrough operators clone attributes; a projected-through bare reference COPIES its target attribute (id and lineage, plus the stamped qualifier — F10); an Alias/computed entry MINTS fresh with ∅ lineage (F8); joins concatenate; USING keys union both donors' lineage onto the donor clone; set-ops keep the first child's ids (Spark `Union.output` precedent); and the sort-rebind's re-stamp is simply **deleted** — append-only promotion provably cannot change the derived scope, and the ids live in the moved schema, not in a derived side-structure. `resolve_column` stamps the matched attribute's `expr_id` onto the reference at exactly the sites that stamp `ordinal` (`Some(ordinal) ⟹ Some(expr_id)`, from the same attribute — a mechanical invariant); semantic equality compares ids where both sides carry them (replacing `ordinals_compatible`); tier-(f) lineage reads come off the attribute (replacing the parallel vector, its mirror derivation, and its length invariant).

**Alternatives considered (and rejected).**
- *Point-`exprId` only* (id field without the schema change): leaves `ordinal`-as-identity and the `source_quals` machinery standing — a second identity encoding on top of the first, net-negative.
- *Ids derived in `RelScope`* (like `source_quals` was): re-mints on re-derivation — the verified stale-id landmine; rejected by the abort review.
- *`expr_id` on the wire `StructField`*: violates INV10's value-type purity, breaks `StructField`'s derived `Eq/Hash` load-bearing uses, and the Arrow boundary drops the field anyway.

**Consequences.**
- (+) One identity encoding: `ordinals_compatible` deleted; `source_quals_of` (~200 lines), the parallel vector, its length invariant, and the unconditional `analyze_sort` re-stamp deleted; lineage and identity cannot desynchronize from the schema because they are fields *of* the schema.
- (+) Strengthens INV5: τ's resolved schema is strictly richer than the wire schema. `StructType`/`StructField` remain the value/Arrow-wire types (INV10), converted only at τ's two public entry points (`mod.rs::generate_with_schema` / `analyze_schema`).
- (+) The N10 substrate is in place: every output attribute and every resolved reference carries an id.
- (−) `ExprId` values are process-lifetime-unique but **not run-deterministic**; N10's id-derived emission aliases must renumber per query (e.g. dense-rank within the plan) so emitted SQL stays deterministic. **N10-lite (stage 1, 2026-07-13) satisfies this obligation vacuously**: it swaps the wrap/merge duplicate-name boundary's binding KEY to `expr_id` but mints no id-derived alias STRING — emitted aliases remain today's `uniquify(names)` — so there is nothing to renumber yet; the obligation resurfaces only if/when a future increment mints alias spellings from `expr_id`.
- (−) Recorded τ divergences from Spark: a column **rename keeps its id** (Spark's Alias-rename mints); the USING coalesced key keeps the **left donor's id** (Spark mints for the Coalesce alias). **Re-examined and CLOSED at N10-lite (2026-07-13):** both are benign for id-keyed dup-boundary binding. Rename-keeps-id's only observable reach is through `bare_dup_slot`/`requalify_column_ref`'s id→slot lookup, which is guarded by the H8 name-agreement `debug_assert` (`schema.fields[k].name` must match `c.name`) — a renamed column's schema entry carries the CURRENT name at its slot, so the id-resolved lookup and the assert stay in agreement; nothing downstream observes the stale id by itself. USING-left-donor-id produces a single coalesced slot with no duplicate-name sibling, so it never reaches the dup-boundary machinery (`name_count >= 2`) at all — no exposure, benign for first-occurrence binding by construction.
- (−) `ColumnReference.ordinal` survives as the execution-side currency Spark's own `BindReferences` also uses (deleting the field is a separate, non-goal future pass), but emission's TRUST in the stamped position at duplicate-name boundaries is retired wherever N10-lite reaches. **Stage 1 (2026-07-13)** retired it at the wrap/merge boundaries: `bare_dup_slot` (formerly `bare_dup_ordinal`) and its callers key the rewrite off `expr_id`. **Stage 2 (2026-07-13)**, gated on the disjointness pin (`self_join_left_right_resolved_schema_ids_are_disjoint`, analyzer.rs — PASSED), retired it at the join-condition path too: `requalify_column_ref` now keys off `expr_id` (`schema.fields.iter().position(|f| f.expr_id == id)`), guarded by a `debug_assert` in `requalify_join_condition` policing the left/right disjointness the side-split depends on. `ordinal` is carried but no longer consumed for identity anywhere in emission.

> **Amendment (2026-07-13, E4):** `ColumnReference.data_type`/`.nullable` are narrowed from `Option<DataType>`/`Option<bool>` to plain `DataType`/`bool` — a `ColumnReference` now exists only *post*-resolution, so there is no unresolved state for these fields to encode; a pre-analysis name is an `UnresolvedColumn` by construction (front-ends emit that; E1.5 deleted the last constructor that could build a bare `ColumnReference`). `expr_id` is unaffected and stays `Option<ExprId>` — its `None` states are the genuine, still-open gaps documented above, not an unresolved-vs-resolved distinction.

---

# Cross-Validation

This section is the instrument for checking the decisions against each other. Refining any single ADR above should be followed by a pass through §CV to confirm the change respects the dependency structure, the tension resolutions, the load-bearing assumptions, and the invariants.

## CV.1 — Layered structure

The ADRs are not peers; they form four strata.

**Premise (the selectors):** ADR-000 (positioning — single-node, no-JVM, DuckDB-backed), ADR-022 (τ is the only path; two error categories). Together these set the product shape and the runtime shape: what Thunderduck *is* and how it *responds*. A change here cascades furthest.

**Spine (the irreducible commitments, given the premise):** ADR-001 (transliterator), ADR-002 (emit-level delegation), ADR-005 (own the divergent slice), ADR-020 (strict-only; extension mandatory). These define what τ *is* and what it *owns*.

**Substrate & front-ends (the IR and how it is populated and analyzed):** ADR-003 (common AST), ADR-004 (both front-ends lower to it; relation-vs-command by parse-root), ADR-006 (analyzer pass structure), ADR-021 (τ owns its protobuf converter, Expression, TypeInferenceEngine). These define the representation and the substrate boundary.

**Consequences (the spine applied to surfaces):** ADR-007 (A/B/C, B retained) from 001+002+005+006+008; ADR-008 (correlated direct) from 001; ADR-009 (declarative table, hand-written match arms) from 003+005+007; ADR-010 (extension fns, C++ project) from 005; ADR-011 (commands) from 001+004; ADR-012 (catalog overlay) from 011+005; ADR-013 (external/lakehouse reads, delegated) from 000+002+012; ADR-017 (Delta append writes) and ADR-018 (UC-managed Iceberg CTAS/INSERT/DELETE/MERGE writes) — the per-format write specializations — both from 005+011+013+016; ADR-019 (the native-read / Iceberg-write lakehouse I/O contract) composing 013+018; ADR-023 (ordinal column references; emission-time qualifier regeneration) from 005+006+021+022 — a refinement of the owned analysis-and-emission reference model.

**Enabled (the testing architecture the rest makes possible):** ADR-014 (two decision spaces), ADR-015 (differential + AnalyzePlan oracle), ADR-016 (version pin). These exist *because* the spine made τ minimal and its decisions enumerable; they are not free-standing choices.

Implication for refinement: a change to the premise (ADR-000) or a spine ADR (esp. ADR-005's scope) propagates widely; a change to an enabled ADR is comparatively contained.

## CV.2 — Dependency matrix

`→` reads "is depended on by / feeds."

| ADR | Depends on | Feeds |
|---|---|---|
| 000 positioning | — | 001, 002, 003, 004, 005, 006, 013 |
| 001 transliterator | 000 | 007, 008, 011, 015 |
| 002 delegation | 000, DuckDB-correct (LB2) | 003, 005, 007, 013 |
| 003 common AST | 000, 002 | 004, 005, 009 |
| 004 front-ends → AST | 000, 003 | 005, 011, 015 |
| 005 type/null inference | 000, 002, 003, 004, 012 | 006, 007, 009, 010, 012, 014, 015, 017, 018, 023 |
| 006 analyzer passes | 000, 005 | 007, 023 |
| 007 A/B/C | 001, 002, 005, 006, 008 | 009 |
| 008 correlated direct | 001 | 007 |
| 009 declarative table | 003, 005, 007 | 014, 015 |
| 010 extension fns | 005 | 015, 016 |
| 011 commands | 001, 004 | 012, 017, 018 |
| 012 catalog overlay | 011, 005 | 005, 013, 015 |
| 013 external/lakehouse reads | 000, 002, 012 | 015, 017, 018, 019 |
| 014 two decision spaces | 005, 009, 016, DuckDB-correct (LB2) | 015 |
| 015 oracle | 001, 004, 005, 009, 010, 012, 013, 014, 016, 017, 018 | — |
| 016 version pin | 010 | 014, 015, 017, 018 |
| 017 Delta writes (append) | 005, 011, 013, 016 | 015 |
| 018 Iceberg writes (UC managed) | 005, 011, 013, 016 | 015, 019 |
| 019 lakehouse I/O contract | 013, 018 | — |
| 020 strict-only extension | 000, 001, 010 | 009, 015 (simplified) |
| 021 τ owns substrate | 002, 003, 004, 005, 014, 015 | every implementation slice; INV10; 023 |
| 022 τ is the only path | 000, 002, 021 | every implementation slice; two-error-category rule; 023 |
| 023 ordinal references | 005, 006, 021, 022 | the qualified-resolution slices (F8/F10/F11 closure); retirement of the emission strip machinery |

**New external dependency:** the [`thunderduck-duckdb-extension`](https://github.com/lastrk/thunderduck-duckdb-extension) C++ project is an external build artifact that ADR-010 depends on. It is not an ADR but it participates in the dependency graph: the dispatch table's `Extension(...)` targets must match its exported functions (INV6), its behavior must be differentially validated (edge 010 → 014), and it is the third member of the version-coordination set (edge 010 → 015, alongside Spark 4.1.1 and the dispatch table).

**Cycle to note:** ADR-005 and ADR-012 are mutually dependent — inference reads the catalog overlay, and the overlay is *sized by* what inference needs to know. Resolve by treating the overlay's contents as defined by inference's requirements: the overlay carries exactly the Spark base-column facts the coercion and nullability lattices consume, nothing more. Refining either must keep them co-designed.

**Most-depended-on nodes:** ADR-000 (premise — feeds five and selects the approach) and ADR-005 (own the divergent slice — feeds six). ADR-005 remains the highest-leverage *implementation* decision; ADR-000 is the highest-leverage *strategic* one. Changing ADR-005's scope, or ADR-000's no-JVM premise, are the two most expensive refinements.

## CV.3 — Tension points

Places where decisions pull against each other, with the resolution each currently relies on. These are the first things to re-examine if an ADR changes.

**T1 — Delegation vs ownership (ADR-002 ↔ ADR-005).** ADR-002 delegates structural resolution; ADR-005 owns type inference; but type inference *needs* resolved structure (you cannot type `a + b` without knowing the types of `a` and `b`, and `a` may have arrived via a delegated, unexpanded `*`). **Resolution:** emit-level delegation, analysis-level ownership — thunderduck internally threads a Spark-typed schema through the *same* structure it emits in delegated form, including an internal resolver/star-expander used only for typing (INV5). This is *the* central tension; if it is mis-resolved, ADR-005 has a blind spot exactly on compositional cases (expression over join over star), which is the highest-priority divergence surface.

**T2 — No-optimization vs expressibility-forced gray zone (ADR-001 ↔ ADR-007/008).** A transformation could be both correctness-forcing and optimization-shaped; separately, cosmetic cleanup is neither forced nor cost-motivated; and SQL-driven desugarings (ADR-003) are expressibility-forced structural rewrites. **Resolution:** classify by *motive* — cost-driven is forbidden; expressibility-forced is required (and lives in B, ADR-007); result-irrelevant syntactic reduction is permitted (cosmetic, under ADR-001's guardrail); and a *narrow, enumerated* carve-out is allowed for correctness-forcing-yet-optimization-shaped transformations recorded in ADR-001's carve-out register. ADR-008 (correlated subqueries emitted directly rather than rewritten to lateral) is the worked example proving the no-rewrite discipline. Watch both the cosmetic set and the carve-out register for creep.

**T3 — Cast-fixes-it vs needs-extension (ADR-005 ↔ ADR-010).** Some Spark/DuckDB mismatches are repairable by inserting casts (stay native); others require an extension function. **Resolution:** prefer casts where a cast sequence reproduces Spark semantics *exactly* — both value and result type; use an extension function only where no cast sequence does. The canonical needs-extension case is a **return-type divergence** (e.g. decimal÷decimal: DuckDB returns `double`, Spark returns `decimal`): a cast on the wrong-typed native result may not recover Spark's value, so the decision is made jointly by inference (ADR-005 infers the Spark result type) and emission (ADR-010 emits a function producing that value and type). Each extension function's *why* annotation records which side of this line it sits on.

## CV.4 — Load-bearing assumptions

Assumptions whose failure cascades across multiple ADRs. Each is empirically checkable; the check is named.

**LB1 — The divergent slice is {type inference, nullability}.** If structural resolution *also* diverges from Spark for some construct, ADR-002's boundary and ADR-005's scope expand, and ADR-006/007/009/013 shift with them. **Check:** AnalyzePlan schema diff (catches type/nullability divergence) plus the result differential (catches behavioral divergence not explained by types). This is the keystone *implementation* assumption — validate it empirically rather than assuming the slice is exactly two items at the outset.

**LB2 — DuckDB correctly executes valid SQL.** If false in cases that matter, ADR-014's excluded third bucket becomes real and attribution gains a genuine ambiguity. **Check:** emitted-SQL capture plus manual triage; partial reliance on DuckDB's own test suite.

**LB3 — DuckDB's structural resolution (binding, star, scope) matches what's wanted.** If false, ADR-002's delegation breaks for those constructs and they must move into the owned slice. **Check:** AnalyzePlan schema diff (ADR-015) on starred / ambiguous-column / deep-scope plans. Treat delegation as provisional until that diff is clean.

**LB4 — τ never needs an optimization to be *correct* (only for performance).** If a correctness-forcing transformation is also optimization-shaped, ADR-001 needs a carve-out. **Check:** monitor the forced-transliteration set and ADR-001's carve-out register; the correlated-subquery case (ADR-008) is the canonical test of the boundary.

**LB5 — The A/B/C structure plus the C++ extension functions is expressive enough to correctly translate every *supported* DataFrame-or-SQL expression into a semantically matching DuckDB SQL expression.** This is an assumption about the world, not a property to preserve, which is why it is an LB and not an INV: it can be false — there may exist a Spark expression whose semantics *no* combination of DuckDB SQL + extension function reproduces, in which case the only correct behavior is to move it from the supported set to the rejected set. It is partly a bet *about the extension project* (ADR-010): "we can always write a C++ extension that closes any remaining semantic gap." The sharpest test cases live among the SQL-only constructs (ADR-003), e.g. recursive CTEs. **Check:** the differential result test (ADR-015) is precisely the falsifier — a *supported* expression that produces a divergence no emission can fix has falsified LB5 for that expression. Failure mode is reclassification (supported → rejected), not silent wrongness.

**LB6 — The single-node ceiling is sufficient for the target workloads.** (From ADR-000.) The entire positioning bets that the target users' workloads fit on one large machine. If they do not, the product thesis — not just an implementation detail — is wrong. **Check:** product/workload validation, not a test; but it is the premise every other decision rests on, so it is recorded here as the assumption with the widest blast radius.

**LB7 — DuckDB's storage extensions read external/lakehouse tables faithfully to Spark.** (From ADR-013.) For Hive-Parquet, Delta, Iceberg, and Unity-Catalog tables, the DuckDB readers (`parquet`/`httpfs`/`delta`/`iceberg`/`uc_catalog`) must produce column schemas *and* values matching what reference Spark produces reading the same table. This is distinct from LB5 (which is about thunderduck's *emission* expressiveness); LB7 is about the *delegated readers*, much of which is outside thunderduck's control. The known-exposed surfaces are partition-column typing on path-based reads and the format-type→Spark-type mapping (ADR-012). **Check:** read the same external table in reference Spark and diff the resolved schema (AnalyzePlan-style, ADR-015) and the values (result differential). Failure response is bounded: pin/track the offending DuckDB extension version, or reject that table/type (move it to the unsupported set) — not silent divergence.

**LB8 — DuckDB's extensions *write* external/lakehouse tables to a state faithful to Spark's write.** (From ADR-017; the write-direction sibling of LB7.) For the supported write paths (Delta append, ADR-017; UC-managed Iceberg CTAS/INSERT/DELETE/MERGE, ADR-018), the resulting table state — visible rows and the format's version/log lineage — must match what Spark's corresponding write would produce. This is a *distinct and currently shakier* bet than read-fidelity: DuckDB's Delta write surface is young (write support only recently de-experimental), with a noted delta-kernel-rs OneLake regression. The Iceberg write path (ADR-018) is better-grounded — Managed Iceberg is GA with explicit external-engine (DuckDB) write support — but carries its own residual fidelity conditions: the target table must be merge-on-read, and partitioning must be a DuckDB-supported transform, else the operation is a typed rejection. **Check:** state-diff oracle (ADR-011 / ADR-015) — read-after-write logical-row comparison plus version lineage, through both engines. Failure response is bounded exactly as LB7: pin/track the extension version, or reject the operation (it is already a typed rejection for everything past append). Scope grows only as DuckDB's writers mature.

**LB9 — Spark parity is a test-time property enforced by ADR-015's differential oracle against reference Spark, not a compile-time property inherited from any substrate.** (From ADR-021.) τ owns its `TypeInferenceEngine` and its analyzer; the oracle validates τ's outputs against reference Spark on an unbounded corpus of DataFrame + SparkSQL cases. Symmetric-omission gaps in the analyzer's function-name enumerations (e.g. a function present in `aggregate_return_type` but missing from `aggregate_is_nullable`) transit into corpus reds — the oracle catches them. **Check:** the harness itself. If it has coverage gaps, LB9 can fail silently for those gaps; mitigation is the same as LB5's — grow the harness, or reject the case class explicitly. Failure mode is silent divergence on un-covered cases, which is exactly what ADR-015 is designed to prevent — so LB9's soundness rests on ADR-015's coverage discipline.

## CV.5 — Cross-cutting invariants

Properties that span ADRs. Any refinement must preserve all of these; a change that breaks one is a signal that the change is larger than it appears.

**INV1 — Both engines receive byte-identical input.** (Touches ADR-015; constrains ADR-001.) Parity-via-identical-bytes is achieved by serialize-once-send-twice. Note this is *not* violated by ADR-001's cosmetic simplifications: cosmetic simplification is a τ transformation applied *once*, upstream of the single serialization, so both engines still receive the same simplified bytes — and DuckDB SQL is consumed only by DuckDB, never by Spark, so the cosmetic DuckDB cleanup is invisible to the comparison. (This is exactly why the rejected production-*canonicalizer* was different: it was proposed as a normalization that could differ from what Spark sees.) A proposal to add production-side normalization that could differ per engine, or that Spark would observe, must demonstrate it does not break this.

**INV2 — Every τ decision is node-local (post-A) or a labeled C escape hatch.** (Touches ADR-007, ADR-009.) A new decision that is non-local must either be made local by the A pass (push the fact into the node) or be a *counted* C entry. It may not be a hidden closure inside the emission table. Genuinely structural forced transliterations live in the retained B layer (ADR-007), not as hidden table closures.

**INV3 — The emission table is the single source of truth for generation and coverage.** (Touches ADR-009, ADR-014, ADR-015.) Refinements to the table must keep both the input grammar and the coverage denominator derived from it; they must not drift into separate artifacts.

**INV4 — Inference is validated in isolation before translation tests run.** (Touches ADR-005, ADR-006, ADR-015.) Preserves attributability (ADR-014). The AnalyzePlan schema diff must be green before result-level translation failures are interpreted as translation bugs. Applies also to rule *provenance*: an LLM-extracted coercion/nullability rule is not trusted until the diff is green for it.

**INV5 — thunderduck knows the schema everywhere, even where it emits delegated structure.** (Touches ADR-002, ADR-005.) The internal resolver/star-expander for type-tracking must not be removed on the grounds that resolution/star-expansion is delegated. Emit-level delegation ≠ analysis-level delegation.

**INV6 — Every `Extension(...)` target in the dispatch table corresponds to an existing, loaded function in the `thunderduck-duckdb-extension` C++ project.** (Touches ADR-009, ADR-010.) Unlike LB5 (an empirical bet about expressiveness), this is a mechanically *checkable, preservable* property — verify at build/test time that the table's emission targets and the extension's exported symbols agree. It is the mechanical complement to LB5: LB5 asserts an adequate extension *can* be written; INV6 asserts every extension the table *names* actually *exists and is loaded*. A compiled-dispatch build (ADR-009) can enforce INV6 at compile time.

**INV7 — none.** No per-run front-end-agreement invariant is imposed. τ has two front-ends (`V2RelationConverter` for Spark Connect protobuf; the SparkSQL lowering for `spark.sql(...)` input), each independently validated against reference Spark by ADR-015's differential oracle. Agreement between them is transitive from Spark parity — Spark itself agrees with itself on the same query. When adding a new CommonAST variant, both front-ends' lowering rules must aim at the same variant for the same construct; this is a design property preserved by construction (via lowering-rule discipline), not by a checked invariant.

**INV8 — External-table access is always delegated to a DuckDB storage extension.** (Added with ADR-013; touches ADR-002, ADR-013.) thunderduck emits the storage-extension surface (`read_parquet`/`iceberg_scan`/`delta_scan`/`ATTACH TYPE iceberg`/`uc_catalog`) and **never** parses a table format, reads a transaction log, or speaks a catalog protocol itself. This is the bounded-scope line for storage, analogous to INV5 (don't remove the internal type-resolver) and INV6 (every extension target exists): it keeps the external-table surface a *translation* concern, not a reimplementation one. A proposal to read a format directly in thunderduck must demonstrate why delegation is impossible — and would reopen ADR-013.

**INV9 — A writable external relation must have attached-catalog provenance; path-scan provenance is read-only.** (Added with ADR-017; touches ADR-011, ADR-013, ADR-017.) External tables reached by a bare path-scan (`read_parquet` / `delta_scan` / `iceberg_scan`) are read-only by construction; any write (append/insert/delete/merge/CTAS) requires the table to be reached via an attachment (`ATTACH … TYPE delta`/`iceberg`, or `uc_catalog`). This is the rule that keeps the write story consistent across formats: every per-format write ADR (Delta ADR-017; Iceberg ADR-018; and any future format) must route writes through an attachment, never a path-scan. This is reinforced externally: Databricks UC forbids path-based access to managed tables outright (ADR-018), so for UC targets the invariant is enforced by the catalog as well as by thunderduck. **Check:** the overlay's recorded provenance (ADR-012/013) gates whether a write command may be emitted at all.

**INV10 — τ imports only value-level types from outside its own module tree.** (Touches ADR-003, ADR-004, ADR-005, ADR-014, ADR-021.) The `crates/core/src/transpiler_v2/` module tree, `crates/connect-server/src/converter/v2_relation_converter.rs`, and `crates/core/src/parser_v2/` are τ. Behavior-carrying types (`LogicalPlan`, `Expression`, `TypeInferenceEngine`, `SqlGenerator`, `FunctionRegistry`) are τ's own — τ does not import any such type from a non-τ module. Value-level types (`DataType`, `StructType`, `StructField`) live in `crate::types::*` and are used verbatim. (Clarification, ADR-024: `Attribute`/`ResolvedSchema`/`ExprId` are τ-owned *analysis* types living in `transpiler_v2`; `StructType`/`StructField` remain the value/Arrow-wire types, converted at τ's `mod.rs` boundary.) This is the *input-side* complement to INV3's *emission-side* single-source-of-truth rule; together INV3 + INV10 bracket τ's substrate boundary. **Check:** `git grep -E 'use crate::(logical|expression|generator|functions)::|use crate::types::TypeInferenceEngine' crates/core/src/transpiler_v2/ crates/connect-server/src/converter/v2_relation_converter.rs crates/core/src/parser_v2/` returns zero. INV10 is checkable regardless of what code lives on the other side of the boundary; when no non-τ modules exist anymore, the grep is trivially satisfied.

### CV.5.1 — Invariant scoping conventions

**Sub-invariants.** Some INV<N> paragraphs cover invariants with multiple orthogonal dimensions. Each dimension is a distinct property to preserve; each activates once the τ substrate that realizes it lands. The invariant paragraph is the canonical statement of the property; the sub-invariant dimensions are the enumerable properties that fill it. A pass's completion is measured against the sub-invariants it *claims* to activate, not against the invariant paragraph as a whole.

**Two-marker convention** for the stubs in `crates/core/src/transpiler_v2/invariants.rs`:

- `TODO INV<N>:` — within-current-slice unblocking work. A `git grep 'TODO INV<N>'` returning empty is the completion signal for that invariant (or sub-invariant) at the current slice.
- `DEFER INV<N> → <owning-slice>:` — the invariant (or sub-invariant) is reassigned to the named future slice; the stub is replaced when that slice's substrate lands. Deferred markers do NOT trip `git grep 'TODO INV<N>'`.

When a reassignment happens, the pass performing it updates the marker in source.

**Cross-check.** `git grep 'TODO INV'` returning empty crate-wide is the load-bearing completion check for whatever slice is currently landing. `git grep 'DEFER INV'` returning entries is expected: each entry is a claim of ownership by a named future slice, not un-owned unblocking work.

## CV.6 — Suggested ratification order

Review premise-first, then spine, then substrate, then consequences, then the enabled testing layer — because downstream ADRs inherit upstream framing:

1. **ADR-000** (positioning) first — it selects the whole approach; ratifying it is the precondition for everything (and rejecting it reopens Alternatives 1/3).
2. **ADR-001 → 002 → 005** (spine), and with 005 resolve Tension T1 and confirm LB1's validation plan.
3. **ADR-003 → 004 → 006** (substrate & front-ends), confirming INV7 and the bounded-extension rule; OQ-1 is closed here by ADR-004.
4. **ADR-007 → 013** (consequences) in dependency order per CV.2 — this group now includes external/lakehouse reads (ADR-013), which depends only on the delegation premise (000/002) and the overlay (012).
5. **ADR-016** (version pin — it scopes the coverage claims, so fix it first) then **ADR-014 → 015** (the enabled testing architecture).
6. **ADR-020** (strict-only extension), **ADR-021** (τ owns substrate), and **ADR-022** (τ is the only path). ADR-020 consolidates the emission target; ADR-021 pins the substrate boundary (τ owns its protobuf converter, Expression, TypeInferenceEngine); ADR-022 pins the runtime position (τ is the only path; two error categories; no fallback). Together with ADR-000's premise, these three shape every implementation slice.
7. **ADR-023** (ordinal column references) last — it refines ADR-005/006's resolve pass and presupposes ADR-021's substrate ownership and ADR-022's error categories, all ratified in the steps above.
8. **ADR-024** (stored attribute identity) supersedes ADR-023's representation while preserving its decision outcomes — ratify in ADR-023's slot; ADR-023 needs no separate ratification.

Defer no ADR's *ratification* past the point where something depending on it is ratified — the matrix in CV.2 gives the order. The two highest-value review items are **ADR-000's no-JVM premise** (widest blast radius; if it moves, Alternative 1 deletes ADR-005/006) and **ADR-005's scope together with LB1** (where the implementation cost and risk concentrate).

---

# Resolved & Open Questions

## OQ-1 — Raw-SQL `spark.sql("…")` handling — RESOLVED by ADR-004

**Status: RESOLVED.** Raw SQL is parsed by Thunderduck's SparkSQL parser into the common AST (ADR-003) and flows through the same analyzer and τ as DataFrame plans; relation-vs-command is decided by parse-tree root (ADR-004 → ADR-011). Option (a) reject-raw-SQL is rejected (incompatible with the `spark.sql` hard requirement); option (c) string/regex transliteration is rejected (tried in practice, produced an unmaintainable regex chasing dialect mismatches). The chosen option (b) — parse to common AST, single path — gives raw-SQL `SELECT`s the same Spark-parity guarantees as DataFrame `SELECT`s, at the cost of owning a SparkSQL parser (accepted under ADR-000). Front-end agreement between the SparkSQL parser and the protobuf converter is transitive from each front-end's independent Spark-parity validation via ADR-015's oracle.

## OQ-2 — External / lakehouse table WRITE paths — PARTIALLY ADDRESSED; remainder deferred per-format

**Status: partially addressed.** Write paths are being tackled one format/operation at a time, each as its own ADR. **Delta append is now specified by ADR-017** (`INSERT`/append into a pre-existing attached Delta table; one action → one transaction → one version). The remainder are still deferred, each gated on DuckDB's evolving write surface:
- **Delta `DELETE` / `MERGE` / `UPDATE` / overwrite / CTAS-create** — rejected by ADR-017 today (DuckDB-Delta has no delete/update/merge and no confirmed table-creation DDL). Each becomes its own ADR (or an amendment to ADR-017) when the Delta extension ships the corresponding capability; ADR-017 carries the revisit-triggers.
- **Iceberg writes into UC-managed Iceberg** (`CREATE TABLE AS`, `INSERT`, `DELETE`, `MERGE`) — specified and **verified (June 2026)** in **ADR-018** (via the attached UC Iceberg REST endpoint, single-table, merge-on-read deletes). The three drafting-time gates resolved favorably (external-engine writes supported with DuckDB named; write-scoped credential vending; CTAS without pre-creation); residual per-table conditions are merge-on-read mode and DuckDB-supported partitioning. Iceberg writes against *non-UC* REST catalogs (Polaris, Lakekeeper, R2, S3 Tables, Glue) would be a near-identical but less-constrained variant, not yet drafted. The end-to-end stance — read inputs in their native format, write results as Iceberg, both via UC — is now fixed by **ADR-019**, which composes the read path (ADR-013) and the Iceberg write path (ADR-018) and deliberately avoids both Delta writes and cross-format single-table access.
- **Unity-Catalog-managed writes** — follow the format split (UC→Delta via `uc_catalog`; UC→Iceberg via the REST endpoint) and inherit the same per-format maturity, with UC managed-table writes additionally subject to the open `uc_catalog` issues noted earlier.

*No other open questions remain at this time.*
