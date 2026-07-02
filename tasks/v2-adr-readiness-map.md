# v2 Transpiler ADR Readiness Map (Restart Track)

**Purpose.** Order the rearchitect ADRs (`docs/thunderduck-rearchitect-ADRs.md` ADR-000 → ADR-021) into implementation slices, and map each slice to the corpus Case IDs (`tests/integration/differential/dataframe_corpus.py`, 324 cases) it should green. A developer reading §3 alone should be able to pick a slice, invoke `/new-feature` citing the ADRs it owns, and know exactly which Case IDs must turn green when their commit lands.

**Restart context (2026-07-02).** The morph-track implementation is discarded (tag `v2-morph-track-end`) in favor of a fresh implementation under ADR-021's substrate-independence commitments. The design work — every ADR, every lesson in `tasks/lessons.md`, the differential harness, the methodology in `tasks/v2-slice-iteration-methodology.md`, the extension pin at `crates/core/build.rs:33` (ext6), the corpus at `tests/integration/differential/dataframe_corpus.py` — is preserved intact. Only the implementation code under `crates/core/src/transpiler_v2/` is deleted. The morph-track high-water mark of 153/324 is empirical validation of the architectural concepts; the concepts survive as design confidence, not as code.

**Inheritance discipline.** The morph track surfaced ~10 concrete bugs (analyzer symmetric-omissions, RelationConverter marshalling gaps, emission-arm shape corrections). Every one is enumerated in `tasks/v2-restart-inheritance-checklist.md` with commit provenance. **A slice's Pass 1 architect MUST cite the applicable checklist items and scope them into the slice's plan.** A slice's reviewer MUST verify each applicable item is present in the implementation before returning APPROVED. Without this discipline, the restart re-discovers fixed bugs at the corpus differential and burns `/fix-bug` pipeline passes on already-known issues.

**Baseline.** `core_v2` restarts at 12/324 (legacy fallback for every case except analyzer-only-passing metadata/schema-only cases). Progress is measured via `tests/scripts/v2-progress.sh`; a monotonic climb toward 324 is the signal. Legacy TPC-H stays 51/51 throughout — legacy path is untouched by the restart.

**Constraint.** INV1–INV10 all have stubs in `crates/core/src/transpiler_v2/invariants.rs` (created by Slice A). Each carries a `TODO INV<N>:` (this-slice-owned) or `DEFER INV<N> → <slice>:` (reassigned) marker per §CV.5.1. §6 below says which slice makes each invariant stop being vacuous.

---

## 1. Layered slice sequence

Slices are named in the strict order §CV.2's dependency matrix imposes. Each names the ADRs it owns, its stub deliverables inside `crates/core/src/transpiler_v2/` (and `crates/connect-server/src/converter/` for Slice A's protobuf converter), and its one-line position justification.

**Slice A — v2 substrate independence.** Owns ADR-003 (common AST), ADR-004 (protobuf-boundary dispatch), ADR-021 (substrate independence), and INV10 (input-side barrier). Delivers:

1. `crates/core/src/transpiler_v2/expression.rs` — v2 `Expression` enum, 1:1 initial seed of legacy `crate::expression::Expression`'s 21 variants. Includes `data_type(&Schema) -> DataType` and `nullable(&Schema) -> bool` methods (Spark parity).
2. `crates/core/src/transpiler_v2/type_inference.rs` — v2 `TypeInferenceEngine`, 1:1 initial seed of legacy `crate::types::TypeInferenceEngine`. **Includes the inheritance-checklist §1.1–1.3 items on day 1** (count_if, hash/xxhash64/murmur3, corr/covar/regr → Double family).
3. `crates/core/src/transpiler_v2/ast.rs` — v2 `CommonAst` / `CommonOp` enum, carrying v2 `Expression` payload. Structured variants for `Values`, `LocalRelation`, `TableFunction`/`Unnest`, `FileScan`, `Join { left_plan_ids, right_plan_ids, ... }` — see checklist §2.2, §2.3.
4. `crates/connect-server/src/converter/v2_relation_converter.rs` — `V2RelationConverter` producing v2 CommonAst directly from Spark Connect protobuf. **Exhaustive typed dispatch for Arrow values (no silent-NULL catch-all — see checklist §2.1).** No `Sql` opaque variant emitted for any of the 6 legacy SqlRelation shortcut categories (see checklist §2.2).
5. `crates/core/src/transpiler_v2/{mod, invariants}.rs` — module wiring, `pub fn generate(plan: &CommonAst, base_types: &BaseTypes) -> Result<String, ...>`, invariants scaffold.
6. `crates/connect-server/src/service.rs` — dispatch moved to the protobuf boundary. Legacy path calls `PlanConverter` → `SqlGenerator`; v2 path calls `V2RelationConverter` → `transpiler_v2::generate`. Applies `plan_has_empty_scan` short-circuit for BaseTypes seeding (checklist §5.5).

