# τ review findings — 2026-07-13 (post-invariant-series, max effort)

**Question reviewed:** with the N1–N10 + D1–D4 invariants landed, what new abstractions /
higher-level invariants / restructurings would reduce state space, complexity, LOC? What
duplication, structurally dead code, and stale comments remain?

**Scope:** `992fca0..HEAD` (18 commits) + the uncommitted D4 tree.
**Method:** 10 finder angles (up to 8 candidates each) → dedup (43 raw → 22 clusters) →
one verifier per cluster (13 verifiers, CONFIRMED/PLAUSIBLE/REFUTED with quoted evidence)
→ gap sweep. One finding (the json_tuple_field debug_assert downgrade) was found AND fixed
during the review itself (it was the D4 Opus review's MAJOR; runtime guards restored).

**Positive verification worth recording:** the cross-file tracer found the series'
migration "unusually complete" — grouped_aggregate at every DataFrame production site,
ensure_named at all 3 arms, every id-consumer handling the D2 present-but-miss path, zero
surviving `.ordinal`/AggregateProjection references; the removed-behavior auditor traced
every series deletion to a sound re-establishment.

---

## Findings (ranked; verdicts from independent verification)

### 1. F-todf-dupname is live (narrow, fail-loud): emitted SQL disagrees with the tracked schema — MEDIUM, CONFIRMED
`emission.rs:2752` (`build_with_columns_renamed`). `toDF("a","b")` / SQL `AS t(a,b)` over a
child with duplicate column names: `analyze_to_df` produces the CORRECT positional schema
`[a, b]`, but emission's by-name `rename_map` collapses the pairs `[("id","a"),("id","b")]`
last-wins to `id→b`, emitting `__td_wcr(b, b)` — an N8 tracked==emitted violation. Terminal
`collect()` is masked correct by `arrow_schema_stamp`'s positional rewrite; any downstream
by-name reference fails LOUD (`select("a")` → DuckDB "column a not found"). NOT the
analyzer arm (inert: dict keys unique; renaming all same-named occurrences is
Spark-conformant) and NOT `analyze_to_df`. Zero corpus coverage. Fix: index-keyed rename
through `TypedOp::WithColumnsRenamed` at the emission site. (Comment + ticket corrected to
these verified mechanics as part of this review.)

### 2. max_by/min_by return wrong data when the ordering-extreme row's value is NULL — KNOWN (xfail-pinned), stale comment — CONFIRMED
`emission.rs:5827`. `arg_max/arg_min` skip NULL-valued rows; Spark returns the value AT the
extreme row even when NULL. Silently wrong scalars, pinned by strict-xfail
`test_max_by_min_by_null_value_at_extreme`. The adjacent comment (5823-5826) "a name rename
is the whole fix — args pass through unchanged" is FALSE and must go; correct targets are
`arg_max_null`/`arg_min_null`.

### 3. ASCII vs Unicode case-folding split across the resolution substrate — LOW (latent-rare), CONFIRMED empirically
Identity/resolution folds ASCII-only (`field_by_name` schema.rs:194, resolve_column
ambiguity 4638, canonicalize 4108/4115) while adjacent rename/drop/USING/withColumns maps
fold Unicode (`to_lowercase`, analyzer.rs 1623/1650/1880/2253). Pinned contradiction: on a
schema with column `É`, `drop("é")` works but `select("é")` errors UNRESOLVED_COLUMN. Java
`equalsIgnoreCase` (Spark) folds MÜLLER/müller — the ASCII side is the Spark-wrong one.
(İ diverges from Spark on BOTH paths — not intra-τ.) Fix: one folding discipline; a
`name_multiplicity`/lookup helper (finding 9) would give the fix a single chokepoint.

### 4. Coercion materialization cannot reach lambda bodies — LOW, pre-existing, CONFIRMED
`transform(date_arr, d -> d + INTERVAL '1' MONTH)` → DuckDB `TIMESTAMP[]` vs Spark
`array<date>`. Lambda is resolve-opaque so `materialize_binary_coercions` never sees the
Binary; N4 also deleted emission's re-derivation — but verified NOT a regression (the old
guard needed resolved types a lambda body never had; lambda vars type as Unresolved). Root
cause is deeper: no lambda-variable type binding. Zero corpus coverage. Document as a known
gap; the real fix is typing lambda variables (relates to the deferred typed expression tree).

