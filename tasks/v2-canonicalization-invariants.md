# τ canonicalization invariants — refactor-driving catalog

**Created:** 2026-07-12 · **Baseline:** `feat/v2-transpiler` @ `992fca0` · Sources:
[review findings](v2-review-findings-2026-07-11.md), [attr-identity task + abort report](v2-attribute-identity-unification.md),
[feasibility dossier](attr-identity-feasibility-dossier.md).

## The concept

A **canonicalization invariant** is a property established once, at a single choke point
(front-end construction or the resolve pass), that every downstream consumer may then
*assume* instead of check, re-derive, or guess. Each missing invariant below has three
observable costs in today's τ: (1) **compensating code** (branches/helpers that re-derive
the fact), (2) a **latent-bug habitat** (the gap between "usually holds" and "always
holds" — E1/E2, the trunc cast, the `__td_wcr` workaround all lived there), and (3)
**hand-maintained lockstep** between the sites that re-derive it. This is Catalyst's core
design discipline (every SELECT entry a `NamedExpression`, every Aggregate folded, ids in
immutable nodes) and CV **INV2** ("push the fact into the node") applied systematically.

**Precedent that this works, from this very branch:** `55577ef` (Pass 33) established
"Aggregate layout is a stored front-end constant" (`AggregateProjection`) and thereby
deleted `grouping_already_folded` (~64 lines, 6 lockstep sites) *and*
`unfold_ungrouped_aggregate_subquery` (~36 lines) — and fixed the E1/E2 bug class at the
root. Every entry below is the same move on a different fact.

## How to use this file

- Each entry: **Invariant → Today → Deletes → Bug classes closed → How → Cost/gates → Depends on.**
- Entries are independently landable unless *Depends on* says otherwise. Tier A items are
  small and lib-test-heavy; Tiers B/C change resolution/emission behavior and are
  **corpus-gated** (CLAUDE.md hard gate: no previously-green case regresses).
- ⚠️ An active incremental stream commits to this branch continuously (Pass 33/34 landed
  mid-review). Before starting any Tier B/C item, check `git log` for overlap and
  coordinate — the aggregate/emission paths are its hot surface.
- Anchors are function names first, line numbers second (they drift fast here).

## Dependency graph & suggested sequence

```
Tier A (independent, small):      N1  N2  N3  N4  N6
Tier B (analyzer normalization):  N5 ──after── N8;   N7 (fold) ──then── N8 (alias-every-entry)
Tier C (identity, large):         N9 (Attribute schema) ──then── N10 (bind-by-id emission)
                                  N10 also needs N8.
```

Suggested order: N1 → N2 → N3 → N4 → N6 → N7 → N8 → N5 → N9 → N10.

---

## Tier A — small, independent, mostly lib-test-gated

### N1. Single opacity authority on `Expression`

- **Status: ✅ LANDED** (Pass A1, `ca23e80`): `is_opaque_unit` + `is_resolve_opaque`; 4/4 sites unified; Opus review clean.
- **Invariant:** "is this variant an opaque/atomic unit (no recursion into it)?" is answered
  by exactly one method.
- **Today:** the `Lambda | LambdaVariable | RawSql | Interval [| subqueries | Window]` roster
  is hand-copied at **four** sites: `substitute_lateral_aliases` (analyzer.rs:~3396),
  `resolve_and_stamp` (~3541), `opaque_to_subtree_promotion` (~3900, its doc admits it
  "mirrors resolve_and_stamp's own opacity list"), and a fourth match ~4934.
- **Deletes:** three of the four rosters.
- **Bug classes closed:** drift — a new `Expression` variant classified opaque in one walker
  but recursed-into by another silently rewrites a tree the resolver treats as atomic.
- **How:** `impl Expression { fn is_opaque_unit(&self) -> bool }` (+ a `promotion_opaque()`
  wrapper adding `Window`); all four sites call it.
- **Cost/gates:** ~30 lines net-negative; lib tests.
- **Depends on:** nothing.

### N2. One type-inference home receiving full argument types

- **Status: ✅ LANDED** (Pass A2) for `map_from_arrays`; scope-corrected during implementation. Discovery during implementation: the invariant's reach is **type-only
  rules**. Rules needing per-argument *expression-level* facts — `map`/`create_map` and the
  `array` family need `f.args[i].nullable(schema)`; `struct`/`to_number`/`from_json` need
  literal values/shapes — cannot move while the resolver receives only `&[DataType]`
  (`DataType` carries no scalar nullability). Those remain a *documented, legitimate* second
  home, not drift. Widening the resolver signature to `(DataType, nullable)` pairs is the
  future completion; `json_tuple_field` (arity-only) is the one cheap remaining move.
- **Invariant (as achievable today):** every function-return-type rule *derivable from arg
  DataTypes alone* lives in `function_return_type`, which sees all of them; exceptions carry
  a comment naming the expression-level fact that pins them in `function_call_data_type`.
- **Today:** the resolver is first-arg-centric, so multi-arg rules are re-derived in a second
  home (`Expression::function_call_data_type`): `map_from_arrays` has a fast-path there that
  only works by *shadowing* the resolver's wrong `Map<String,String,true>` default;
  `date_trunc` hardcodes `Timestamp` because it "can't see arg[1]".
- **Deletes:** the expression.rs fast-paths for `map`/`create_map`/`map_from_arrays`; the
  hard-coded map default shrinks to a true malformed-call fallback.
- **Bug classes closed:** split-brain typing — a rule fixed in one home and not the other
  (the `date_trunc`-defaults-to-Timestamp latency is this class).
- **How:** move the `[Array, Array]` match (already the pattern used by `array_intersect`,
  type_inference.rs:~768) into the resolver's map arm; audit other `function_call_data_type`
  arms for the same migration.
- **Cost/gates:** ~60 lines moved; lib tests + corpus for the touched functions.
- **Depends on:** nothing.

### N3. Type-driven return coercion at ONE choke point

- **Status: ✅ LANDED** (Pass A3, `fb17e8b`), with the architect's sharper design: wrapper at `render_function_call`'s single exit + `needs_date_return_cast` roster + `DATE_RETURNING_FNS` const + a DuckDB-executing audit test (the forgotten-cast class is now mechanically checkable). trunc bug fixed; `last_day` verified cast-free; full corpora green.
- **Invariant:** "the emitted SQL's type must equal the node's inferred type" is enforced
  once, where the type is read off the node — not remembered per function arm.
- **Today:** `spark_return_cast` (emission.rs:~6330) IS the choke point but handles only 3
  ad-hoc cases; the DATE coercion (DuckDB promotes `DATE ± INTERVAL` → TIMESTAMP) is
  re-hardcoded at ~7 sites (`add_months`, `date_add`, `date_sub`, `to_date`,
  `render_binary`, DATE literal, `next_day` macro) — and **`trunc` was forgotten**: typed
  Date (type_inference.rs:~660) but emits bare `date_trunc(fmt, d)` → ships TIMESTAMP
  (verified on the DuckDB binary).
- **Deletes:** the 7 per-arm `CAST(... AS DATE)`s.
- **Bug classes closed:** the forgotten-cast class (trunc today; every future date function
  tomorrow). Fixes the live trunc type bug as a side effect.
- **How:** generalize `spark_return_cast`: when `expr.data_type(schema)` is Date (extend to
  other divergences as found) and the rendered DuckDB expression would not produce it, wrap
  in a cast. *Caveat:* it fires at projection top level; a nested `date_add` inside another
  expression still needs N4's tree normalization (or typed nodes) for full coverage —
  state this in the change, don't overclaim.
- **Cost/gates:** ~40 lines net-negative + test-string churn; corpus (date cluster).
- **Depends on:** nothing (N4 completes it).

### N4. Implicit coercions materialized as explicit `Cast` nodes at analysis

- **Status: ✅ LANDED** (Pass A4, `800d18c`): `materialize_binary_coercions` in `resolve_and_stamp`'s Binary arm; `implicit: bool` on CastExpression (name- and semantic_eq-transparent); `decimalize` private again; render_binary's two re-derivations deleted; R1-6 deferred onto the `date_like_interval_result` seam (`SEAM(R1-6)`); full corpora green.
- **Invariant:** if Spark's semantics insert a coercion (decimal widening, Date±Interval →
  the date-typed side), the analyzed tree *contains the `Cast` node*; emission renders what
  it sees and never re-derives a coercion.
