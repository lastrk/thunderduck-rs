# ADR-004 — SQL and DataFrame both lower to the common AST; relation-vs-command is decided by parse-tree root

**Status:** Proposed
**Depends on:** ADR-000, ADR-003
**Depended on by:** ADR-005, ADR-011, ADR-015, ADR-026
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

**Two v2 front-ends, one CommonAST.** τ has two front-ends: `V2RelationConverter` (Spark Connect protobuf → CommonAST) for DataFrame calls, and the v2 SparkSQL lowering (raw SQL text via `spark.sql(...)`) for statement-shaped calls. Both produce the same operator variants for the same construct. Source-only metadata is exempt: Connect supplies `plan_id`, while SparkSQL uses `None` (ADR-026). Each front-end is independently validated against Spark by ADR-015; no per-run equality check is imposed.

---