### 5. Debug-assert-only release invariants (systemic theme) — CONFIRMED; one site deserves promotion
Three sites replaced by-construction guarantees with checks that compile out of the release
builds the server/harness actually run: analyze_sort's deleted re-stamp (analyzer.rs:3575 —
proven sound today), N5 lowercase consumption (partial tripwire only; the real invariant is
front-end normalization), and — most fragile — the requalify side-split's left/right id-set
disjointness (`emission.rs:449-456`), where a future plan-cloning optimization would produce
silently wrong SQL with only a single-shape unit pin. Recommendation: promote the
disjointness check to always-on (it's O(fields) once per join condition) or add a property
test; the other two need only their comments to name the flip conditions (largely done).

### 6. Complete ADR-024 tier-3e: retire the name-only fallback regime — the biggest remaining architectural win, MOSTLY CONFIRMED
17 op kinds (SetOp, WithColumns*, Values, FileScan, Pivot, …) plus Star-Projects leave
`source_quals_tracked=false`, routing tier-(f) qualified refs through the legacy name-only
fallback (analyzer.rs:4801-14) — the silent-wrong-column regime the identity work exists to
kill. Verified correction to the finder's framing: SetOp and Star lineage content is
ALREADY seeded on the attributes (first-child id donation; verbatim clones) — those are
audit+gate-flips; real work remains for the WithColumns family/LateralView/Pivot/Unpivot.
Completing it deletes the flag (4 placeholder sites — one non-trivial at 4321),
`source_quals_tracked_of` (55 lines), the growth assert, the fallback branch, and a
PartialEq carve-out. ADR-024 anticipates this (line 631/679) but does not mandate it.

### 7. Narrow ColumnReference's data_type/nullable Options — executable NOW, CONFIRMED
All 10 production construction sites stamp `Some`; front-ends build zero ColumnReferences
(the "7 converter sites" claim was a type misattribution — those are UnresolvedColumn).
Only `untyped()` (test-only, one helper serving 105 test sites) and canonicalize's
transient form produce None. PartialEq exclusion of data_type/nullable is verified safe (no
Eq/Hash; semantic_eq canonicalizes first; no production `==`). Deletes the two
schema-lookup fallback arms in Expression::data_type/nullable, `stamp_column_reference`
(finding 8), the is_fully_resolved arm, and shrinks canonicalize — ~30+ production LOC and,
more importantly, the phantom "partially-stamped" state every reader must reason about.

### 8. stamp_column_reference is a production no-op — delete it, CONFIRMED with a CORRECTION (implementation-falsified detail)
Single caller (analyzer.rs:3383); every PRODUCTION reaching ref is already fully stamped.
**Correction (Pass E1, empirical):** the verifier's "no test feeds an untyped ref through
the arm" claim was FALSE — 7 emission tests (12 sites) build `ColumnReference::untyped`
Sort keys/projections and drive them through `analyze()`, where the fallback is
load-bearing; naive deletion panicked the debug_assert and poison-cascaded 133 failures.
The deletion is still right, but requires the fixture migration first — staged plan
recorded (architect, 2026-07-13): migrate the 7 firing + 94 incidental `untyped` fixtures
to `UnresolvedColumn`, prove the census with a temporary `unreachable!()` gate, THEN
delete (+ retire `untyped` itself, discharging finding 13b). Do NOT assert expr_id;
do NOT add id backfill. Moots the double-resolution pair-call.

### 9. One id-lookup home: `ResolvedSchema::field_by_id` — CONFIRMED, −25..35 LOC
Four hand-rolled find/position-by-expr_id + near-identical name-agreement debug_asserts
(output_attribute 4958, promote_project_subtree 3894, bare_dup_slot 1184,
requalify_column_ref 545). Bonus verified: requalify_column_ref's gate is byte-equivalent
to bare_dup_slot's and should just call it (also kills an already-dead branch at 568-571).
The COPY-or-MINT shape duplicated between output_attribute/promote is the same cluster; its
case-naming asymmetry is verified benign but the shared helper should pick one policy
deliberately. Inversion: the legacy name-lookup has an owning method; the canonical
identity lookup doesn't.

### 10. Merge field_index into column_info_in — the last hand-maintained lockstep, CONFIRMED, −15..22 LOC
Two order-identical walks (exact-name-first-then-dotted) independently maintained across
two files; every resolver tier walks the slice twice and re-bases sub-slice indices by hand
(`range.start + i`). field_index's own doc admits the mirror requirement. One lookup
returning `(dt, nullable, &Attribute)` (or whole-schema index) kills the double walk, the
misleadingly-named `ordinal` locals, and the per-site offset bookkeeping.

### 11. Stamp the two trivially-available expr_ids — CONFIRMED (documented deferred)
derive_implicit_grouping (analyzer.rs:5734) and try_rewrite_nested_struct_path's root
(4438) discard ids that are in scope, so SQL-PIVOT grouping outputs and nested-struct roots
mint fresh identity with empty lineage. Both sites already carry deferred-gap comments.
Small, safe, corpus-gated.

### 12. Unify single-level struct access onto ExtractValue — CONFIRMED feasible, naming is the risk
Multi-level `a.b.c` already lowers to ExtractValue chains; single-level tier-(d) (and its
outer twin) instead emit id-less qualified ColumnReferences rendered as raw `q.name`.
Unifying copies the struct column's real id and deletes the special case — but DuckDB
renders `(addr).city` as the column header, so the projection needs an explicit alias to
keep Spark output names (`render_projection_slot` currently emits bare exprs without AS).
Low/Medium refactor, not trivial. (tier-(d)'s id-less-ness itself is intentional per
ADR-024 — this unification is how to retire it, not a bug.)

### 13. Mechanically enforce the identity-substrate conventions — PLAUSIBLE/CONFIRMED, cheap
(a) The `Attribute{}`/`ResolvedSchema{}` literal ban is doc-only (all fields pub; honored
today, zero violations) — an invariants.rs INV source-scan fits the existing INV3/INV10
idiom exactly; field privatization verified NOT viable (non_exhaustive is cross-crate-only;
getter churn). (b) `ColumnReference::untyped` is pub but test-only — `#[cfg(test)]` verified
fully feasible (zero production/cross-crate callers).

### 14. Stale comments (4 sites) — CONFIRMED with quote-pairs
(a) expression.rs:947 "Interval ± Date/Timestamp → preserve the date-like side" —
contradicts the R1-6 promote arm added this series (blame-proven pre-dates it).
(b) analyzer.rs:4422-27 — claims type/nullable "left as None … re-run
stamp_column_reference"; code stamps eagerly and returns terminally; contradicts its own
next paragraph. (c) schema.rs:89-93 with_quals "leaf-only, never re-derive" — contradicted
by the USING union at analyzer.rs:2248 (correct behavior, unenumerated exception).
(d) emission.rs:5823-26 max_by "a name rename is the whole fix" — false, see finding 2.
Sweep for other stale deleted-symbol references: clean.

### 15. Interval field-span on the type (`DayTimeInterval{start,end}`) — PLAUSIBLE, needs ADR
The D3 fix works by re-kinding literals because τ's field-less type cannot express spans —
leaving the documented day-only wire-column over-promotion (accepted trade-off at
expression.rs:1745+) and the `Slot::Days`-stays-Calendar workaround. The Spark proto
ALREADY carries start/end fields, discarded at the converter. Field-span on the type
deletes both workarounds and the over-promotion; cost ~42 match sites across 10 files and
an Eq/Hash change on a wire value type — an ADR-scale decision, recorded here as the
deeper mechanism.

---

## Verified but cut from the top 15 (honorable mentions)
- semantic_eq loops recompute the loop-invariant side's canonicalization (5 sites,
  fallback-path-only); hoist is cheap and feasible — CONFIRMED Low/Low.