- **Today:** `render_binary` re-runs the analyzer's decimal widening (`Expression::decimalize`
  made `pub(crate)` solely so emission can reach it — the comment admits the two sites are
  kept "in lockstep" by hand) and re-pattern-matches Date±Interval operand types, duplicating
  `binary_data_type` (expression.rs:~889).
- **Deletes:** `render_binary`'s decimalize branch + `is_date_plus_interval` re-match; the
  `pub(crate)` leak.
- **Bug classes closed:** analyzer/emission coercion drift (two implementations of one
  rule); also creates the natural seam to fix the **Date + DayTimeInterval → should be
  TIMESTAMP** parity gap (findings R1-6) in exactly one place.
- **How:** Spark precedent is `ImplicitCast`/`TimeAdd` — a small analyzer post-resolve step
  (or inside the Binary resolution arm) rewrites `Binary(l, op, r)` into
  `Cast(Binary(cast(l), op, cast(r)))` per the inference rules already in
  `binary_data_type`.
- **Cost/gates:** ~100 lines moved analyzer-ward; corpus (decimal + date clusters).
- **Depends on:** nothing; pairs with N3.

### N6. `FunctionCall` carries its resolved classification (`FunctionKind`)

- **Status/scope correction (2026-07-12, N6-lite):** the `kind: FunctionKind` field design was
  evaluated and **deferred into N9**: `kind` is a pure function of `name`, so stamping it
  duplicates a derivable fact and creates a staleness class; the lambda-body opacity hole (N1)
  forces a roster fallback regardless, for a net **-3/+80 lines**. Landed instead as
  **N6-lite**: roster concentration only — `NONDETERMINISTIC_FN_NAMES` moved next to
  `AGG_SPECS` behind `is_nondeterministic_fn_name`; `contains_nondeterministic_call` calls it.
