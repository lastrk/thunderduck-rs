# Slice C — Initial `/new-feature` prompt (pass 1)

Use this file verbatim as the `/new-feature` prompt for pass 1 of Slice C
under the iteration methodology in `tasks/v2-slice-iteration-methodology.md`.

---

Bring the v2 emission table online (Slice C — core dispatch, first tranche)
per rearchitect ADR-009. Wire `transpiler_v2::generate` to lower a
`LogicalPlan` into a `CommonAst`, run the Slice-B analyzer, dispatch each
`TypedOp` through a declarative `EmissionTable`, and emit DuckDB SQL for the
trivial-operator/scalar-function surface the corpus concentrates on
(`proj-*`, `filt-*`, `cast-001..011`, most `cond-*`, `str-001..020`,
`math-001..014`, `dt-002..017`, primitive aggregates, `ord-*`,
`set-001..010`, `misc-003..005`+`misc-008..010`, `struc-001/002/005/006`).
~250 total Case IDs; expected v2-progress.sh delta 12 → 180-200 (this is
where the corpus gate empirically moves for the first time).

**Design mandate (authoritative):** docs/thunderduck-rearchitect-ADRs.md

- ADR-009 (declarative emission table, compiled dispatch): each
  `(TypedOp variant, [operand types])` → an `EmissionRow` carrying a
  template string, ordered arg slots, and an optional top-level projection
  CAST. Dispatch is compiled — a match arm per row, NOT a HashMap keyed on
  strings.
- ADR-001 (transliterator, not optimizer): emit exactly what Spark would
  emit shape-wise. No fusion, no CSE, no plan rewrites beyond what Spark's
  analyzed plan already carries. Cosmetic-flagged corpus cases (join-014,
  misc-008..010, struc-003, ord-006) are free once no-op emit exists.
- ADR-002 (emit-level delegation): star-expansion, alias resolution, USING
  column dedup — all delegated to DuckDB at emit time via the SQL text
  itself, NOT rewritten in the AST. Slice B already resolves these
  internally *for typing*; Slice C must not remove that internal resolution.
- ADR-014 (two decision spaces): types come from the analyzer, shape comes
  from the emitter — never confuse them. If a corpus case needs a CAST
  because Spark's return type ≠ DuckDB's default, that CAST is a Slice-C
  emission-table row, not a Slice-B analyzer change.
- ADR-015 (differential oracle): the corpus is the oracle. A row that says
  `spark_column_name(expr) AS out` matches Spark iff the corpus's schema
  diff is clean; do not "improve" naming beyond `spark_column_name`.

**Inputs to read:**

- `docs/thunderduck-rearchitect-ADRs.md` — ADR-002, ADR-009, ADR-014,
  ADR-015, §CV.5 INV1/INV2/INV3 (invariants Slice C activates).
- `crates/core/src/transpiler_v2/analyzer.rs` (post-Slice-B substrate on
  commit `f5a54c3`): `analyze()`, `TypedAst`, `TypedOp`, `TypedAttr`,
  `HasSchema`, `AnalyzerError`. This is the surface Slice C consumes.
- `crates/core/src/transpiler_v2/ast.rs`: 15-op `CommonAst`. Extending the
  operator surface (adding `by_name: bool` to `ast::Union`, etc.) IS
  allowed for this slice — see "Slice-B carryover" below.
- `crates/core/src/transpiler_v2/emission.rs`: current stubs
  (`EmissionTable`, `ExternalEmit`, `external_emit_paths`,
  `extension_targets`). Slice C fills the first tranche.
- `crates/core/src/generator/` — the legacy `SqlGenerator`. **Read-only
  reference.** Its per-operator SQL shape is the emission oracle for
  operators the legacy path already emits correctly; do not modify it.
- `crates/core/src/logical/mod.rs` — the legacy `LogicalPlan` (29
  variants); needed for the `LogicalPlan → CommonAst` adapter.
- `crates/connect-server/src/service.rs` — the dispatch surface. Slice C
  must actually route `TranspilerPath::V2` requests through the new path.
- `tests/integration/differential/dataframe_corpus.py` — the acceptance
  gate. Case IDs listed in "Acceptance" below.
