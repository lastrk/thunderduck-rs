# v2 Transpiler ADR Readiness Map (Restart Track)

**Purpose.** Order the rearchitect ADRs (`docs/thunderduck-rearchitect-ADRs.md` ADR-000 → ADR-022) into implementation slices with **only inherent dependencies**, expose parallel-track opportunities, and map each slice to the corpus Case IDs (`tests/integration/differential/dataframe_corpus.py`, 324 cases) it should green. A developer reading §1 + §7 alone should be able to (a) pick a slice, (b) invoke `/goal` citing the ADRs it owns, (c) know exactly which Case IDs must turn green when their commit lands, and (d) know which other slices can safely run in parallel.

**Position.** Per ADR-022, τ is the only production path. Legacy source may remain in the workspace as reference material during reimplementation, but does not compile as a service backend and is not exercised by tests; it is deletable at any point. There is no runtime fallback: τ's output is the response — generated DuckDB SQL, a Spark-emulated error, or a Thunderduck-boundary error (ADR-022's two-error-category rule). Preserved substrate for this map's work: the ADRs, `tasks/lessons.md`, the differential harness, the methodology in `tasks/v2-slice-iteration-methodology.md`, the extension pin at `crates/core/build.rs:33` (ext6), the corpus at `tests/integration/differential/dataframe_corpus.py`.

**Inheritance discipline.** `tasks/v2-restart-inheritance-checklist.md` enumerates concrete bug classes and design constraints (analyzer symmetric-omissions, RelationConverter marshalling gaps, emission-arm shape corrections) that each slice's implementation must present on day one. **A slice's Pass 1 architect MUST cite the applicable checklist items and scope them into the slice's plan.** A slice's reviewer MUST verify each applicable item is present before returning APPROVED. Without this discipline, the restart re-discovers fixed bug classes at the corpus differential.

**Open decisions.** All twelve decisions previously listed in `tasks/v2-restart-open-decisions.md` are **RESOLVED** as of 2026-07-02 — see that file for the resolution record. The largest cluster (Decisions 3, 4, 9, 11) is consolidated by **ADR-022**. Other resolutions land in the slice sections below.

**Baseline.** `core_v2` currently reports 153/324 (measured at commit `e604193`) because `THUNDERDUCK_TRANSPILER=v2` is a no-op post-morph-track-deletion and all requests fall through to the legacy path. Slice A.3 relocates dispatch to τ (per ADR-022), which returns `Unsupported` for every op until subsequent slices grow coverage — expected floor after A.3 is ≤12/324 (schema-only / nondeterministic cases). Slices B/C/D/E/F/G then climb from that floor toward 324. Progress is measured via `tests/scripts/v2-progress.sh`. The DataFrame corpus (324 cases) is the fitness function; TPC-H is temporarily red until τ covers its query surface and rejoins the gate.

**Constraint.** INV1–INV10 all have stubs in `crates/core/src/transpiler_v2/invariants.rs` (created by Slice A.1). Each carries a `TODO INV<N>:` (this-slice-owned) or `DEFER INV<N> → <slice>:` (reassigned) marker per §CV.5.1. INV7 does not exist. §6 below says which slice makes each remaining invariant stop being vacuous.

---

## 1. Slice sequence (in dependency order)

Slices are named in the strict order §CV.2's dependency matrix imposes, **with sub-slices called out where an internal split enables clarity or parallelism**. Each slice names its inherent dependencies (previous slices genuinely required) AND its explicit non-dependencies (things that look like dependencies but aren't — surfaced to reject accidental serialization).

Sub-slices declare **cumulative per-sub-slice corpus targets** per iteration methodology §CV.7 amendment (Open Decision 10 resolution): a sub-slice's §Targets in its scope file is the set expected to be green at that sub-slice's termination, cumulative from prior sub-slices.

### Slice A — v2 substrate: types, plan, protobuf converter, v2 SparkSQL front-end, dispatch relocation

**LANDED 2026-07-02** across three sub-slices (A.1 + A.2 + A.3) on branch `feat/v2-transpiler`, uncommitted. Corpus signal: 153/324 (pre-slice legacy fallthrough) → 0/324 (post-A.3 designed regression per ADR-022; τ is the only path and returns `UnsupportedOp` for every input until Slice B/C wire real analysis + emission). See `.agent-output/archive-slice-A-1/005-summary.md`, `.agent-output/archive-slice-A-2/005-summary.md`, `.agent-output/005-summary.md` for per-sub-slice records; `tasks/v2-slice-A-iteration-log.md` for the iteration log. 105 tests added cumulatively. INV10 walker scope covers `transpiler_v2/`, `parser_v2/`, `v2_relation_converter.rs`, and `service.rs`.

Owns ADR-003 (common AST), ADR-004 (protobuf-boundary dispatch), ADR-021 (τ owns substrate), **ADR-022 (τ is the only path)**. Activates INV10 (τ imports only value-level types from outside its module tree).

**Sub-split (sequential; three sub-slices per methodology §CV.7).**

**A.1 — Types substrate.** No inherent v2 dependencies (only shared value-level types `DataType`/`StructType`/`StructField`). Deliverables:

