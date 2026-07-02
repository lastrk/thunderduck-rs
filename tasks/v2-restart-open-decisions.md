# v2 Restart — Open Architectural Decisions

**Purpose.** Decisions that are load-bearing for parallel-track execution of the v2 restart but were **not covered** by `docs/thunderduck-rearchitect-ADRs.md` at the time of the /goal template's landing. Each entry names the decision, why the ADRs were silent on it, what would happen if it stayed undecided, and the options with their trade-offs. Once a decision closes, the corresponding ADR is amended (or a new one added) and the entry here is stamped **RESOLVED**.

**Status as of 2026-07-02:** all twelve decisions RESOLVED (user directive, 2026-07-02). See the summary table below and the per-decision resolution notes. **ADR-022** was drafted to consolidate the largest cluster of resolutions (no runtime fallback; legacy is superseded; two error categories; INV7 deletion). Other resolutions are absorbed by amendments to existing ADRs (see per-decision entries) or are project-level scheduling decisions not requiring ADR text.

**Discipline going forward.** New parallel-track blockers surfaced during slice execution are appended here as new decisions (Decision 13, 14, ...); a Pass 1 architect who hits an unclosed decision HALTS-AND-FLAGS per the /goal template rather than resolving it unilaterally.

---

## Resolutions summary (2026-07-02)

| # | Decision | Resolution | Landed in |
|---|---|---|---|
| 1 | v2 SparkSQL front-end lowering location | Option **1b** — `crates/core/src/parser_v2/` fork | Readiness map §Slice A.2 |
| 2 | V2RelationConverter exhaustiveness | Option **2c** — hybrid (structured shapes in A.2; complex shapes with owning slice) | Readiness map §Slice A.2 |
| 3 | Fallback-eligibility predicate | **NEVER FALL BACK.** Two error categories (Spark-emulated vs Thunderduck-boundary) | **ADR-022** |
| 4 | INV7 structural equality scope | **INV7 DELETED.** Each v2 front-end independently validated against Spark by ADR-015 | **ADR-022** + INV7 amendment |
| 5 | Set-op widening scope | Option **5b** — UNION + INTERSECT + EXCEPT widen; UNION BY NAME deferred to Slice G | Readiness map §Slice B |
| 6 | B-layer module location | Option **6a** — `crates/core/src/transpiler_v2/rewrites.rs` | Readiness map §Slice C.1 |
| 7 | Compiled dispatch commitment | Option **7a** — Approach A (hand-written match) permanent | ADR-009 (recorded in refinement post-Slice-C) |
| 8 | BaseTypes overlay sharing | **Per-path** — v2 has `crate::transpiler_v2::BaseTypes` seeded independently | Readiness map §Slice A.2 |
| 9 | Corpus test attribution instrumentation | **MOOT** — no fallback, no dual testing (per Decision 3 + Decision 11) | **ADR-022** |
| 10 | Sub-slice progress signal | Option **10c** — cumulative per-sub-slice targets in scope files | Iteration methodology §CV.7 |
| 11 | Legacy path deletion timing | **Delete on this branch.** Legacy is not maintained during the reimplementation; may stay as reference source only, no test/runtime path | **ADR-022** |
| 12 | CommonAST plan_id semantics across front-ends | Option **12a** — `Option<i64>`; SparkSQL leaves None; analyzer handles both | Readiness map §Slice A.2 + §Slice B (follows from INV7 deletion) |

---

## Decision 1 — v2 SparkSQL front-end lowering: crate, module, and timing — **RESOLVED**

**RESOLVED (2026-07-02):** Option **1b** — new crate module `crates/core/src/parser_v2/` (full fork of the parser + lowering). User: *"the duplication is only temporary, we will eventually rip out the legacy path completely."* The parser fork is bounded — under ADR-022 legacy is superseded, so the parser fork's ~500 LOC is a temporary duplication that ends when legacy is deleted. Owned by Slice A.2. See ADR-022 for the "legacy is superseded" endpoint.

---

**The gap.** ADR-021's refinement of INV7 (post-Slice-D, 2026-07-02) narrows the front-end-agreement invariant to *the two v2 front-ends*: `V2RelationConverter` (Spark Connect protobuf → CommonAST) **and** the SparkSQL parser lowering into CommonAST for `spark.sql("...")` input. The `V2RelationConverter` is explicit in ADR-021 and in the readiness map's Slice A scope. The v2 SparkSQL lowering is not — no ADR names its crate, module, or the slice that owns it.

