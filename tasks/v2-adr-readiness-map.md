# v2 Transpiler ADR Readiness Map

**Purpose.** Order the rearchitect ADRs (`docs/thunderduck-rearchitect-ADRs.md` ADR-000 → ADR-020) into implementation slices, and map each slice to the corpus Case IDs (`tests/integration/differential/dataframe_corpus.py`, 324 cases) it should green. A developer reading §3 alone should be able to pick a slice, invoke `/new-feature` citing the ADRs it owns, and know exactly which Case IDs must turn green when their commit lands.

**Baseline.** `core_v2` currently passes 12/324 (see `tests/integration/v2_progress.md`). Progress is measured via `tests/scripts/v2-progress.sh`; a monotonic climb toward 324 is the signal.

**Constraint.** Each of INV1–INV9 has a stub in `crates/core/src/transpiler_v2/invariants.rs` with a `TODO INV<N>` marker. §6 says which slice makes each invariant stop being vacuous.

---

## 1. Layered slice sequence

Slices are named in the strict order §CV.2's dependency matrix imposes. Each names the ADRs it owns, its stub deliverables inside `crates/core/src/transpiler_v2/`, and its one-line position justification.

**Slice A — IR bootstrapping.** Owns ADR-003 (common AST) and ADR-004 (front-end convergence: SparkSQL parser + Connect-proto deserializer both lower to the same AST). Fills `ast::CommonAst` (currently a unit struct) with the real proto-inspired node set; wires `transpiler_v2::generate` to construct the AST from the shared proto/SQL layer. First because every downstream slice consumes the AST.

**Slice B — Type & nullability analyzer.** Owns ADR-005 (type/nullability inference over the AST) and ADR-006 (bounded pass sequence). Fills `analyzer::has_resolved_schema` with a real walker; grows an inference engine that produces per-node Spark types + nullability + resolves the `DataType::Unresolved` placeholders. Second because emission cannot dispatch without knowing operand types (LB1's divergent slice is *exactly* {type, nullability}).

**Slice C — Core emission table (first tranche).** Owns ADR-009 (declarative emission table, compiled dispatch), scoped to the "trivial" node kinds: `Project`, `Filter`, plain scalar `Cast`, comparison operators, primitive `Aggregate` (count/min/max/first/last), `OrderBy`, `Limit`, `Distinct`, most `string`/`math`/`datetime` scalar functions, `Union`/`Intersect`/`Except` set ops. Fills `emission::EmissionTable` with its first ~50–70 rows. This is the largest single-slice green-count contributor.

**Slice D — Extension dispatch for Spark-divergent semantics.** Owns ADR-010 (extension functions as gap-fillers). Populates `emission::extension_targets()` with the `spark_*` names the table's `Extension(...)` rows reference. INV6 gains real teeth here. **Structured as two phases per the 2026-07-01 up-front audit:**
- **Phase 1 (ext4 wiring — substrate landed 2026-07-01 at commit `f5756b5`; target cases blocked pending Slice C.3):** wire the `spark_*` functions already in the pinned `ext4` release (`spark_hash`, `spark_xxhash64`, `spark_decimal_div`, `spark_sum`, `spark_avg`, `spark_skewness`) plus native-DuckDB functions matching Spark (stddev family, percentile_approx, median). Landed the substrate: 6 scalar arms + 2 verify-first arms (kurtosis, count_if — wired native), `render_binary` DECIMAL-div branch, `spark_aggregate_rewrite` helper, `extension_targets()` with 6 entries, **INV6 activated over the ext4 subset with real `duckdb_functions()` containment check**. **BUT: measured progress signal delta = 0** (134 → 134) at Phase 1 halt; partial unblock landed later via Slice C.3-4 (134 → 149 after the `LocalRelation` Decimal marshalling fix, 2026-07-01). The Phase 1 target case IDs do NOT pass because scoped-differential at termination surfaced pre-existing Slice C.2 latent bugs the 2026-07-01 up-front audit misclassified as "already-passing native": `sha2`'s arg-count mismatch (v2 emits `SHA256(col, bits)` but DuckDB's `SHA256` is single-arg; legacy strips via `FunctionRegistry::translate_typed`), `hash`/`xxhash64` nullability mismatch, Decimal-to-Boolean type inference for `count_if`, and other latent gaps. **Halt-and-flag** per the /goal constraint — a **new Slice C.3 (Slice C.2 latent-bug corrections)** is needed before Phase 1's target case IDs can meaningfully turn green. See `tasks/v2-slice-d-iteration-log.md` §"Phase 1 termination" for the diagnostic. Legacy TPC-H 51/51 unregressed.
- **Phase 2 (post-`ext5` pin):** wire the ~8 functions that require new C++ extension work (see `tasks/duckdb-extension-specs/` for the pre-drafted handoff specs). Blocked on the `thunderduck-duckdb-extension` project shipping `ext5` and this repo pinning it. Progress signal delta: ~142 → ~145-148.

Slice D **terminates only when Phase 2 completes** (all 14 target case IDs pass). Between phases, the Slice D iteration pauses with a "waiting for ext5 pin" state recorded in `tasks/v2-slice-d-iteration-log.md`; a follow-up `/goal` invocation resumes when ext5 is pinned.