- **Invariant:** "is this call an aggregate / window / scalar / nondeterministic?" is stamped
  once at resolution, read thereafter.
- **Today:** `contains_aggregate_call` re-scans the `AGG_SPECS` roster at every node of every
  sort key; `promote_aggregate_subtree` scans again per subtree;
  `contains_nondeterministic_call` walks against a second hand-synced roster
  (`NONDETERMINISTIC_FN_NAMES` — self-admittedly non-exhaustive).
- **Deletes:** the per-node roster scans (the rosters remain as the *stamping* source).
- **Bug classes closed:** classification drift between walk sites; the non-exhaustive-roster
  hazard concentrates at one stamping point where it can be audited.
- **How:** `kind: FunctionKind` field on `FunctionCall`, defaulted `Unclassified` at
  construction, stamped in `resolve_and_stamp`'s FunctionCall pass; walkers read the field.
- **Cost/gates:** field-add churn on `FunctionCall {` literals; lib tests.
- **Depends on:** nothing. (Synergy with N9: per-instance ids on nondeterministic calls
  later delete the roster outright.)

---

## Tier B — analyzer/front-end normalization (corpus-gated)

### N7. Every Aggregate is folded at construction

- **Status: ✅ LANDED** (Pass B1): `ast::grouped_aggregate` (Spark `RelationalGroupedDataset.toDF`
  verbatim — empirically verified: `groupBy(k).agg(k,…)` yields k TWICE); `AggregateProjection`
  retired (user-approved); offset arithmetic + `AggregateRebindCtx` + keys-chain deleted;
  `bind_aggregate_slot`/`bind_project_slot` merged into `bind_slot`. New corpus witness
  `agg-026` (restated grouping key). Side effect: the review-found ORDER-BY-grouping-expression
  binder error is FIXED (the sort key now whole-matches the folded entry) — its differential
  test de-xfailed. Opus review clean; full corpora green (DataFrame 403/0, SQL 404/0).
  *Traceability note (review MINOR, informational):* when the sort fallback alias-pins a folded
  passthrough grouping column, its `source_quals` lineage collapses to ∅ (same as the Project
  and SQL-Folded paths always did; barely reachable — bare columns resolve at step 1; failure
  mode is the conservative wrap, not a wrong result). Root fix is N8/N9 lineage-through-alias.