1. `crates/core/src/transpiler_v2/expression.rs` — τ's `Expression` enum, 21 variants covering Spark's expression surface at the reference version. Each variant carries `data_type(&Schema) -> DataType` and `nullable(&Schema) -> bool` methods per Spark parity.
2. `crates/core/src/transpiler_v2/type_inference.rs` — τ's `TypeInferenceEngine`: aggregate return-type table, coercion lattice, nullability derivations, decimal formulas. **MUST include inheritance-checklist §1.1–1.3 on day 1:** `count_if` in count family + non-nullable + `aggregate_is_non_nullable`; `hash`/`murmur3`/`xxhash64` in non-nullable literal list; `corr`/`covar_samp`/`covar_pop`/`regr_slope`/`regr_r2`/`regr_intercept`/`regr_avgx`/`regr_avgy`/`regr_sxx`/`regr_sxy`/`regr_syy` in the `→ Double` arm and in `aggregate_is_always_nullable`.
3. `crates/core/src/transpiler_v2/mod.rs` — module scaffold + `pub fn generate(...)` stub returning `EmissionError::UnsupportedOp` for every input. Per ADR-022, this is a Thunderduck-boundary error surfaced directly to the caller. No fallback machinery is wired.
4. `crates/core/src/transpiler_v2/invariants.rs` — INV1–INV10 stubs per §CV.5.1 marker convention. INV7 stub is omitted (INV7 is deleted per ADR-022).
5. Unit tests in `type_inference.rs::tests` for every checklist §1.1–1.3 name — mandatory red-line proving inheritance discipline.

**Inherent dependencies:** none. **Non-dependencies:** the analyzer (B), any emission (C), V2RelationConverter (A.2).

**A.2 — Plan substrate + protobuf converter + v2 SparkSQL front-end.** Depends on A.1 (uses v2 `Expression` as payload). Deliverables:

1. `crates/core/src/transpiler_v2/ast.rs` — v2 `CommonAst`/`CommonOp` enum carrying v2 `Expression` payload. Structured variants for `Values`, `LocalRelation` (Arrow-IPC parsed rows, not synthesized SQL — checklist §2.1), `TableFunction`/`Unnest`, `FileScan`, `Join { left_plan_ids: Vec<i64>, right_plan_ids: Vec<i64>, ... }`. `UnresolvedColumn { plan_id: Option<i64>, ... }` per checklist §2.3 and **Open Decision 12 resolution**: `Option<i64>` — protobuf front-end sets `Some(_)` from `attr.plan_id`, SparkSQL front-end leaves `None`, analyzer's resolve pass handles both.
2. `crates/connect-server/src/converter/v2_relation_converter.rs` — `V2RelationConverter` producing v2 CommonAst directly from Spark Connect protobuf. **Exhaustive typed dispatch for Arrow values** (checklist §2.1). **No `Sql` opaque variant emitted** for any of the 6 shortcut categories catalogued in the inheritance checklist §2.2. Scope of proto shapes covered per **Open Decision 2 resolution**: hybrid — structured shapes (Project/Filter/Sort/Limit/primitive Aggregate) covered in Slice A.2; complex shapes (Join, complex types, table functions) grow with their owning slice; un-handled shapes surface as `Unsupported*` Thunderduck-boundary errors per ADR-022.
3. **`crates/core/src/parser_v2/` — τ's SparkSQL front-end module tree** per **Open Decision 1 resolution (Option 1b)**. Contains `dialect.rs` (sqlparser-rs SparkDialect), `mod.rs` (parser entry), `v2_lowering.rs` (parse tree → v2 CommonAst). SparkSQL front-end sets `UnresolvedColumn::plan_id = None` (per Decision 12).
4. `crates/core/src/transpiler_v2/base_types.rs` — τ's **`BaseTypes` overlay** per **Open Decision 8 resolution**. Seeded from the DuckDB catalog by V2RelationConverter at request time. Applies `plan_has_empty_scan` short-circuit for seeding (checklist §5.5).

**Inherent dependencies:** A.1. **Non-dependencies:** the analyzer (B), dispatch relocation (A.3).

**A.3 — Dispatch relocation.** Depends on A.2. Deliverables:

1. `crates/connect-server/src/service.rs` — dispatch site routes all Spark Connect requests to τ. No runtime path selection; no `THUNDERDUCK_TRANSPILER` env var. Any non-τ source present in the workspace is reference material only — not compiled as a service backend, not exercised by tests.
2. **No fallback machinery** — no `V2FallbackEligible` trait (Open Decision 3 moot per ADR-022), no `X-Thunderduck-Path` header (Open Decision 9 moot per ADR-022). v2 errors surface directly to the caller per ADR-022's two-error-category rule.

**Inherent dependencies:** A.2. **Non-dependencies:** analyzer (B) and emission (C) — the stub `generate` from A.1 returns Thunderduck-boundary errors for every input; the corpus stays at 12/324 until C lands emission arms.

**Slice-A-level activated invariants:** INV10 (τ imports only value-level types from outside its own module tree — grep test returns zero at A.2 termination and stays zero).

**Progress signal target:** 12/324 → 12/324 (foundational; no case unlocks).

