# Phase 6: phase-typed IR experiment

Branch: `experiment/phase-typed-ir`

Base: Phase 5 commit `302b0d9`

## Question

Can phase typing remove duplicated plan structure and impossible mixed-phase
states without making irregular operators, analysis, or emission harder to
understand?

This is a compile-tested experiment, not a production migration. Production
architecture changes only if the adoption gate at the end passes.

## Baseline

- `CommonOp` and `TypedOp` occupy 578 and 350 physical declaration lines,
  respectively. Their 928-line combined surface includes documentation but is
  still the maintenance surface that every operator change crosses.
- `CommonAst` has 2,473 typed references and `TypedAst` has 1,196.
- `SubqueryPlan` has 127 typed references across five files because the shared
  `Expression` enum must carry either a `CommonAst` or `TypedAst`.
- The plan enums are intentionally non-isomorphic: source forms can disappear
  during analysis, while typed forms gain schemas, scopes, widening, and
  missing-column recovery facts.

## Prototype matrix

| Construct | Pressure on the design |
|---|---|
| Project | Phase-specific expressions and resolved output facts |
| Filter | Schema-preserving unary operator |
| Join | Binary children, two resolution roots, USING metadata |
| Subquery | Recursive plan inside an expression |
| Aggregate | Several expression lists and analyzer-owned output shape |
| SetOp | N-ary children and typed-only widened schema |
| Pivot/Crosstab | Runtime-dependent shape and source-only lowering |
| WithColumns/NaDrop | Typed-only missing-column recovery state |
| Generate | Structured generator with phase-specific arguments/output |
| RecursiveCte | Mutually dependent two-leg recursive analysis |

## Designs under test

1. A sealed `Plan<P>` / `Expr<P>` model. Associated types carry phase facts;
   a closed source-only operator enum becomes `Infallible` after analysis.
2. If the first design needs pervasive phase switches, a hybrid keeps separate
   outer plan enums while sharing generic payload structs and phase-typed
   expressions.

Neither design may use trait objects, downcasts, runtime phase tags, optional
resolved identities, or an opaque operator payload.

## Running friction log

- The analyzer must be a consuming lowering, not a shape-preserving cast: one
  source node may become a differently shaped typed subtree.
- Runtime-shape discovery is already a distinct pre-analysis transition. A
  two-phase type model must not pretend implicit Pivot/Crosstab/FileScan state
  is resolved merely because it is represented by `CommonAst`.
- The sealed `Plan<P>` model compiles with ordinary derived
  `Clone`/`Debug`/`PartialEq`/`Eq`; recursive phase-associated subquery and
  source-only payloads do not require manual implementations.
- `P::SourceOnlyOp = Infallible` gives resolved emission an exhaustive closed
  match and permits Crosstab/ToDf to lower into different canonical nodes.
- Phase-wide plan facts are clean, but operator-specific differences accumulate
  on `Phase`: recovery, widened set schema, generated schema, and source-only
  forms already need separate associated types in the eleven-operator model.
- Concrete analyzer and emitter functions do not need generic bounds once their
  input phase is fixed. The generic burden remains concentrated in the IR.
- The equal-capability hybrid is smaller in executable prototype code. Excluding
  fixtures/support, its declarations/analyzer/emitter are 178/229/107 nonblank
  lines versus 162/309/137 for the fully generic model. The generic declaration
  wins 16 lines, but its consuming conversion and resolved traversal lose 110.
- The prototype totals are 831 lines (`Plan<P>`) and 700 lines (hybrid), a
  16% reduction for the hybrid after equalizing binary expressions,
  nullability, and recovery facts. These are experimental, not production LOC.
- Deliberately mixing phases produces useful compiler errors in both designs.
  The generic error is `expected PlanKind<Resolved>, found
  PlanKind<Unresolved>`; the hybrid is the more domain-direct `expected
  TypedOp, found SourceOp`.
- The real migration surface is much larger than the declaration experiment:
  exact SCIP reports 5,522 `CommonOp`, 1,270 `TypedOp`, 3,922 `Expression`,
  2,473 `CommonAst`, and 1,196 `TypedAst` references.
- Phase-typing the production expression tree would parameterize much more than
  `Expression`: `expression.rs` has 39 public expression-related types and 286
  intra-file `Expression` occurrences. This is likely pervasive generic spread,
  not a contained subquery fix.
