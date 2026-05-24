# Spark Connect Plan Canonicalization for Thunderduck — High-Level Design

**Audience:** Architect agent responsible for implementing this in the thunderduck-rs codebase (https://github.com/lastrk/thunderduck-rs).

**Status:** Design for review.

**Pinned Spark version:** 4.0.0 (the canonicalizer is version-pegged; bump the version constant and re-run the property suite when upgrading).

---

## 1. Background and Motivation

Thunderduck is a Spark Connect server that compiles incoming Spark Connect plans to DuckDB SQL and returns Arrow results. The Spark Connect DataFrame API is intentionally redundant at the wire level: the same logical query can arrive as any of dozens of syntactically distinct protobuf shapes. `where` and `filter` are the same operator; `withColumn` is sugar for a `Project`; `drop` is sugar for a `Project` with a smaller output list; `crossJoin` is sugar for `Join(condition=true, type=cross)`; aliases like `where`/`filter`, `orderBy`/`sort`, and the various `unionByName` forms each name the same internal operator.

Handling every wire form natively in the compiler is wasteful and error-prone. It also makes the differential test harness against reference Spark vastly harder, because every sugar variant becomes a separate equivalence class in the input space the harness must cover.

This document specifies a two-layer canonicalization pipeline that runs on every incoming Spark Connect plan, between protobuf deserialization and compilation. The pipeline reduces a sprawling input space to a closed, minimal set of relations and expressions, with documented correctness properties at each transformation.

## 2. Goals and Non-Goals

### Goals

The canonicalization pipeline shall produce a *canonical* protobuf `Relation` for any input `Relation` such that two inputs Spark would resolve to the same `LogicalPlan` produce bytewise-identical canonical outputs (modulo documented exceptions in Layer 2). The output of canonicalization must remain a valid Spark Connect `Relation` — serializable to the wire and accepted by an unmodified Spark Connect server. Every transformation must carry a documented correctness justification: definitional equivalence, faithful mirroring of Spark's own analyzer rules, or three-valued-logic equational reasoning under explicit preconditions.

The pipeline must be fast enough to run on every incoming plan in production. Target: under 1 ms for plans up to a few hundred nodes, with allocation kept proportional to the input size.

### Non-Goals

The pipeline is not an optimizer. It does not push down predicates, prune projections, reorder joins, or fuse operators. It is a structural rewriter that produces a canonical wire representation; downstream compilation and DuckDB itself handle optimization.

The pipeline does not attempt to handle Spark commands (DDL, function registration, view creation), only `Plan.Root` containing a `Relation`. Commands pass through unchanged.

The pipeline does not handle streaming-specific relations (`WithWatermark`, `ApplyInPandasWithState`, etc.), ML pipeline extensions, or Pandas/Arrow UDF wrapping in this iteration. These pass through with a stable error.

## 3. Architecture Overview

The pipeline sits between protobuf deserialization and the existing thunderduck-rs compiler. The data flow is:

```
incoming wire bytes
        │
        ▼
prost::Message::decode → spark::connect::Plan
        │
        ▼
extract Plan.Root → Relation
        │
        ▼
┌─────────────────────────────────────────┐
│ Layer 1: Syntactic Canonicalization     │
│   (always on, semantics-preserving)     │
└─────────────────────────────────────────┘
        │
        ▼
┌─────────────────────────────────────────┐
│ Layer 2: Normalization                  │
│   (opt-in via config; conditional)      │
└─────────────────────────────────────────┘
        │
        ▼
canonical Relation
        │
        ├─► hash / dedupe / snapshot
        │
        ▼
existing thunderduck-rs compiler
        │
        ▼
DuckDB SQL
```

Both layers operate on the prost-generated Spark Connect proto types directly. There is no intermediate hand-rolled AST. The output of Layer 2 is a `Relation` that can be re-serialized to the wire byte-for-byte and would be accepted by an unmodified Spark Connect server.

The two layers are configured independently. Layer 1 runs unconditionally; Layer 2 is gated by a `CanonicalizerConfig` struct that defaults to "syntactic only" and allows progressive opt-in to specific normalization passes once they have been property-tested against Spark.

## 4. Data Model

### Proto types are the canonical AST

The canonicalizer operates on `prost`-generated types from `spark/connect/*.proto`. The relevant types are:

- `spark::connect::Plan` — top-level, with `op_type: Option<plan::OpType>` discriminating `Root(Relation)` from `Command(Command)`.
- `spark::connect::Relation` — has `common: Option<RelationCommon>` and `rel_type: Option<relation::RelType>` (the big oneof).
- `spark::connect::Expression` — has `expr_type: Option<expression::ExprType>` (oneof of literal, attribute, function, alias, cast, etc.).

The architect should regenerate the prost bindings from Spark 4.0.0's published proto files and pin the version. A CI check must compare the regenerated bindings against the checked-in copy and fail on drift.

### The closed core relation set

After Layer 1, only the following relation types remain in the canonical plan:

`Read`, `LocalRelation`, `Project`, `Filter`, `Join`, `Aggregate`, `Sort`, `Limit`, `Offset`, `SetOperation`, `Deduplicate`, `SubqueryAlias`, `Tail`, `Range`.

Every other wire relation is either lowered to one of these or passes through with a documented exception (Sample, Repartition, RepartitionByExpression are passthroughs because they affect physical execution but not logical results; streaming/ML extensions return an error).

The closed core expression set is similarly small: `Literal`, `UnresolvedAttribute`, `UnresolvedFunction`, `Alias`, `Cast`, `SortOrder`, `Window`, `UnresolvedStar`, `UnresolvedExtractValue`, `UpdateFields`, `LambdaFunction`, `UnresolvedNamedLambdaVariable`, `CommonInlineUserDefinedFunction`. `ExpressionString` is always lowered to structured form in Layer 1.

### Optional view layer

Pattern-matching on prost types is verbose because of the nested `Option<oneof>` shape. The architect may introduce a thin view layer of borrowed wrapper types (`RelView<'a>`, `ExprView<'a>`) for readability, as long as the canonical form remains the protobuf. The view layer is convenience, not a parallel data model.

## 5. The De Bruijn Binding System

### Motivation

The Spark Connect wire format identifies columns by string name: `UnresolvedAttribute { unparsed_identifier: "t1.col_a", .. }`. Alias names are arbitrary — `df.alias("foo")` and `df.alias("bar")` produce different wire bytes for semantically identical plans. Naive canonicalization leaves alpha-equivalent plans as distinct canonical forms, which defeats the entire purpose. With three nested subquery aliases and a name pool of ten, roughly 10³ = 1000 alpha-variants of every real plan persist after canonicalization.

The fix is positional encoding of column references in the canonical form's internal representation, with deterministic fresh names re-attached at serialization. This is the standard de Bruijn technique adapted to relational scopes.

### Representation

```rust
/// A column reference resolved against the lexical scope stack.
/// `up` counts how many enclosing scopes to walk outward from the
/// use site; `ordinal` indexes into that scope's output schema.
#[derive(Copy, Clone, Eq, PartialEq, Hash, Debug)]
pub struct ColRef {
    pub up: u8,
    pub ordinal: u16,
}
```

`u8` for `up` is sized for correlation depth, which is realistically bounded at 0–3 in human-written SQL and never exceeds a low double-digit value even in machine-generated queries. The cap at 255 is a documented invariant; producing a value at the cap should panic.

`u16` for `ordinal` is sized for schema width. Real wide-fact tables reach the low thousands of columns in enterprise data warehouses; 65 535 is a safe ceiling.

The struct is 3 bytes (4 with alignment), bitwise-comparable, hash-friendly, and allocation-free.

### Scope stack

A scope is the output schema of a binding-introducing relation. The relations that introduce a scope are `Read`, `LocalRelation`, `Range`, `Project`, `Aggregate`, `SetOperation`, `Join` (both sides combined), `SubqueryAlias`. The canonicalizer maintains a `Vec<Scope>` during traversal:

```rust
struct Scope {
    /// Ordered list of (canonical_name, datatype) for each output column.
    columns: Vec<(String, DataType)>,
    /// Optional qualifier — Some when this scope was introduced by a
    /// SubqueryAlias or a Read with an alias, None for anonymous scopes
    /// (intermediate Project, etc.).
    qualifier: Option<String>,
}
```

### Binding normalization algorithm

The canonicalizer walks the plan top-down, pushing scopes onto the stack as it enters relations and popping when it leaves. At each `UnresolvedAttribute`, it parses the identifier path (`t1.col_a` or `col_a`), resolves it against the scope stack (most recent first, with qualifier matching when the path is qualified), and replaces the proto's `unparsed_identifier` with a deterministic canonical form: `t{depth}.c{ordinal}`, where `depth` is computed at *serialization time* from the final stack depth and `ordinal` is the column position in the resolved scope.

The architect should maintain the resolved `(up, ordinal)` pair as the internal representation during canonicalization (used for plan hashing and snapshot equality) and reify the wire-form name only when serializing back to protobuf. Two helper functions express this:

```rust
fn resolve(attr: &UnresolvedAttribute, scopes: &[Scope]) -> ColRef;
fn serialize_ref(c: ColRef, final_scopes: &[Scope]) -> String;  // returns "t3.c1"
```

### Output schema name preservation

Spark exports the names of the top-level output columns to the client; those names are observable. The canonicalizer must preserve them at the plan root. Specifically: when the root relation is a `Project`, the `Alias` expressions in its output list retain their user-visible `name`. Internal subquery aliases, intermediate `Alias` expressions, and `SubqueryAlias` qualifiers are all freshened to `t0, t1, …` / `c0, c1, …` form. The boundary between "freshen" and "preserve" is exactly the root projection list.

For plans that don't have a root projection (e.g., the root is a `Filter` over a `Read`), the original schema names flow through unchanged from the leaf `Read`.

### Resolution rules summary

When resolving `UnresolvedAttribute { unparsed_identifier: "x.y" }`:

The identifier is parsed into a path with one of three shapes: a single name `c`, a qualified pair `t.c`, or a multi-part struct access `t.c.f1.f2`. For a single name, walk the scope stack from the innermost outward; the first scope whose columns contain a matching name resolves the reference (and `up` is set to the stack distance). For a qualified pair, walk outward looking for a scope whose qualifier matches the first part; resolve the second part against that scope's columns. For multi-part struct access, the first one or two parts resolve to a `ColRef` as above; the remaining parts become a chain of `UnresolvedExtractValue` wrappers.

Ambiguity (a single name matches columns in multiple scopes) is resolved by inner-scope precedence, matching Spark's analyzer behavior. Unresolved references are an error and abort canonicalization.

## 6. Layer 1: Syntactic Canonicalization

This layer runs unconditionally on every incoming plan. Every transformation in it is either definitionally equivalent or a faithful mirror of a Spark analyzer rule. No transformation in this layer changes any observable behavior under any Spark configuration.

### 6.1 Relation alias unification

`Where` is an alias for `Filter` (same backing operator in Spark). `OrderBy` is an alias for `Sort`. Where the proto exposes both, normalize to the canonical name. **Correctness:** Spark deserializes both into the same `LogicalPlan` node.

### 6.2 Sugar expansion to Project

The following relations are lowered to `Project`, mirroring Spark's `SparkConnectPlanner`:

`WithColumns(input, cols)` becomes `Project(input, replace_or_append(input.output, cols))`. The replacement rule: for each column in `input.output`, if a `cols` entry shares its name, use the entry's expression with the original column's metadata preserved; append any `cols` entries that don't match. **Correctness:** mirrors `SparkConnectPlanner.transformWithColumns`. The metadata-preservation detail is essential — naive replacement loses Spark's column metadata and breaks downstream tools that read it.

`WithColumnsRenamed(input, renames)` becomes `Project(input, [c if c.name not in renames else c.as(renames[c.name]) for c in input.output])`. **Correctness:** mirrors `SparkConnectPlanner.transformWithColumnsRenamed`.

`Drop(input, cols)` becomes `Project(input, [c for c in input.output if c not in cols])`. **Correctness:** mirrors `SparkConnectPlanner.transformDrop`.

`ToDF(input, names)` becomes `Project(input, [c.as(names[i]) for i, c in enumerate(input.output)])`. **Correctness:** mirrors `SparkConnectPlanner.transformToDF`.

### 6.3 Cross-join and using-join lowering

`CrossJoin(left, right)` becomes `Join(left, right, condition=lit(true), join_type=CROSS)`. **Correctness:** definitional.

`Join(left, right, using_columns=[k1, k2], join_type=t)` becomes `Join(left, right, condition=AND(left.ki == right.ki for ki in using_columns), join_type=t)` wrapped in a `Project` that collapses the duplicated key columns to one copy per key (matching Spark's `USING` semantics). **Correctness:** mirrors Spark's `ResolveUsingJoinReferences` analyzer rule.

### 6.4 Distinct lowering

`Distinct(input)` and `Deduplicate(input, all_columns_as_keys=true)` both become `Aggregate(input, grouping=input.output, aggregations=[])`. **Correctness:** mirrors Spark's `ReplaceDistinctWithAggregate` analyzer rule.

### 6.5 SubqueryAlias collapsing

`SubqueryAlias(n1, SubqueryAlias(n2, child))` becomes `SubqueryAlias(n1, child)`. The inner alias is unreachable from any enclosing expression. **Correctness:** alpha-equivalence; no expression can reference the shadowed inner qualifier.

### 6.6 Identity-Project elimination

`Project(child, exprs)` where `exprs` is exactly `child.output` (same `Alias` names, same `Expression` ASTs, same metadata) becomes `child`. The metadata equality check is essential and must include column comments, nullability, and any per-column metadata fields. **Correctness:** mirrors `RemoveRedundantProjects` (with the conservative restriction).

### 6.7 Hint stripping

`Hint(child, name, params)` becomes `child`. Hints are optimizer guidance only; stripping them changes physical execution choices but not logical results. **Correctness:** Spark's `EliminateResolvedHint` performs the same removal post-optimization.

Note: if the architect later wants to test hint propagation behavior, hint stripping should be made configurable. Default is to strip.

### 6.8 Sort default-fill

For each `SortOrder` in a `Sort` relation, fill `direction` and `null_ordering` to explicit values. The proto defaults are `direction=ASC, null_ordering=NULLS_FIRST` for `ASC` and `NULLS_LAST` for `DESC`; explicit-fill avoids ambiguity from the proto3 "field not set" / "field set to default" indistinguishability. **Correctness:** proto-level normalization; no semantic change.

### 6.9 Boolean operator surface unification

`UnresolvedFunction { function_name: "!=", arguments: [a, b] }` becomes `UnresolvedFunction { function_name: "<>", arguments: [a, b] }` (or vice versa — pick one as canonical). Similarly unify `==` and `=`, and unify spellings of `is_null` / `is_not_null`. **Correctness:** Spark's parser treats these as the same operator; the analyzer produces the same Catalyst expression for either spelling.

`Not(EqualTo(a, b))` becomes `NotEqualTo(a, b)`. **Correctness:** Spark's optimizer rewrites the former to the latter unconditionally (`SimplifyBinaryComparison`).

### 6.10 Double-negation elimination

`Not(Not(e))` becomes `e`. Evaluation order is preserved (the operand is evaluated exactly once in both forms). **Correctness:** holds in three-valued logic (`NOT(NOT NULL) = NULL`, same as `NULL`).

### 6.11 Expression-string lowering

`ExpressionString(s)` is parsed via `sqlparser-rs` (Spark dialect) into structured `Expression` form. The structured form goes through the rest of canonicalization normally. **Correctness:** Spark's analyzer does the same parse on the server; the only risk is parser-dialect drift between `sqlparser-rs` and Spark's ANTLR parser. The architect should include a property test that round-trips a corpus of expression strings through both parsers and asserts equivalence.

If `sqlparser-rs` cannot parse a given expression string, leave the `ExpressionString` in place and emit a diagnostic; do not silently drop it.

### 6.12 Fresh-name assignment

After de Bruijn resolution, all `SubqueryAlias` qualifiers and all non-output `Alias` names are reassigned in left-to-right traversal order: `t0, t1, …` for subquery aliases, `c0, c1, …` for intermediate column aliases. Top-level output column aliases are preserved (see §5). **Correctness:** definitional alpha-equivalence; the only observable consequence is at the wire layer, where names become deterministic.

### 6.13 Map field sorting

For any proto field of map type (notably `Read.NamedTable.options` and `Read.DataSource.options`), serialize entries with keys sorted lexicographically. Proto3 maps have no canonical wire order, so two semantically identical plans hash to different bytes depending on the producer's hash-map seed unless sorted. **Correctness:** proto-level; no semantic change.

### 6.14 Repeated-field ordering

For repeated fields that Spark treats as set-like or multiset-like rather than positional, sort by canonical hash. The classification (per relation/expression) is hard-coded in a static table; the architect should not derive it from proto annotations because they don't carry that information. The unambiguous set-like cases include `Aggregate.grouping_expressions` (set of grouping keys) and `Deduplicate.column_names`. The positional cases include `Project.expressions`, `Sort.order`, `SetOperation.input` (for positional `Union`).

### 6.15 Strip layer 1 closes here

After Layer 1, the plan contains only the closed core relation set and the closed core expression set, with all binding names freshened, all proto defaults explicitly filled, all maps and unordered repeated fields sorted, and all sugar lowered. Two plans Spark would resolve to the same `LogicalPlan` are bytewise-identical at this point — modulo only the transformations deferred to Layer 2.

## 7. Layer 2: Normalization (Conditional)

This layer is gated by `CanonicalizerConfig`. Each transformation is correct only under stated assumptions and must be enabled deliberately.

### 7.1 Static safety analysis

Before any Layer 2 transformation, run a static analysis pass that tags each `Expression` subtree with three booleans:

```rust
struct ExprSafety {
    /// True iff the expression contains no operations that can throw under
    /// ANSI mode (no CAST to narrower type, no divide, no mod, no assert_true,
    /// no element_at on possibly-missing keys, no array indexing).
    ansi_total: bool,
    /// True iff the expression contains no UDF, no subquery, no
    /// non-deterministic function (rand, current_timestamp, etc.).
    pure: bool,
    /// True iff the expression has no floating-point arithmetic and
    /// cannot produce NaN.
    nan_free: bool,
}
```

A transformation that requires `pure && ansi_total` may only fire on subtrees that pass the corresponding check. The analysis is a simple bottom-up traversal; the architect should keep it in a separate module so transformations can compose it cheaply.

### 7.2 AC flattening and sorting of boolean connectives

`AND(AND(a, b), c)` flattens to a single n-ary `AND(a, b, c)`, then operands are partitioned into a *safe-to-reorder* group (those whose subtree satisfies `pure && ansi_total`) and a *fixed-position* group (the remainder). The safe group is sorted by canonical hash; the fixed group retains its original relative order. The result interleaves: safe operands fill positions left to right, fixed operands hold their absolute positions. Same logic for `OR`.

**Correctness:** AC laws of Boolean algebra hold in three-valued logic (verified by case analysis on NULL). The reordering of safe operands does not change observable behavior because they have no side effects and cannot throw. Fixed-position operands are not reordered, preserving short-circuit semantics for the cases that need them.

**Implementation note:** the n-ary representation is internal only; serialize back to the proto's binary form by right-folding (`AND(a, AND(b, c))`) for consistency with Spark's reader.

### 7.3 De Morgan pushdown

`Not(And(a, b))` becomes `Or(Not(a), Not(b))`; `Not(Or(a, b))` becomes `And(Not(a), Not(b))`. Apply only when both operands satisfy `pure && ansi_total` (otherwise reordering the negations changes the evaluation order of throwing expressions).

**Correctness:** De Morgan holds in three-valued logic (verified by case analysis: `NOT(NULL AND false) = NOT(false) = true`; `(NOT NULL) OR (NOT false) = NULL OR true = true`).

### 7.4 Comparison inversion (non-float guard)

`Not(LessThan(a, b))` becomes `GreaterThanOrEqual(a, b)`. Apply only when both `a` and `b` have non-floating, non-decimal-NaN-capable types (checked via `expr_safety.nan_free` on both operands).

**Correctness:** the rewrite fails on NaN because IEEE-754 makes all comparisons against NaN return false, so `NOT(NaN < x)` is `true` while `NaN >= x` is `false`. The type guard rules out NaN-bearing operands.

### 7.5 Set-operation child sorting

`SetOperation(union_all, [A, B, C], by_name=true, allow_missing_columns=false)` may sort `[A, B, C]` by canonical hash. The `by_name=true, allow_missing_columns=false` precondition ensures the output schema is determined by name-matching, not by left-child position, so reordering doesn't change the output schema.

Positional `Union` (`by_name=false`) is **not** sortable: the output column names are inherited from the left child, so reordering changes the result schema. The canonicalizer must check `by_name` before attempting this transformation.

**Correctness:** multiset union/intersect is commutative; with by-name schema unification the output is invariant under child reordering.

### 7.6 Trivial constant folding

`Filter(child, lit(true))` becomes `child`. `Filter(child, lit(false))` becomes `LocalRelation(child.schema, [])`. `Limit(child, 0)` becomes `LocalRelation(child.schema, [])`. Apply only to literal-typed filter conditions; do not attempt to evaluate non-literal expressions.

**Correctness:** trivially semantics-preserving for the cases listed.

**Note:** this transformation is *opt-out* for testing scenarios where the harness is exercising thunderduck-rs's own optimizer. The default for production is opt-in (off until property-tested).

## 8. Out of Scope (Explicitly)

The following transformations are tempting but unsafe in Spark's actual semantics; the architect must not implement them, and CI should fail if any sneak in:

Arithmetic AC normalization (`(a + b) + c → a + (b + c)`). Fails on floating-point non-associativity, ANSI integer overflow, and decimal precision propagation.

Reordering of any expression containing a non-deterministic function (the proto carries the `deterministic` flag on `UnresolvedFunction`; respect it).

Any transformation that changes intermediate cardinality (removing a `Limit`, fusing across `Distinct`, dropping a `SubqueryAlias` whose qualifier is referenced downstream).

`Not(LessThan)` rewrite on floating types (see §7.4).

Inlining of `CommonInlineUserDefinedFunction` calls or any UDF rewriting.

Streaming-specific relations (`WithWatermark`, etc.) and ML pipeline extensions.

## 9. Module Layout

```
crates/
  thunderduck-canon/
    Cargo.toml
    src/
      lib.rs                # public API: canonicalize(Relation) -> Relation
      config.rs             # CanonicalizerConfig
      error.rs              # CanonError type
      proto.rs              # re-exports of spark::connect::* + helpers
      view.rs               # optional borrowed view types (RelView, ExprView)
      traverse.rs           # generic top-down / bottom-up traversal combinators
      scope.rs              # Scope, ScopeStack, ColRef
      resolve.rs            # UnresolvedAttribute resolution against ScopeStack
      layer1/
        mod.rs              # orchestration: order of passes
        relation_alias.rs   # Where→Filter, OrderBy→Sort, etc.
        sugar.rs            # WithColumns, Drop, ToDF, etc. → Project
        joins.rs            # CrossJoin, using-join → Join
        distinct.rs         # Distinct → Aggregate
        subquery_alias.rs   # collapse nested SubqueryAlias
        identity_proj.rs    # remove identity Project
        hints.rs            # strip Hint
        sort_defaults.rs    # fill SortOrder defaults
        bool_unify.rs       # operator surface unification
        double_neg.rs       # NOT(NOT x) → x
        expr_string.rs      # parse ExpressionString via sqlparser-rs
        fresh_names.rs      # assign t0, c0, ... after de Bruijn
        map_sort.rs         # sort proto map fields
        repeated_sort.rs    # sort unordered repeated fields
      layer2/
        mod.rs              # orchestration
        safety.rs           # ExprSafety analysis
        bool_ac.rs          # AC flatten + sort safe operands
        demorgan.rs         # NOT pushdown
        cmp_invert.rs       # NOT(comparison) → inverse
        setop_sort.rs       # sort byName Union children
        const_fold.rs       # trivial constant folding
      testing/
        roundtrip.rs        # helpers for property tests against Spark
        snapshot.rs         # prototext snapshot helpers
    tests/
      layer1_unit.rs        # per-pass unit tests
      layer2_unit.rs
      e2e_roundtrip.rs      # full canonicalize-and-execute against Spark
      snapshot/             # insta snapshots
```

The crate is independent of the rest of thunderduck-rs's compiler; the compiler depends on it but not vice versa.

## 10. Public API

```rust
pub struct CanonicalizerConfig {
    /// Layer 2 toggles. All default to false.
    pub bool_ac: bool,
    pub demorgan: bool,
    pub cmp_invert: bool,
    pub setop_sort: bool,
    pub const_fold: bool,
    /// Pinned Spark version this canonicalizer matches.
    pub spark_version: SparkVersion,
}

impl Default for CanonicalizerConfig {
    fn default() -> Self { /* all false, version = Spark 4.0.0 */ }
}

#[derive(Debug, thiserror::Error)]
pub enum CanonError {
    #[error("unsupported relation: {0}")]
    UnsupportedRelation(&'static str),
    #[error("unresolved column reference: {0}")]
    UnresolvedAttribute(String),
    #[error("ambiguous column reference: {0}")]
    AmbiguousAttribute(String),
    #[error("expression string parse error: {0}")]
    ExprStringParse(String),
    #[error("unknown proto oneof variant in {context}")]
    UnknownVariant { context: &'static str },
    // ...
}

pub fn canonicalize(
    rel: Relation,
    config: &CanonicalizerConfig,
) -> Result<Relation, CanonError>;

/// Convenience wrapper: deserialize, canonicalize, re-serialize.
pub fn canonicalize_bytes(
    bytes: &[u8],
    config: &CanonicalizerConfig,
) -> Result<Vec<u8>, CanonError>;

/// Stable hash of the canonical form. Two plans Spark would resolve identically
/// produce the same hash (modulo Layer 2 opt-ins).
pub fn canonical_hash(rel: &Relation) -> [u8; 32];
```

The integration point in the thunderduck-rs server: at the moment an incoming `ExecutePlanRequest` is deserialized, replace its `plan.root` with `canonicalize(plan.root, &config)?` before handing to the compiler.

## 11. Testing Strategy

### 11.1 Per-pass unit tests

Each transformation has a dedicated unit-test file in `tests/layer1_unit.rs` or `tests/layer2_unit.rs`. The test pattern is: construct a small input `Relation` programmatically (using a Rust DSL builder, not raw proto), apply the single pass under test, and assert equality against an expected output `Relation`. No live Spark is involved at this level.

### 11.2 Snapshot tests with `insta`

For a curated set of hand-written canonical plans, snapshot the prototext rendering of the post-canonicalization `Relation`. The snapshot files live in `tests/snapshot/` and are reviewed via `cargo insta review` when intentional changes occur.

### 11.3 Property tests against Spark Connect

The load-bearing test for correctness. For each transformation T, write a `proptest` strategy that generates a `Relation` r where T applies, then assert that running r and canonicalize(r) through Spark Connect (a live Spark 4.0.0 server, started by `testcontainers-rs` in the CI environment) produces identical results after the canonicalization rules in §6/§7 of the broader research note (sort rows, normalize floats, etc.).

For the unconditional Layer 1 transformations, the property must hold over a generator that includes ANSI mode on/off, UDFs, NaN-bearing types, decimal types at precision boundaries, NULL-rich inputs, and edge-case literals. For Layer 2 transformations, the generator may restrict to the preconditions stated in §7.

### 11.4 Resolved-plan diffing (strongest correctness)

For the unconditional Layer 1 transformations specifically, the strongest available check is to send r and canonicalize(r) to Spark's `AnalyzePlan` API and compare the resolved `LogicalPlan` byte-for-byte (after stripping Catalyst `exprId`s, which differ run-to-run). If Spark's analyzer produces the same resolved plan for both, they are behaviorally identical post-analyzer. This is much stronger than diffing results because it eliminates the input space of "the test inputs happened not to expose the bug."

This test runs nightly rather than per-commit because of the cost.

### 11.5 Differential against thunderduck-rs's compiler

A regression test: for a corpus of canonical plans, compile to DuckDB SQL both before and after the canonicalizer is enabled in the server pipeline. The SQL output should match (modulo deterministic improvements). This catches the case where the canonicalizer changes the input to the compiler in a way that breaks an existing compiler path.

### 11.6 Fuzz target

A `cargo fuzz` target consumes byte tape, deserializes as a `Relation`, canonicalizes, re-serializes, and asserts the result is a valid `Relation` that Spark Connect would parse. This catches panics and malformed-output bugs over weeks of compute, complementing the proptest runs.

## 12. Phasing

### Phase 1: Skeleton (week 1)

Create the `thunderduck-canon` crate. Regenerate prost bindings for Spark 4.0.0. Implement `traverse.rs` with the generic top-down recursion. Implement `Scope`, `ScopeStack`, `ColRef`. Wire the canonicalizer into the thunderduck-rs server behind a feature flag that defaults to off. No transformations yet — the canonicalizer is the identity function. End-to-end CI runs against an existing thunderduck-rs test suite to confirm no regression.

### Phase 2: Layer 1 sugar expansion (week 2)

Implement `WithColumns`, `WithColumnsRenamed`, `Drop`, `ToDF` lowering. Implement `CrossJoin` and using-join lowering. Implement `Distinct` → `Aggregate`. Implement `Hint` stripping. Implement relation alias unification. Unit tests per pass. Snapshot tests for the lowering of each sugar form.

### Phase 3: Layer 1 binding normalization (week 3)

Implement de Bruijn resolution. Implement fresh-name assignment. Implement `SubqueryAlias` collapsing. Implement identity-Project elimination with the metadata-equality check. Snapshot tests showing alpha-variant inputs canonicalizing identically.

### Phase 4: Layer 1 expression normalization (week 4)

Implement boolean operator surface unification. Implement double-negation elimination. Implement `ExpressionString` lowering via `sqlparser-rs`. Implement Sort default-fill. Implement map sorting and repeated-field sorting.

### Phase 5: Layer 1 property tests (week 5)

Stand up a Spark 4.0.0 instance via `testcontainers-rs` in CI. Build the proptest generators. Run the property tests for every Layer 1 transformation. Triage and fix discrepancies. Enable the canonicalizer's feature flag by default in thunderduck-rs once green.

### Phase 6: Layer 2 (weeks 6–7)

Implement `ExprSafety` analysis. Implement AC flattening, De Morgan pushdown, comparison inversion, set-op child sorting, constant folding. Each behind its own config flag, all defaulting to off. Property test each against Spark with the appropriate preconditions enabled in the generator.

### Phase 7: Resolved-plan diffing (week 8)

Build the `AnalyzePlan` differential test. Run it nightly. Investigate every divergence (each one is either a bug in the canonicalizer or a documented version-pinning issue).

### Phase 8: Hardening (ongoing)

Fuzz target. Performance benchmarks. Documentation. Migration notes for the next Spark version when the time comes.

## 13. Open Questions

**Spark version pinning policy.** We pin to 4.0.0. When Spark 4.1 ships, do we (a) maintain parallel canonicalizers per Spark version, (b) bump and re-run the property suite, or (c) parameterize transformations on a `SparkVersion` enum? Recommended default: (b), with a `SparkVersion` field in the config so the answer can shift later without API breakage.

**Hint preservation for performance testing.** Layer 1 strips hints. If a user later wants to test hint propagation, hint-stripping needs to be a config flag. Default off is correct for now; revisit when use cases appear.

**Streaming plans.** Plans containing `WithWatermark` or `ApplyInPandasWithState` currently error out. The architect should confirm with the product owner whether this is the desired behavior for v1 or whether streaming plans should pass through Layer 1 unchanged.

**Performance budget.** Target is 1 ms per plan of up to a few hundred nodes. The architect should establish a benchmark suite early (Phase 1) so regressions are visible.

**Interaction with existing optimizer in thunderduck-rs.** If the existing compiler has its own simplification logic that overlaps with Layer 1 or Layer 2, the architect should audit and decide whether to remove the redundant logic from the compiler or leave both as defense-in-depth. Removing is cleaner; defense-in-depth is safer during the transition.

---

*End of design document.*