**Slice C.3 — Slice C.2 latent-bug corrections** (added post-Slice-D-Phase-1 halt-and-flag, 2026-07-01). Owns the pre-existing latent bugs that Slice D Phase 1's audit misclassified as "already-passing native." Positioned between Slice C and Slice D because Slice D Phase 1's target case IDs cannot pass until these are corrected. Scope:
- **`sha`/`sha1`/`sha2` arg-stripping** in `emission.rs::render_function_call` (line 1277): DuckDB's `SHA256` is single-arg; v2 passes all args through. Legacy strips extra args via `FunctionRegistry::translate_typed`. Fix: drop args beyond arg 0 for these specific arms. Add regression test (would have caught `hash-002`).
- **`hash`/`xxhash64` return nullability** (Slice B analyzer gap): Spark's `hash` returns non-nullable INT (seed 42); `xxhash64` returns non-nullable BIGINT. V2 analyzer marks these nullable. Fix: add explicit return-nullability rules to the analyzer's function-return-type inference. Corpus unblock: `hash-003`.
- **`count_if` aggregate-context Decimal-to-Boolean inference** (Slice B analyzer or emission gap): the predicate `salary > 90000` inside `count_if` returns Boolean but somewhere in the pipeline is being routed as Decimal. Fix: investigate at implementation time; may be an analyzer or a rendering issue. **LANDED 2026-07-01** via `/fix-bug` pipeline; delivered **+2** core_v2 delta (149 → 151) closing `agg-020` and `agg2-006`. Root cause was a **symmetric-omission pattern**: `TypeInferenceEngine::aggregate_return_type` and `Expression::FunctionCall::nullable` both enumerated the count family (`count`, `count_distinct`, `grouping`, `grouping_id`) but both omitted `count_if`; two one-token additions (`type_inference.rs` — return `Long` instead of the argument type; `expression/mod.rs` — mark non-nullable) closed both files. Sibling `aggregate_is_non_nullable` extended in parallel to prevent future drift. 4 regression tests added.
- **C.3-4 — `Binary(Div, Decimal, Decimal)` routing verification. LANDED 2026-07-01 (commit forthcoming); +15 delta.** Scope was **overturned by the diagnostician**: the C.3-4 bug report named `emission.rs::render_binary` / `render_spark_decimal_div`, but multi-hypothesis diagnosis proved that path was byte-correct. Actual root cause was upstream in `crates/connect-server/src/converter/relation_converter.rs:2513`: a silent-NULL catch-all in `local_relation_to_values_sql::val()` mapped every unhandled Arrow type (including `Decimal128`) to the string `"NULL"`, corrupting every DECIMAL cell in `createDataFrame` payloads. Fix landed there, not on the v2 substrate: added a `Decimal128(p, s)` arm with a `format_decimal128` helper (renders the scaled literal DuckDB requires) and replaced the catch-all with a loud `Err` so any future marshalling gap surfaces immediately instead of silently substituting NULL. The regression test the initial prompt asked for landed in `emission.rs::tests` as well, locking the (already-correct) Div routing invariant in place. Progress signal delta **+15** (134 → 149) — far above the +3 prediction from `type-003/004/005`; decimal-payload cases across the corpus (`emp.bonus`, `dept.budget`, `nums.d1`/`nums.d2`) were all being silently NULL'd and now flow through correctly.
- **Div-in-`type-005` / `math-011` verification**: `type-005` closed by C.3-4; `math-011` deferred (reference-side Spark 4.x ANSI `DIVIDE_BY_ZERO`, not a Thunderduck bug).
- **Legacy-parity approximation review** for `sha1`/`sha2` → `SHA256`: the differential-side tolerance for hash-001 may need explicit `TODO Slice D Phase 2:` if the reviewer decides they should be extension-provided (`spark_sha1`/`spark_sha2` per new ext5 specs).

**Unlocks the Slice D Phase 1 target case IDs** (`hash-001..003`, `agg-007`, `agg-013`, `agg-020`, `agg2-006`, `math-011`, `type-005`) plus indirect unlocks. C.3-4 already delivered +15; the remaining five fixes should recalibrate against the fresh 149 baseline (the +3 → +15 overshoot on C.3-4 alone suggests the original 134 → ~140-142 map estimate under-counted decimal-payload cases that the halt-and-flag audit had no visibility into; expect the final Slice C.3 landing to overshoot the +6-8 map estimate as well).

**Slice E — Outer-join nullability + set-op widening.** Extends Slices B & C with the nullability-preserving join emitter and the union type-widening rule (`int ∪ decimal → decimal` etc.). Owns the `EMISSION_JOIN` + `SETOP` cells in ADR-009's table. Second-tranche emission focused on the divergent-slice cases the corpus deliberately concentrates on.

**Slice F — Complex-type emission (arrays, maps, structs, HOFs, inline).** Owns the second, denser tranche of ADR-009 rows: `array()`, `map()`, `struct()`, dot access, `explode`/`posexplode`/`inline`, HOF lambdas (`transform`, `filter`, `aggregate`, `zip_with`, `map_filter`, etc.), and the `_new` variants (`array_append`, `map_contains_key`, `str_to_map`, …).

**Slice G — Vertical extensions: temporal, grouping/pivot, windows, JSON, parsing.** Owns targeted emission verticals that share little internal structure but each unlock a compact case cluster: `EMISSION_INTERVAL` (Date/Timestamp ± Interval, `make_interval`, `timestampadd`), `EMISSION_CTE` (rollup/cube/pivot/unpivot/stack expansion), `EMISSION_WINDOW` (frame specs, `nth_value`, `first`/`last IGNORE NULLS`), `EMISSION_JSON` (`get_json_object`, `from_json`/`to_json`, `from_csv`/`to_csv`), `EMISSION_PARSING` (`parse_url`, `to_number`, `split_part`). Any of these can land independently.

**Slice H — Command/lakehouse writes.** Owns ADR-011 (command arm), ADR-012 (catalog overlay), ADR-013 (external table reads), ADR-017 (Delta append), ADR-018 (Iceberg UC-managed writes), ADR-019 (lakehouse I/O contract). Fills `provenance::emit_write` with real SQL; populates `emission::external_emit_paths()`. **Unlocks zero DataFrame-corpus cases** (the corpus is read-only in-memory DFs, per its own docstring), but activates INV8 and INV9. Landed last of the corpus-progression slices because it has no bearing on the `core_v2` gate.

**Slice I — Differential-harness activation** (added post-Slice-C, 2026-07-01). Owns the *full* INV1 activation. Slice C.1 activated INV2's dispatch-is-only-writer companion via `EMIT_TAP`, but INV1's byte-identical-input-to-both-engines check requires the ADR-015 differential harness in `tests/integration/` to install a real serializer tap and diff payloads. Fills the `set_emit_tap`-consuming test in the harness with the real payload-hash check; converts the `DEFER INV1 → differential-harness slice:` markers in `crates/core/src/transpiler_v2/{invariants.rs, mod.rs}` into a load-bearing assertion; removes the deprecated `set_serializer_tap` alias if no external harness still consumes it. **Unlocks zero DataFrame-corpus cases** (the harness is oracle machinery, not translation). Position: orthogonal to Slices B–H's corpus progression; can land any time after Slice A completes (INV7 must hold for the harness's front-end parity to be meaningful). Prerequisite: Slice A (both front-ends producing the same AST).