- `tasks/v2-adr-readiness-map.md` §Slice C — target case ID list.
- `.agent-output/003-review-findings.md` (Slice B review, archived under
  the Slice B pass) — the six Mediums Slice C MUST close before merge.
- `.agent-output/004-perf-findings.md` (Slice B perf) — the two skipped
  MEDIUMs Slice C may revisit if it changes the pass shape.

**Slice-B carryover — MUST resolve in this slice:**

The Slice-B reviewer approved with 0 Critical / 0 High but flagged six
Medium hazards that materialize the moment emission is wired. Close them
in this order (M1/M2/M4 are correctness-load-bearing once real requests
flow):

1. **M1 — `has_resolved_schema` false-positive on bare `Star`.** Pass 2
   in analyzer.rs (~lines 1163-1172) writes `TypedAttr { Unresolved, true }`
   for Star slots; the walker (~lines 396-406) then reports the whole
   TypedAst as unresolved. First `SELECT *` from Slice C's emitter will
   trip INV5. Fix: either skip Star slots in the walker's
   projection_types check, or fill the Star slot with a sentinel type
   derived from the child schema. Analyzer change, not emitter change —
   but Slice C is when it starts biting.

2. **M2 — RIGHT join with USING places USING key on the wrong side.**
   `compute_join_output_schema` (~lines 1310-1350) dedups USING keys from
   the LEFT unconditionally; Pass 3 then sets left_nullable=true, wrongly
   marking the USING key nullable. Fix: for RIGHT joins, take USING keys
   from the right side (or defer to a Pass-3-aware helper). This bug
   will manifest the moment `join-*` cases start running through the v2
   path.

3. **M3 — Union widening updates outer op schema but not deeper
   expression slots.** `repropagate_union_widening` (~lines 1376-1431)
   updates each child's `schema` and `projection_types` but leaves the
   deeper `Expression` (e.g. `CastExpression::to_type`) untouched.
   Slice C's emitter must decide which "wins" — the child expression's
   declared type or the union parent's widened schema. Document the
   choice at the emission call site and add a regression test on
   `type-019`.

4. **M4 — `unionByName` not modeled in `ast::Union`.**
   `widen_union_fields` uses `lf.name.clone()` positionally, wrong for
   `unionByName` with reordered columns. Add `pub by_name: bool` to
   `ast::Union` and route the reorder in Pass 1 (name-match) before
   Pass 2 unifies. The lowering from `LogicalPlan → CommonAst` also
   needs to set this bit.

5. **M5 — `AnalyzerError::AmbiguousColumn` and `TypeMismatch` unused.**
   Slice C's real dispatch will hit ambiguous-column scenarios (self-
   joins, column-name collisions post-join). Either wire the analyzer to
   construct these variants when it detects the case, or drop them if
   Slice C decides ambiguity is emit-time only (unlikely — analysis-
   level ownership per T1).

6. **M6 — Pass 1 seeds `Unresolved` fields Pass 2 overwrites.** Dead
   work. Either remove Pass 1's `fields`/`attrs` seeding for
   Project/Aggregate/WithColumns (leave them empty for Pass 2 to fill),
   or add a doc-comment at `resolve_project` (~line 517),
   `resolve_aggregate` (~line 622), and `resolve_with_columns`
   (~line 788) explaining the intent. Recommend the seeding removal —
   it's ~50 duplicated lines of `spark_column_name` calls and Vec
   allocations.

Perf MEDIUMs 1 and 3 from `004-perf-findings.md` were deliberately
skipped in Slice B (correctness risk and self-flagged optional,
respectively). Slice C may revisit *perf-M1* (Pass 1 / Pass 2 shaping
duplication) as part of resolving review M6 — the two overlap.

**Slice-B open question that Slice C must answer:**

The `LogicalPlan → CommonAst` adapter is NOT in Slice B;
`transpiler_v2::generate` still returns `TranspilerError::Unsupported`.
Slice C owns:

- The adapter itself (a new file, likely
  `crates/core/src/transpiler_v2/lowering.rs`, mapping each of the 29
  `LogicalPlan` variants either to a `CommonOp` or to a `CommonOp::Punt`
  with a reason string). Every legacy variant not in Slice B's §4.1 gets
  a Punt; the legacy `SqlGenerator` remains the fallback for those.