- `ColumnReference::expr_id` remains optional despite the resolved-only type.
  Production construction before test modules does not create `expr_id: None`;
  the optional state is retained mainly for defensive/test shapes. Making bound
  identity non-optional is a narrower opportunity that does not require
  phase-typing the whole plan or expression graph.
- Expanding from eleven operators to the real enum adds many distinct phase
  switches: `FileScan.schema` changes optional→required; Join drops `natural`;
  SetOp adds widening; rename/drop/with-column/dedup/NA nodes add recovery;
  Summary adds materialized columns; RecursiveCte drops source validation
  fields; and Unnest/Crosstab/ToDf have no typed counterpart. A universal
  `Phase` would need operator-specific associated payloads for most of these,
  eroding the value of the shared enum.
- Runtime discovery is effectively a third phase: it fills Pivot values and
  file schemas and replaces Crosstab with Aggregate. Encoding it as another
  `Plan<P>` phase would require rebuilding every unaffected node or adding a
  generic phase-mapping framework. A private validated newtype offers most of
  the boundary safety without a third tree representation.

## Opportunities to test

- Replace `SubqueryPlan::{Unanalyzed, Analyzed}` with `Expr<P>` containing a
  `Plan<P>` directly.
- Share payload structs only for operators whose structure is genuinely the
  same across phases.
- Make resolved column identity required by construction.
- Move typed-only schema, scope, widening, and recovery data into phase facts
  without adding `Option`-based state leaks.
- Consider phase-typing expressions independently of plans. The hybrid shares
  `Expr<P>` and removes mixed subquery state while retaining concrete source and
  typed plan enums for non-isomorphic operators.
- Consider a private `PreparedCommonAst` newtype at the runtime-discovery
  boundary. It can prove that the service ran shape discovery without adding a
  third generic phase and rebuilding every unchanged plan node.

## Adoption gate

Adopt only if the adversarial prototype reduces representative production LOC,
prevents current invalid states, preserves exhaustive readable matches, and
does not spread generic bounds or conversion boilerplate. Otherwise retain
`CommonOp`/`TypedOp` and document the narrower simplifications that remain
worthwhile.

## Result

### Full `Plan<P>` migration: reject

The model prevents mixed-phase subqueries, requires resolved identity, and
keeps source-only lowering closed. It fails the simplification gates:

- It is 16% larger than the equal-capability hybrid prototype.
- Eleven operators already require six phase-associated payload families;
  the production enum would require several more.
- A runtime-prepared phase would add another whole-tree transition for three
  exceptional shapes.
- The expected migration crosses thousands of plan/expression references and
  would parameterize most expression container types.

The escape hatch is technically sound, but using it often enough to model τ
turns `Phase` into an indirect second operator schema.

### Hybrid whole-tree migration: reject

Concrete source/typed outer enums plus shared payload structs are easier to
analyze and emit than `Plan<P>`. However, the current plan enums already embody
that separation directly. Wrapping their variants in generic payload structs
would cause a broad construction/pattern migration for little declaration
reduction. A shared `Expr<P>` would remove `SubqueryPlan`, but its generic
parameter propagates through the 39-type expression graph.

### Narrow follow-ups worth considering

1. Change resolved `ColumnReference.expr_id` from `Option<ExprId>` to `ExprId`.
   The separate `UnresolvedColumn` type already owns unresolved names, and the
   production constructors audited by this experiment do not create a missing
   ID. This removes a real invalid state without phase generics.
2. Return a private `PreparedCommonAst` newtype from runtime-shape discovery and
   require it at the service→analysis seam. This proves discovery ran while
   retaining the current in-place, exceptional-node rewrite.

Keep `SubqueryPlan` for now. Its mixed state is undesirable, but removing it in
isolation does not justify genericizing the complete expression hierarchy.

## Verification

- `cargo fmt --all --check`
- `cargo clippy --workspace --all-targets -- -D warnings`
- Workspace tests: Connect 165/165; core library 1332 passed with four declared
  ignores; phase-typed experiment 3/3; hybrid experiment 2/2.
- DataFrame corpus: 422 green with the same seven deferred cases.
- SQL corpus: 426 green with two intentional skips.
- The checked-in 829-case prior-green oracle reports zero regressions and all
  14 witness flips remain green.