**Architect MAY propose within-slice sub-split (§CV.7).** Natural boundaries:
- A.1: v2 Expression + v2 TypeInferenceEngine (types substrate — no plan/protobuf work yet).
- A.2: v2 CommonAst + V2RelationConverter (plan substrate + protobuf ingestion).
- A.3: dispatch relocation + wiring in service.rs.

Activates: INV10 (grep check: `use crate::{logical,expression}::` and `use crate::types::TypeInferenceEngine` return zero in `transpiler_v2/` + `v2_relation_converter.rs`). All other invariants stubbed with `TODO INV<N>:` markers.

**Progress signal target: 12/324 → 12/324** (build-only, dispatch active but no analyzer/emission).

**Slice B — Type & nullability analyzer.** Owns ADR-005, ADR-006. Delivers `crates/core/src/transpiler_v2/analyzer.rs` with:

1. `TypedAst`, `TypedAttr`, `Schema`/`BaseTypes` aliases, sealed `HasSchema` trait, `AnalyzerError` (six `thiserror` variants: `UnknownTable`, `UnknownColumn`, `AmbiguousColumn`, `TypeMismatch`, `PuntedOperator`, `Other`).
2. Three bounded passes: (a) `resolve` (bottom-up structural), (b) `assign_types` (bottom-up + one downward sub-sweep for `Union` widening per ADR-006), (c) `derive_nullability` (outer-join propagation, `when`-without-`otherwise`, coalesce-of-nullables, aggregate return nullability).
3. `analyzer_fixtures.rs` — five input-relation fixtures (`emp`/`dept`/`emp2`/`nums`/`raw`) matching `dataframe_corpus.py::build_inputs`, plus 5+ mini-fixtures probing specific inference rules.
4. `inference_smoke()` — INV4 activation entry point. Panics with per-field diffs on schema mismatch.
5. `has_resolved_schema(&TypedAst)` — INV5 activation predicate. Rejects `DataType::Unresolved` anywhere in the tree.
6. `is_aggregate_function` (in the SparkSQL parser path) MUST include `count_if`, `try_sum`, `try_avg`, `try_divide` (checklist §1.4).

All type/nullability logic delegates to v2 `Expression::{data_type, nullable}` and v2 `TypeInferenceEngine` — NOT legacy (INV10 discipline).

Activates: INV4 (isolation smoke), INV5 (schema-everywhere walker).

**Progress signal target: 12/324 → ~15–25** (analyzer-only-passing schema_only/nondeterministic cases surface as green once dispatch runs; most cases still red until Slice C emission wires).

**Slice C — Core emission (first tranche).** Owns ADR-009. Delivers `crates/core/src/transpiler_v2/emission.rs` with:

1. `EmittedSql` newtype + `EMIT_TAP` counter + `EMIT_TAP_MUTEX` (checklist §5.3) — INV2 activation via dispatch-is-only-writer counting.
2. `dispatch_op(&TypedOp, &Schema) -> Result<String, EmissionError>` — exhaustive match over v2 `TypedOp` variants. Per-op renderers: `render_project`, `render_filter`, `render_sort`, `render_limit`, `render_tail` (CTE rewrite per checklist §5.4), `render_distinct`, `render_with_columns`, `render_drop_columns`, `render_aliased_relation`, `render_table_scan`, `render_local_relation`, `render_range_relation`, `render_values`, `render_file_scan`, `render_table_function`, `render_union`, `render_intersect`, `render_except`, `render_aggregate`.
3. `render_expr` — exhaustive match over v2 `Expression` variants.
4. `render_function_call` — ~130 scalar-function arms for `str-*` / `math-*` / `dt-*` / `cast-*` / `cond-*` clusters, plus primitive-aggregate arms. Includes checklist §3.1 (`sha`/`sha1`/`sha2` → `SHA256(arg_0)` — single-arg only) and §3.2 (`percentile_approx` → `approx_quantile({arg_0}, CAST({arg_1} AS FLOAT))`).
5. `render_cast` — includes `Cast::try_cast` branch emitting `TRY_CAST(expr AS ty)` (checklist §4.2 first item).
6. `spark_return_cast` (projection-slot level) + `spark_aggregate_return_cast` (aggregate-return level) — kept separate per checklist §5.1.
7. `quote_ident` no-quote fast path (checklist §5.6).
8. INV3 grep barrier: `use crate::generator::*` and `use crate::functions::*` in emission.rs return zero.
9. `extension_targets()` stub — empty at Slice C, populated by Slice D.

