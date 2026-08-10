# τ architecture simplification and code-reduction plan

**Status:** In progress — Phase 3 complete; Phase 4 not started

**Recorded:** 2026-08-07

**Scope:** `crates/core/src/transpiler_v2/`, `crates/core/src/parser_v2/`,
`crates/connect-server/`, and the first-party `extension/` sources

## Outcome

Reduce the system by deleting parallel protocols and synthetic representations,
not by merely shortening `match` arms. The intended end state has one
representation for plan origin, one explicit representation for row-generating
expressions, one live source of truth for ordinary function semantics, and a
smaller runtime API whose types state whether an operation queries, mutates, or
invalidates cached schema.

This plan deliberately separates proven deletions from architectural bets. A
prototype that adds machinery without removing more machinery does not advance.
The current design remains in place whenever a proposed replacement does not
produce a measured reduction in concepts, code, or invalid states.

## Authority and non-negotiable constraints

- [`docs/thunderduck-rearchitect-ADRs.md`](../docs/thunderduck-rearchitect-ADRs.md)
  is authoritative. Any representation change that conflicts with it requires
  an ADR amendment before production code changes.
- τ remains the only production path under ADR-022. No fallback, dispatch flag,
  legacy transpiler, or runtime-owned semantic path may be introduced.
- ADR-024's must-preserve outcomes remain intact: stable attribute identity,
  source-qualifier lineage, exact ambiguity behavior, and emission-time binding.
- Spark-invalid input remains a Spark-emulated error; Spark-valid but
  unsupported input remains an honest Thunderduck-boundary error.
- DuckDB does not become the semantic oracle. Spark parity remains owned by τ
  and by the first-party extension where native implementation is the right
  altitude.
- Closed sets use enums and exhaustive matches. Do not replace the current
  design with a trait-object hierarchy or plugin architecture.
- Every implementation slice must be independently reviewable and leave both
  corpora with no previously-green regression.

## Audit baseline

These figures are a 2026-08-07 directional baseline. Recompute them at the
start of implementation from a named commit; do not use them as an immutable
scorecard.

| Surface | Baseline observation |
|---|---:|
| First-party Rust implementation | 69,547 lines |
| First-party extension C++ | 2,893 lines |
| Combined first-party implementation | 72,440 lines |
| Rust comment-bearing lines | 15,028 |
| Rust standalone comment lines | 14,988 |
| Rust doc / ordinary standalone comments | 7,579 / 7,409 |
| Rust history/process-tagged comments | 679 |
| Rust decorative separators | 410 |
| Rust future/deferred comments | 50 |
| C++ comment-bearing / leading comments | 577 / 513 |

The baseline excludes vendored DuckDB. Large Rust files still contain embedded
test modules, so splitting tests changes navigability but not total line count.
The main candidates are `emission.rs` (tests begin near line 7,121),
`analyzer.rs` (near 6,132), and `v2_lowering.rs` (near 4,780).

## Problem framing

The largest simplification opportunities are protocols that cross several
modules:

| Concern | Current representation | Target representation |
|---|---|---|
| Spark plan identity | Partial raw-proto traversal plus join-only left/right ID vectors | Origin metadata carried by every plan node and consumed generically by analysis |
| Row generators | Reserved scalar-function names interpreted by parser, analyzer, inference, and emission | A structured generator node with explicit kind, arguments, aliases, outer semantics, and output |
| Function semantics | Independent catalog, type, nullability, aggregate, emission, extension, and runtime-macro lists | One live `FunctionSpec` registry for ordinary cases; exhaustive special handlers for irreducible cases |
| Analysis phase | Near-parallel `CommonOp`/`TypedOp` trees plus unresolved/resolved expression states | Keep the current split unless a small phase-typed prototype measurably removes duplication and invalid states |
| Runtime intent | SQL text classified by prefixes; cache effects duplicated at callers | Explicit query/command APIs and typed cache effects |
| Comments | Current rationale mixed with review history, status ledgers, and code narration | Current non-local constraints in source; history in ADRs, tasks, or the development journal |

## Success measures

Measure these per slice and for the full program:

1. Net production lines deleted, excluding generated files and test moves.
2. Number of independent semantic lists or protocols removed.
3. Number of invalid states made unrepresentable.
4. Number of comments deleted, rewritten, or moved, classified by reason.
5. Full DataFrame and SQL corpus results against the pre-slice baseline.
6. Compile time and binary size as guardrails; neither may regress materially
   merely to save source lines.

