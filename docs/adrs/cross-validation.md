# Cross-Validation

This section is the instrument for checking the decisions against each other. Refining any single ADR above should be followed by a pass through §CV to confirm the change respects the dependency structure, the tension resolutions, the load-bearing assumptions, and the invariants.

## CV.1 — Layered structure

The ADRs are not peers; they form four strata.

**Premise (the selectors):** ADR-000 (positioning — single-node, no-JVM, DuckDB-backed), ADR-022 (τ is the only path; three error categories). Together these set the product shape and the runtime shape: what Thunderduck *is* and how it *responds*. A change here cascades furthest.

**Spine (the irreducible commitments, given the premise):** ADR-001 (transliterator), ADR-002 (emit-level delegation), ADR-005 (own the divergent slice), ADR-020 (strict-only; extension mandatory). These define what τ *is* and what it *owns*.

**Substrate & front-ends (the IR and how it is populated and analyzed):** ADR-003 (common AST), ADR-004 (both front-ends lower to it; relation-vs-command by parse-root), ADR-006 (analyzer pass structure), ADR-021 (τ owns its protobuf converter, Expression, TypeInferenceEngine), ADR-024 (stored attribute identity), ADR-025 (interval field spans), and ADR-026 (Spark Connect plan-ID lookup). These define the representation and the substrate boundary.

**Consequences (the spine applied to surfaces):** ADR-007 (A/B/C, B retained) from 001+002+005+006+008; ADR-008 (correlated direct) from 001; ADR-009 (closed dispatch: one live interpreted function registry plus exhaustive structural match arms) from 003+005+007; ADR-010 (extension fns, C++ project) from 005; ADR-011 (commands) from 001+004; ADR-012 (catalog overlay) from 011+005; ADR-013 (external/lakehouse reads, delegated) from 000+002+012; ADR-017 (Delta append writes) and ADR-018 (UC-managed Iceberg CTAS/INSERT/DELETE/MERGE writes) — the per-format write specializations — both from 005+011+013+016; ADR-019 (the native-read / Iceberg-write lakehouse I/O contract) composing 013+018.

**Enabled (the testing architecture the rest makes possible):** ADR-014 (two decision spaces), ADR-015 (differential + AnalyzePlan oracle), ADR-016 (version pin). These exist *because* the spine made τ minimal and its decisions enumerable; they are not free-standing choices.