**Architect MAY sub-split.** Natural boundary: C.1 = operator arms + dispatch substrate; C.2 = scalar-function arms + return-type CASTs.

Activates: INV2 (dispatch is only writer, companion), INV3 (single emission table). INV1 (byte-identical input) stays stubbed with `DEFER INV1 → Slice I:`; INV2's escape-hatch dimension stays stubbed with `DEFER INV2 → Slice J:`.

**Progress signal target: ~15–25 → ~180–200.**

**Slice D — Extension dispatch.** Owns ADR-010. Populates `extension_targets()` with the 9-entry ext6-set (checklist §4.1) and wires the arms. Activates INV6 (extension targets exist in loaded `duckdb_functions()`).

Ext-arm targets (already-shipped in ext6):
- `math-016` — `try_divide` → `spark_try_divide`.
- `agg2-004` — `try_sum` / `try_avg` → `spark_try_sum` / `spark_try_avg`.
- (Plus the ext4-set inherited: `spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`, `spark_decimal_div`.)

Native-arm targets (native DuckDB matches Spark per verify-native-first discipline, checklist §4.3):
- `hash-001..003` — `md5`/`sha1`/`sha2`/`crc32` + `spark_hash`/`spark_xxhash64` (from ext4-set).
- `agg-007` — `sum(decimal)` via `spark_aggregate_rewrite` (checklist §5.7).
- `agg-008` — `stddev`/`variance` family native.
- `agg-009` — `skewness` (extension) + `kurtosis` → `KURTOSIS_POP` (native, population/excess, per checklist §4.2).
- `agg-012` — `corr` / `covar_samp` → `CORR` / `COVAR_SAMP` (native, checklist §4.2).
- `agg-013` — `percentile_approx` / `median` (native, per checklist §3.2 for the FLOAT CAST).
- `agg2-003` — `regr_slope` / `regr_r2` → `REGR_SLOPE(y,x)` / `REGR_R2(y,x)` (native, direction-sensitive).
- `agg2-006` — `count_if` → `COUNT_IF` (native, checklist §1.1 + §4.2).
- `cast-012` — `try_cast` → `TRY_CAST` (routed via `Cast::try_cast` flag; Slice C wires the arm).
- `math-011`, `type-005` — `int/int → DOUBLE` correction (via `spark_return_cast`).

**Progress signal target: ~180–200 → ~195–215.**

**Slice E — Outer-join nullability + set-op widening.** Owns join emitter with outer-join nullability + union type-widening rule (`int ∪ decimal → decimal` etc.). Unlocks:
- `join-001..014` (14; `join-014` cosmetic-flagged broadcast hint, free).
- `chain-001`, `chain-003`, `chain-005`, `chain-006` (4; join-dependent integration chains).
- `type-011` (outer-join nullability), `type-019` (set-op widening), `type-020` (least-common-type).

**No dormant-fix activator scope.** The morph-track's Slice E scope included "activate hash-002/agg-013/math-016/agg2-004 dormant v2 fixes via SqlRelation + AggregateSelectOrder lowering" — those don't exist in the restart because V2RelationConverter produces structured plans directly. hash-002/agg-013/math-016/agg2-004 turn green at Slices C/D, not Slice E.

**Progress signal target: ~195–215 → ~230–250.**

**Slice F — Complex-type emission.** Owns array/map/struct/HOF/inline emission. Unlocks `arr-001..017` (17), `map-001..007` (7), `struct-001..008` (8), `hof-001..010` (10), `arr2-001..005`, `map2-001..002`, `inl-001..002`, `chain-002`+`chain-004`.

