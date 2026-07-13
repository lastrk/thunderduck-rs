# τ open work — consolidated backlog (2026-07-13)

The single live successor to the four retired review/task files (now under
`tasks/archive/`): `v2-attribute-identity-unification.md`,
`v2-canonicalization-invariants.md`, `v2-review-findings-2026-07-11.md`,
`v2-review-findings-2026-07-13.md`. Everything those files contained that is
DONE is recorded in their archived copies and in the commit history
(`992fca0..HEAD`: the N1–N10 canonicalization series, D1–D4 follow-ups, and
E1/E1.5/E4/E2/E3 review-fix passes). This file holds only what is NOT done and
judged worth doing. `tasks/v2-corpus-followups.md` remains the live per-pass
corpus ledger; its open rows are mirrored here (§4) so this file is complete.

In flight at time of writing: Pass E3 (case-folding unification via
`canon_char`/`eq_fold`/`fold_key`, INV literal-ban check, stale comments,
hygiene) — not listed below; it lands imminently.

---

## 1. Architectural (the two big levers)

### O1. Complete ADR-024 tier-3e — retire the name-only fallback regime
**Size: L. The one remaining architectural gap in the identity story.**
17 op kinds (`SetOp, LateralView, WithColumns, WithColumnsRenamed, DropColumns,
SingleRow, Values, LocalRelation, FileScan, TableFunction, Unnest, Describe,
Summary, FreqItems, Unpivot, Pivot, RecursiveCte`) plus Star-Projects and
length-mismatched Project/Aggregate leave `source_quals_tracked = false`, so
tier-(f) qualified resolution routes through the legacy permissive name-only
fallback (`analyzer.rs` ~4801 — "retired in 3e" per its own comment).
Verified facts (2026-07-13 verifier): SetOp lineage is ALREADY seeded
(first-child attribute clones carry id+quals — `widen_by_position`/
`widen_by_name`) and Star-Project clones fields verbatim, so those are
audit-plus-gate-flips; genuine per-op lineage work remains for the
WithColumns family / LateralView / Pivot / Unpivot. Completion deletes the
flag (4 placeholder sites — one non-trivial synthetic-scope site at ~4321),
`source_quals_tracked_of` (~55 lines), the growth-invariant assert, the
fallback branch, and a PartialEq carve-out on RelScope. ADR-024 anticipates
this (lines ~631/679) but does not mandate it as a discrete step.

### O2. Typed expression tree (R2-4) — stamped `(DataType, nullable)` on computed nodes
**Size: XL, needs an ADR. DEFERRED BY EXPLICIT USER DECISION (2026-07-13) —
listed for completeness, not for scheduling.**
Binary/FunctionCall/Unary/CaseWhen/Window discard their inferred type; ~218
call sites re-run `data_type/nullable(schema)` subtree walks. Stamping once
makes emission provably unable to drift from analysis. Also the only real fix
for O7 (lambda-body coercion gap): lambda variables type as `Unresolved`, so
no coercion rule — old or new — can fire inside HOF bodies.

---

## 2. Correctness / parity (medium)

### O3. Single-level struct access via ExtractValue (finding 12)
**Size: M. Now carries THREE motivations.** Multi-level `a.b.c` already lowers
to ExtractValue chains; single-level tier-(d) (and its `resolve_in_outer`
twin) emits an id-less qualified ColumnReference rendered as raw `q.name`.
Unifying (a) copies the struct COLUMN's real id (Spark GetStructField-child
model), closing the last semantic "resolved-but-id-less" state; (b) fixes the
E4-documented equality widening (two same-named struct fields of DIFFERENT
types through different structs can alias in a rebind scan — see the loud
warning on `ColumnReference::eq`); (c) unifies two hand-kept-consistent code
paths. Verified DuckDB-data-equivalent (`(addr).city`); the migration risk is
OUTPUT NAMING — `render_projection_slot` emits bare exprs without `AS`, so the
projection needs an explicit alias to keep Spark's `city` column header.

### O4. Interval field-span on the type: `DayTimeInterval { start, end }` (finding 15)
**Size: L, needs an ADR (wire value-type Eq/Hash change, ~42 sites/10 files).**
The Spark proto ALREADY carries start/end fields; τ's converter discards them.
Landing the span deletes the D3 literal re-kind workaround (`lower_interval`'s
Slot::Days-stays-Calendar), the "DayTimeInterval ⟹ sub-day span by
construction" comment argument at `date_like_interval_result`, and the
documented day-only wire-column over-promotion (Date + DayTimeIntervalType
(DAY) column → τ Timestamp vs Spark DATE).

### O5. Boundary-error trio (ADR-022 per-function parity; witness-first)
**Size: S each, independent.**
- `to_char(numeric, picture)` → raw DuckDB binder error (`strftime(DECIMAL,…)`);
  should be an honest `Unsupported*` boundary error or implement the numeric
  picture. Repro: `SELECT to_char(78.12,'99.99')`.
- `json_object_keys` on invalid/non-object JSON: DuckDB `json_keys` RAISES on
  invalid input and returns `[]` on non-object; Spark returns NULL for both.
  Repro: `SELECT json_object_keys('not json')`, `('[1,2,3]')`.
- `bit_get`/`getbit`: no pos-bounds check; Spark raises
  `INVALID_PARAMETER_VALUE` for `pos<0` or `pos>=bit-width`. Repro:
  `bit_get(1L, 64)`.