Implication for refinement: a change to the premise (ADR-000) or a spine ADR (esp. ADR-005's scope) propagates widely; a change to an enabled ADR is comparatively contained.

## CV.2 — Dependency matrix

`→` reads "is depended on by / feeds."

| ADR | Depends on | Feeds |
|---|---|---|
| 000 positioning | — | 001, 002, 003, 004, 005, 006, 013 |
| 001 transliterator | 000 | 007, 008, 011, 015, 026 |
| 002 delegation | 000, DuckDB-correct (LB2) | 003, 005, 007, 013, 026 |
| 003 common AST | 000, 002 | 004, 005, 009, 026 |
| 004 front-ends → AST | 000, 003 | 005, 011, 015, 026 |
| 005 type/null inference | 000, 002, 003, 004, 012 | 006, 007, 009, 010, 012, 014, 015, 017, 018, 024, 025 |
| 006 analyzer passes | 000, 005 | 007, 024, 026 |
| 007 A/B/C | 001, 002, 005, 006, 008 | 009 |
| 008 correlated direct | 001 | 007 |
| 009 closed dispatch and live function registry | 003, 005, 007 | 014, 015 |
| 010 extension fns | 005 | 015, 016 |
| 011 commands | 001, 004 | 012, 017, 018 |
| 012 catalog overlay | 011, 005 | 005, 013, 015 |
| 013 external/lakehouse reads | 000, 002, 012 | 015, 017, 018, 019 |
| 014 two decision spaces | 005, 009, 016, DuckDB-correct (LB2) | 015 |
| 015 oracle | 001, 004, 005, 009, 010, 012, 013, 014, 016, 017, 018 | 026 |
| 016 version pin | 010 | 014, 015, 017, 018, 026 |
| 017 Delta writes (append) | 005, 011, 013, 016 | 015 |
| 018 Iceberg writes (UC managed) | 005, 011, 013, 016 | 015, 019 |
| 019 lakehouse I/O contract | 013, 018 | — |
| 020 strict-only extension | 000, 001, 010 | 009, 015 (simplified) |
| 021 τ owns substrate | 002, 003, 004, 005, 014, 015 | every implementation slice; INV10; 024, 025, 026 |
| 022 τ is the only path | 000, 002, 021 | every implementation slice; two-error-category rule; 024, 025, 026 |
| 024 stored attribute identity | 005, 006, 021, 022 | resolved-reference binding; future N10 work; 026 |
| 025 interval field spans | 005, 015, 016, 021, 022 | interval lowering/inference; AnalyzePlan and Arrow interval boundaries |
| 026 Spark Connect plan-ID lookup | 001, 002, 003, 004, 006, 015, 016, 021, 022, 024 | DataFrame reference resolution; join-only plan-ID deletion |

**New external dependency:** the [`thunderduck-duckdb-extension`](https://github.com/lastrk/thunderduck-duckdb-extension) C++ project is an external build artifact that ADR-010 depends on. It is not an ADR but it participates in the dependency graph: the dispatch table's `Extension(...)` targets must match its exported functions (INV6), its behavior must be differentially validated (edge 010 → 014), and it is the third member of the version-coordination set (edge 010 → 015, alongside Spark 4.1.1 and the dispatch table).

> **2026-07-14 note (historical text above kept as-is):** no longer external. The C++ project's source has been absorbed in-tree at `extension/` (see `extension/README.md`'s Provenance section); the version-coordination set (010 → 015) is now enforced mechanically by `scripts/dev/build-extension.sh`'s three-way lock (`extension/duckdb` submodule tag, `extension/BUILD_PINS.toml`, the `duckdb` crate version) rather than by coordinating with an externally-hosted repository's releases.

**Cycle to note:** ADR-005 and ADR-012 are mutually dependent — inference reads the catalog overlay, and the overlay is *sized by* what inference needs to know. Resolve by treating the overlay's contents as defined by inference's requirements: the overlay carries exactly the Spark base-column facts the coercion and nullability lattices consume, nothing more. Refining either must keep them co-designed.

**Most-depended-on nodes:** ADR-000 (premise — feeds five and selects the approach) and ADR-005 (own the divergent slice — feeds six). ADR-005 remains the highest-leverage *implementation* decision; ADR-000 is the highest-leverage *strategic* one. Changing ADR-005's scope, or ADR-000's no-JVM premise, are the two most expensive refinements.

## CV.3 — Tension points

Places where decisions pull against each other, with the resolution each currently relies on. These are the first things to re-examine if an ADR changes.

**T1 — Delegation vs ownership (ADR-002 ↔ ADR-005/026).** Ordinary structural binding remains delegated at emission. Analysis still resolves enough structure to derive Spark types (ADR-005), and τ additionally owns Connect plan-ID lookup because DuckDB cannot observe that protocol (ADR-026). These are enumerated exceptions, not a general Catalyst reimplementation.

**T2 — No-optimization vs expressibility-forced gray zone (ADR-001 ↔ ADR-007/008).** A transformation could be both correctness-forcing and optimization-shaped; separately, cosmetic cleanup is neither forced nor cost-motivated; and SQL-driven desugarings (ADR-003) are expressibility-forced structural rewrites. **Resolution:** classify by *motive* — cost-driven is forbidden; expressibility-forced is required (and lives in B, ADR-007); result-irrelevant syntactic reduction is permitted (cosmetic, under ADR-001's guardrail); and a *narrow, enumerated* carve-out is allowed for correctness-forcing-yet-optimization-shaped transformations recorded in ADR-001's carve-out register. ADR-008 (correlated subqueries emitted directly rather than rewritten to lateral) is the worked example proving the no-rewrite discipline. Watch both the cosmetic set and the carve-out register for creep.

**T3 — Cast-fixes-it vs needs-extension (ADR-005 ↔ ADR-010).** Some Spark/DuckDB mismatches are repairable by inserting casts (stay native); others require an extension function. **Resolution:** prefer casts where a cast sequence reproduces Spark semantics *exactly* — both value and result type; use an extension function only where no cast sequence does. The canonical needs-extension case is a **return-type divergence** (e.g. decimal÷decimal: DuckDB returns `double`, Spark returns `decimal`): a cast on the wrong-typed native result may not recover Spark's value, so the decision is made jointly by inference (ADR-005 infers the Spark result type) and emission (ADR-010 emits a function producing that value and type). Each extension function's *why* annotation records which side of this line it sits on.

## CV.4 — Load-bearing assumptions

Assumptions whose failure cascades across multiple ADRs. Each is empirically checkable; the check is named.

**LB1 — The owned divergent slice is {type inference, nullability, Connect plan-ID lookup}.** The first two belong to ADR-005; the last is ADR-026's bounded protocol exception. Any further structural divergence reopens ADR-002. **Check:** AnalyzePlan plus result/error differential witnesses.

**LB2 — DuckDB correctly executes valid SQL.** If false in cases that matter, ADR-014's excluded third bucket becomes real and attribution gains a genuine ambiguity. **Check:** emitted-SQL capture plus manual triage; partial reliance on DuckDB's own test suite.

**LB3 — DuckDB's ordinary SQL binding, star, and scope behavior matches what's wanted.** Connect plan-ID lookup is explicitly excluded and owned by ADR-026. Any other failure moves that construct across ADR-002's delegation boundary. **Check:** AnalyzePlan and result differentials on starred, ambiguous-column, and deep-scope plans.

**LB4 — τ never needs an optimization to be *correct* (only for performance).** If a correctness-forcing transformation is also optimization-shaped, ADR-001 needs a carve-out. **Check:** monitor the forced-transliteration set and ADR-001's carve-out register; the correlated-subquery case (ADR-008) is the canonical test of the boundary.

**LB5 — The A/B/C structure plus the C++ extension functions is expressive enough to correctly translate every *supported* DataFrame-or-SQL expression into a semantically matching DuckDB SQL expression.** This is an assumption about the world, not a property to preserve, which is why it is an LB and not an INV: it can be false — there may exist a Spark expression whose semantics *no* combination of DuckDB SQL + extension function reproduces, in which case the only correct behavior is to move it from the supported set to the rejected set. It is partly a bet *about the extension project* (ADR-010): "we can always write a C++ extension that closes any remaining semantic gap." The sharpest test cases live among the SQL-only constructs (ADR-003), e.g. recursive CTEs. **Check:** the differential result test (ADR-015) is precisely the falsifier — a *supported* expression that produces a divergence no emission can fix has falsified LB5 for that expression. Failure mode is reclassification (supported → rejected), not silent wrongness.

**LB6 — The single-node ceiling is sufficient for the target workloads.** (From ADR-000.) The entire positioning bets that the target users' workloads fit on one large machine. If they do not, the product thesis — not just an implementation detail — is wrong. **Check:** product/workload validation, not a test; but it is the premise every other decision rests on, so it is recorded here as the assumption with the widest blast radius.

**LB7 — DuckDB's storage extensions read external/lakehouse tables faithfully to Spark.** (From ADR-013.) For Hive-Parquet, Delta, Iceberg, and Unity-Catalog tables, the DuckDB readers (`parquet`/`httpfs`/`delta`/`iceberg`/`uc_catalog`) must produce column schemas *and* values matching what reference Spark produces reading the same table. This is distinct from LB5 (which is about thunderduck's *emission* expressiveness); LB7 is about the *delegated readers*, much of which is outside thunderduck's control. The known-exposed surfaces are partition-column typing on path-based reads and the format-type→Spark-type mapping (ADR-012). **Check:** read the same external table in reference Spark and diff the resolved schema (AnalyzePlan-style, ADR-015) and the values (result differential). Failure response is bounded: pin/track the offending DuckDB extension version, or reject that table/type (move it to the unsupported set) — not silent divergence.

**LB8 — DuckDB's extensions *write* external/lakehouse tables to a state faithful to Spark's write.** (From ADR-017; the write-direction sibling of LB7.) For the supported write paths (Delta append, ADR-017; UC-managed Iceberg CTAS/INSERT/DELETE/MERGE, ADR-018), the resulting table state — visible rows and the format's version/log lineage — must match what Spark's corresponding write would produce. This is a *distinct and currently shakier* bet than read-fidelity: DuckDB's Delta write surface is young (write support only recently de-experimental), with a noted delta-kernel-rs OneLake regression. The Iceberg write path (ADR-018) is better-grounded — Managed Iceberg is GA with explicit external-engine (DuckDB) write support — but carries its own residual fidelity conditions: the target table must be merge-on-read, and partitioning must be a DuckDB-supported transform, else the operation is a typed rejection. **Check:** state-diff oracle (ADR-011 / ADR-015) — read-after-write logical-row comparison plus version lineage, through both engines. Failure response is bounded exactly as LB7: pin/track the extension version, or reject the operation (it is already a typed rejection for everything past append). Scope grows only as DuckDB's writers mature.

**LB9 — Spark parity is a test-time property enforced by ADR-015's differential oracle against reference Spark, not a compile-time property inherited from any substrate.** (From ADR-021.) τ owns its `TypeInferenceEngine` and its analyzer; the oracle validates τ's outputs against reference Spark on an unbounded corpus of DataFrame + SparkSQL cases. Symmetric-omission gaps in the analyzer's function-name enumerations (e.g. a function present in `aggregate_return_type` but missing from `aggregate_is_nullable`) transit into corpus reds — the oracle catches them. **Check:** the harness itself. If it has coverage gaps, LB9 can fail silently for those gaps; mitigation is the same as LB5's — grow the harness, or reject the case class explicitly. Failure mode is silent divergence on un-covered cases, which is exactly what ADR-015 is designed to prevent — so LB9's soundness rests on ADR-015's coverage discipline.

## CV.5 — Cross-cutting invariants

Properties that span ADRs. Any refinement must preserve all of these; a change that breaks one is a signal that the change is larger than it appears.

**INV1 — Both engines receive byte-identical input.** (Touches ADR-015; constrains ADR-001.) Parity-via-identical-bytes is achieved by serialize-once-send-twice. Note this is *not* violated by ADR-001's cosmetic simplifications: cosmetic simplification is a τ transformation applied *once*, upstream of the single serialization, so both engines still receive the same simplified bytes — and DuckDB SQL is consumed only by DuckDB, never by Spark, so the cosmetic DuckDB cleanup is invisible to the comparison. (This is exactly why the rejected production-*canonicalizer* was different: it was proposed as a normalization that could differ from what Spark sees.) A proposal to add production-side normalization that could differ per engine, or that Spark would observe, must demonstrate it does not break this.

**INV2 — Every τ decision is node-local (post-A) or a labeled C escape hatch.** (Touches ADR-007, ADR-009.) A new decision that is non-local must either be made local by the A pass (push the fact into the node) or be a *counted* C entry. It may not be hidden in an open callback or fallback. Genuinely structural forced transliterations live in the retained B layer (ADR-007), not inside callable registry rows.

**INV3 — Callable dispatch has one live authority.** (Touches ADR-009, ADR-014, ADR-015.) Every supported callable spelling selects one closed `FunctionImplementation` route in the live registry, and every semantic consumer reads it. Structural AST emission remains exhaustive handwritten dispatch. Neither path may acquire an open native fallback or import the retired implementation.

**INV4 — Inference is validated in isolation before translation tests run.** (Touches ADR-005, ADR-006, ADR-015.) Preserves attributability (ADR-014). The AnalyzePlan schema diff must be green before result-level translation failures are interpreted as translation bugs. Applies also to rule *provenance*: an LLM-extracted coercion/nullability rule is not trusted until the diff is green for it.

**INV5 — thunderduck knows the schema everywhere, even where it emits delegated structure.** (Touches ADR-002, ADR-005, ADR-026.) The internal resolver/star-expander remains for type tracking, and plan-ID lookup uses the resulting `ExprId`s. Emit-level delegation does not permit either analysis path to disappear.

**INV6 — Every extension target in the live registry corresponds to an existing, loaded function in the `thdck_spark_funcs` C++ extension.** (Touches ADR-009, ADR-010.) Unlike LB5 (an empirical bet about expressiveness), this is a mechanically *checkable, preservable* property — verify at build/test time that registry targets and exported extension symbols agree. It is the mechanical complement to LB5: LB5 asserts an adequate extension *can* be written; INV6 asserts every named extension target actually exists and is loaded.
(2026-07-14: "the `thunderduck-duckdb-extension` C++ project" now names the in-tree `extension/` directory, not an external repository — see ADR-010's note above.)

**INV7 — none.** No per-run front-end equality invariant is imposed. Both front-ends target the same operator variant for the same construct, but source-only metadata may differ: Connect nodes may carry `plan_id`; SparkSQL nodes carry `None` (ADR-026). Each front-end is independently validated against Spark by ADR-015.

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
6. **ADR-020** (strict-only extension), **ADR-021** (τ owns substrate), and **ADR-022** (τ is the only path). ADR-020 consolidates the emission target; ADR-021 pins the substrate boundary (τ owns its protobuf converter, Expression, TypeInferenceEngine); ADR-022 pins the runtime position (τ is the only path; three error categories; no fallback). Together with ADR-000's premise, these three shape every implementation slice.
7. **ADR-024** (stored attribute identity), **ADR-025** (interval field spans), then **ADR-026** (Spark Connect plan-ID lookup). ADR-026 presupposes ADR-024's `ExprId` filtering model; ADR-025 is independent of both identity models. Superseded records under `docs/adrs/retired/` are outside the active ratification order.

Defer no ADR's *ratification* past the point where something depending on it is ratified — the matrix in CV.2 gives the order. The two highest-value review items are **ADR-000's no-JVM premise** (widest blast radius; if it moves, Alternative 1 deletes ADR-005/006) and **ADR-005's scope together with LB1** (where the implementation cost and risk concentrate).

---