**Progress signal target: ~230–250 → ~290–305.**

**Slice G — Vertical extensions.** Any of temporal/interval, grouping/pivot, windows, JSON, parsing can land independently. Unlocks `intv-*` (5), `grp-*`+`piv-*` (9), `win-*` (12+1), `json-*` (8), `parse-*` (7).

**Non-case-ID acceptance (defensive audits inherited from morph-track lessons):**
- `render_projection_slot` symmetry check.
- Alias-inside-fn-args parity with legacy (M3 review).
- Binary CAST precedence for DATE+INTERVAL (M5 review) — parity anchor.
- Non-agg DISTINCT usage audit (M6 review) — anchor for when new DISTINCT-eligible arms land.
- Subquery-body walking in `ensure_no_ambiguous_columns` — inherited from morph-track C.2 spillover.

**Progress signal target: ~290–305 → ~320–324.**

**Slice H — Command/lakehouse writes.** Owns ADR-011/012/013/017/018/019. **Zero DataFrame-corpus movement** (corpus is read-only in-memory). Activates INV8 (external access delegated) + INV9 (writable requires attached provenance).

**Slice I — Differential-harness activation.** Owns full INV1 activation. Wires the byte-identical-input assertion via the ADR-015 harness in `tests/integration/`. Deletes the `DEFER INV1 → Slice I:` markers. **Zero DataFrame-corpus movement.** Prerequisite: Slice A (substrate is stable).

**Slice J — ADR-007 escape-hatch enumeration.** Owns INV2's escape-hatch dimension. Populates `C_ESCAPE_HATCHES` with named unique labels for every structural forced transliteration retained in the B layer. Deletes the `DEFER INV2 → Slice J:` markers. **Zero DataFrame-corpus movement.**

**Not a slice — cross-cutting discipline.** ADR-000 (positioning), ADR-001 (transliterator-not-optimizer), ADR-002 (delegation boundary), ADR-007 (A/B/C layer contract; escape-hatch substrate is owned by Slice J), ADR-008 (correlated subqueries direct), ADR-014 (two decision spaces), ADR-015 (differential oracle harness; INV1 substrate owned by Slice I), ADR-016 (version pin), ADR-020 (strict-only, already landed), ADR-021 (substrate independence, activated by Slice A) are premises, disciplines, and testing architecture. They shape *how* Slices A–J are implemented; none unlocks corpus cases directly.

---

## 2. ADR → Case ID coverage table

| ADR | One-line scope | Case IDs unlocked | Count |
|---|---|---|---|
| ADR-000 | positioning (premise) | — | 0 |
| ADR-001 | transliterator, not optimizer | (constrains `cosmetic`-flagged cases: `join-014`, `misc-008..010`, `struc-003`, `ord-006`) | 6 (constraint) |
| ADR-002 | emit-level delegation | (enables schema threading via delegated `*`) | 0 direct |
| ADR-003 | common AST | (prereq to all) | 0 |
| ADR-004 | protobuf-boundary dispatch | (prereq to all) | 0 |
| ADR-005 | type & nullability analyzer | `type-001..type-022`, `cond-003..cond-011`, `agg-013`, `agg-018..020`, `set-009`, `chain-004` (schema-only), all `schema_only`/`nondeterministic`-flagged deterministic-ref cases | ≈35–45 |
| ADR-006 | bounded analyzer passes | (analyzer implementation discipline) | 0 |
| ADR-007 | A/B/C layer structure | (enables `C_ESCAPE_HATCHES` growth via Slice J) | 0 |
| ADR-008 | correlated subqueries direct | (corpus is DataFrame-only; correlated lives in SQL corpus) | 0 |
| ADR-009 | declarative emission table | `proj-*` (15), `filt-*` (15), `cast-001..011` (11), `cond-*` (16), `str-*` (20), `math-001..014` (14), `dt-*` (17), `agg-*` primitive (18), `grp-*` (6), `win-*` (12), `join-*` (14), `set-*` (10), `ord-*` (12), `arr-*` (17), `map-*` (7), `struct-*` (8), `hof-*` (10), `misc-*` (10), `piv-*` (3), `arr2-*` (5), `map2-*` (2), `inl-*` (2), `agg2-006`, `struc-*` (6), `win2-002` | ≈250 |
| ADR-010 | extension functions | `hash-*` (3), `agg-007`, `agg-008`, `agg-009`, `agg-012`, `cast-012`, `math-016`, `agg2-003..005` | ≈14 |
| ADR-011 | command arm | (DataFrame corpus has no DDL) | 0 |
| ADR-012 | catalog overlay | (corpus builds DFs in-session) | 0 |
| ADR-013 | external/lakehouse reads | (corpus uses createDataFrame, no path-scan) | 0 |
| ADR-014 | two decision spaces | (test-attribution discipline) | 0 |
| ADR-015 | differential oracle | (harness itself; enables INV1) | 0 |
| ADR-016 | version pin | (scoping) | 0 |
| ADR-017 | Delta append writes | (corpus is read-only) | 0 |
| ADR-018 | Iceberg UC writes | (corpus is read-only) | 0 |
| ADR-019 | lakehouse I/O contract | (composition) | 0 |
| ADR-020 | strict-only target | (already landed) | 0 (already in `main`) |
| ADR-021 | v2 substrate independence | (prereq to all v2 slices; enables INV10) | 0 direct |