**Cumulative sub-slice §Targets:** A.1 = 12/324; A.2 = 12/324; A.3 = 12/324.

---

### Slice B — Full v2 analyzer (typing, nullability, set-op widening, outer-join nullability)

Owns ADR-005 (owned type/nullability inference), ADR-006 (bounded analyzer passes).

**Inherent dependencies:** A.1 (types substrate), A.2 (plan substrate). **Explicit non-dependency:** A.3 (dispatch relocation). The analyzer is unit-tested against `analyzer_fixtures.rs`; A.3 can proceed in parallel.

Deliverables in `crates/core/src/transpiler_v2/analyzer.rs`:

1. `TypedAst`, `TypedAttr`, `Schema`/`BaseTypes` aliases, sealed `HasSchema` trait, `AnalyzerError` (per ADR-022, variants split into Spark-emulated — `UnknownTable`, `UnknownColumn`, `AmbiguousColumn`, `TypeMismatch`, `Other` — and Thunderduck-boundary — `PuntedOperator`, `UnsupportedRule`).
2. Three bounded passes per ADR-006: (a) `resolve` (bottom-up structural; handles both `plan_id = Some(_)` from protobuf and `plan_id = None` from SparkSQL per Open Decision 12), (b) `assign_types` (bottom-up), (c) `derive_nullability`. Additional coordinated sub-passes:
   - **Set-op widening sub-sweep** — bottom-up to compute widened schema on set-op parents, then pushed back down into child projections. **Scope per Open Decision 5 resolution (Option 5b): UNION + INTERSECT + EXCEPT all widen.** UNION BY NAME deferred to Slice G.
   - **Outer-join nullability derivation** — LEFT/RIGHT/FULL OUTER makes the appropriate side's columns nullable in the join output.
3. `analyzer_fixtures.rs` — five input-relation fixtures (`emp`/`dept`/`emp2`/`nums`/`raw`) matching `dataframe_corpus.py::build_inputs`, plus 5+ mini-fixtures probing specific inference rules including outer-join nullability and set-op widening across all three set-ops.
4. `inference_smoke()` — INV4 activation entry point.
5. `has_resolved_schema(&TypedAst)` — INV5 activation predicate.
6. **INV7 is DELETED (per ADR-022)** — no front-end-agreement check runs in Slice B. Each v2 front-end is independently validated by ADR-015's differential oracle; agreement between them is transitive from Spark-correctness (Spark itself agrees with itself). The design property "both front-ends target the same CommonAST variant" is preserved by construction in Slice A.2's lowering rules.
7. SparkSQL parser lowering path's `is_aggregate_function` classifier MUST include `count_if`, `try_sum`, `try_avg`, `try_divide` (checklist §1.4).

All type/nullability logic delegates to τ's own `Expression::{data_type, nullable}` and `TypeInferenceEngine` (INV10 discipline).

**Activated invariants:** INV4 (isolation smoke), INV5 (schema everywhere).

**Progress signal target:** 12/324 → ~15–25.

---

### Slice C — Core emission substrate + operator arms + scalar/aggregate arms

Owns ADR-009 (declarative emission table). Per **Open Decision 7 resolution (Option 7a)**, ADR-009's dispatch shape is **Approach A (hand-written match arms) permanently** — the compiled-dispatch codegen macro is formally demoted to a considered-and-rejected alternative for the corpus emission path.

**Inherent dependencies:** A.1, A.2, B. **Non-dependency:** A.3.

**Sub-split (per §CV.7; architect MAY re-scope).** After C.1 substrate lands, C.2 and C.3 may proceed in parallel.

**C.1 — Emission substrate + operator arms.** Deliverables:

1. `EmittedSql` newtype + `EMIT_TAP` counter + `EMIT_TAP_MUTEX` (checklist §5.3) — INV2 activation.
2. `dispatch_op(&TypedOp, &Schema) -> Result<String, EmissionError>` — exhaustive match. Per-op renderers: `render_project`, `render_filter`, `render_sort`, `render_limit`, `render_tail` (CTE rewrite per checklist §5.4), `render_distinct`, `render_with_columns`, `render_drop_columns`, `render_aliased_relation`, `render_table_scan`, `render_local_relation`, `render_range_relation`, `render_values`.
3. `render_expr` — exhaustive match over v2 `Expression` variants.
4. `render_cast` — includes `Cast::try_cast` branch emitting `TRY_CAST(expr AS ty)` (checklist §4.2 first item).
5. `spark_return_cast` + `spark_aggregate_return_cast` kept separate per checklist §5.1.
6. `quote_ident` with no-quote fast path (checklist §5.6).
7. INV3 grep barrier: `use crate::generator::*` and `use crate::functions::*` in emission.rs return zero.
8. **`crates/core/src/transpiler_v2/rewrites.rs` created empty** per **Open Decision 6 resolution (Option 6a)** — B-layer home for future SQL desugarings; empty at C.1, populated by Slice G.
9. `extension_targets()` stub — empty at C.1, populated by Slice D.
10. `EmissionError` variants split per ADR-022's two-error-category rule: Spark-emulated (nothing emission-side that Spark would reject — those live in AnalyzerError) vs Thunderduck-boundary (`UnsupportedOp`, `UnsupportedExpression`, `UnsupportedFunction`).