Line count is evidence, not the objective. A new abstraction must also reduce
the number of places needed to add the next operator or function. Moving code
to a helper, generated table, macro, or another language does not count as a
reduction unless the old knowledge source disappears.

## Phase 0 — establish the executable baseline

### Work

1. Record the commit, dirty-tree exclusions, Rust/C++ line counts, comment
   counts, and largest production/test modules using reproducible commands.
2. Run the full unit and differential gates and save the per-case DataFrame and
   SQL outcomes. The regression baseline is case-level, not just a pass total.
3. Build a cross-layer function matrix containing:
   - catalog exposure;
   - scalar/aggregate classification;
   - arity validation;
   - return type and nullability;
   - emission route;
   - required extension symbol;
   - any runtime macro with the same public name.
4. Inventory all first-party inline comments and assign one disposition:
   `keep-rationale`, `rewrite-stale`, `move-history`, `delete-narration`, or
   `delete-decorative`.
5. Map every architectural workstream below to the ADR text it preserves or
   amends. Resolve contradictions before implementation.

### Exit gate

- The baseline can be regenerated by another developer.
- Every currently green corpus case is identified by name.
- Every proposed deletion has a reference/call-path proof, not only a textual
  search result.
- Architectural prototypes have an explicit go/no-go metric before they begin.

## Phase 1 — delete proven residue and narrow representations

Land each numbered group separately. Do not mix these deletions with plan
identity, generator, or function-registry work.

### 1A. Rust scaffolding and unreachable states

- Delete the one-line `transpiler_v2/rewrites.rs` module if it is still empty
  and remove its module declaration.
- Make invariant enforcement test-only at the module boundary if production
  code cannot call it. Audit the ignored INV1/INV6/INV8/INV9 stubs against the
  Cross-Validation section first: implement active obligations or amend the
  ADR before deletion. Do not silently erase a `DEFER INV<N>` ownership marker.
- Remove `EMIT_TAP` and its mutex instrumentation if no production diagnostic
  consumer exists. Replace any test that only observes the tap with a direct
  output or structural assertion.
- Move `has_resolved_schema` behind `#[cfg(test)]`, turn it into a real
  production postcondition, or delete it. Do not retain a public-looking helper
  with no production caller.
- Delete `RowConstructorExpression` after proving that neither frontend can
  construct it.
- Delete `TypedOp::Unnest` if the common form is always rejected before a typed
  node can exist. Keep the boundary input variant needed to return the honest
  unsupported error.
- Replace the three Arrow interval uses of `RawSqlExpression` with a typed
  interval expression, then delete `RawSqlExpression` if no producer remains.
  Coordinate this with the interval-span decision in
  [`tasks/adr-025-draft-interval-field-span.md`](adr-025-draft-interval-field-span.md).
- Do not preserve an empty `extension_targets()` list. Phase 1 may delete it if
  it has no live consumer; otherwise Phase 4 must derive it from the live
  function registry and activate the relevant invariant. Never add another
  hand-maintained list.

### 1B. Shadowed runtime macros

Validate, then remove runtime SQL macros whose accepted calls are completely
rewritten by emission. Initial candidates are:

- `startswith`, `endswith`, `btrim`, and `substring_index`;
- `bit_get`, `arrays_zip`, `shiftleft`, `shiftright`, and `conv`;
- the shadowed form of `to_char`.

Validation must exercise both DataFrame expressions and direct Spark SQL. A
macro is not dead merely because the DataFrame emitter bypasses it; raw SQL may
still resolve it in DuckDB. For each candidate, record every construction and
call path, add a regression witness where coverage is absent, remove the macro,
and rerun session initialization tests.

Treat the large `SPARK_CRC32` lookup macro as a separate altitude decision. A
move into `thdck_spark_funcs` is acceptable only if it reduces total logic and
retains Spark parity; it must not be disguised as routine dead-code cleanup.

### 1C. Extension cleanup

- Delete the unused `add_int` and `add_double` lambdas in
  `extension/src/include/spark_try_aggregates.hpp` after compile proof.