**Numeric health check.** ADR-005 + ADR-009 + ADR-010 together account for ≈300 of 324 cases. The remaining ≈24 fall out of Slices F/G (interval, chain, pivot, structural, sampling, metadata). ADRs 011–019 unlock zero corpus cases; they exist to activate INV8/INV9 and serve the SQL corpus + write path, both explicitly out of scope for `core_v2`. ADR-021 is a substrate ADR — it enables every v2 slice but unlocks zero corpus cases directly.

---

## 3. Per-slice acceptance gate

Cases flagged `schema_only`/`nondeterministic` are marked ⌂ (schema-diff only under the harness).

**Slice A — v2 substrate independence.** *No case-ID unlocks by itself.* Acceptance:
- `crates/core/src/transpiler_v2/generate` no longer returns `Unsupported` for any input the connect-server dispatches with `THUNDERDUCK_TRANSPILER=v2`. Legacy still fully functional.
- INV10 grep passes (`git grep -E 'use crate::(logical|expression)::|use crate::types::TypeInferenceEngine' crates/core/src/transpiler_v2/ crates/connect-server/src/converter/v2_relation_converter.rs` returns zero).
- Every inheritance-checklist item in §2 (RelationConverter items) is present in V2RelationConverter by construction.
- Every checklist item in §1 (TypeInferenceEngine items) is present in v2 `TypeInferenceEngine` by construction — verified by unit tests in `type_inference.rs::tests`.
- Prerequisite for every subsequent slice.
- Progress signal: 12/324 → still 12/324 (build-only).

**Slice B — Analyzer.** Acceptance:
- `analyze()` returns `Ok(TypedAst)` for all corpus cases whose plan lowers without `Punt`.
- INV4 (isolation smoke) and INV5 (schema everywhere) tests pass on the 5+ analyzer_fixtures.
- Target case IDs whose AnalyzePlan-derived schema must match Spark: `type-001..022`, `cond-003..011`, `agg-013`, `agg-018..020`, `chain-004` (schema-only ⌂), + the `schema_only`/`nondeterministic` cluster (~15).
- Progress signal: 12/324 → ~15–25.

**Slice C — Core emission (first tranche).** Acceptance:
- Case IDs green on `core_v2`: `proj-001..015`, `filt-001..015`, `cast-001..011`, `cond-*` (12+), `str-001..020` (except `str-020` spark4→Slice G), `math-001..014`, `dt-002..017` (except `dt-001` ⌂), `agg-*` primitives, `ord-*` (except schema-only variants), `set-001..010`, `misc-003..005`+`misc-008..010`, `struc-001/002/005/006`.
- Every inheritance-checklist §3 (emission-arm shapes) and §5 (design patterns) item present.
- INV2 companion (`inv2_dispatch_is_only_sql_writer`) and INV3 grep barrier tests pass.
- Legacy TPC-H 51/51 unregressed.
- Progress signal: ~15–25 → ~180–200.

**Slice D — Extension dispatch.** Acceptance:
- Case IDs green: `hash-001..003`, `agg-007`, `agg-008`, `agg-009` (kurtosis half), `agg-012`, `agg-013`, `agg2-003`, `agg2-004`, `agg2-006`, `cast-012`, `math-011`, `math-016`, `type-005`.
- Every inheritance-checklist §4 item present (ext arms + native parity arms + verify-native-first discipline).
- `extension_targets()` returns 9 entries; INV6 containment check passes against loaded ext6.
- Progress signal: ~180–200 → ~195–215.