**C.2 — Scalar function arms.** Depends on C.1. Deliverables:

1. `render_function_call` — ~130 scalar-function arms for `str-*` / `math-*` / `dt-*` / `cast-*` / `cond-*` clusters. Approach A hand-written match arms (per Decision 7).
2. Includes checklist §3.1 (`sha`/`sha1`/`sha2` → `SHA256(arg_0)` — single-arg only) as day-1 shape.

**C.3 — Primitive-aggregate arms + DECIMAL widening.** Depends on C.1. Parallel with C.2. Deliverables:

1. Primitive-aggregate arms in `render_aggregate` (count, sum, avg, min, max, stddev, variance families).
2. `spark_aggregate_rewrite` helper (checklist §5.7).
3. `percentile_approx` FLOAT CAST for quantile arg (checklist §3.2).

**Slice-C-level activated invariants:** INV2 companion, INV3.

**Progress signal target:** ~15–25 → ~180–200.

**Cumulative sub-slice §Targets:** C.1 ≈ 30–50; C.2 ≈ 120–150 (cumulative); C.3 ≈ 180–200 (cumulative).

---

### Slice D — Extension dispatch (parallel-track after Slice C.1)

Owns ADR-010 (extension functions).

**Inherent dependencies:** A.1, A.2, B, **C.1**. **Non-dependency:** C.2, C.3.

Populates `extension_targets()` with the 9-entry ext6-set (checklist §4.1) and wires the arms. Activates INV6.

Extension arms (checklist §4.1): `try_divide` → `spark_try_divide`; `try_sum` / `try_avg` → `spark_try_sum` / `spark_try_avg` (unconditional pass-through); plus ext4-set inherited: `spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`, `spark_decimal_div`.

Native-arm targets (checklist §4.2, verify-native-first per §4.3): `md5`/`sha1`/`sha2`/`crc32`, `sum(decimal)` via `spark_aggregate_rewrite`, `stddev`/`variance` family, `skewness`/`kurtosis` (kurtosis → `KURTOSIS_POP`), `corr`/`covar_samp`, `percentile_approx`/`median`, `regr_slope`/`regr_r2`, `count_if`, `try_cast`, int/int → DOUBLE correction.

**Activated invariants:** INV6.

**Progress signal target:** ~180–200 → ~195–215.

---

### Slice E — Set-op emission (with CAST wrapper) + Join emission (parallel-track after Slice C.1 + B)

**Inherent dependencies:** A.1, A.2, B (analyzer emits widened schemas + outer-join nullability), C.1. **Non-dependency:** C.2, C.3, D.

Deliverables in `crates/core/src/transpiler_v2/emission.rs`:

1. `render_union` / `render_intersect` / `render_except` — apply per-column CAST wrapper from the widened schema on the set-op parent (per ADR-006 refinement + Open Decision 5 — all three set-ops widen).
2. `render_join` — natural-flat-join branch + SEMI/ANTI break (the flat-chain rendering breaks at semi/anti boundaries to preserve tree shape). Uses first-class `plan_ids` on `Join` (checklist §2.3, Open Decision 12) — SparkSQL-derived joins with `plan_id = None` use textual qualifier resolution from the analyzer.
3. `ensure_no_ambiguous_columns` — walks subquery bodies.

Unlocks: `join-001..014` (14), `chain-001`/`003`/`005`/`006` (4), `type-011`/`type-019`/`type-020`, `set-*` widening cases.

**Progress signal target:** ~195–215 → ~230–250.

---

### Slice F — Complex-type emission (parallel-track after Slice C.1)

**Inherent dependencies:** A.1, A.2, B, C.1. **Non-dependencies:** C.2, C.3, D, E.

Array/map/struct/HOF/inline emission. Architect MAY sub-split into F.array, F.map, F.struct, F.hof.

Unlocks: `arr-001..017` (17), `map-001..007` (7), `struct-001..008` (8), `hof-001..010` (10), `arr2-001..005` (5), `map2-001..002` (2), `inl-001..002` (2), `chain-002`/`chain-004` (2).

**Progress signal target:** ~230–250 → ~290–305.

---

### Slice G — Vertical extensions (parallel-track after Slice C.1)

**Inherent dependencies:** A.1, A.2, B, C.1. **Non-dependencies:** C.2, C.3, D, E, F.

Sub-slices (each independently landable):

- **G.temporal** — `intv-001..005` (5) + `dt-004`/`dt-015` edges + `type-022`.
- **G.grouping** — `grp-001..006` (6), `piv-004..006` (3). Populates `rewrites.rs` with GROUPING SETS desugaring (per Decision 6 module).
- **G.windows** — `win-001..012` (12), `win2-002`.
- **G.json** — `json-001..008` (8).
- **G.parsing** — `parse-001..007` (7). UNION BY NAME (per Open Decision 5 deferral) also lands here as a G.parsing-adjacent item.

**Non-case-ID acceptance:** defensive audits (render_projection_slot symmetry; alias-inside-fn-args parity; Binary CAST precedence for DATE+INTERVAL; non-agg DISTINCT usage audit).

**Progress signal target:** ~290–305 → ~320–324.

