# ADR-021 — τ owns its substrate: protobuf converter, Expression, TypeInferenceEngine

**Status:** Proposed
**Depends on:** ADR-002 (delegation boundary), ADR-003 (common AST), ADR-004 (front-end convergence), ADR-005 (owned inference), ADR-014 (two decision spaces), ADR-015 (differential oracle)
**Depended on by:** every implementation slice; ADR-022 (τ is the only path — runtime-position companion); ADR-024 (stored attribute identity); ADR-025 (interval field-span representation); ADR-026 (Spark Connect plan-ID resolution)

**Context.** τ must own its substrate — the protobuf-to-CommonAST converter, the `Expression` payload in CommonAST, and the `TypeInferenceEngine`. If τ consumed an upstream plan type produced by a converter it does not own, τ inherits every quirk of that converter: synthesized-SQL shortcut shapes for structured operations (VALUES, table functions, file scans, Arrow-IPC LocalRelation), stringly-typed qualifier encodings (`__plan_id_{N}__`), silent-NULL fallbacks in Arrow value marshalling. If τ delegated type/nullability calls to an upstream inference engine, symmetric-omission gaps in that engine (a function present in `aggregate_return_type` but missing from `aggregate_is_nullable`) transit silently — τ arms can be individually correct yet the corpus stays red because the input's schema or nullability was wrong before the arm ran. Substrate ownership is the design lever that makes τ's correctness a τ-local concern.

**Decision.** τ owns its substrate from the protobuf boundary onward.

1. **Protobuf conversion.** τ's `V2RelationConverter` (in `crates/connect-server/src/converter/`) produces CommonAST directly from Spark Connect protobuf. The converter is exhaustive over the proto surface τ targets; un-handled proto messages surface as Thunderduck-boundary errors per ADR-022, not as silent shortcut shapes. Structured operations (`Values`, `LocalRelation` from Arrow-IPC, `TableFunction`/`Unnest`, `FileScan`, `Join`) get first-class CommonAST variants — no opaque SQL-string variants. Relation-node and DataFrame-reference plan IDs follow ADR-026, not a stringly-typed qualifier or join-only field.

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