- Consider a small overload-registration helper only if at least three live
  sites become simpler. Do not introduce templates that obscure overflow,
  alignment, null handling, or Spark-specific aggregate behavior.

### 1D. Immediate comment corrections

Fix comments that currently contradict the code or the authoritative ADRs:

- the `invariants.rs` module header claiming only INV10 is active;
- two-category ADR-022 language where the amended model has three categories;
- stale `ColumnReference.expr_id` claims that `None` is a live analyzed state;
- `has_resolved_schema` prose describing fields that are no longer optional;
- references to the deleted `spark_aggregate_return_cast` helper;
- module-level implementation ledgers and pass-status prose that belong in
  `tasks/` or `docs/dev_journal/`.

### Phase 1 exit gate

- Every deleted symbol is absent from definitions and type-resolved references.
- No new `#[allow(dead_code)]` is introduced.
- No empty module, test-only runtime hook, or unconstructible typed variant
  remains without an explicit reason.
- Production line count is net negative. If a replacement abstraction costs
  as much as the deleted code, split it out and evaluate it separately.

## Phase 2 — make plan origin ordinary node metadata

### Current duplication

`CommonAst` carries only an operator even though it is the natural preservation
boundary for relation metadata. The converter compensates with the deliberately
partial `collect_relation_plan_ids` raw-proto traversal and stores
`left_plan_ids` / `right_plan_ids` only on joins. Those vectors are threaded
through both common and typed operators; the audit found roughly 125 references
to each name and about 150 empty-vector initializers.

This is not join semantics. It is source-origin metadata forced into a join
special case because the common plan discarded it too early.

### Target invariant

Every converted relation node preserves its own Spark plan ID, when present.
Analysis derives the IDs visible from a child subtree through one generic
metadata/scope path. Join resolution consumes its left and right child scopes;
the join operator carries no special plan-ID vectors. No code walks selected
raw proto variants merely to reconstruct metadata after conversion.

### Work

1. Write the ADR-024 amendment or companion decision first. Preserve its
   identity and qualifier outcomes; distinguish relation origin from attribute
   `ExprId`.
2. Prototype metadata on the smallest viable shape, initially
   `CommonAst { op, plan_id: Option<i64> }`, plus the minimum typed metadata
   needed for bottom-up scope construction. Do not commit to a collection type
   until self-join and nested-plan witnesses establish what must be retained.
3. Convert plan IDs at the same point each relation is converted. Missing IDs
   remain explicit `None`, not a magic integer.
4. Derive subtree bindings through the exhaustive child structure or the
   already-computed child scopes.
5. Rewrite join-condition resolution to use those generic child bindings.
6. Delete `collect_relation_plan_ids`, `left_plan_ids`, `right_plan_ids`, and
   all empty-vector plumbing.

### Witnesses

- self-joins with duplicate column names and explicit plan IDs;
- nested aliases and projections on each side of a join;
- `USING`, natural, semi, and anti joins;
- a missing-plan-ID case that must retain name/qualifier semantics;
- ambiguity errors whose class and candidates must remain byte-for-byte stable.

### Go/no-go gate

Proceed only if the prototype removes the partial proto traversal and both
join-only vectors without replacing them with an equally special side table.
The resulting metadata flow must be exhaustive by construction and net-delete
production code.

## Phase 3 — replace synthetic generator calls with generator IR

### Current duplication

Row-generating behavior is encoded as ordinary scalar calls with reserved names
such as `posexplode_pos`, `posexplode_val`, `map_explode_key`,
`map_explode_val`, `stack_multi_alias`, `stack_col`, `inline_field`,
`inline_outer_field`, and `json_tuple_field`. Parser/lowering, converter,
analyzer prepasses, type inference, and emission all need to recognize pieces
of that private naming protocol. It creates collision risk and spreads one
semantic operation across multiple passes.

`multi_alias.rs` is a related symptom: sentinel rewriting followed by a
post-parse AST splice exists because the parser cannot directly express the
needed alias shape.

### Target invariant

A generator is never represented as a general scalar `FunctionCall`. Its IR
states the generator kind, arguments, aliases, `outer` behavior, and output
fields directly. Both frontends lower to the same structure, analysis resolves
it once, and emission handles it exhaustively.

The initial closed set should cover `explode`, `posexplode`, `inline`,
`json_tuple`, and `stack`, including map/array and outer variants. Choose
between a dedicated `GeneratorProjection` and normalization to the existing
lateral-view/table-function representation based on which deletes more
special-case code.

