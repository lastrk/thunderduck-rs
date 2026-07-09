# SELECT-block builder — follow-up plan

Deferred improvements unlocked by the 2026-07-09 emission refactor
(`feat/select-block-emission`: `sql_block.rs` SELECT-block builder + analyzer
`RelScope` stamp + Join `*_requires_synthetic` flags). Both items below were
identified during that work and deliberately deferred; neither blocks the
merge. Evidence gate for both: the corpus (no previously-green case regresses)
plus the `tracing::debug` "wrap strands qualifiers" witness added in Phase E.

## 1. FromItem leaf generators (Values / LocalRelation / FileScan / Range) — DONE

Implemented 2026-07-09 (this branch). `build_values` / `build_local_relation`
/ `build_file_scan` / `build_table_function` produce leaf blocks over
`FromItem::Raw`; the empty-`LocalRelation` and bare-`explode` forms stay Raw
units (genuine FROM-less SELECTs); `range` keeps its `id` bind via
`default_projections` (pinned by `dispatch_project_over_range_binds_id_column`
+ `bare_range_dispatch_keeps_id_default_projection`). Corpora: zero movement.

**Original plan:** These ops render as `Raw` units today, so every parent wraps them:
`SELECT cols FROM (SELECT * FROM read_parquet(...)) AS __td_sub`. But their
SQL essence is a FROM-item generator, not a SELECT statement. Convert each to
a leaf `SelectBlock` over `FromItem::Raw { sql, exposed }`:

- `FileScan` → `read_parquet('…')` / `delta_scan('…')` (from body verbatim;
  `exposed: []`).
- `Values` → `(VALUES …) AS __td_values(c1, …)` (`exposed: ["__td_values"]`
  — or keep `[]`; nothing qualifies through it today).
- `LocalRelation` (non-empty) → same shape as Values. The empty-relation
  `SELECT CAST(NULL AS T) … WHERE 1=0` form is a real SELECT — keep it Raw.
- `TableFunction` `range` → `range(start, end, step) AS __td_range(id)`;
  note the enclosing `SELECT id` today performs the column BIND — as a
  FromItem the default projection must keep `id` (set
  `default_projections: "id"`), or parents merging `*` would emit DuckDB's
  raw `range` column name. Same care for the bare-`explode` TVF arm.

**Why.** Node reduction (`SELECT cols FROM read_parquet(...) WHERE …` in one
block), fewer `__td_sub` layers in every corpus case touching files, and it
retires the last "SELECT-shaped Raw" ops so `legacy_render` shrinks to the
genuinely self-contained generators (Pivot family, Sample, RecursiveCte).

**Risks / checks.**
- `__td_values(cols)` column-rename syntax must survive as the FROM item
  alias (it is part of the FromItem sql string — fine).
- The `range`/`explode` output-column bind (see above) is the one real trap;
  pin with an emission test asserting `SELECT id FROM range(...) AS
  __td_range(id)` merges rather than wraps.
- Gate: corpus per-case diff, zero green→red.

## 2. Qualified-star expansion via the RelScope stamp — DONE

Implemented 2026-07-09 (this branch). `project_output_schema` expands `q.*`
to the qualifier's stamped range; `input_relation_binds_qualifier` deleted.
7 new corpus cases (join-015..017, jn-019..022) all PASS against real Spark.
Implementation surfaced two systemic findings: (a) the differential runner
only built the server when the binary was MISSING — every prior per-phase
corpus gate had silently tested the pre-refactor binary (runner fixed:
always builds; see tasks/lessons.md); (b) the first honest full-suite run
exposed a correlated-subquery regression cluster (sq-*, tbl-005,
tpcds-q006): merge visibility wrongly required correlated OUTER qualifiers
to be bound by the inner FROM scope. Fixed by exempting qualifiers the
input's own RelScope does not bind (they are outer refs / struct quals /
flag-guaranteed synthetics); pinned by
`correlated_scalar_subquery_inner_filter_merges_into_one_block`. Net result
vs pre-refactor baseline: 0 regressions, 25 improvements (incl. tpch-q07/
q08 and 16 TPC-DS queries).

**Original plan:** `input_relation_binds_qualifier` (analyzer.rs) gates qualified-star
(`q.*`) expansion in `project_output_schema`, allowing only three shapes:
bare `TableScan`, `AliasedRelation`, and the LEFT of a semi/anti join.
Everything else — notably `e.*` over a plain multi-relation join — is an
`UnknownColumn` boundary error. Its own doc says the restriction is an
EMISSION-scoping question: it was calibrated to the old wrap-then-flatten
renderer, which would have buried `q` under `__td_proj` for exactly the
shapes it refuses. That renderer no longer exists.