### O6. VERIFY-FIRST: promoted hidden sort column name not uniquified (07-11 f7)
**Size: S (verification), possibly already dead.** `SELECT y AS x FROM t ORDER
BY t.x` (t has x and y) → projections `[y AS x, x]`, schema `[x, x]` — was a
silently-wrong-pick risk pre-N10. Post-N10/E1 emission binds duplicate names
by expr_id and the trim-Project re-binds by id, so the hazard may be
neutralized. Nobody has re-verified. Write the witness; if green, record as
closed-by-N10; if red, fix via `unique_hidden_output_name` (the aggregate path
already does this).

### O7. Lambda-body coercion gap (finding 4)
**Size: blocked on O2.** `transform(date_arr, d -> d + INTERVAL '1' MONTH)` →
DuckDB TIMESTAMP[] vs Spark array<date>. Pre-existing (NOT an N4 regression —
verified), zero corpus coverage, root cause is untyped lambda variables.
Documented at the seam; revisit with O2.

### O8. Trim-Project buries the top-level ORDER BY (07-11 f9)
**Size: S–M, fragility not a current break.** The Sort arm emits
`SELECT <trim> FROM (… ORDER BY … [LIMIT n]) sub` with no outer ORDER BY —
row order relies on DuckDB preserving a derived table's ORDER BY, which SQL
does not guarantee (empirically preserved today; q078/q098 green). Fix: hoist
sort keys onto the trim Project or re-emit a top-level ORDER BY.

---

## 3. Simplification / efficiency (now-unblocked)

### O9. Sort-resolver shrink (R2-2) — the newly unblocked one
**Size: M–L.** The 2026-07-11 analysis established the ~600-line ORDER-BY
resolver is the right ALGORITHM (Spark runs the same walk-match-promote-trim)
but ~2.4× overweight because τ lacked three ambient invariants. All three have
since landed: N7 (Aggregate folded at construction — no offset/duality), N8
(alias-every-entry — binding is `entry.toAttribute`-like), N9/E4 (stored ids
in by-value nodes — no re-stamp hazard, id-first semantic_eq). The predicted
~600 → ~250-line collapse has never been attempted or re-measured. Absorbs
07-11 f12's remaining near-duplicate walks (`promote_*_subtree` /
`rebind_over_*` pairs; `contains_aggregate_call`/`contains_nondeterministic_call`).

### O10. Array set-ops are O(n²) per row (07-11 f10)
**Size: S–M, efficiency only.** `order_preserving_distinct`/`array_union`
inline `list_concat(a,b)` twice and pay O(len) `list_position` per element.
Correct output; runtime cost on large arrays. Consider CTE/lateral
materialization.

### O11. N2 completion — widen the resolver signature
**Size: S–M.** `function_return_type` receives `&[DataType]`; rules needing
per-argument nullability (map/create_map, array family) or literal shapes
(struct/to_number/from_json) remain a documented second home in
`expression.rs`. Widening to `(DataType, nullable)` pairs moves the
nullability-dependent rules into the one home (R2-9's残 remainder).

---

## 4. Corpus-ledger open rows (mirrored from tasks/v2-corpus-followups.md)

- **F-explode-map** (red case): unaliased `explode(map)` must emit two
  default-named cols `key`,`value`; needs a schema-aware Project pre-pass
  generator expansion (mirror json_tuple/stack; Array=1col vs Map=2cols).
- **F-count-distinct-name / F-countstar-name / F-upper-fn-name** (naming
  latents): `count(DISTINCT x)` name, DataFrame `count(*)`→`count(1)`, SQL
  uppercase `SUM(x)`→`sum(x)` output-name parity.
- **F-nondistinct-multicount**: non-DISTINCT `count(a,b)` emits invalid DuckDB.
- **F-negative-emit**: `negative`/`negate` has a type arm but no emission arm.
- **F-unary-math-nullable**: rest of the UnaryMathExpression family missing
  from the always-nullable roster.
- **F-dead-macros** (hygiene): session.rs macros shadowed by emission rewrites.
- **F-decimal-sum-route / F-orderby-ordinal** (arch, no red witness).
- STALE rows to fix in the ledger (housekeeping, not work): F-orderby-computed-
  groupkey (fixed by N7, witness de-xfailed), F-decimal-div-dup-logic (the
  render_binary duplicate was DELETED by N4), F-sourcequals-restamp (the
  re-stamp was DELETED by N9).

---

## 5. Tiny (grab-bag)

- Document the `grouped_aggregate` weak-form contract ("aggregates IS the
  complete output list") at BOTH constructors (ast.rs + the legitimate direct
  SQL construction in v2_lowering.rs ~1664) — review-verified that unification
  is WRONG (SQL's fold differs); the shared contract deserves a comment pair.
- Optional `__td_` synthetic-name prefix reservation (hardening; all synthetic
  emission arms already guard gracefully — verified table 2026-07-13).
- Typed `NamedExpression` output-list element (makes N8 structural, unifies
  WithColumns' `Vec<(String, Expression)>` second naming representation) —
  ADR-003-touching, ADR-scale; listed for the next architecture window.
- max_by/min_by: `arg_max_null` retarget landed (E2); if the thdck extension
  ever ships native Spark-semantics aggregates, revisit.

## Explicitly rated NOT worth doing (do not resurrect)
- id→index HashMap for find-by-id scans (slower at real schema widths).
- Attribute BTreeSet clone reduction (relocation of deleted vectors, ~neutral).
- Micros-per-unit const consolidation (cross-module coupling > payoff).
- `__td_wcr` by-name revert (positional is the correct mechanism — WON'T-DO).
- FunctionKind field on FunctionCall (kind is a pure function of the name;
  roster concentration landed as N6-lite).