- **Invariant:** `aggregates` IS the complete output list for every Aggregate, from both
  front-ends — the DataFrame converter constructs `grouping ++ aggregates` exactly like
  Spark's `RelationalGroupedDataset.toDF`. The layout fact stops existing.
- **Today:** `55577ef` made the layout an explicit stored flag (`AggregateProjection`) — the
  right fix at the flag level. Folding at construction removes even the flag: the
  Grouped-prepend arm in schema construction, the `offset = match projection` arithmetic in
  `rebind_over_aggregate`, the `offset + aggregates.len() != schema.len()` guard, the
  `keys`-chain in `build_aggregate`, and the `rebind_over_aggregate`/`rebind_over_project`
  duality (they merge).
- **Bug classes closed:** every future consumer that would have had to consult the flag; the
  documented `.groupBy(k1,k2).agg(k1, f)` residual edge disappears definitionally.
- **How:** in `v2_relation_converter::convert_aggregate` (+ `convert_cov`/`convert_corr`/
  `convert_approx_quantile`, `crosstab_to_aggregate`), clone grouping exprs to the front of
  `aggregates`; keep the `grouping` list for GROUP BY rendering. Delete `AggregateProjection`
  and its match arms.
- **Cost/gates:** moderate; corpus (aggregate + TPC clusters). ⚠️ Retires a flag the active
  stream added days ago — **coordinate first**.
- **Depends on:** nothing technically; do before N8.

### N8. Every output-list entry is a NamedExpression (alias-every-entry)

- **Status: ✅ LANDED** (Pass B2): `ensure_named` at exactly 3 op arms (Project, Aggregate
  aggregates, Pivot grouping); `bind_slot` now READ-ONLY (the last SELECT-list
  mutation-under-iteration site is gone; N7's lineage MINOR partially resolved — bare
  entries keep their lineage). Name-neutral by construction (alias minted from the same
  `expression_output_name` the schema uses): only ONE emission test churned (insertion-only).
  New witness `ord-013` (qualified passthrough orderBy over same-named join column) guards
  the bare-ref no-pin decision. Deferred: `__td_wcr` by-name revert (duplicate-name children).
  Opus review clean (1 MINOR test-name staleness, fixed); full corpora green (DataFrame 404/0,
  SQL 404/0).
- **Invariant:** after resolution, every entry of a Project/Aggregate/WithColumns output list
  is either a bare `ColumnReference`/`Star` or an `Alias` whose name equals the schema
  field's name (Spark's `UnresolvedAlias` → `Alias` invariant).
- **Today:** entries may be unaliased computed expressions; the schema name
  (`expression_output_name`) exists only on the schema, so: `bind_aggregate_slot`/
  `bind_project_slot` retrofit aliases by *mutating the child's SELECT list mid-iteration*;
  DuckDB's auto-name for an unaliased expression diverges from τ's tracked name, forcing the
  positional `__td_wcr(...)` workaround (Pass 31) and leaving a latent binder error when the
  trim Project references a pretty name across a derived-table wrap (Review 1, cross-file
  finding).
- **Deletes:** the alias-pinning mutation branch in `bind_*_slot`; the `__td_wcr` positional
  workaround (revertible to by-name, duplicates aside); repeated `expression_output_name`
  consumption-site calls (name read off the tree).