---

### Slice H — Command / lakehouse writes (parallel-track after Slice A)

Owns ADR-011 (commands), ADR-012 (catalog overlay), ADR-013 (external/lakehouse reads), ADR-017 (Delta append writes), ADR-018 (UC Iceberg writes), ADR-019 (lakehouse I/O contract).

**Inherent dependencies:** A. **Optional dependency:** B for write-side type inference. **Non-dependencies:** C, D, E, F, G.

Activates INV8 + INV9.

**Progress signal target:** 0 corpus movement.

---

### Slice I — Differential-harness activation (parallel-track after Slice A)

Owns ADR-015 (differential + AnalyzePlan oracle) INV1 activation.

**Inherent dependencies:** A. **Non-dependencies:** B, C, D, E, F, G, H.

Deletes the `DEFER INV1 → Slice I:` markers. Zero DataFrame-corpus movement.

---

### Slice J — ADR-007 escape-hatch enumeration (parallel-track after Slice C.1)

Owns INV2's escape-hatch dimension.

**Inherent dependencies:** C.1. **Non-dependencies:** everything else.

Deletes the `DEFER INV2 → Slice J:` markers. Zero DataFrame-corpus movement.

---

### Slice K — Legacy source deletion (optional; can happen incrementally)

Per **Open Decision 11 resolution + ADR-022**, legacy source can be deleted from the workspace at any point. Slice K is a bookkeeping slice — no architectural decision to make, just delete `crates/core/src/logical/`, `crates/core/src/expression/`, `crates/core/src/generator/`, `crates/core/src/functions/`, `crates/core/src/types/type_inference.rs`, `crates/connect-server/src/converter/{expression_converter,plan_converter,relation_converter,type_converter}.rs` etc.

**When to run.** Practical: after Slice C.1 (v2 has enough substrate that legacy is provably unreferenced by v2's runtime path — INV10's grep confirms). Or incrementally per-slice: each slice that adds v2 coverage may delete the corresponding legacy files that its coverage supersedes.

**Effect on invariants:** INV10 retires (nothing to import). INV3 remains load-bearing for v2's internal discipline.

**Non-dependencies:** any slice — Slice K is orthogonal.

---

### Not a slice — cross-cutting discipline