**Slice E — Outer-join nullability + set-op widening.** Acceptance:
- `join-001..014` (14) green (`join-014` free via ADR-001 cosmetic).
- `chain-001`/`003`/`005`/`006` (4) green.
- `type-011` (outer-join column nullable), `type-019` (int ∪ decimal), `type-020` (least-common-type array literal) green.
- Progress signal: ~195–215 → ~230–250.

**Slice F — Complex-type emission.** Acceptance:
- `arr-001..017` (17), `map-001..007` (7), `struct-001..008` (8), `hof-001..010` (10), `arr2-001..005` (5), `map2-001..002` (2), `inl-001..002` (2), `chain-002`/`chain-004` (2) green.
- Progress signal: ~230–250 → ~290–305.

**Slice G — Vertical extensions.** Acceptance (any subset landable independently):
- Temporal: `intv-001..005` (5) + dt-004/015 edges + `type-022`.
- Grouping/pivot: `grp-001..006` (6), `piv-004..006` (3).
- Windows: `win-001..012` (12), `win2-002`.
- JSON: `json-001..008` (8).
- Parsing: `parse-001..007` (7).
- Non-case-ID acceptance: defensive audits per §1 Slice G bullet list.
- Progress signal: ~290–305 → ~320–324.

**Slice H — Command/lakehouse writes.** Zero corpus cases. INV8 + INV9 activation.

**Slice I — Differential-harness activation.** Zero corpus cases. Acceptance:
- `git grep 'DEFER INV1'` returns empty crate-wide.
- The ADR-015 harness installs a real payload-hashing tap and diffs across at least one round-tripped fixture per front-end.
- Sub-invariant scoping confirmed per §CV.5.1.

**Slice J — ADR-007 escape-hatch enumeration.** Zero corpus cases. Acceptance:
- `crates/core/src/transpiler_v2/mod.rs::C_ESCAPE_HATCHES` non-empty, names unique.
- `git grep 'DEFER INV2'` returns empty crate-wide.
- Sub-invariant scoping confirmed per §CV.5.1.

---

## 4. Sequencing rationale

Each slice's dependency on prior slices, citing §CV.2 dependency edges and §CV.4 load-bearing assumptions:

1. **Slice B (analyzer) depends on Slice A (substrate) — §CV.2 edges `005 ← 003`, `005 ← 004`, `005 ← 021`.** The analyzer is `infer : (v2 CommonAst, BaseTypes) → v2 TypedAst`. Without ADR-003's IR being real (Slice A), the analyzer has no input; without ADR-004's dispatch relocation (Slice A), the analyzer never runs; without ADR-021's v2 `Expression` + v2 `TypeInferenceEngine` (Slice A), the analyzer has nothing to delegate to.

2. **Slice C (emission) depends on Slice B (analyzer) — §CV.2 edge `009 ← 005`, LB1.** LB1 says the divergent slice is exactly {type, nullability}; every emission decision keys on Spark-accurate types from the analyzer. Emission before analyzer is guessing.

3. **Slice D (extensions) depends on Slice C (emission) — §CV.2 edge `010 ← 009`, INV6.** The dispatch table's `Extension(...)` cells are rows in ADR-009's table. Growing the extension set before the table exists is putting variants in an enum without a discriminant.

4. **Slice E (joins with nullability) depends on Slice B (analyzer) — §CV.2 edge `009 ← 005`, LB1 nullability half.** Outer-join nullability is not deducible at emission time; it requires the analyzer to have marked columns nullable during a left-outer.

5. **Slice H (writes) depends on Slice A + Slice B — §CV.2 edges `011 ← 004`, `017 ← 005`, `018 ← 005`.** Write emission needs both the parsed command AND the type-checked target schema, plus INV9's structured error must fire before SQL emission.

6. **The whole map is scoped by ADR-020 (strict-only) and ADR-021 (substrate independence).** If either reverts, every emission decision below shifts. Both are ratified — ADR-020 in production, ADR-021 committed 2026-07-02.

---

## 5. Quick-wins highlight

**Quick-win 1 — Slice A (substrate).** ~3000 LOC. No corpus movement, but every subsequent slice is a no-op without it. Not really a "quick win" in the corpus sense, but the highest-leverage foundational work.