### Semantic matrix (Spark 4.1.1)

The implementation source of truth is Catalyst's `Generator`, `Generate`,
`GeneratorResolution`, `Stack`, `ExplodeBase`, `Inline`, and `JsonTuple`.
Aliases are either absent (use the default names below) or must match the
resolved output arity exactly.

| Kind | Arguments and input | Default output | Rows per input | `outer` |
|---|---|---|---|---|
| `explode` | One array or map | array: `col`; map: `key`, `value` | One per element; none for null/empty | Emits one all-null row for null/empty and makes every output nullable |
| `posexplode` | One array or map | array: `pos`, `col`; map: `pos`, `key`, `value` | As `explode`, with zero-based position | Same outer-row rule; `pos` is null on the synthetic row |
| `inline` | One `array<struct<...>>` | Struct field names, types, and nullability; nullable array elements make every field nullable | One per struct; none for null/empty | Emits one all-null struct row for null/empty |
| `json_tuple` | JSON string plus one or more string field expressions | `c0..cN`, nullable strings | Exactly one, including null/invalid JSON | Not an outer generator |
| `stack` | Positive foldable integer `N` plus values | `col0..colK`, nullable; `K = ceil(values/N)` and values are row-major | Exactly `N`; missing final-row slots are null | Not an outer generator |

### Representation decision

Normalize to a structured unary `Generate` operator, matching Spark's logical
shape, rather than adding a combined `GeneratorProjection`. A projection-only
unresolved marker lets output arity remain schema-dependent until analysis;
analysis then produces ordinary `Project(Generate(input))`. This also subsumes
the existing `LateralView` node and the `explode` special case in
`TableFunction`. The decision is viable only if the inline, JSON-tuple, and
stack expansion passes disappear and the fake scalar names are deleted.

The sqlparser alias workaround remains because neither an upstream grammar
fork nor a projection-path side table is smaller. Its sentinel is now scoped
per statement: lowering chooses a prefix absent from every input identifier and
passes that exact prefix to the splice, so user aliases cannot collide with it.

The migration preserves the prior literal-only subset of Spark's foldable
`stack` row-count rule. General constant folding is a parity concern separate
from replacing the synthetic representation.

### Work

1. Specify the semantic matrix: input arity/type, output cardinality and
   fields, default names, alias arity, nullability, and outer-row behavior.
2. Prototype one difficult vertical slice, preferably map `posexplode_outer`,
   through both the Spark Connect and SQL frontends.
3. Add the structured common and typed representation only after the prototype
   shows that analyzer prepasses disappear rather than move.
4. Lower all generator families to it and consolidate schema analysis.
5. Render generators from the structured node and delete the fake scalar
   names and their type/emission arms.
6. Revisit `multi_alias.rs` after generator IR exists:
   - prefer a narrowly-scoped upstream `sqlparser` fix if maintainable;
   - otherwise keep parser state in a lowering-context side table;
   - retain sentinel rewriting only if both alternatives are larger and the
     sentinel is mechanically collision-proof.

### Go/no-go gate

- No reserved generator name reaches general function analysis or emission.
- At least two analyzer prepasses disappear entirely.
- Both frontends share one semantic implementation after lowering.
- Alias and outer semantics remain corpus-identical.
- Net production code and number of recognition sites decrease.

### Phase 3 result (2026-08-10)

- Both frontends now lower `explode`, `posexplode`, `inline`, `json_tuple`, and
  `stack` to one structured `Generator` / `Generate` representation.
- `LateralView`, the explode table-function special case, three projection
  expansion passes, and all private fake scalar generator names are gone.
- Multi-column aliases now attach to the generator without 1:N AST expansion;
  statement sentinels and emission's internal generator alias are
  collision-free.
- Production Rust decreased by 640 physical lines, measured before each
  file's first `#[cfg(test)]` module from the branch base to this worktree.
- `cargo fmt --check`, `cargo clippy -- -D warnings`, and `cargo test` pass.
  The DataFrame corpus remains at 421 green plus the same 7 known deferred
  cases; the SQL corpus remains at 424 green with 2 intentional skips.

## Phase 4 — create one live function-spec registry

### Current duplication