ADR-000 (positioning), ADR-001 (transliterator-not-optimizer), ADR-002 (delegation boundary; two-error-category rule), ADR-007 (A/B/C layer contract; B-layer module `rewrites.rs` created by Slice C.1, populated by Slice G), ADR-008 (correlated subqueries direct), ADR-014 (two decision spaces; seam-and-drain pattern for v2-internal sub-slice cross-cuts), ADR-015 (differential oracle), ADR-016 (version pin), ADR-020 (strict-only extension mandatory), ADR-021 (τ owns substrate), ADR-022 (τ is the only path; no fallback) are premises, disciplines, and testing architecture. They shape *how* Slices A–K are implemented; none unlocks corpus cases directly.

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
| ADR-006 | bounded analyzer passes (incl. set-op widening + outer-join nullability sub-sweeps) | (analyzer implementation discipline) | 0 |
| ADR-007 | A/B/C layer structure | (enables `C_ESCAPE_HATCHES` growth via Slice J; enables SQL desugarings via Slice G's B-layer rules) | 0 |
| ADR-008 | correlated subqueries direct | (corpus is DataFrame-only) | 0 |
| ADR-009 | declarative emission table | `proj-*` (15), `filt-*` (15), `cast-001..011` (11), `cond-*` (16), `str-*` (20), `math-001..014` (14), `dt-*` (17), `agg-*` primitive (18), `grp-*` (6), `win-*` (12), `join-*` (14), `set-*` (10), `ord-*` (12), `arr-*` (17), `map-*` (7), `struct-*` (8), `hof-*` (10), `misc-*` (10), `piv-*` (3), `arr2-*` (5), `map2-*` (2), `inl-*` (2), `agg2-006`, `struc-*` (6), `win2-002` | ≈250 |
| ADR-010 | extension functions | `hash-*` (3), `agg-007`, `agg-008`, `agg-009`, `agg-012`, `cast-012`, `math-016`, `agg2-003..005` | ≈14 |
| ADR-011 | command arm | (DataFrame corpus has no DDL) | 0 |
| ADR-012 | catalog overlay | 0 |
| ADR-013 | external/lakehouse reads | 0 |
| ADR-014 | two decision spaces | (test-attribution discipline) | 0 |
| ADR-015 | differential oracle | (harness itself; enables INV1) | 0 |
| ADR-016 | version pin | (scoping) | 0 |
| ADR-017 | Delta append writes | 0 |
| ADR-018 | Iceberg UC writes | 0 |
| ADR-019 | lakehouse I/O contract | 0 |
| ADR-020 | strict-only target | 0 (already in `main`) |
| ADR-021 | v2 substrate independence | (prereq to all v2 slices; enables INV10) | 0 direct |
| ADR-022 | τ is the only path; two error categories | (no fallback machinery; no INV7) | 0 direct |

**Numeric health check.** ADR-005 + ADR-009 + ADR-010 together account for ≈300 of 324 cases. The remaining ≈24 fall out of Slices F/G. ADRs 011–019 unlock zero corpus cases. ADR-021 and ADR-022 are substrate/positioning ADRs.

---

## 3. Per-slice acceptance gate

**Slice A (A.1 + A.2 + A.3) — v2 substrate.** *No case-ID unlocks by itself.* Acceptance:
- `crates/core/src/transpiler_v2::generate` returns `Err(EmissionError::UnsupportedOp)` for every input — a Thunderduck-boundary error surfaced to the caller per ADR-022. No fallback machinery is wired.
- INV10 grep passes across `transpiler_v2/`, `V2RelationConverter`, `parser_v2/` (per Open Decision 1).
- Every inheritance-checklist item in §1 (TypeInferenceEngine) present in v2 `TypeInferenceEngine` — verified by unit tests.
- Every inheritance-checklist item in §2 (RelationConverter) present in `V2RelationConverter` by construction.
- `crates/core/src/parser_v2/` module tree exists (Open Decision 1 Option 1b).
- v2's per-path `BaseTypes` overlay seeded at request time (Open Decision 8).
- No `THUNDERDUCK_TRANSPILER` env var; no `V2FallbackEligible` trait; no fallback-path instrumentation (ADR-022).
- Prerequisite for every subsequent slice.
- Progress signal: 12/324 → 12/324. Sub-slice cumulative targets: A.1 = 12, A.2 = 12, A.3 = 12.

**Slice B — Analyzer.** Acceptance:
- `analyze()` returns `Ok(TypedAst)` for all corpus cases whose plan lowers without an `Unsupported*` error.
- INV4 and INV5 tests pass on the 5+ analyzer_fixtures.
- **No INV7 check** (INV7 is deleted per ADR-022). Each v2 front-end is independently validated by ADR-015's oracle; agreement is transitive.
- Target case IDs: `type-001..022`, `cond-003..011`, `agg-013`, `agg-018..020`, `chain-004`, + schema_only/nondeterministic cluster (~15).
- Set-op widening sub-sweep active for UNION + INTERSECT + EXCEPT (Open Decision 5).
- Outer-join nullability derivation active on LEFT/RIGHT/FULL OUTER.
- CommonAST `UnresolvedColumn::plan_id` handled for both `Some(_)` (protobuf) and `None` (SparkSQL) in the resolve pass (Open Decision 12).
- Progress signal: 12/324 → ~15–25.

**Slice C (C.1 + C.2 + C.3) — Core emission.** Acceptance:
- Case IDs green on `core_v2`: `proj-001..015`, `filt-001..015`, `cast-001..011`, `cond-*` (12+), `str-001..020` (except `str-020` spark4→Slice G), `math-001..014`, `dt-002..017` (except `dt-001`), `agg-*` primitives, `ord-*`, `misc-003..005`+`misc-008..010`, `struc-001/002/005/006`.
- Every inheritance-checklist §3 and §5 item present.
- INV2 companion (`inv2_dispatch_is_only_sql_writer`) and INV3 grep barrier tests pass.
- Empty `rewrites.rs` module present (Open Decision 6).
- Approach A hand-written match arms — no data-driven dispatch interpreter (Open Decision 7).
- Progress signal: ~15–25 → ~180–200. Sub-slice cumulative targets: C.1 ≈ 30–50, C.2 ≈ 120–150, C.3 ≈ 180–200.

**Slice D — Extension dispatch.** Acceptance:
- Case IDs green: `hash-001..003`, `agg-007`, `agg-008`, `agg-009`, `agg-012`, `agg-013`, `agg2-003`, `agg2-004`, `agg2-006`, `cast-012`, `math-011`, `math-016`, `type-005`.
- Every inheritance-checklist §4 item present.
- `extension_targets()` returns 9 entries; INV6 containment check passes against loaded ext6.
- Progress signal: ~180–200 → ~195–215.

**Slice E — Set-op + Join emission.** Acceptance:
- `join-001..014` (14) green.
- `chain-001`/`003`/`005`/`006` (4) green.
- `type-011`, `type-019`, `type-020` green.
- `set-*` widening cases end-to-end green.
- Progress signal: ~195–215 → ~230–250.

**Slice F — Complex-type emission.** Acceptance:
- `arr-001..017` (17), `map-001..007` (7), `struct-001..008` (8), `hof-001..010` (10), `arr2-001..005` (5), `map2-001..002` (2), `inl-001..002` (2), `chain-002`/`chain-004` (2) green.
- Progress signal: ~230–250 → ~290–305.

**Slice G — Vertical extensions.** Acceptance (any subset landable in parallel):
- G.temporal: `intv-001..005` (5) + edges + `type-022`.
- G.grouping: `grp-001..006` (6), `piv-004..006` (3); B-layer `rewrites.rs` populated with GROUPING SETS desugaring.
- G.windows: `win-001..012` (12), `win2-002`.
- G.json: `json-001..008` (8).
- G.parsing: `parse-001..007` (7); UNION BY NAME (deferred from Slice B).
- Progress signal: ~290–305 → ~320–324.

**Slice H — Command/lakehouse writes.** Zero corpus cases. INV8 + INV9 activation.

**Slice I — Differential-harness activation.** Zero corpus cases. `git grep 'DEFER INV1'` returns empty.

**Slice J — ADR-007 escape-hatch enumeration.** Zero corpus cases. `git grep 'DEFER INV2'` returns empty.

**Slice K — Legacy source deletion.** Zero corpus cases. `crates/core/src/{logical,expression,generator,functions}/` deleted; INV10 retires; INV3 remains.

---

## 4. Sequencing rationale

Each slice's dependency on prior slices, citing §CV.2 edges and §CV.4 assumptions.

1. **Slice B depends on A.1 + A.2 — §CV.2 edges `005 ← 003`, `005 ← 021`.** Analyzer needs CommonAst (A.2) and v2 Expression/TypeInferenceEngine (A.1). **Non-dependency:** A.3 (analyzer is fixture-tested).

2. **Slice C depends on B — §CV.2 edge `009 ← 005`, LB1.** LB1 says the divergent slice is {type, nullability}; every emission decision keys on analyzer-derived types. **Non-dependency:** A.3 for C's own code, but A.3 must be wired for corpus signal to move.

3. **Slice D depends on C.1 — §CV.2 edge `010 ← 009`, INV6.** Extension arms are rows in the emission table. **Non-dependency:** C.2, C.3.

4. **Slice E depends on B (analyzer's set-op widening + outer-join nullability) + C.1 — §CV.2 edge `009 ← 005`, ADR-006 refinement.** **Non-dependency:** C.2, C.3, D, F, G.

5. **Slice F depends on C.1 — §CV.2 edge `009 ← 005`.** **Non-dependency:** C.2, C.3, D, E, G.

6. **Slice G depends on C.1 — §CV.2 edges `009 ← 005`, `007 ← 001`.** G.grouping additionally consumes `rewrites.rs` (Open Decision 6). **Non-dependency:** C.2, C.3, D, E, F.

7. **Slice H depends on Slice A — §CV.2 edges `011 ← 004`, `017 ← 005`, `018 ← 005`.** Optional dep on B. **Non-dependency:** C, D, E, F, G.

8. **Slice I depends on Slice A.** **Non-dependency:** B, C, D, E, F, G, H.

9. **Slice J depends on C.1.** **Non-dependency:** C.2, C.3, D, E, F, G, H, I.

10. **Slice K (legacy deletion) depends on nothing architecturally.** Practically: run after C.1 so v2 substrate is provably unreferenced. Or run incrementally per-slice.

11. **The whole map is scoped by ADR-020 (strict-only extension), ADR-021 (τ owns substrate), and ADR-022 (τ is the only path).**

---

## 5. Quick-wins highlight

**Quick-win 1 — Slice A (substrate).** ~2000 LOC (hybrid per Decision 2). Foundation. No corpus movement.

**Quick-win 2 — Slice B (analyzer).** ~2000 LOC. +3 to +15 cases. First visible signal.

**Quick-win 3 — Slice C.1 (emission substrate + operator arms).** ~3000 LOC. +15–25 cases. **Unlocks six parallel corpus-moving tracks** (C.2, C.3, D, E, F, G) — largest leverage multiplier.

**Cumulative after A+B+C.1:** 12 → ~30–50, with the foundation in place for six parallel tracks.

---

## 6. Invariant activation map

| INV | §CV.5 name | Activates in | Stub deleted |
|---|---|---|---|
| **INV1** | byte-identical input | Slice I | ADR-015 harness installs payload-hashing tap |
| **INV2** | node-local or labeled C escape hatch | Slice C.1 (dispatch-is-only-writer) + Slice J (escape-hatch dimension) | Slice C.1 activates `EMIT_TAP`; Slice J populates `C_ESCAPE_HATCHES` |
| **INV3** | single emission table | Slice C.1 | Grep rejects `use crate::generator::*`/`use crate::functions::*` in emission.rs |
| **INV4** | inference validated in isolation | Slice B | `inference_smoke()` iterates analyzer_fixtures |
| **INV5** | schema everywhere | Slice B | `has_resolved_schema` walker rejects `DataType::Unresolved` |
| **INV6** | extension targets exist | Slice D | `extension_targets()` 9-entry allow-list checked against `duckdb_functions()` |
| **INV7** | **DELETED per ADR-022** | — | Each v2 front-end independently Spark-validated by ADR-015; agreement transitive |
| **INV8** | external access delegated | Slice H | `external_emit_paths()` non-empty |
| **INV9** | writable requires attached provenance | Slice H | `emit_write()` gated on provenance |
| **INV10** | τ imports only value-level types from outside its own module tree | Slice A (partial at A.1; full at A.2 termination) | Grep barrier active permanently; trivially satisfied once no non-τ source exists |

**Cross-check.** Every remaining slice activates at least one invariant. INV7 does not exist.

---

## 7. Parallelism map

The dependency graph in §4 admits the following parallel execution schedule.

```
                                                        ┌─── C.2 (scalar arms) ───┐
                                                        │                          │
                          ┌─── A.1 ── A.2 ── A.3 ─┐     ├─── C.3 (aggregate arms) ─┤
                          │  types    plan   disp │     │                          │
    (foundation seed)  ───┤                       │     ├─── D (extensions) ───────┤
                          │                       ▼     │                          │
                          └─── B (analyzer) ── C.1 ─────┼─── E (set-op + join) ────┤
                                incl. set-op   emission │                          │
                                widening +     substrate├─── F (complex types) ────┤
                                outer-join                │                          │
                                nullability             ├─── F.array, F.map, ... ─┤
                                                        │   (F sub-slices || )     │
                                                        │                          │
                                                        ├─── G (vertical exts) ───┤
                                                        │                          │
                                                        └─── G.temporal, G.grp,   │
                                                            G.win, G.json,        │
                                                            G.parse (|| within G)  │
                                                                                    │
    ┌─── H (writes) ─────────────────────────────────────────────────────────────┤
    │   (parallel to any corpus-moving slice)                                    │
    │                                                                            │
    ├─── I (harness) ────────────────────────────────────────────────────────────┤
    │   (parallel to any corpus-moving slice)                                    │
    │                                                                            │
    ├─── J (escape-hatch enum) ──────────────────────────────────────────────────┤
    │   (needs C.1 substrate; parallel to C.2/C.3/D/E/F/G)                       │
    │                                                                            │
    └─── K (legacy deletion) ────────────────────────────────────────────────────┘
        (orthogonal; run after C.1 or incrementally per-slice)
```

**Parallel groups:**

- **Group 1 (foundation, sequential).** A.1 → A.2 → A.3 → B → C.1. ~5 sequential slice-passes.
- **Group 2 (corpus climb, six-way parallel post-C.1).** {C.2, C.3, D, E, F, G}.
- **Group 3 (non-corpus, post-A).** {H, I}. Plus Slice J (post-C.1) and Slice K (any time).

**Practical execution schedule with N engineers:**

- **N=1.** A.1 → A.2 → A.3 → B → C.1 → C.2 → C.3 → D → E → F → G → H → I → J → K. ~14 slice-passes.
- **N=2.** Post-C.1, split emission-heavy from complex-heavy tracks.
- **N=3+.** Post-C.1, six tracks; wall-clock climb ~30 → ~320 shortens roughly with headcount.

**Non-obvious rules the map enforces:**

- **B does not block C.1 on set-op widening or outer-join nullability** — those are B's scope.
- **D is not sequential after C** — D is peer to C.2 and C.3.
- **E is not "after D"** — E is peer to D.
- **F is not blocked on E**, **G's sub-slices don't depend on each other**, **H does not block anything corpus-moving**.
- **J does not block G** — J audits whatever emission table exists when it runs.
- **K (legacy deletion) is orthogonal** — no slice needs to wait for it; some slices may accelerate it by deleting the legacy files their coverage supersedes.

---

## 8. Open decisions — CLOSED

All twelve decisions from `tasks/v2-restart-open-decisions.md` are **RESOLVED** as of 2026-07-02 per user directive. The largest cluster (Decisions 3, 4, 9, 11) is consolidated by **ADR-022** (τ is the only path; two error categories). See `tasks/v2-restart-open-decisions.md` for the full resolution record and the summary table.

**Follow-up ADR amendments required** (Pass 1 architect of each slice must confirm they have landed):
- **ADR-006 refinement** — set-op widening scope covers UNION + INTERSECT + EXCEPT (Open Decision 5). Amendment lands with Slice B.
- **ADR-009 refinement** — Approach A (hand-written match arms) is permanent (Open Decision 7). Amendment lands with Slice C.1.
- **Iteration methodology §CV.7 amendment** — sub-slice cumulative targets (Open Decision 10). Amendment lands ahead of any sub-splitting slice.

**Follow-up project-level edits** (not ADR-level; recorded for tracking):
- **CLAUDE.md §4** — "TPC-H differential (mandatory)" is amended in spirit during the restart. DataFrame corpus is the fitness function; TPC-H rejoins the gate once v2 covers its query surface.

New parallel-track blockers surfaced during slice execution should be appended to `tasks/v2-restart-open-decisions.md` as new decisions (Decision 13, 14, ...).

---

## Notes on measurement

- `tests/scripts/v2-progress.sh` records the `core_v2` PASSED count. Under ADR-022, every green case is a v2 native emission — no fallback attribution needed.
- The agent-pipeline `## Quality Gate` in `CLAUDE.md` excludes the differential suites; `v2-progress.sh` is a separate manual measurement.
- The `all` differential suite exercises TPC-H + TPC-DS. Under ADR-022, TPC-H is temporarily red until τ covers its query surface. The DataFrame corpus is the fitness function during the restart.
- `spark4` cases assume the pinned Spark 4.1.1 reference; ADR-016 governs the pin.

## Lessons embedded in the design

Concrete bug classes and design constraints inherited from prior implementation work are recorded in `tasks/v2-restart-inheritance-checklist.md`. Each slice's Pass 1 architect cites the applicable checklist items; each slice's reviewer verifies presence before returning APPROVED. This is how prior implementation experience enters the current design — not as narrative footnotes here, but as forward-facing constraints on each slice's scope. Chronological narrative (if wanted for readers of this branch's history) lives in `git log` and `docs/dev_journal/`, not in this map.