- Constructing `BaseTypes` from whatever catalog the dispatched request
  carries (for Slice C's read-only DataFrame corpus, this is the
  session's registered temp views + resolved TableScan schemas;
  `service.rs` already has enough context).
- Wiring `AnalyzerError → ThunderduckError` (add a new variant
  `ThunderduckError::V2Analyzer(#[from] AnalyzerError)`). Slice B's plan
  §10 explicitly deferred this to Slice C.

**Scope:**

1. **Lowering adapter** — `crates/core/src/transpiler_v2/lowering.rs`
   (new). `pub fn lower(plan: &LogicalPlan) -> Result<CommonAst,
   TranspilerError>`. Every `LogicalPlan` variant covered by Slice B's
   `CommonOp` set maps 1:1; everything else produces `CommonOp::Punt {
   kind, reason }` and `transpiler_v2::generate` falls through to the
   legacy path (do NOT raise an error at the dispatch layer for Punts —
   Slice C is *additive* per CLAUDE.md, both paths coexist).

2. **`EmissionTable` — first tranche.** Populate `emission.rs` with a
   `pub fn dispatch(op: &TypedOp) -> Result<EmittedSql, EmissionError>`
   that matches on the op kind, uses the op's `TypedAttr`s to pick the
   correct row, and produces a SQL fragment. Rows for:
   - Project, Filter, Sort (Order + Limit + Offset combinations),
     Limit/Tail, Distinct, WithColumns (as SELECT with COALESCE alias
     rewrites), DropColumns (as SELECT exclusion list), AliasedRelation.
   - Aggregate: primitive functions (count, count-distinct,
     approx-count-distinct, min, max, mean, first, last, mode). Do NOT
     wire spark_sum/spark_avg/spark_skewness/spark_hash — those are
     Slice D.
   - Set ops: UNION/UNION ALL, INTERSECT/INTERSECT ALL, EXCEPT/EXCEPT ALL.
   - TableScan/LocalRelation/RangeRelation.
   - Scalar expressions inside projections/filters: cast (except
     `try_cast` — Slice D), when/coalesce/nullif/isnan/nanvl, string
     functions (str-001..020, except str-020 which needs Spark-4 syntax
     — punt to Slice D), math functions (math-001..014, except
     math-015/016), datetime functions (dt-002..017, except dt-001
     which is nondet Slice B ⌂).

3. **Dispatch wiring** — `crates/connect-server/src/service.rs`. When
   `TranspilerPath::V2` is active AND the lowering adapter returns a
   Punt-free `CommonAst` AND `analyze` succeeds AND
   `EmissionTable::dispatch` returns Ok — use the v2 SQL. Otherwise fall
   back to the legacy `SqlGenerator`. Do NOT change legacy behavior when
   `THUNDERDUCK_TRANSPILER` is unset / `legacy`.

4. **INV1 activation.** `crates/core/src/transpiler_v2/invariants.rs` —
   `inv1_no_c_escape_hatches` (currently vacuous on empty
   `C_ESCAPE_HATCHES`) should assert that Slice C did not introduce any.
   `C_ESCAPE_HATCHES` stays `&[]`. If Slice C genuinely needs an escape
   hatch, that's a stop-and-flag moment.

5. **INV2 activation.** `inv2_serializer_is_only_writer` — Slice C's SQL
   output goes through a single choke point (`EmissionTable::dispatch`
   → the query executor). Instrument it via `set_serializer_tap` per
   the existing stub, and assert the tap fires exactly once per v2
   request.

6. **INV3 activation.** `inv3_ab_layer_boundary` — Slice C's emission
   rows are pure functions of `TypedOp`; assert no row reaches for
   legacy `SqlGenerator` state or `FunctionRegistry` runtime lookup.
   Grep-based assertion is fine (the emission module MUST NOT import
   `crate::generator::*` or `crate::functions::FunctionRegistry`).

**Acceptance:**

- Case IDs green in core_v2 (via `tests/scripts/v2-progress.sh`) after
  this slice lands. The full target list is in
  `tasks/v2-adr-readiness-map.md` §Slice C — key clusters:

    proj-001..015, filt-001..015, cast-001..011,
    cond-001, cond-002, cond-006, cond-007, cond-008,
    cond-012..016, str-001..019, math-001..014, dt-002..017,
    agg-001..006, agg-010, agg-014..017,
    ord-001..005, ord-007, ord-010..012, set-001..010,
    misc-003..005, misc-008..010, struc-001, struc-002,
    struc-005, struc-006

  Plus every case Slice B was expected to green via AnalyzePlan (the 35
  case IDs from Slice B) should now also pass on collect, not just on
  schema-diff. `type-011` (outer-join nullability) is a specific
  regression-guard for M2.