- **Bug classes closed:** (1) mutation-under-iteration on SELECT lists (E2's habitat);
  (2) the whole tracked-name ≠ emitted-name class — **`tracked == emitted` becomes a global
  invariant**, making by-name references across ANY wrap boundary safe by construction;
  (3) R2-6 (stamped output name) satisfied structurally, with no new node field.
- **How:** one `ensure_named(expr)` applied after `resolve_and_stamp` of each output-list
  entry (Project / folded-Aggregate / WithColumns arms): wrap non-`Alias`, non-`ColumnReference`,
  non-`Star` entries as `Alias(expr, expression_output_name(&expr))`. Scope rules: only
  output lists (never the GROUP BY `grouping` list); bare refs stay bare (Spark parity).
  Wire schema unaffected (`arrow_schema_stamp` stamps names from `resolved_schema` — verify
  once before relying on it).
- **Cost/gates:** ~40–80 core lines + wide mechanical test-string churn (emitted SQL gains
  `AS "sum(salary)"`); corpus.
- **Depends on:** best after N7 (folded lists make "output list" the single wrap target).

### N5. Canonical substrate function names at conversion

- **Status: LANDED** (2026-07-12, feat/v2-transpiler). `FunctionCall.name` is now
  ASCII-lowercased once at construction, at the two front-end entry points
  (`v2_relation_converter::convert_unresolved_function`,
  `v2_lowering::lower_function`/`table_function_node`) — never re-derived downstream.
  `resolve_and_stamp` carries a `debug_assert!` on its `FunctionCall` arm enforcing the
  invariant mechanically at the one choke point every resolved call passes through.
- **Invariant:** `FunctionCall.name` is the canonical, lowercase substrate identity; Spark's
  as-written spelling lives only in the output name (N8's alias/pretty-name), plus a small
  `SPARK_UPPER_PRETTY` roster (`analyzer.rs`) of 30 `UnaryMathExpression`/
  `BinaryMathExpression` PRIMARY names (`ceil`, `floor`, `sqrt`, `radians`, …) whose Spark
  `toPrettySQL` auto-name is UPPERCASE regardless of written case — verified against a
  vendored Spark 4.1.1 session, 2026-07-12. Alias spellings (`ceiling`, `pow`, `sign`,
  `ucase`, …) are NOT in the roster and keep their own lowercase pretty spelling.
- **Site-count correction:** the ~92-site estimate mixed function-NAME sites with
  column/alias/field-name case-insensitive sites. Actual split: only ~35 were genuine
  `FunctionCall.name` sites; ~130 additional `eq_ignore_ascii_case`/`to_lowercase` sites
  found during the audit are COLUMN/alias/struct-field-name case-insensitivity (Spark
  resolver semantics) and are explicitly out of scope for this invariant — left untouched.
- **What stayed (verified by content, not a violation):** a handful of `type_inference.rs`
  helpers (`aggregate_return_type`, `aggregate_is_non_nullable`/`aggregate_is_always_nullable`
  test wrappers, `function_return_type`, `is_aggregate_classifier_name`,
  `is_nondeterministic_fn_name`) keep a defensive `to_lowercase()`/`eq_ignore_ascii_case`:
  each has either (a) a caller outside the canonical `FunctionCall` substrate (e.g.
  `is_aggregate_classifier_name` is also called from `v2_lowering::function_call_has_aggregate`
  over the raw pre-lowering `sqlparser` AST, genuinely non-canonical), or (b) a dedicated unit
  test exercising mixed-case input directly (`count_if_case_insensitive`, `frt("Window", ...)`,
  `is_nondeterministic_fn_name("RAND")`). Same rationale as `function_catalog.rs:273`'s
  defensive boundary. The `substr`→`substring` DuckDB emission remap also stays — it is
  FUNC_ALIAS rendering parity (two distinct Spark function names that both lower to one
  DuckDB builtin), not a naming collapse; N5 does not touch it.
- **Deletes:** the mechanical case-insensitive compares/re-lowercasing in `analyzer.rs`,
  `emission.rs`, `expression.rs`, `type_inference.rs` (window family), `multi_alias.rs` whose
  sole input is already a canonical `FunctionCall.name`; the `canonicalize_for_semantic_eq`
  `FunctionCall` arm (now redundant — the default clone covers it).
- **Bug classes closed:** a case-variant or alias spelling slipping past one arm's compare
  but not another's.
- **How:** lowercase (and alias-normalize where safe) once in the converter/lowering; N8's
  alias preserves the user-visible spelling for naming; `SPARK_UPPER_PRETTY` covers the one
  Catalyst family whose auto-name diverges from the lowercased registry key.
- **Cost/gates:** mechanical but wide; lib tests + corpus spot-checks — landed with 0
  regressions (1178/1178 `thunderduck-core` lib tests, 132/132 `thunderduck-connect-server`
  tests, new witnesses `agg-025`/`fn-021`/`fn-022` (SQL corpus) and `math-017` (DataFrame
  corpus) green).
- **Depends on:** **N8** (until the output name is stamped, `name` must keep the user's
  spelling — this is exactly the Pass 28 lesson). N8 landed first, so N5 could canonicalize
  `name` immediately without a compensating remap for aliased output.

---

## Tier C — identity (large, corpus-gated, Step-2-first per the abort report)

### N9. Attribute identity stored in the schema (`ResolvedSchema(Vec<Attribute>)`)

- **Invariant:** every output column is an `Attribute { name, data_type, nullable, expr_id,
  source_quals }` stored in `TypedAst.resolved_schema`; identity/lineage/type ride through
  re-stamps *by value*.
- **Today / Deletes / Hazards:** see the [attr-identity task](v2-attribute-identity-unification.md)
  — deletes `ordinal`-as-identity (`ordinals_compatible`), the `source_quals(_tracked)`
  parallel vectors + mirror fn + length invariant + unconditional `analyze_sort` re-stamp,
  and (with per-instance ids) the nondeterminism roster.
- **Non-negotiables from the abort:** ids must be **stored state**, never derived in
  `RelScope` (re-mint-on-re-stamp landmine, dossier Claim A); execute Step-2-first; corpus
  oracle required per increment; does NOT deliver R2-4 (computed-node types need a typed
  expression tree — separate).
- **Cost/gates:** 483 `resolved_schema` touches across a 33.7k-line module tree; the
  largest item here by an order of magnitude.
- **Depends on:** sequence after the active stream quiets; N7/N8 first shrink the sort
  machinery it must migrate.

### N10. Unique emitted aliases / bind-by-id emission

- **Invariant:** every emitted output column carries a unique (id-derived) physical alias;
  references bind by id → alias, so duplicate names cannot be ambiguous at the SQL level.
- **Today:** emission re-derives uniqueness positionally at 4 wrap sites
  (`output_uniquified`, `bare_dup_ordinal`, `requalify_column_ref`, `wrap_reprojected`)
  guarded by load-bearing asserts.
- **Deletes:** all of the positional duplicate-name machinery (~90+ lines + asserts).
- **Bug classes closed:** wrong-column binding at wrap/merge boundaries under duplicate
  names (the self-join class).
- **How:** thread an id→alias map through the `SelectBlock` builders; Spark-verbatim names
  remain on the wire via the schema stamp.
- **Depends on:** N8 (aliasing plumbing) + N9 (the ids).

---

## What is deliberately NOT here

- **Typed expression tree** (types on every `Expression` node — the real R2-4 fix): a
  larger, separate decision; N3+N4 capture the cheap 80%.
- **ADR work:** N9/N10 require the ADR-024/ADR-023-supersession package described in the
  attr-identity task §5; Tier A/B items are INV2-style code changes needing no ADR.
- **Boundary-error fixes** (`to_char` numeric, `json_object_keys`, `bit_get` bounds — review
  findings 4/5/8): real, but they are per-function parity fixes, not invariants.

## Scoreboard (what each invariant deletes / closes)

| # | Invariant | Compensating code deleted | Latent-bug class closed |
|---|---|---|---|
| N1 | one opacity authority | 3 of 4 hand-synced rosters | walker/resolver opacity drift |
| N2 | full-arg type home | expression.rs map fast-paths | split-brain typing (date_trunc class) |
| N3 | choke-point return coercion | 7 scattered DATE casts | forgotten-cast (live: trunc) |
| N4 | explicit Cast nodes | render_binary decimalize+date re-derivation | analyzer/emission coercion drift |
| N6 | stamped FunctionKind | per-node roster scans | classification drift |
| N7 | Aggregate folded at construction | AggregateProjection + offset/duality | layout-consultation bugs (E1/E2 family, definitionally) |
| N8 | alias-every-entry | bind_*_slot mutation, __td_wcr, name re-derivations | mutation-under-iteration; tracked≠emitted |
| N5 | canonical substrate names | ~92 case-compares, substr remap | case/alias spelling slip-through |
| N9 | stored attribute identity | ordinals_compatible, source_quals machinery | wrong-column identity (t1.x/t2.x) |
| N10 | unique emitted aliases | positional dup-name machinery | wrap-boundary wrong-column |