Function knowledge is independently encoded in `SUPPORTED_FUNCTIONS`, scalar
return-type logic, aggregate specs, expression nullability, the large emission
match, extension-symbol validation, and runtime macros. These sources already
drift: the audit found functions handled by inference and emission but absent
from the catalog, including `approx_count_distinct`, `bit_get`, `btrim`,
`json_object_keys`, `max_by`, `min_by`, `substring_index`, `try_avg`,
`try_sum`, and `any_value`.

Literal/schema parsers also currently point the wrong direction:
`parse_number_format_for_type_inference`,
`from_json_ddl_to_struct_for_type_inference`, and
`from_csv_ddl_to_struct` make type reasoning depend on emission-owned helpers.

### Target invariant

One τ-owned registry is read in production by catalog exposure, arity and kind
classification, ordinary return-type/nullability inference, ordinary emission,
and extension-target validation. Complex Spark semantics remain hand-written,
but the registry names their exhaustive special handler. No declarative row is
allowed to exist solely for tests.

A likely closed representation is:

```text
FunctionSpec {
    canonical_name, aliases, kind, arity,
    type_rule, null_rule, emit_rule
}

EmitRule = Native | Rename(name) | Extension(name) | Special(SpecialFunction)
```

These are enums/data interpreted by every live client, not function pointers or
a new dynamic dispatch layer.

### Work

1. Extract pure DDL/format parsing into a neutral τ module consumed by both
   analysis and emission.
2. Build a vertical prototype with representative cases:
   - one native scalar;
   - one renamed DuckDB scalar;
   - one extension scalar;
   - one aggregate;
   - one irreducible special function.
3. Require the catalog, inference, nullability, emission, and extension checks
   to consume those prototype rows in production.
4. Compare added interpreter code with deleted match/list code. Reject the
   design if a second hand-written path remains authoritative.
5. Migrate simple families incrementally. Keep complex cases in exhaustive
   `SpecialFunction` handlers until repetition demonstrates a smaller rule.
6. Generate the extension-target set from `EmitRule::Extension` and make its
   invariant live.
7. Delete shadowed runtime macros only after Phase 1's direct-SQL validation.
8. Add exact-set consistency tests so a cataloged function cannot lack a live
   route and a public live route cannot disappear from the catalog unnoticed.

### Go/no-go gate

- A new ordinary function is added in one spec row plus semantic code only when
  its rule is genuinely special.
- Every registry field has at least one production reader.
- The prototype deletes more production code than its interpreter adds.
- No behavior-bearing list remains duplicated for migrated functions.
- ADR-009 and INV3/INV10 remain satisfied; this is a τ-owned registry, not a
  reintroduction of the deleted v1 `FunctionRegistry`.

## Phase 5 — simplify runtime and service intent

This phase is independent of the generator and function work and should land
in small service/runtime slices.

### 5A. Query versus command

- Replace `run_query`'s textual SQL-prefix classification with explicit
  `query` and `execute_batch` session requests.
- Make callers choose the operation type before crossing the session thread.
- Preserve DuckDB's single-owner threading model and current error mapping.

### 5B. Commands and schema-cache effects

- Collapse `SessionCommand::CreateView` and `CreateViewWithSchema` into one DDL
  command with an optional typed cache effect.
- Introduce a small closed `SchemaCacheEffect` enum and one `apply` method.
  Production and tests must call the same implementation; tests must not mirror
  the invalidation algorithm.
- Extract shared request/reply and connection-initialization mechanics only
  where multiple live callers exist.

### 5C. Relation preparation

- Extract one `prepare_relation` seam for proto conversion, runtime-shape
  discovery, implicit schema resolution, analysis, and finalization where those
  steps are currently repeated by ExecutePlan and AnalyzePlan.
- Parameterize only the genuinely different terminal result. Do not hide
  execution and analysis behind a trait hierarchy.
- Rename `resolve_implicit_pivots` to reflect that it also resolves crosstab and
  file schemas.

### Exit gate

- No runtime behavior depends on matching SQL text prefixes.
- Cache mutation is represented by a closed type and implemented once.
- ExecutePlan and AnalyzePlan share preparation without losing their distinct
  error/result behavior.
- Session-thread ownership, cancellation, and Arrow wire tests remain green.

## Phase 6 — evaluate phase-typed plans and expressions