**Why it bites.** Without a landing decision, one of three failure modes is guaranteed:

- The v2 SparkSQL path silently falls through to legacy for every raw-SQL case (INV7 is half-active — dispatch cannot honor the "each path has its own front-end" commitment from ADR-004's refinement).
- Slice A's architect improvises a module location that a later slice must renegotiate.
- A parallel slice (e.g. Slice B analyzer, Slice C emission) writes tests against DataFrame-only inputs, and the v2 SparkSQL path shows up as red-on-corpus only after multiple slices have shipped.

**Options.**

- **1a — Co-located under `crates/core/src/transpiler_v2/sql_lowering.rs`.** The v2 SQL lowering shares the sqlparser-rs parse tree with legacy (`crates/core/src/parser/dialect.rs`) but has its own converter. Advantage: v2 stays in one crate; INV10's grep barrier is one path prefix. Disadvantage: the parser module (`crates/core/src/parser/`) is now half-legacy, half-v2-adjacent, which is a naming smell.
- **1b — New crate module `crates/core/src/parser_v2/`.** *(CHOSEN)* Full fork of the parser + lowering. Advantage: cleanest INV10 barrier — v2 and legacy share nothing at the parser layer either. Disadvantage: sqlparser-rs's SparkDialect (`crates/core/src/parser/dialect.rs`) would be duplicated or moved; ~500 LOC parser fork is real — but temporary given ADR-022's legacy-deletion endpoint.
- **1c — Shared parse tree, split lowering.** Legacy's `sql_converter.rs` stays as-is producing legacy `LogicalPlan`; a new sibling `v2_sql_lowering.rs` in `crates/core/src/parser/` produces v2 CommonAST. The sqlparser-rs parse tree (dialect + AST) is the shared substrate.

---

## Decision 2 — V2RelationConverter exhaustiveness: big-bang or incremental Punt? — **RESOLVED**

**RESOLVED (2026-07-02):** Option **2c** — hybrid. Slice A.2 handles the "obvious" proto shapes (Project/Filter/Sort/Limit + primitive Aggregate). Complex shapes (Join, complex types, table functions) grow with their owning slice. Estimated Slice A: ~2000 LOC.

Under ADR-022's no-fallback commitment, an un-handled proto shape does not `Punt` to legacy — it surfaces as a Thunderduck-boundary error (an `Unsupported*` variant per the two-error-category rule). Each subsequent slice replaces boundary errors for its target shapes with structured CommonAST variants.

---

**The gap.** ADR-021's refinement hooks say "V2RelationConverter's protobuf surface must be exhaustive over the Spark Connect proto set that v2 targets. Any un-handled proto message shape is a typed `Punt` that routes to legacy per the additive-landing model of ADR-002 — not a silent black-box." This admits both a big-bang implementation (all ~1000 LOC of proto handling in one slice) and an incremental implementation (only enough to cover the target case IDs, everything else `Punt`s). ADR-021 does not choose.

**Options.**

- **2a — Big-bang exhaustive V2RelationConverter in Slice A.2.** All Spark Connect Relation proto message shapes handled or explicit-Punted.
- **2b — Incremental with corpus-driven growth.** Slice A ships only proto shapes needed for the analyzer-only-passing set (~15–25 cases). Each subsequent slice extends V2RelationConverter for its target proto shapes.
- **2c — Hybrid: structured shapes in Slice A, complex shapes in later slices.** *(CHOSEN)* Slice A handles the "obvious" proto shapes; complex shapes grow with their owning slice.

---

## Decision 3 — Fallback-eligibility predicate — **RESOLVED**

**RESOLVED (2026-07-02):** No fallback. User directive: *"we should never fall back to legacy paths."* Predicate is not needed because there is no fallback path. v2 errors surface directly to the caller and split into exactly two categories:

1. **Spark-emulated errors** — inputs Spark would itself reject. v2 emulates Spark's error semantics (same class, same message shape, same failure mode).
2. **Boundary errors ("unsupported by Thunderduck")** — inputs Spark accepts but v2 has not implemented. Simplest possible handling; these are the one place Thunderduck-specificity leaks through the Spark Connect facade — deliberately, and named.

See **ADR-022** for the full contract. The `V2FallbackEligible` trait, the dispatch env-var routing, and the corpus attribution instrumentation (Decision 9) all become unnecessary.

---

**The gap (historical).** ADR-002's post-Slice-C refinement introduced the additive-landing fallback contract: v2 produces typed errors, the dispatcher exposes `is_v2_fallback_eligible` to decide whether to route to legacy. The refinement did not specify the predicate's location, signature, or which error variants qualify.

The gap is closed by ADR-022's supersession of the fallback contract.

---

## Decision 4 — CommonAST structural equality: INV7's "identical" scope — **RESOLVED**

**RESOLVED (2026-07-02):** **INV7 is DELETED.** User: *"I am wondering what are we really getting out of this constraint at all."* The reasoning: under ADR-022's no-fallback / single-implementation stance, each of v2's two front-ends is independently validated against reference Spark via ADR-015's differential oracle. If both front-ends are Spark-correct, they must agree with each other transitively (Spark itself agrees with itself). INV7's per-run check is redundant with ADR-015. The design property "both front-ends target the same CommonAST variant for the same construct" survives, preserved by the lowering rules aiming at the same variants — but no per-run check is performed.

See **ADR-022** + the INV7 amendment in the ADRs file's CV.5 section for the retirement details.

---

**The gap (historical).** INV7 (v2 front-ends produce the same CommonAST node for semantically-equivalent inputs) was stated at the paragraph level in ADR-021's refinement of INV7. What "the same node" meant precisely — bit-identical, structural-modulo-metadata, or semantically-equivalent-post-analyzer — was not specified.

The gap is closed by INV7's deletion.

---

## Decision 5 — Set-op widening scope: UNION only or UNION + INTERSECT + EXCEPT? — **RESOLVED**

**RESOLVED (2026-07-02):** Option **5b** — all three set-ops widen: UNION/INTERSECT/EXCEPT get widened schemas from analyzer. UNION BY NAME is deferred to Slice G. Owned by Slice B (analyzer's set-op widening sub-sweep) and Slice E (emitter's per-column CAST wrapper).

**Requires an ADR-006 refinement amendment** naming the three-set-op scope. The amendment is small and can land inline with Slice B's implementation.

---

**The gap.** ADR-006's post-Slice-C refinement pins the analyzer/emitter contract for **UNION**: analyzer computes the widened schema; emitter's `render_union` applies per-column CAST wrappers. The refinement mentions "`render_intersect` / `render_except`, once those set-ops learn to widen" — an aside admitting the scope was not yet decided.

**Options.**

- **5a — UNION only.** INTERSECT and EXCEPT rely on DuckDB's native binder.
- **5b — All three set-ops widen.** *(CHOSEN)* Analyzer sub-pass covers UNION/INTERSECT/EXCEPT; emitter ships three CAST-wrapping renderers.
- **5c — All three plus UNION BY NAME.** Spark supports `UNION BY NAME` and `UNION ALL BY NAME` — column-position vs column-name matching adds a second axis. BY NAME semantics is deferred to Slice G.

---

## Decision 6 — B-layer module location for future SQL desugarings — **RESOLVED**

**RESOLVED (2026-07-02):** Option **6a** — `crates/core/src/transpiler_v2/rewrites.rs`. Empty at Slice C.1 (created as substrate); populated by Slice G when GROUPING SETS / LATERAL VIEW / PIVOT desugarings arrive.

---

**The gap.** ADR-007 defined the B layer (tree-rewrite / forced transliteration) as "retained but currently empty of cost-driven rules and minimal in forced ones." Future SQL desugarings from ADR-003 (GROUPING SETS → UNION ALL of grouped aggregates; LATERAL VIEW / explode → UNNEST; PIVOT) are the canonical B-layer citizens. But ADR-007 did not specify B's code location.

**Options.**

- **6a — `crates/core/src/transpiler_v2/rewrites.rs`.** *(CHOSEN)* A `TypedAst → TypedAst` transform layer between analyzer and emitter.
- **6b — Rewrite hook inside emission.** Each renderer internally calls `rewrites::maybe_desugar`.
- **6c — Deferred until B has ≥2 rules.**

---

## Decision 7 — Compiled dispatch commitment — **RESOLVED**

**RESOLVED (2026-07-02):** Option **7a** — Approach A (hand-written match arms) is permanent. ADR-009's compiled-dispatch commitment is formally demoted to a considered-and-rejected alternative for the corpus emission path. INV6 (extension_targets ⊆ duckdb_functions) is validated at unit-test time. INV3 is validated via grep discipline + the coverage anchor.

**Requires an ADR-009 refinement amendment** ratifying Approach A. The amendment is small and can land inline with Slice C.1's implementation.

---

**The gap.** ADR-009 chose compiled dispatch ("the table is the input and the compiled control flow is the output"). Its refinement (post-Slice-C) admitted Approach A (hand-written match arms) as acceptable when arms are trivial. Slice C.1's lesson entrenched Approach A. When (if ever) does the compiled dispatch codegen macro land?

**Options.**

- **7a — Never. Approach A is permanent.** *(CHOSEN)*
- **7b — Deferred to a future Slice K after G, before I.**
- **7c — Landed with Slice C.1 as tooling substrate.**

---

## Decision 8 — BaseTypes overlay: shared across paths, or one-per-path? — **RESOLVED**

**RESOLVED (2026-07-02):** **Per-path overlay.** User: *"v2 has `crate::transpiler_v2::BaseTypes` seeded independently, as discussed above, we will eventually rip out legacy paths."* Owned by Slice A.2 (V2RelationConverter seeds the v2 overlay from the DuckDB catalog).

Under ADR-022, legacy is superseded and per-path collapses to "v2 owns the overlay" once legacy is deleted. During the reimplementation, v2's overlay is independent of legacy's — no synchronization needed because legacy is not being run.

---

**The gap.** ADR-012 said the overlay was "shared state between the command arm and the relation arm." ADR-021's INV10 said v2 was substrate-independent from legacy. Whether the `BaseTypes` overlay was shared between legacy and v2 was unspecified.

**Options.**

- **8a — Shared overlay, INV10 carve-out.**
- **8b — Per-path overlay.** *(CHOSEN)*
- **8c — Shared read-only, per-path write cache.**

---

## Decision 9 — Corpus test attribution during v2 development — **RESOLVED (moot)**

**RESOLVED (2026-07-02):** **MOOT.** User: *"I would not waste time keeping the legacy path function in this branch, the legacy code path can stay for reference while the reimplementation is underway, we do not need to run any tests against legacy... with that life is simpler and decision 9 becomes moot."*

Under ADR-022, no test runs against legacy. Every corpus-green case is a v2 case. There is no attribution problem because there is no dual-path.

See **ADR-022** for the "legacy is reference-only during reimplementation" contract.

---

**The gap (historical).** During v2 development under the additive-landing model, a corpus case could be green via v2 native emission or green via legacy fallback. `tests/scripts/v2-progress.sh` couldn't distinguish. Under ADR-022 this distinction no longer exists.

---

## Decision 10 — Sub-slice progress signal — **RESOLVED**

**RESOLVED (2026-07-02):** Option **10c** — cumulative per-sub-slice targets in scope files. Each sub-slice's §Targets is the set expected to be green *at that sub-slice's termination*, cumulative from prior sub-slices.

**Requires an iteration methodology amendment** at §CV.7 naming the per-sub-slice cumulative target convention.

---

**The gap.** The iteration methodology's §CV.7 (post-Slice-C, 2026-07-01) permitted sub-splits. The methodology said "the 5-pass hard cap counts sub-slice passes" but did not say whether the corpus target for termination was the parent slice's or the sub-slice's.

**Options.**

- **10a — Sub-slice terminations are per-sub-slice targets.**
- **10b — Parent-slice-only termination.**
- **10c — Sub-slices declare cumulative targets.** *(CHOSEN)*

---

## Decision 11 — Legacy path deletion timing — **RESOLVED**

**RESOLVED (2026-07-02):** **Delete on this branch immediately.** User: *"to save effort and simplify rearchitecting, we are going to lift that requirement of keeping legacy intact, we can delete immediately on this branch."*

ADR-022 permits (but does not mandate) full legacy deletion at any point. Practical implementation may keep legacy source visible as reference material during the reimplementation for cross-checking purposes, but there is no test or runtime path exercising it. The scheduling decision — delete-all-at-once vs delete-incrementally-per-slice — is orthogonal to the ADR-level commitment.

---

**The gap (historical).** ADR-021 said legacy stayed intact throughout the restart. No ADR named when legacy would be deleted. Under ADR-022, deletion is permitted immediately and expected during the reimplementation.

**Options.**

- **11a — After 324/324 on v2's own emission.**
- **11b — After Slice I's INV1 harness.**
- **11c — Never — legacy stays as safety net.**
- **11d — Delete on this branch immediately.** *(CHOSEN, post-hoc — this option was not enumerated originally; the user directive introduced it.)*

---

## Decision 12 — CommonAST plan_id semantics across two front-ends — **RESOLVED**

**RESOLVED (2026-07-02):** Option **12a** — `plan_id: Option<i64>`; V2RelationConverter assigns from proto; v2 SparkSQL lowering leaves it `None`; analyzer handles both cases in the `resolve` pass. User: *"we are inclined to lift INV7 anyway, so we can follow the Spark way and go with 12a."*

With INV7 deleted (Decision 4), the two front-ends do not need to produce structurally-identical CommonAST — semantic equivalence is validated by ADR-015's oracle. The `Option` discipline captures the reality: protobuf carries plan_ids, raw SQL doesn't.

---

**The gap.** Related to Decision 4 (INV7 scope). Under the pre-ADR-022 framing, INV7 asserted the two v2 front-ends produced the same CommonAST for the same semantic input, but plan_id fields would inevitably diverge (protobuf-native vs parse-tree-derived). The gap is closed by INV7's deletion and the `Option<i64>` policy.

**Options.**

- **12a — plan_id is `Option<i64>`; SparkSQL lowering leaves it `None`.** *(CHOSEN)*
- **12b — SparkSQL lowering assigns synthetic plan_ids at parse time.**
- **12c — plan_ids exist only for the proto path.**

---

## What changes as a consequence of these resolutions

**In the ADRs file (`docs/thunderduck-rearchitect-ADRs.md`):**
- **ADR-022 added** — supersedes ADR-002's post-Slice-C fallback refinement; amends ADR-021; deletes INV7; reframes LB9; retires INV10 at legacy deletion. Covers Decisions 3, 4, 9, 11.
- **INV7 marked DELETED** in CV.5 with pointer to ADR-022. Covers Decisions 4, 12.
- **LB9 reframed** in CV.4 — "one implementation validated by ADR-015" replaces "two implementations validated by ADR-015."
- **T5 and T6 tensions** in CV.3 amended with the post-ADR-022 resolutions.
- **CV.2 dependency matrix** — ADR-022 row added.
- **CV.6 ratification order** — ADR-022 added as the third post-Slice-D commitment.
- **OQ-1 closure** — the INV7 obligation removed from the closure note.

**In the readiness map (`tasks/v2-adr-readiness-map.md`):**
- Baseline claim "Legacy TPC-H stays 51/51 throughout" is REMOVED (Decision 11 lifts that requirement).
- Slice A no longer wires `V2FallbackEligible` trait or fallback machinery (Decisions 3, 9).
- Slice A no longer routes via `THUNDERDUCK_TRANSPILER` env var (Decision 11).
- Slice A.2 uses `crates/core/src/parser_v2/` for v2 SparkSQL front-end (Decision 1).
- Slice A.2 seeds a per-path `crate::transpiler_v2::BaseTypes` overlay (Decision 8).
- Slice B no longer runs an INV7 check (Decision 4). §6 invariant activation map updates INV7 to DELETED.
- Slice B's set-op widening scope is UNION + INTERSECT + EXCEPT (Decision 5).
- Slice C.1 creates the empty `rewrites.rs` module (Decision 6).
- Slice C uses Approach A permanently (Decision 7).
- Sub-slice progress signal is cumulative per §CV.7 amendment (Decision 10).
- §8 references the resolutions here as the closure record.
- Slice B (analyzer)'s CommonAST plan_id handling covers both `Some(_)` and `None` (Decision 12).

**In the /goal template (`tasks/v2-slice-goal-prompt-template.md`):**
- Preflight step 2 "Legacy TPC-H differential green (51/51)" is REMOVED (Decision 11).
- Reference to ADR-022 added under §Design authority.

**In the iteration methodology (`tasks/v2-slice-iteration-methodology.md`):**
- §CV.7 amended with the sub-slice cumulative-targets convention (Decision 10).

**In CLAUDE.md (project-level, not ADR-level; noted for follow-up):**
- `### 4. Verification Before Done` step 4 ("TPC-H differential mandatory") is amended in spirit during the restart: DataFrame corpus is the fitness function; TPC-H rejoins the gate once v2 covers its query surface. The CLAUDE.md text itself is a separate follow-up edit.

---

## Decision 13 — Renderers named in C.1 scope with no Slice-B `TypedOp` sink — **RESOLVED (tentative, 2026-07-02; awaiting Slice-C.1 reviewer confirmation)**

**Surfaced by:** Slice C.1 Pass 1 architect (2026-07-02).

**Context.** The C.1 scope file (`tasks/v2-slice-C-scope.md` §Sub-slice sketch, and readiness map §Slice C → C.1 item 2) lists thirteen operator renderers as deliverables: `render_project, render_filter, render_sort, render_limit, render_tail, render_distinct, render_with_columns, render_drop_columns, render_aliased_relation, render_table_scan, render_local_relation, render_range_relation, render_values`. **Six of the thirteen have no matching `TypedOp` variant in the analyzer as landed by Slice B** (`analyzer.rs::TypedOp`): `Tail`, `Distinct`, `WithColumns`, `DropColumns`, `AliasedRelation` (`SubqueryAlias`), `Range`. These proto shapes are not lowered by the current `V2RelationConverter` either — per Open Decision 2's hybrid growth model they surface as `UnsupportedProtoShape` at the front-end boundary, so they never reach `dispatch_op`.

**Why the ADRs are silent.** ADR-009's Approach A specifies the emission shape but does not address renderers landed ahead of their `TypedOp` sinks. The success criteria for C.1 (§5.4 CTE for `render_tail`, §5.6 `quote_ident` fast path) require concrete code today — deferring the six renderers would defer those anchors.

**Options.**

- **13-A** — Land the six unwired renderers as private helpers with worked bodies (implementing §5.4 CTE, §5.6 quote_ident use, etc.), tested directly rather than via `dispatch_op`. Mark `#[allow(dead_code)]`. When a future substrate slice adds the missing `CommonOp`/`TypedOp` variants and the `V2RelationConverter` arms, wiring is a single new `dispatch_op` arm per variant. **Cost:** six dead-code helpers in the compiled binary until sunk. **Benefit:** every C.1 checklist anchor lives in real, tested code today.
- **13-B** — Reject the scope's renderer list as ill-formed for C.1; land only the seven renderers with `TypedOp` sinks. Defer §5.4 CTE anchor and the `render_range_relation`-style helpers to whichever slice adds the corresponding `TypedOp` variants. **Cost:** several C.1 success criteria unmet at C.1 (they migrate to the sub-slice that lands `TypedOp::Tail` etc.). **Benefit:** zero dead code.
- **13-C** — Extend `CommonOp`/`TypedOp` in C.1 to add the six missing variants (retro-broadening Slice B substrate). **Rejected** by scope non-goal "Legacy modifications — none. Don't touch Slice A/B substrate."

**Resolution (tentative — orchestrator-recorded 2026-07-02, awaiting reviewer confirmation):** **Option 13-A.**

**Rationale.**
1. The scope EXPLICITLY LISTS these renderers as C.1 deliverables. The scope authors expected them present.
2. Two C.1 success criteria (§5.4 CTE for `render_tail`; §5.6 `quote_ident` fast path) require concrete code today. 13-B silently drops those.
3. 13-C violates the "no legacy/substrate modification" non-goal.
4. `#[allow(dead_code)]` costs — six functions in the compiled binary, unreachable from `dispatch_op` — are cheap. The Rust compiler emits nothing for unused private functions; the review-time cost is a one-line attribute per helper.
5. Wiring cost when the missing `TypedOp` variants land: one new arm in `dispatch_op`'s match per variant, zero refactor.

**Landed in:** `crates/core/src/transpiler_v2/emission.rs` (Slice C.1, pending).

**Follow-ups the reviewer must confirm:**
- The six unwired renderers each carry an `#[allow(dead_code)]` attribute with a comment naming the future slice that will wire them.
- The §5.4 CTE anchor test (`render_tail_uses_cte_not_double_embed`) invokes `render_tail` directly and asserts CTE shape (not double-embed).
- A comment near each unwired renderer names its owning future-slice for wiring (`TypedOp::Tail → substrate extension slice; Range → probably Slice A.2 hybrid growth`).

**If the reviewer prefers 13-B**, C.1's Pass N+1 removes the six helpers and demotes the §5.4/§5.6 anchors to comments-only until the corresponding `TypedOp` slice lands.
