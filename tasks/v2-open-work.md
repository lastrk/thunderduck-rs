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

All seven E-series passes have landed (E1 `cf8ea20`, E1.5 `a4b3c6e`, E4
`e56072a`, E2 `5dfc097`, E3 `08a3094`); this file is the complete open set.

---

## 1. Architectural (the two big levers)

### O1. Complete ADR-024 tier-3e — DONE (Pass F3, `f8e4b26`, 2026-07-13)
Lineage is authoritative for EVERY operator; source_quals_tracked, its
derivation (~90 lines), the PartialEq carve-out, the four placeholder sites,
and the tier-(f) name-only fallback are DELETED. Probe-driven: SetOp needed
NO content edit (Spark resolves first-child qualifiers over unions — τ was
already faithful, both behaviors pinned); WithColumnsRenamed clears lineage
on renamed slots (Spark severs pre-rename addressability — probe-verified).
The join-condition scope's regime change is the F8-class fix. ADR-024
amendment appended. Opus review: APPROVED (reviewer traced every formerly-
fallback class end-state clean). NEW grab-bag NIT: unionByName with
allowMissingColumns — a name present only in a non-first child donates that
child's quals (b.z would resolve; Spark pads first-child null alias and
rejects) — narrow, strictly less permissive than the old fallback, optional
future witness.

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

### O3. Single-level struct access via ExtractValue — DONE (Pass F2, `802b085`, 2026-07-13)
All struct access is now an ExtractValue chain rooted at the struct COLUMN's
real expr_id (inner tier-(d) AND the correlated outer twin; live-Spark probe
verified the correlated shape, witness sq-024 green). try_rewrite_nested_
struct_path deleted (fixed the latent multi-level+plan_id emission bug).
NO production site returns a resolved ColumnReference with expr_id: None —
the identity story is complete; E4's equality-widening residual CLOSED.
Naming handled by N8's ensure_named (witness struct-009). Opus review: zero
findings. NEW grab-bag item from the review: StructType::field_by_name (inner
struct-FIELD lookup, types/struct_type.rs ~53) still folds ASCII-only vs the
E3 eq_fold authority — narrow non-ASCII field-name divergence, pre-existing.

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

### O6. Promoted hidden sort column name not uniquified — VERIFIED DEAD (closed-by-N10, 2026-07-13)
The hypothesized silently-wrong-pick (`SELECT y AS x FROM t ORDER BY t.x` →
projections `[y AS x, x]`, schema `[x, x]`) is neutralized: post-N10 emission
binds duplicate names by expr_id and the trim-Project re-binds by id. Witness
`ord-014` (SQL corpus: `SELECT salary AS id FROM emp ORDER BY emp.id`, unique
hidden sort key for deterministic order) added and GREEN differentially — it
now pins the shape permanently. No fix needed; `unique_hidden_output_name`
stays aggregate-only.

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

### O9. Sort-resolver shrink (R2-2) — DONE (Pass F1, `62e122e`, 2026-07-13)
Architect re-measurement found the E-series had already collapsed the bulk
(~345 code lines, not ~600). F1 landed the remaining genuine dedup: the
redundant increment-1 whole-key match deleted; `contains_matching_call`,
`rebind_over_child`/`SortChild`, and `promote_subtree` (mint-vs-copy kept
explicit in two helpers) merged the three near-duplicate pairs; docs pruned.
345 → 310 code lines, file net −65; assertion-identical (Opus review: zero
findings). 07-11 f12's walks absorbed.

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