- Progress signal: 12 → 180-200 on `tests/scripts/v2-progress.sh`. This
  is the empirical validation moment — Slice B's expected [+5, +15] was
  substrate-only; Slice C is where the corpus gate actually moves.

- Quality gate (per CLAUDE.md `## Quality Gate`): `cargo check -p
  thunderduck-core` and `cargo check -p thunderduck-connect-server`
  both clean; `rustfmt --check` clean on touched files; `cargo test -p
  thunderduck-core --lib --tests` passes; `cargo test -p thunderduck-
  connect-server --lib --tests` passes.

- Zero new clippy warnings on files touched (workspace-wide clippy is
  not part of the pipeline gate per CLAUDE.md).

- `git grep 'TODO<space>INV<n>'` for each n in {1,2,3} (i.e. the marker <!--rewritten post-termination; see iteration-log §Marker-convention note-->
  `TODO` followed by `INV1`, `INV2`, `INV3` in source)
  all return empty.

- Legacy `SqlGenerator` behavior UNCHANGED — verify by running
  `./tests/scripts/run-differential-tests.sh tpch` at the end (this IS
  the human-verification gate for SQL-generation changes per CLAUDE.md
  §4). Legacy path must still pass. Differential run is out of scope
  for the agent pipeline itself; run it manually before commit.

**Out of scope (deferred to later slices):**

- Extension functions (`spark_sum`, `spark_avg`, `spark_hash`,
  `spark_skewness`, `spark_decimal_div`, `spark_xxhash64`, `try_cast`,
  `try_divide`) — Slice D. Their `EmissionTable` rows are Slice D's.
  `agg-007`, `agg-008`, `agg-009`, `agg-012`, `agg-013`, `hash-*`,
  `cast-012`, `math-016` remain red after Slice C.
- Outer-join emission beyond what M2 fixes structurally — Slice E owns
  the full `join-*` cluster (though `join-014` cosmetic will fall out
  of ADR-001's no-op emit for free).
- Complex-type emission (arrays, maps, structs, HOFs, explode/inline) —
  Slice F. `arr-*`, `map-*`, `struct-*`, `hof-*` remain red.
- Verticals (interval, pivot, window, JSON, parsing) — Slice G.
- Command arm / lakehouse writes — Slice H.
- Pivot/Unpivot in `ast::Union` — the M4 fix is `unionByName` only, not
  runtime-data-dependent pivot. `piv-*` stays red.
- Any change to the corpus, harness, or `tests/integration/**`.

**Non-goals (do NOT do any of these):**

- Do NOT refactor legacy `SqlGenerator` or `TypeInferenceEngine`. Their
  behavior must remain identical on the legacy code path. Slice C is
  additive.
- Do NOT introduce `Visitor` or `AnalyzerPass` traits. Exhaustive
  `match` on `TypedOp` remains the visitor.
- Do NOT introduce string-manipulation post-processing on emitted SQL
  (per CLAUDE.md "SQL Generation Architecture Principles" #1 and #2).
  Every SQL fragment is built from the typed AST via
  `EmissionTable::dispatch`.
- Do NOT block on perf-MEDIUM 1 (Pass 1/2 duplication) unless review M6
  demands the cleanup; correctness comes first, and the analyzer is not
  on any hot loop.

Note to the architect: Slice C is where the transpiler-v2 rearchitecture
stops being paper. Everything upstream (Slices A, B, invariants, corpus,
progress dashboard) exists to make Slice C possible; everything
downstream (Slices D-H) plugs into the emission table Slice C builds.
The `EmissionTable` shape (rows, dispatch discipline, extension-target
signaling) is a load-bearing design decision — please spend more time on
its skeleton than on the sheer volume of the ~50-row first tranche. The
review Mediums M1-M4 are correctness-load-bearing before any real
request flows; treat them as prerequisites, not follow-ups.