**Quick-win 2 — Slice B (analyzer).** ~2000 LOC. Estimated delta +3 to +15 corpus cases (schema_only/nondeterministic bucket surfaces once dispatch runs). First slice with a visible progress signal.

**Quick-win 3 — Slice C (core emission first tranche).** ~5000 LOC (dispatch scaffolding + operator arms + scalar-function arms). Estimated delta +160 to +180 corpus cases. **The single largest corpus-mover.**

**Cumulative after A+B+C:** 12 → ~180–200 (≈60% corpus green) with three focused slice sequences.

---

## 6. Invariant activation map

| INV | §CV.5 name | Activates in | Stub deleted |
|---|---|---|---|
| **INV1** | byte-identical input | Slice I | ADR-015 harness installs payload-hashing tap; `DEFER INV1 → Slice I:` markers cleared |
| **INV2** | node-local or labeled C escape hatch | Slice C (dispatch-is-only-writer companion) + Slice J (escape-hatch dimension) | Slice C activates `EMIT_TAP`/`EMIT_TAP_MUTEX` counting; Slice J populates `C_ESCAPE_HATCHES` |
| **INV3** | single emission table | Slice C | Grep rejects `use crate::generator::*`/`use crate::functions::*` in emission.rs; coverage anchor names every renderer helper |
| **INV4** | inference validated in isolation | Slice B | `inference_smoke()` iterates analyzer_fixtures with per-field diffs |
| **INV5** | schema everywhere | Slice B | `has_resolved_schema` walker rejects `DataType::Unresolved` anywhere in the tree |
| **INV6** | extension targets exist | Slice D | `extension_targets()` 9-entry allow-list checked against loaded `duckdb_functions()` |
| **INV7** | v2 front-ends produce same CommonAST | Slice A (both v2 front-ends in place) — refined per ADR-021 to v2-only scope | AnalyzePlan schema diff harness confirms V2RelationConverter + SparkSQL parser agree; legacy is out of INV7's scope |
| **INV8** | external access delegated | Slice H | `external_emit_paths()` non-empty; exhaustive match load-bearing |
| **INV9** | writable requires attached provenance | Slice H | `emit_write()` gated on provenance |
| **INV10** | v2 substrate independence | Slice A | Grep barrier tests reject `use crate::logical::`, `use crate::expression::`, `use crate::types::TypeInferenceEngine` from `transpiler_v2/` + `v2_relation_converter.rs` |

**Cross-check.** Every slice in §1 activates at least one invariant, and every invariant is activated by exactly one slice. Slice A is the "heavy" one — activates INV7 + INV10 + creates the stubs for all other invariants. Slice C activates INV2 + INV3. Slice B activates INV4 + INV5. Slice D activates INV6. Slices H/I/J activate the write-path/harness/escape-hatch invariants. Landing a slice deletes both its ADR obligations and its INV `TODO` markers in one pass.

---

## Notes on measurement

- `tests/scripts/v2-progress.sh` records the `core_v2` PASSED count after every slice's landing. Slice A adds one baseline row (12/324 stays 12/324). Slices B/C/D/E/F/G each add one row with a measurable jump. Slices H/I/J do not move the number.
- The agent-pipeline `## Quality Gate` in `CLAUDE.md` excludes the differential suites; `v2-progress.sh` is a separate manual measurement — invoke it after each slice's PR merges, before writing the summary.
- The `all` differential suite still exercises TPC-H + TPC-DS on the legacy path; those must remain green throughout (legacy is not touched by any slice in this map).
- `spark4` cases assume the pinned Spark 4.1.1 reference; ADR-016 governs the pin. No slice here changes the pin.

## Historical reference

The morph-track implementation lived on this branch through commit `0194178` (Slice D Phase 2 landing at 153/324) and was discarded at tag `v2-morph-track-end`. See `tasks/v2-restart-inheritance-checklist.md` for the bug-inheritance list distilled from the morph track's debugging arc. See `docs/dev_journal/2026-07-01-*.md` and `docs/dev_journal/2026-07-02-*.md` (once written) for the chronological narrative of the morph track. The morph-track iteration logs at `tasks/v2-slice-{c-*, c3-*, d-*}` were deleted at end-of-session 2026-07-01; the ADR file preserves the load-bearing design decisions from that arc.