This is an experiment, not a committed migration. Run it after the earlier
workstreams, because they may remove enough duplication that the experiment no
longer pays.

### Prototype

On a private branch or throwaway patch, model only Project, Filter, Join, and a
subquery with one of:

- `Plan<P>` / `Op<P>` with sealed phase markers; or
- `Op<Child, Expr>` with separate unresolved and resolved expression types.

The prototype must eliminate the current `SubqueryPlan::{Unanalyzed,
Analyzed}` state leak and make resolved column identity non-optional where the
compiler can prove it. It must not spread generic bounds through emission or
make routine pattern matching harder to read.

### Adoption gate

Adopt only if the prototype:

- removes duplicated operator declarations or substantial conversion code;
- prevents at least one currently representable invalid state;
- has lower net production LOC on the representative slice;
- produces clearer compiler errors and exhaustive matches; and
- does not require public trait objects, pervasive boxing, or phase casts.

Otherwise document the negative result and keep `CommonOp`/`TypedOp`. Their
duplication is preferable to a generic architecture that is smaller only on
paper.

## Phase 7 — complete the inline-comment audit

Perform the broad comment sweep after structural work so effort is not spent
polishing code scheduled for deletion. Stale contradictions from Phase 1 are
fixed immediately.

### Keep in source

- Spark/DuckDB semantic mismatches and the exact behavior τ must emulate;
- safety, overflow, alignment, vector-selection, thread-ownership, and wire
  boundary constraints;
- mathematical/date bounds that are not apparent from the expression;
- error-category mappings and non-local invariants;
- concise rationale for an intentionally surprising implementation.

### Delete or move

- comments that narrate the next line or restate a field/type name;
- review findings, pass numbers, implementation status, and migration ledgers;
- commented-out alternatives and future work without an active task/ADR;
- corpus stories inside production code when a named test or case ID suffices;
- decorative separator banners that add no navigation value;
- repeated field documentation after visibility or types make the invariant
  local and unambiguous.

Historical rationale moves to an ADR or `docs/dev_journal/`; executable future
work moves to `tasks/`. Do not delete a surprising constraint merely to improve
the comment metric.

### Optional test extraction

Move embedded test modules out of `emission.rs`, `analyzer.rs`, and
`v2_lowering.rs` only if it materially improves navigation and ownership. Count
this as a file-size improvement, never as LOC reduction, and do not combine it
with semantic changes.

### Exit gate

- Every first-party inline comment has an audited disposition.
- Remaining comments describe current truth and pass an independent stale-name
  search.
- No production comment refers to a deleted symbol, completed pass, or obsolete
  error model.
- Before/after counts are reported by category, with examples of rationale that
  was intentionally retained.

## Delivery order

Use this order unless a phase's evidence invalidates the next step:

1. Phase 0 baseline and ADR map.
2. Phase 1 proven deletions, typed interval replacement, and stale comments.
3. Phase 2 plan-origin metadata.
4. Phase 5 runtime/service slices; these may proceed independently but must not
   share a change set with Phase 2.
5. Phase 3 generator IR prototype and migration.
6. Phase 4 live function registry prototype and incremental migration.
7. Phase 6 phase-typed experiment, with an expected possible outcome of “do
   not adopt.”
8. Phase 7 whole-tree comment audit and optional test extraction.

Each numbered bullet is a program boundary, not necessarily one commit. Within
it, prefer one coherent deletion or one vertical semantic slice per change.
Never combine a representation migration with unrelated comment churn; that
destroys reviewability and makes corpus regressions harder to localize.

## Verification gates

For every non-trivial Rust slice, run in this order and stop at the first red
gate:

```bash
cargo fmt --check
cargo clippy -- -D warnings
cargo test
./tests/scripts/run-differential-tests.sh core
./tests/scripts/run-differential-tests.sh sql_v2
```

Compare differential output to Phase 0 by case name and require no
previously-green regression. TPC-H and TPC-DS cases are part of those corpora
and receive no exemption. Use targeted tests during iteration, but they do not
replace the final gates.

For any C++ source change under `extension/`, also run from `extension/`:

```bash
make release
make format-check
make test
make tidy-check
```

Changed parity behavior requires Spark-sourced SQLLogicTest goldens. If a gate
is impossible because of an environmental failure, record the exact blocker;
do not mark the slice complete.