**Slice J — ADR-007 escape-hatch enumeration** (added post-Slice-C, 2026-07-01). Owns the *escape-hatch dimension* of INV2 (INV2's dispatch-is-only-writer companion is already active from Slice C.1). Populates `crates/core/src/transpiler_v2/mod.rs::C_ESCAPE_HATCHES: &[&str]` with named, unique labels for every structural forced transliteration retained in the B layer per ADR-007. Deletes the `DEFER INV2 → ADR-007 slice:` markers in `invariants.rs`. **Unlocks zero DataFrame-corpus cases**. Position: orthogonal to corpus slices; may benefit from Slice H's ADR-011/012 landing to identify DDL-adjacent escape hatches, but no hard prerequisite.

**Not a slice — cross-cutting discipline.** ADR-000 (positioning), ADR-001 (transliterator-not-optimizer), ADR-002 (delegation boundary), ADR-007 (A/B/C layer contract; substrate for the escape-hatch dimension of INV2 is owned by Slice J), ADR-008 (correlated subqueries direct), ADR-014 (two decision spaces), ADR-015 (differential oracle harness; the harness substrate for INV1 is owned by Slice I), ADR-016 (version pin), ADR-020 (strict-only, already landed) are premises, disciplines, and testing architecture. They shape *how* Slices A–J are implemented; none of them unlocks corpus cases directly.

---

## 2. ADR → Case ID coverage table

Each row lists the corpus Case IDs whose passing demonstrably requires that ADR's substrate. Cases are grouped by category prefix; ranges are inclusive.

| ADR | One-line scope | Case IDs unlocked | Count |
|---|---|---|---|
| ADR-000 | positioning (premise) | — | 0 |
| ADR-001 | transliterator, not optimizer | (constrains `cosmetic`-flagged cases: `join-014`, `misc-008..010`, `struc-003`, `ord-006`) | 6 (constraint, not unlock) |
| ADR-002 | emit-level delegation | (enables `SCHEMA_THREADING` via delegated `*` — indirect) | 0 direct |
| ADR-003 | common AST | (prereq to all — no direct unlocks) | 0 |
| ADR-004 | SQL + DataFrame → same AST | (prereq to all — no direct unlocks) | 0 |
| ADR-005 | type & nullability analyzer | `type-001..type-022`, `cond-003..cond-011`, `agg-013`, `agg-018..020`, `set-009`, `chain-004` (as schema-only), all `schema_only`/`nondeterministic`-flagged cases whose ref schema is deterministic | ≈35–45 |
| ADR-006 | bounded analyzer passes | (analyzer implementation discipline; no direct unlocks) | 0 |
| ADR-007 | A/B/C layer structure | (enables `C_ESCAPE_HATCHES` growth; no direct unlocks) | 0 |
| ADR-008 | correlated subqueries direct | (corpus is DataFrame-only; correlated subqueries live in the SQL-side corpus) | 0 |
| ADR-009 | declarative emission table | `proj-*` (15), `filt-*` (15), `cast-001..cast-011` (11), `cond-001..cond-016` (16), `str-*` (20), `math-001..math-014` (14), `dt-*` (17), `agg-001..agg-006`+`agg-010`+`agg-014..agg-024` (18), `grp-*` (6), `win-*` (12), `join-*` (14), `set-*` (10), `ord-*` (12), `arr-*` (17), `map-*` (7), `struct-*` (8), `hof-*` (10), `misc-*` (10), `piv-004..piv-006` (3), `arr2-*` (5), `map2-*` (2), `inl-*` (2), `agg2-006` (1), `struc-*` (6), `win2-002` (1) | ≈250 |
| ADR-010 | extension functions | `hash-*` (3), `agg-007` (sum on decimal), `agg-008` (stddev family — spark_ variants), `agg-009` (skewness/kurtosis), `agg-012` (corr/covar via extension), `cast-012` (`try_cast` — spark4), `math-016` (`try_divide`), decimal-div `math-011` correction, `agg2-003..005` (regression aggregates, histogram_numeric) | ≈14 |
| ADR-011 | command arm | (DataFrame corpus has no DDL; `chain-*` are pure-relation) | 0 |
| ADR-012 | catalog overlay | (corpus builds DFs in-session; no overlay reads required) | 0 |
| ADR-013 | external/lakehouse reads | (corpus uses `createDataFrame`, no path-scan) | 0 |
| ADR-014 | two decision spaces | (test-attribution discipline) | 0 |
| ADR-015 | differential oracle | (harness itself; enables INV1) | 0 |
| ADR-016 | version pin | (scoping) | 0 |
| ADR-017 | Delta append writes | (corpus is read-only) | 0 |
| ADR-018 | Iceberg UC writes | (corpus is read-only) | 0 |
| ADR-019 | lakehouse I/O contract | (composition of ADR-013 + ADR-018) | 0 |
| ADR-020 | strict-only target | (already landed) | 0 (already in `main`) |

**Numeric health check.** ADR-005 + ADR-009 + ADR-010 together account for ≈300 of 324 cases. The remaining ≈24 are integration chains (`chain-*`, 6), pivot/unpivot (`piv-*`, 3), interval-specific (`intv-*`, 6), sampling (`samp-*`, 2), metadata (`meta-*`, 4), and structural (`struc-*`, some) — all of which fall out of the specialized slice work in Slices F/G. ADRs 011–019 unlock zero corpus cases; they exist to activate INV8/INV9 and to serve the SQL corpus + write path, both explicitly out of scope for `core_v2`.

---

## 3. Per-slice acceptance gate

For each slice from §1, the exact Case-ID set that must turn green (grep for these IDs in the next `tests/scripts/v2-progress.sh` output after the slice lands). Cases flagged `schema_only`/`nondeterministic` are marked ⌂ (schema-diff only under the harness — see `test_dataframe_corpus_differential.py`).

**Slice A — IR bootstrapping.** *No case-ID unlocks by itself.* Acceptance: `crates/core/src/transpiler_v2/generate` no longer returns `Unsupported` for any input the connect-server dispatches with `THUNDERDUCK_TRANSPILER=v2`; instead it returns an AST-shaped placeholder that hands off to Slice B/C. Prerequisite for every subsequent slice. Progress signal: 12/324 → still 12/324 (build-only).

**Slice B — Type & nullability analyzer.** *Substrate landed 2026-07-01* (`.agent-output/001-architecture-plan.md` → `.agent-output/004-perf-findings.md`; commit pending). What shipped: `CommonAst` grew from a unit struct to a 15-operator enum + `Punt` (`ast.rs`); `analyzer.rs` gained `TypedAst`, `TypedAttr`, `Schema`/`BaseTypes` aliases, `AnalyzerError` (six `thiserror` variants), the sealed `HasSchema` trait, `pub(crate) analyze(...)`, `pub has_resolved_schema(&TypedAst)`, `pub inference_smoke()`, and three bounded passes — `resolve` (bottom-up structural), `assign_types` (bottom-up + one downward sub-sweep for `Union` widening per ADR-006 line 168), `derive_nullability` (outer-join + grouping-sets). New file `analyzer_fixtures.rs` carries five literal input-relation fixtures (`emp`/`dept`/`emp2`/`nums`/`raw`) matching `dataframe_corpus.py::build_inputs`, and five mini-fixtures (`smoke_type_001`, `smoke_cond_003`, `smoke_agg_013`, `smoke_type_011` outer-join widening, `smoke_type_019` union widening — expected value corrected to `Decimal(10,2)` per the legacy `TypeInferenceEngine::unify_decimal` oracle). All type/nullability logic delegates to `Expression::{data_type, nullable}` and `TypeInferenceEngine` verbatim — zero rule re-derivation per ADR-015 discipline. Invariants activated: **INV4** (`inv4_inference_isolation` calls `inference_smoke()`) and **INV5** (`inv5_no_unresolved_after_analyzer` runs `analyze`, then plants a `DataType::Unresolved` in a `TypedAst` slot to prove the walker looks past top-level schema); both TODO markers deleted. Scope discipline: `transpiler_v2::generate` still returns `Unsupported` — no `LogicalPlan → CommonAst` adapter yet, no dispatch wiring; that lands with Slice C. Zero edits outside `crates/core/src/transpiler_v2/`. Review verdict APPROVED, 6 Medium non-blocking follow-ups for Slice C (M1 Star-in-projection walker gap, M2 RIGHT-USING nullability, M3 union expression-slot type not repropagated, M4 unionByName positional-only, M5 unused error variants `AmbiguousColumn`/`TypeMismatch`, M6 Pass-1 seed doc-comment); perf M2 (`to_lowercase` → `eq_ignore_ascii_case`) applied in `analyzer.rs`. Progress signal: **not yet re-measured** — `tests/integration/v2_progress.md` still reads the 12/324 baseline; expected delta on next `tests/scripts/v2-progress.sh` run once Slice C wires dispatch is +5 to +15 (analyzer alone can't move differential counts without emission).

The un-changed target list this slice underwrites (unlocks land with Slice C dispatch on top of the analyzer):
- `type-001` through `type-022` (22 cases; deliberate type-promotion stress)
- `cond-003` (`when` without `otherwise` → nullable), `cond-004`/`005` (coalesce nullability), `cond-009` (`nullif`), `cond-010`/`011` (isnan/nanvl)
- `agg-013` (percentile_approx return type), `agg-018` (multi-agg aliases), `agg-019` (agg over expression), `agg-020` (count_if)
- `chain-004` ⌂
- `arr-005` ⌂, `agg-011` ⌂ (`collect_list`/`collect_set`)
- `misc-001` ⌂ (`describe`), `misc-002` ⌂ (`summary`), `misc-006` ⌂ (`crosstab`), `misc-007` ⌂ (`freqItems`)
- `ord-006` ⌂ (`sortWithinPartitions`)
- `struc-004` ⌂ (`withMetadata`)
- `agg2-001` ⌂ (`any_value`), `agg2-002` ⌂ (`array_agg`), `agg2-005` ⌂ (`histogram_numeric`)
- `meta-002` ⌂ (`spark_partition_id`), `meta-004` ⌂ (`input_file_name`)
- `dt-001` ⌂ (`current_date`/`current_timestamp`), `math-015` ⌂ (`rand`), `intv-006` ⌂ (`timestampadd`/`diff`)
- `samp-001` ⌂, `samp-002` ⌂
- `meta-001` ⌂ (`monotonically_increasing_id`)

**Progress signal target: 12 → ≈45–55.** Roughly 22 (type_inference) + 12 (schema-only) + 12 (nondeterministic) + ~5 (analyzer-derived cond/agg) — precise count depends on how many nondeterministic cases already pass at baseline via infer_schema.

**Slice C — Core emission (first tranche).** *Slice C landed 2026-07-01 across 2 passes.* The
architect proposed a within-slice sub-split (`.agent-output/001-architecture-plan.md` §0) — C.1
(substrate + operator emission + Slice-B carryover close) and C.2 (scalar-expression rows + seam
drain). The iteration methodology honors the split: each sub-slice is its own pass with its own
carryover. **Pass 1 = C.1 substrate** (commit `208e9b1`). **Pass 2 = C.2 scalar rows + seam drain**
(commit forthcoming).

**C.1 shipped**: `lowering.rs` (new file, 29-variant `LogicalPlan → CommonAst` adapter with
`CommonOp::Punt` for unsupported variants); `emission.rs` grew into a hand-written `match` over
`TypedOp` in `dispatch_op` with per-op renderers (`render_project`, `render_filter`,
`render_sort`, `render_limit`, `render_tail`, `render_distinct`, `render_with_columns`,
`render_drop_columns`, `render_aliased_relation`, `render_table_scan`, `render_local_relation`,
`render_range_relation`, `render_union`, `render_intersect`, `render_except`, `render_aggregate`)
delegating scalar expressions to `SqlGenerator::gen_expr` via a `render_expr` seam explicitly
marked for C.2 drain; `EmittedSql` newtype with module-private `emit()` constructor gives INV2
teeth by type construction; `EmissionError` (`UnsupportedOp`/`ChildFailed`/`MissingField`/
`LegacyRenderFailed`); `mod.rs` gained `pub fn generate(plan, base_types)` composing
`lower → analyze → dispatch`; `set_serializer_tap` renamed to `set_emit_tap` (deprecated alias
retained), `EMIT_TAP` atomic. `service.rs` `TranspilerPath::V2` arm now dispatches through
`transpiler_v2::generate` with a synchronous `build_base_types_from_plan` walk and a
`is_v2_fallback_eligible` predicate (Punt / `UnknownTable` / `UnsupportedOp` → legacy fallback;
everything else surfaces). `error.rs` gained `V2Lowering` / `V2Analyzer` / `V2Emission` variants.
All six Slice-B mediums closed: M1 walker Star fix, M2 RIGHT-USING dedup + Pass-3 ordinal
rewrite, M3 documented at `render_union` (widened schema wins), M4 `ast::Union.by_name` +
Pass-2 name-match reorder, M5 `AmbiguousColumn` + `TypeMismatch` wired (Pass-2 Project +
Filter), M6 Pass-1 seed removal. Invariants activated: **INV2** (`inv2_dispatch_is_only_sql_writer`
installs a counting tap, asserts exactly-once) and **INV3** (grep-based invariant test asserts
`emission.rs` does not `use crate::functions::FunctionRegistry` or glob-import
`crate::generator::*`, plus positive coverage anchors for every `render_<op>` helper and
`dispatch_op` / `pub fn dispatch(`). Perf **OPT-M1** applied (`quote_ident` no-quote fast path
mirroring legacy). Pipeline: 2 passes — iteration 1 verdict `NEEDS_CHANGES` (2 Critical: C1
half-declarative `EMISSION_TABLE` scaffolding-without-interpreter, C2 `AggregateCall.is_distinct`
dropped; 2 High: H1 aliased-relation alias not emitted, H2 INV3 grep test dishonest); iteration 2
verdict `APPROVED` (all 10 items closed — C1 fixed by deleting the scaffolding rather than
adding an interpreter, C2 fixed via `inject_distinct` helper, H1 emits `AS <ident>` + column
list, H2 grep tightened + `TODO Slice C.2:` seam markers). Tests: 230 core + 14 connect-server.
Progress signal **not re-measured** (per methodology, only at final termination); `core_v2`
`v2_progress.md` still at 12/324 baseline.

**C.2 shipped**: Approach A chosen per plan §1 (hand-written per-variant / per-function `match`
arms; no declarative-row substrate — dead-data lesson from Pass 1 iteration 1 applied). `render_expr`
became an exhaustive match over all 27 `Expression` variants; `render_function_call` grew ~130
lowercased-name arms hand-copied verbatim from `FunctionRegistry` (str, math, dt, cond, aggregate
shapes). The `SqlGenerator::gen_expr` seam is drained: `use crate::generator::SqlGenerator`
removed from `emission.rs`, `.with_schema_for_v2(` / `SqlGenerator::new()` / `.gen_expr(` all gone.
`EmissionError::LegacyRenderFailed` deleted; new `UnsupportedExpression` / `UnsupportedFunction`
variants are fallback-eligible in `service.rs`. Spark-parity CASTs: `spark_return_cast` for
projection-slot decisions (int/int Div → DOUBLE incl. the aliased-Div case fixed in iteration 2);
`spark_aggregate_return_cast` for integer SUM/AVG return-type wrapping inside `render_aggregate`.
INV3 tightened (8 grep rejections + 26-entry `REQUIRED_RENDERERS` coverage anchor). M5 closed via
a module-scoped `EMIT_TAP_MUTEX` (no new dep); M6 closed via `render_tail` CTE rewrite;
UpdateFields walking added to `ensure_no_ambiguous_columns`; Union / Intersect / Except gained a
per-column CAST wrapper (`maybe_wrap_widened_child`). OPT-M2 closed by seam-drain subsumption;
OPT-M3 closed via `plan_has_empty_scan` short-circuit in `service.rs` + a fallback-only contract
doc-comment on `pub type BaseTypes` in `analyzer.rs`. Two review iterations: iteration 1
`APPROVED` with 2 CLOSE_NOW-in-this-pass Mediums (M1 qualified `Star` in projection slot, M4
aliased Div CAST) plus M2 log correction; iteration 2 closed all three. Perf verdict `OPTIMIZED`
(0 HIGH + 0 MEDIUM); the perf agent noted the seam drain silently absorbed OPT-M2 and Pass-1's
L1 wins. Files changed (Pass 2): 5 (`emission.rs`, `invariants.rs`, `analyzer.rs`, `service.rs`,
`generator/mod.rs`); +1947 / -125 lines; 42 tests added (269 core + 14 connect-server pass).
Progress signal **12 → 134 core_v2 passing** (+122 cases) at commit `28f74b4` per
`tests/integration/v2_progress.md`. Below the initial-prompt estimate of 180-200 but
substantial validation of Slice C — every `str-*`/`math-*`/`dt-*`/`cast-*` case that
lowers cleanly, plus most `proj-*`/`filt-*`/`ord-*`/`set-*` and primitive `agg-*`,
now passes end-to-end on the DataFrame corpus. **Legacy TPC-H regression check: 51/51
PASSED** — legacy `SqlGenerator` behavior unchanged. The 46-case gap between measured
(134) and estimated (180) is the honest cost of the DEFER carryover: some corpus cases
require extension functions (Slice D), the full join cluster (Slice E), complex types
(Slice F), or verticals (Slice G) that intentionally remain punted.

**Cumulative DEFER carryover to future slices** (from Pass 2 review + perf):
- **Extension functions** (`spark_*`, `try_cast`, `try_divide`, `spark_sum`/`spark_avg` on
  decimal) — Slice D.
- **Full join cluster** (`Join` still `UnsupportedOp` in `dispatch_op`) — Slice E.
- **Complex-type emission** (Array/Map/Struct literals, HOF lambdas, ExtractValue,
  RowConstructor) — Slice F.
- **Vertical extensions** (Window / subqueries / Interval / Between / InList / Like / IsDistinctFrom
  / `to_utc_timestamp` / `from_utc_timestamp` / `extract` spark4 syntax) — Slice G.
- **Command / write path** (`UpdateFields` emission, na.fill / na.drop / na.replace operator
  arms) — Slice H (or an earlier operator-level slice that expands `CommonOp`).
- **INV1 full activation** — new differential-harness slice (Pass-1 INV1 stub remains).
- **INV2 escape-hatch dimension** — ADR-007 slice (`C_ESCAPE_HATCHES: &[]` remains empty).
- **Subquery-body walking** in `ensure_no_ambiguous_columns` — Slice G.
- **M3 (alias-in-fn-args)** — parity with legacy; Slice G defensive hardening.
- **M5 review (Binary CAST precedence for DATE+INTERVAL)** — parity with legacy; benign today.
- **M6 review (non-agg DISTINCT check)** — defensive hardening.

**C.2 acceptance targets** (unchanged from original list): projection-CAST-dependent cases in the
`cast-*` / `str-*` / `math-*` / `dt-*` / `agg-*` / `set-*` clusters. Expected progress delta:
12 → ≈180–200 once measured.

**DEFER carryover to C.2** (`tasks/v2-slice-c-iteration-log.md` for details): M5 (global
`EMIT_TAP` not test-isolated — latent flake), M6 (`render_tail` embeds `child_sql` twice —
legacy has same shape), L1 (`render_expr` allocates fresh `SqlGenerator` per call — dies with
the seam), `UpdateFields` walking in `ensure_no_ambiguous_columns`, subquery-body walking for
ambiguity (Slice G), union per-column CAST wrapper for widened schema, and the
`SqlGenerator::gen_expr` seam drain itself. Perf **OPT-M2** (schema clone in `render_expr`) and
**OPT-M3** (`build_base_types` unconditional clones) deferred to C.2 — both interact with code
C.2 restructures.

**Note.** The list below is C.1+C.2 combined; C.1 alone lands the operator-level surface. Cases
whose only projection expressions render unchanged through legacy `SqlGenerator::gen_expr` turn
green with C.1 dispatch; the projection-CAST-dependent cases wait for C.2.

Original unlock list:
- `proj-001..015` (15)
- `filt-001..015` (15)
- `cast-001..011` (11; `cast-012` is `spark4`+extension, deferred to Slice D)
- `cond-001`/`002`/`006`/`007`/`008`/`012`/`013`/`014`/`015`/`016` (10; `na.fill`/`na.drop`/`na.replace` + basic `when`)
- `str-001..020` (20; `str-020` `spark4`)
- `math-001..014` (14; `math-015` nondet ⌂ Slice B, `math-016` extension Slice D)
- `dt-002..017` (16; `dt-001` nondet ⌂ Slice B)
- `agg-001..006` (6; count/count-distinct/approx-count-distinct/min/max/mean-basic), `agg-010` (first/last), `agg-014..017` (4; mode + groupBy variants)
- `ord-001..005`, `ord-007`, `ord-010..012` (9; excluding schema-only variants)
- `set-001..010` (10)
- `misc-003..005`, `misc-008..010` (6; incl. the three `cosmetic`-flagged which are free once no-op emit exists — ADR-001)
- `struc-001`, `struc-002`, `struc-005`, `struc-006` (4)

**Progress signal target: ≈45–55 → ≈180–200.**

**Slice D — Extension dispatch.** Unlocks Spark-divergent-semantics cases, split into two phases per the 2026-07-01 up-front audit (`tasks/duckdb-extension-specs/README.md` for the handoff convention):

**Phase 1 acceptance (ext4-only; landable immediately):**
- `hash-001..003` (3; `hash-001`/`002` via native DuckDB md5/sha1/sha2/crc32; `hash-003` via `spark_hash`/`spark_xxhash64` already in ext4)
- `agg-007` (`sum(decimal)` → `spark_sum` — routing only, function already in ext4)
- `agg-008` (`stddev`/`variance` family — native DuckDB, already partially wired)
- `agg-013` (`percentile_approx`/`median` — native DuckDB)
- `math-011` (`int/int → DOUBLE` correction — already handled by `spark_return_cast`)
- `type-005` + Div-in-`chain-*` (`spark_decimal_div` routing in `render_binary` — function already in ext4)
- `agg-009 skewness` half (`spark_skewness` already in ext4)

**Verify-first cases (Phase 1 or Phase 2 depending on DuckDB parity check):**
- `agg-009 kurtosis` half — DuckDB has `KURTOSIS_POP`; if it matches Spark's excess-kurtosis Fisher's-definition semantics, wire native; else spec `spark_kurtosis` (spec file already drafted).
- `agg2-006 count_if` — DuckDB has `COUNT_IF`; if its NULL-as-FALSE semantics match Spark, wire native; else spec `spark_count_if`.

**Phase 1 progress signal target: 134 → ~140-142.** Post-halt-and-flag delta update (2026-07-01): **134 → 151 after Slice C.3-4 + Slice C.3-3 landed** — C.3-4 delivered +15 (the `LocalRelation` Decimal marshalling fix that closed `type-005`/`type-003`/`type-004` plus ~12 other silently-NULL'd decimal-payload cases), C.3-3 delivered +2 (`agg-020` + `agg2-006` via the `count_if` symmetric-omission fix). The halt-and-flag audit blocker is **partially resolved**; the remaining C.3-1/2/5/6 fixes still gate the Phase 1 hash/sum/percentile target cases.

**Phase 2 acceptance (post-`thdck_spark_funcs ext5` pin):**
- `math-016` (`try_divide` → `spark_try_divide`)
- `cast-012` (`try_cast` → `spark_try_cast` OR native `TRY_CAST` if verify-first confirms parity)
- `agg-012` (`corr`/`covar_samp` → `spark_corr`/`spark_covar_samp`, or native if verify-first confirms parity)
- `agg2-003` (`regr_slope`/`regr_r2` → `spark_regr_slope`/`spark_regr_r2`, or native)
- `agg2-004` (`try_sum`/`try_avg` → `spark_try_sum`/`spark_try_avg`)

**Phase 2 progress signal target: ~142 → ~145-148.**

**Pending C++ extension work (`ext5` release):** 10 spec files pre-drafted in `tasks/duckdb-extension-specs/` — the handoff artifact for the separate `thunderduck-duckdb-extension` project session. Blocked corpus cases: `math-016`, `cast-012`, `agg-012`, `agg2-003`, `agg2-004` (definite); potentially `agg-009 kurtosis` and `agg2-006` (verify-first cases). Once `ext5` ships and this repo pins the new binary, Slice D Phase 2 wires the newly-available functions and closes.

**Cumulative Slice D progress signal target: 134 → 145-148.** (Not the naive +14 the pre-audit estimate assumed; the honest number accounts for corpus cases whose Spark parity depends on functions not yet in a pinned extension release.) **INV6 gains partial teeth in Phase 1** (over the ext4-available subset) and **full teeth in Phase 2** (over the ext5 additions).

**Slice E — Outer-join nullability + set-op widening.** Unlocks:
- `join-001..014` (14; `join-014` is `cosmetic`-flagged broadcast hint, free)
- `chain-001`, `chain-003`, `chain-005`, `chain-006` (4; join-dependent integration chains)
- `type-011` (outer-join column becomes nullable), `type-019` (set-op widening int ∪ decimal), `type-020` (least-common-type array literal)

**Progress signal target: ≈210–220 → ≈240–250.**

**Slice F — Complex-type emission.** Unlocks:
- `arr-001..017` (17; array constructors, explode, HOF-adjacent)
- `map-001..007` (7; map constructors, explode)
- `struct-001..008` (8; struct constructors, field access, star-expand)
- `hof-001..010` (10; transform/filter/aggregate/exists/forall/zip_with/map_filter/transform_values/transform_keys)
- `arr2-001..005` (5), `map2-001..002` (2), `inl-001..002` (2)
- `chain-002`, `chain-004` (2; explode+HOF chains — the second already schema-only from Slice B)

**Progress signal target: ≈240–250 → ≈295–305.**

**Slice G — Vertical extensions.** Any of the following can land independently:
- **Temporal/interval**: `intv-001..005` (5; `intv-006` schema-only from Slice B) + edge cases in `dt-004`/`dt-015` + `type-022` (`try_cast(lng as int)`)
- **Grouping/pivot**: `grp-001..006` (6), `piv-004..006` (3)
- **Windows**: `win-001..012` (12; `win2-002` time-window), plus window-dependent chain cases
- **JSON**: `json-001..008` (8)
- **Parsing**: `parse-001..007` (7; several `spark4`)

**Progress signal target: ≈295–305 → ≈320–324.**

**Slice H — Command/lakehouse writes.** **Zero corpus cases.** The DataFrame corpus is read-only in-memory; every write-path Case belongs to the SQL corpus (out of scope). Slice H's purpose is INV8 (external-access delegation) and INV9 (writable-requires-attached-provenance) activation, not `core_v2` movement.

**Slice I — Differential-harness activation.** **Zero corpus cases.** Progress-signal-neutral; INV1's byte-identical-input assertion is oracle machinery, not translation. Acceptance:
- `crates/core/src/transpiler_v2/invariants.rs::inv1_both_engines_receive_byte_identical_input` and `mod.rs`'s `set_serializer_tap` no longer carry `DEFER INV1 → differential-harness slice:` markers.
- `git grep 'DEFER INV1'` returns empty crate-wide.
- The ADR-015 harness in `tests/integration/` installs a real payload-hashing tap via `set_emit_tap` and diffs the byte-identical-input claim across at least one round-tripped fixture per front-end (SparkSQL and Connect-proto), per §CV.5.1's sub-invariant model.
- Sub-invariant scoping confirmed: the dispatch-is-only-writer companion (Slice C.1) and the differential-harness dimension (this slice) share the INV1 paragraph without contradiction.

**Slice J — ADR-007 escape-hatch enumeration.** **Zero corpus cases.** Progress-signal-neutral. Acceptance:
- `crates/core/src/transpiler_v2/mod.rs::C_ESCAPE_HATCHES: &[&str]` is non-empty and contains named, unique labels for every structural forced transliteration retained in the B layer per ADR-007.
- The `inv2_node_local_or_labeled_escape_hatch` test's uniqueness-and-non-empty check becomes load-bearing (currently vacuously-true on the empty slice).
- `git grep 'DEFER INV2'` returns empty crate-wide.
- Sub-invariant scoping confirmed: the dispatch-is-only-writer companion (Slice C.1) and the escape-hatch dimension (this slice) share the INV2 paragraph without contradiction.

---

## 4. Sequencing rationale

Five citations from §CV.2's dependency matrix and §CV.4's load-bearing assumptions explaining why moving any slice earlier violates a prereq:

1. **Slice C (emission) depends on Slice B (analyzer) — §CV.2 edge `009 ← 005`, LB1.** Moving emission before the analyzer means the dispatch table has no operand types to key on. LB1 says the divergent slice *is* {type, nullability}; without those flowing through the AST, every emission decision is a guess. Concretely: `cast-006` (`cast(salary AS DECIMAL(12,4))`) needs to know `salary` is DOUBLE to route to the correct DuckDB cast, and only the analyzer can carry that fact.

2. **Slice B (analyzer) depends on Slice A (AST) — §CV.2 edge `005 ← 003`, `005 ← 004`.** The analyzer is `infer : (CommonAST, BaseTypes) → TypedAST` (ADR-005 line 141). Without ADR-003's IR being real, the analyzer has no input; without ADR-004's front-end convergence, the two front-ends produce different inputs and the analyzer would need per-front-end logic — INV7 forbids this.

3. **Slice D (extensions) depends on Slice C (emission) — §CV.2 edge `010 ← 005`, and reinforced by INV6.** The dispatch table's `Extension(name)` cells are *rows* in ADR-009's table (ADR-009 line 223: "the emission target — native DuckDB op, native-with-casts, or extension function — is data"). Growing the extension set before the table exists is putting variants in an enum that has no discriminant.

4. **Slice E (joins with nullability) depends on Slice B (analyzer) — §CV.2 edge `009 ← 005` again, plus the nullability half of LB1.** Outer-join nullability (per `type-011`, `join-003..005`) is not deducible at emission time; it requires the analyzer to have marked the right side's columns nullable during a left-outer. Attempting to add this to the emitter creates a non-local decision, which INV2 forbids.

5. **Slice H (writes) depends on Slice A + Slice B — §CV.2 edges `011 ← 004`, `017 ← 005`, `018 ← 005`.** Write emission needs both the parsed command (front-end → AST) and the type-checked target schema (analyzer). Additionally, INV9's structured error must fire *before* SQL is generated, which requires the command arm to have a resolved AST to inspect. Moving Slice H earlier would produce writes that emit SQL for path-scan relations and only fail at DuckDB — bypassing the invariant.

6. **The whole map is scoped by ADR-020 (strict-only, already landed) — §CV.2 edge `020 ← 010`.** Every emission decision below assumes the extension is loaded. If the pipeline reverts to relaxed mode, every `Extension(...)` row in Slice C/D/G becomes conditional and INV6 loses its teeth. This is why ADR-020 was ratified before Slice A begins.

---

## 5. Quick-wins highlight

Top three slices ordered by green-count-per-implementation-effort against the current 12/324 baseline. The first two are the natural `/new-feature` targets to break past **50/324** and **200/324** respectively.

### Quick-win 1 — Slice B (analyzer)

**Estimated delta: +30 to +45 cases (12 → ≈45–55).**

Why it's a quick win: unlocks the entire schema-only + nondeterministic bucket (which already has most of the ref-side machinery in place) plus the 22 `type-*` cases that are the ADRs' explicit test surface (LB1). The analyzer is bounded work per ADR-006 (a fixed sequence of passes, not an iterate-to-fixed-point engine); the type-coercion lattice and nullability derivation are separately-testable sub-units. INV4 (inference validated in isolation) is *specifically* set up to validate this slice before emission has to be correct in concert.

**First `/new-feature` prompt should cite:** ADR-005 (analyzer scope), ADR-006 (pass structure), INV4 (isolation test target), INV5 (schema-everywhere predicate).

### Quick-win 2 — Slice C (core emission first tranche)

**Estimated delta: +130 to +150 cases (≈45–55 → ≈180–200).**

Why it's a quick win: the "trivial" emission surface (projection, filter, cast, basic string/math/datetime, primitive aggregates, set ops, ordering) is dense in the corpus (≈160 of 324 cases) and the dispatch rows are largely 1:1 Spark→DuckDB mappings. The legacy generator already has all of these — this slice is effectively porting those mappings into ADR-009's declarative table form, not inventing new emit paths. INV3 (single-source-of-truth table) gains teeth as the rows land.

**First `/new-feature` prompt should cite:** ADR-009 (declarative table shape), INV3 (single source of truth), and reference the existing legacy generator's mapping table as the row source. Skip complex types, joins-with-nullability, windows, and extensions — those are Slices E/F/D respectively.

### Quick-win 3 — Slice D (extension dispatch)

**Estimated delta: +15 to +25 cases (≈180–200 → ≈205–220), but smallest per-row cost.**

Why it's a quick win: each `Extension(name)` row is ~3 lines added to the emission table plus a name in `extension_targets()`. The C++ extension already exports the functions (per ADR-020's "extension is mandatory" post-audit); the work is naming them. INV6 gains its full checkable form (loaded extension vs. table entries) with a single test run.

**First `/new-feature` prompt should cite:** ADR-010 (extension gap-filler role), INV6 (extension-target existence), and the `thdck_spark_funcs` release notes for the current exported set.

**Cumulative after Q1+Q2+Q3:** 12 → ≈205–220 (≈65% of the corpus green) with three focused pipeline runs.

---

## 6. Invariant activation map

For each of INV1–INV9, the slice from §1 that turns its stub (`crates/core/src/transpiler_v2/invariants.rs`) into a load-bearing check. When that slice lands, the corresponding `TODO INV<N>` marker can be deleted and the vacuous body replaced with the real assertion the plan already stubs.

| INV | §CV.5 name | Activates in | Stub deleted |
|---|---|---|---|
| **INV1** | byte-identical input to both engines | Slice I (differential-harness activation) — full activation; sub-invariant companion (single-writer) already active from Slice C.1 per §CV.5.1 | The ADR-015 harness sets a real payload-hashing tap via `set_emit_tap`; the `DEFER INV1 → differential-harness slice:` markers in `invariants.rs` + `mod.rs` are deleted |
| **INV2** | node-local or labeled C escape hatch | Slice C.1 (dispatch-is-only-writer companion, landed 2026-07-01) + Slice J (ADR-007 escape-hatch dimension) per §CV.5.1 sub-invariant model | `inv2_dispatch_is_only_sql_writer` uses a counting `EMIT_TAP` (test-serialized via `EMIT_TAP_MUTEX`), landed with C.2 M5; Slice J populates `C_ESCAPE_HATCHES: &[&str]` and deletes the `DEFER INV2 → ADR-007 slice:` markers, making the uniqueness/non-empty check load-bearing |
| **INV3** | single emission table | Slice C.1 (activated 2026-07-01) + Slice C.2 (tightened 2026-07-01) | `emission.rs` `dispatch_op` match + `render_expr` exhaustive match are the single source of truth; grep rejects `SqlGenerator` / `FunctionRegistry` imports and their transitive-use forms (`SqlGenerator::new()`, `.gen_expr(`, `.with_schema_for_v2(`) after C.2's seam drain; `REQUIRED_RENDERERS` coverage anchor names every renderer helper |
| **INV4** | inference validated in isolation | Slice B *(landed 2026-07-01)* | `inference_smoke()` iterates the five `analyzer_fixtures` mini-corpus and panics with per-field diffs on any schema mismatch; `inv4_inference_isolation` calls it |
| **INV5** | schema everywhere | Slice B *(landed 2026-07-01)* | `has_resolved_schema(&TypedAst)` walks every op's schema, every `projection_types`/`grouping_types`/`aggregate_types` `TypedAttr`, and rejects `DataType::Unresolved`; two-part test in `inv5_no_unresolved_after_analyzer` verifies both happy-path and planted-`Unresolved` slot detection |
| **INV6** | extension targets exist | Slice D Phase 1 (ext4 subset, landed 2026-07-01) + Slice D Phase 2 (post-ext5) | `extension_targets()` empty slice becomes non-empty; the DuckDB `duckdb_functions()` diff becomes a hard check. Phase 1 landed a 6-entry allow-list (`spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`, `spark_decimal_div`) and the containment check turned green; Phase 2 extends the list with the ext5 additions |
| **INV7** | both front-ends produce the same AST | Slice A | The (`sql_str`, `expected_ast_root`) fixture gains entries as the SparkSQL parser and Connect deserializer both normalize |
| **INV8** | external access is delegated | Slice H (ADR-013) | `external_emit_paths()` empty slice becomes non-empty; the exhaustive match becomes structurally load-bearing |
| **INV9** | writable requires attached provenance | Slice H (ADR-011 + ADR-017) | `emit_write()` stub (empty SQL for Attached, `ReadOnlyProvenance` for PathScan) becomes real SQL emission gated on provenance |

**Cross-check.** Every slice in §1 activates at least one invariant, and every invariant is activated by exactly one slice (INV2 and INV3 both activate at Slice C — INV3 as soon as `EmissionTable` gains a real body, INV2 as soon as any non-node-local decision gets recorded). This closes the loop between the ADR readiness plan and the scaffolding already on disk: landing a slice deletes both its ADR obligations and its INV `TODO` markers in one pass.

---

## Notes on measurement

- `tests/scripts/v2-progress.sh` records the `core_v2` PASSED count after every `/new-feature` slice lands. Slice A adds one baseline row (12/324 stays 12/324 — build-only). Each of Slices B/C/D/E/F/G adds one row with a measurable jump. Slice H does not move the number.
- The agent-pipeline `## Quality Gate` in `CLAUDE.md` excludes the differential suites, so `v2-progress.sh` is a **separate manual measurement** — invoke it after each slice's PR merges, before writing the summary.
- The `all` differential suite still exercises TPC-H + TPC-DS on the legacy path; those must remain green throughout (legacy is not touched by any slice in this map).
- `spark4` cases assume the pinned Spark 4.1.1 reference; ADR-016 governs the pin. No slice here changes the pin.