Rewire the gate to consult the stamped `RelScope`: `q.*` expands to the
qualifier's bound field RANGE (not the whole input schema — today's
whole-schema expansion is only correct because the allowed shapes bind `q`
to the full range) whenever `input.scope.lookup(q)` yields exactly one
binding. Emission already holds up its end: `expr_qualifiers` vis-checks
`Star` qualifiers, and the block builder keeps `q` visible in every shape
whose scope exposes it.

**Why.** Feature coverage, not just cleanup: `SELECT e.*, d.dept_name FROM
emp e JOIN dept d ON …` is a common SparkSQL shape and currently a boundary
error; same for `e.*` through Filter/Sort/Limit stacks over joins, and over
lateral-view chains. Likely flips SQL-corpus reds green. Also deletes the
last of the three original scope re-derivations.

**Risks / checks.**
- Schema expansion must use the qualifier's RANGE from the stamp
  (`resolved_schema.fields[range]`), preserving field order — the range-
  slice is exactly what `resolve_column`'s qualifier arm already trusts.
- USING joins stay excluded automatically (their `RelScope` is empty).
- The wrap-fallback interplay: if a `q.*` projection ends up above a wrap
  (vis fail), it breaks loudly — same class as item 3 below; acceptable, and
  the strand trace will witness it.
- Spark parity check: Spark resolves `q.*` per its own scoping rules —
  differential AnalyzePlan comparison on the new shapes is the oracle
  (ADR-015); add corpus cases (currently expected-error) before flipping.

## 3. Wrap-boundary qualifier rewriting (retire the strand class)

**Witnesses added 2026-07-09**: `filt-016` (alias-qualified filter above
LIMIT) and `filt-017` (alias-qualified filter above DISTINCT) in the
DataFrame corpus — both red on τ with the exact strand signature
(`Binder Error: Referenced table "e" not found! Candidate tables:
"__td_sub"`) and green on reference Spark. The evidence gate is satisfied;
this item is now actionable.

**What.** The one residual alias-occlusion class: a qualified reference above
a slot-conflict wrap. Example: `df.orderBy(x).limit(5).filter(e.y > 1)` — the
analyzer resolves `e.y` (Limit/Sort are scope-passthrough), but emission
cannot merge a WHERE past a LIMIT, so it wraps, and `e` is stranded behind
`__td_sub` → DuckDB binder error. Today this is traced
(`tracing::debug` "wrap strands qualifiers…") but still fails.

Fix: at the wrap boundary, REWRITE the stranded references instead of
emitting them verbatim. When the wrap's child block exposed qualifier `q`,
a reference `q.c` above the wrap can be rewritten to the unqualified output
name `c` **iff `c` is unambiguous in the child's output schema** (exactly
one case-insensitive match). Where it is ambiguous, re-qualify with the wrap
alias (`__td_sub.c`) only if positional identity is provable — otherwise
keep today's loud failure.

**Where.** Emission-side, at the wrap fallbacks (`SelectBlock::wrap` call
sites in `build_project` / `build_filter` / `build_sort` /
`block_with_projections`): the rendered expression strings are already
built, so the rewrite must happen BEFORE rendering — restructure those
builders to (1) attempt merge-vis, (2) on wrap, re-render the expressions
with a qualifier-stripping wrapper around `render_expr` (an expression-tree
clone with `ColumnReference.qualifier = None` for stranded-but-unambiguous
refs). Do NOT touch the analyzer: the stamped qualifier is correct analysis
data; this is an emission-scoping concern (same division as ADR-002).

**Why.** Closes the main residual class from the post-refactor risk review
(qualified refs above occupied LIMIT/ORDER BY/DISTINCT slots, including the
correlated-subquery variant). Converts the strand trace from "latent error
witness" to "handled".

**Risks / checks.**
- Ambiguity: never strip when the name matches 2+ output columns (self-join
  outputs). The unambiguity check runs against the child block's OUTPUT
  schema (the analyzer's resolved_schema of the wrapped node).
- Correlated refs: a stranded qualifier used by a subquery-INNER plan cannot
  be rewritten from outside (the inner plan is already rendered). Detect via
  the existing shallow `expr_qualifiers` walk (subquery bodies excluded) —
  inner correlated strands stay loud failures until witnessed.
- Add corpus cases first (they will be red), then implement: e.g.
  `df.alias("e").orderBy(...).limit(5).filter(col("e.y") > 1)` and the SQL
  equivalent.

## Sequencing

1 is mechanical and low-risk — good warm-up or agent-pipeline task. 2 is the
highest user-visible value (new SparkSQL surface) and should come with new
corpus cases validated via the AnalyzePlan diff first. 3 needs a corpus
witness (red case) before implementation; if the strand trace never fires
across the corpus and real workloads, 3 can stay deferred indefinitely
(that is the ADR-001/ADR-015 evidence discipline).