## Program-level acceptance criteria

- τ remains the sole request path and retains all INV3/INV10 contamination
  barriers.
- Plan IDs are preserved as ordinary node metadata; no join carries
  `left_plan_ids` or `right_plan_ids`, and no partial raw-proto traversal exists
  solely to collect them.
- No generator is encoded through a reserved general-function name.
- Catalog, classification, ordinary inference/nullability, ordinary emission,
  and extension validation consume one live function-spec source for migrated
  functions.
- Direct SQL and DataFrame calls have exactly one reachable implementation for
  every runtime macro removed.
- No new empty module, ignored `todo!()` invariant, `#[allow(dead_code)]`, or
  test-only production hook remains without a written justification.
- Comments contain current rationale rather than development history.
- Every landed phase is net simpler by its predeclared metric and leaves all
  unit tests and previously-green DataFrame/SQL corpus cases green.

## Explicit non-goals

- Adding an optimizer; ADR-001's direct translation remains intact.
- Reintroducing the deleted v1 modules or any fallback path.
- Replacing closed enums with dynamic trait hierarchies.
- Switching wholesale to `sqlparser`'s SQL AST without a measured prototype.
- Encoding every special function in tables merely to eliminate `match` arms.
- Moving complexity into C++ solely to reduce Rust LOC.
- Splitting large test modules and claiming that as code reduction.

## Safe-deletion checkpoint — 2026-08-09

Executed on branch `refactor/architecture-simplification-safe-deletions`, based
on `5a0ba81a`. The changes remain uncommitted pending user review.

### Result

- Removed the unproduced `RowConstructorExpression` and its general-expression
  plumbing.
- Removed unconstructible `TypedOp::Unnest` while retaining `CommonOp::Unnest`
  and its analyzer boundary error.
- Removed `EMIT_TAP`, its mutex, atomic write on successful emission, tap-only
  tests, and 133 test serialization guards. The deleted check did not enforce
  authoritative INV2; it only counted one leaf dispatch and was false for
  recursive emission.
- Removed ten shadowed runtime macros: `startswith`, `endswith`, `btrim`,
  `substring_index`, `bit_get`, `arrays_zip`, `shiftleft`, `shiftright`, `conv`,
  and `to_char`.
- Added one table-driven direct-SQL parser → analyzer → emission test for the
  three previously unwitnessed routes (`endswith`, `arrays_zip`, and `conv`).
- Removed the unused C++ `add_int` and `add_double` registration lambdas.
- Corrected the two stale comments that claimed `Unnest` reached typed
  emission.

The first independent review rejected deletion of the empty `rewrites.rs`:
ADR-007 explicitly retains the B-layer seam. The module and declaration were
restored. Final independent review reported no remaining Critical, High, or
Medium findings.

Across the nine changed implementation/test files: **33 lines added, 339
deleted, net −306**. No interval, generator, plan-origin, function-registry, or
service architecture change was mixed into this checkpoint.

### Verification

- `cargo fmt --check` — passed.
- `cargo clippy -- -D warnings` — passed.
- `cargo test` — passed: connect-server 134/134; core 1,265 passed and 5
  ignored; all other configured suites passed or were intentionally ignored.
- Direct-SQL runtime-macro routing test — passed.
- DataFrame corpus — 421 passed, 7 documented deferred reds (`errcls-006`,
  `sqlwrap-001..005`, `prettyname-004`).
- SQL corpus — 424 passed, 2 explicitly deferred/skipped.
- Checked-in pass baseline — 829 cases, `REGRESSIONS: 0`; all 14 select-block
  witnesses remain green.
- Extension `make release` — passed.
- Extension `make test` — passed: 547 assertions in 24 cases.
- Extension `make format-check` — passed with the script's supported
  `DUCKDB_FORMAT_SKIP_VERSION_CHECKS=1` override because this environment has
  clang-format 18 rather than 11; the full `src`/`test` tree was checked.
- Extension `make tidy-check TIDY_BINARY=clang-tidy-18` — passed.

### Next implementation checkpoint

Stop here for review. Before Phase 2, refresh the baseline and write the
plan-origin ADR amendment. Do not treat the remaining typed-interval or
invariant candidates as safe deletion: each changes a represented contract and
must be planned and witnessed independently.