- `name_lower = f.name.clone()` shims post-N5 (self-documented churn-avoidance) — trivial.
- Micros-per-unit constants spelled 4× — values agree; consolidation costlier than it looks
  (cross-module); demoted.
- Name-multiplicity filter-count idiom ×5 — thresholds differ correctly; helper would also
  chokepoint the finding-3 folding fix.
- Missing `///` on a few new schema.rs public items (coding-standards.md:19).
- `grouped_aggregate` is NOT the sole Aggregate constructor (SQL front-end builds directly,
  legitimately — different fold); document the weak-form shared contract ("aggregates IS
  the complete output list") at both sites rather than unify — unification verified WRONG.
- Synthetic function names are user-reachable by design; post-D4 every arm guards
  gracefully (full table verified); optional `__td_` prefix reservation is cheap hardening.
- Typed NamedExpression output-list element (makes N8 structural; unifies WithColumns'
  second naming representation) — unverified altitude candidate, ADR-scale.

## Efficiency non-findings (explicitly rated noise — do not "fix")
- find-by-id linear scans: ~10-20μs/query at TPC-DS scale; an id→index HashMap would be
  SLOWER for tens-of-columns schemas.
- Attribute/BTreeSet clone traffic: relocation of the deleted parallel vectors, ~cost-neutral.
