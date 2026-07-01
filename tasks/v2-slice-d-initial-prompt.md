# Slice D — Initial `/new-feature` prompt (pass 1)

Use this file verbatim as the `/new-feature` prompt for pass 1 of Slice D
under the iteration methodology in `tasks/v2-slice-iteration-methodology.md`.

---

Bring the v2 extension-dispatch tranche online (Slice D) per rearchitect
ADR-010. Populate `crates/core/src/transpiler_v2/emission.rs`'s function-
call arms with `spark_*` extension calls for the ~14 Spark/DuckDB-divergent-
semantics corpus cases; populate
`crates/core/src/transpiler_v2/emission.rs::extension_targets()` (currently
returns an empty slice) with the `spark_*` names the emitter now references;
activate **INV6** (every emitted `Extension(...)` target corresponds to an
existing, loaded function in the `thdck_spark_funcs` C++ extension). Expected
`v2-progress.sh` delta at Slice D completion: **134 → 145-160 core_v2 passing**
(the readiness map §Slice D estimate is +14; the wider range acknowledges
that some corpus cases in the target list are indirectly gated on join or
window support and will need Slice E/G to fully unblock).

## Design mandate (authoritative)

- `docs/thunderduck-rearchitect-ADRs.md`:
  - **ADR-010** — extension functions are minimal gap-fillers, implemented in
    the C++ project. Their emission-table representation is `Extension(name)`
    rows in ADR-009's table.
  - **ADR-020** — the `thdck_spark_funcs` extension is **mandatory** and
    bundled into every session at startup; there is no "relaxed mode". Every
    `spark_*` function Slice D emits MUST already exist in the `ext4` release
    of the extension (see `crates/core/src/runtime/`); do NOT plan any C++
    extension work.
  - **ADR-009 (post-Slice-C Approach A/B/C refinement)** — Slice D's ~10-20
    function shapes are trivial 3-5-line format strings, so Approach A (hand-
    written match arms in `render_function_call`) is the correct choice. Do
    NOT reintroduce declarative rows.
  - **ADR-014 (post-Slice-C deliberate-seam refinement)** — Slice D does NOT
    need a seam; `emission.rs` stands alone after Slice C.2's `SqlGenerator::
    gen_expr` seam drain. Do NOT reintroduce legacy imports.
  - **§CV.5 INV6** — mechanically checkable: `extension_targets()`'s returned
    names must exist in DuckDB's `duckdb_functions()` output after the
    session loads the extension.
  - **§CV.5.1 (invariant scoping conventions)** — INV6 is a single-dimension
    invariant activated fully by this slice. Use `TODO INV6:` markers only
    for within-Slice-D unblocking work; use `DEFER INV6 → <slice>:` if any
    sub-dimension surprises you and needs a named future slice.
  - **§CV.7 (slice sub-split legitimacy)** — if you assess Slice D as too
    large for one pass, propose a sub-split per §Loop step 4.
- `docs/adrs/adr-13-duckdb-extension-loading.md` — how the C++ extension
  loads at session start; the loading is a hard error if it fails (per
  ADR-020's "mandatory"), so INV6 can rely on the extension being present.
- `tasks/v2-adr-readiness-map.md §Slice D` — the target case-ID list.

## Inputs to read

- `crates/core/src/transpiler_v2/emission.rs` — `render_function_call`
  (the ~130 arm match on lowercased function name), `render_aggregate`
  (where SUM/AVG/etc live), `extension_targets()` (currently `&[]`), and
  the `spark_return_cast` / `spark_aggregate_return_cast` helpers.
- `crates/core/src/transpiler_v2/invariants.rs` — INV6's stub test
  (`inv6_extension_targets_exist_in_loaded_extension`); its docstring
  names the DuckDB-side check.
- `crates/core/src/runtime/session.rs` (or equivalent) — where
  `thdck_spark_funcs` is loaded at session startup. **Read-only reference**
  to confirm the extension binary is loaded before any query.
- `crates/core/src/functions/` — legacy `FunctionRegistry`. **Read-only
  reference** for the Spark→spark_* shape mapping. INV3 forbids importing
  from here in `emission.rs`; duplicate the shape verbatim per lessons.md.
- `tests/integration/differential/dataframe_corpus.py` — the acceptance
  gate for the case-ID list below.
- `.agent-output/archive-pass-1/*` and `.agent-output/archive-slice-b/*`
  — historical context, only if you need to cross-reference prior
  architectural decisions.

## Slice-C carryover — CLOSE_NOW in Slice D (if not already closed)

Per the Slice C iteration log (`tasks/v2-slice-c-iteration-log.md`),
Slice C.2 landed with these DEFER carryovers that touch code Slice D
also modifies. Verify none regress:

- Slice C.2's INV3 tightening rejects `use crate::generator::SqlGenerator`
  and `use crate::functions::FunctionRegistry` in `emission.rs`. Slice D
  must preserve this — hard-copy the Spark→spark_* shapes from
  `FunctionRegistry` verbatim.
- The `spark_return_cast` / `spark_aggregate_return_cast` separation
  prevents double-cast. Slice D's aggregate extension calls
  (`spark_sum`/`spark_avg`/`spark_skewness`) go through
  `spark_aggregate_return_cast`, NOT the projection-slot helper.
- The Union widened-schema-wins-at-emission policy (ADR-006 post-Slice-C
  refinement) applies unchanged; Slice D does not touch it.

## Scope

1. **Extension-function rows** in `emission.rs::render_function_call`:
   - `hash` → `spark_hash` (single arg or variadic, INT return)
   - `xxhash64` → `spark_xxhash64` (variadic, BIGINT return)
   - `try_divide` → `spark_try_divide` OR DuckDB-native `TRY(a/b)` — **verify at
     implementation time** which the extension provides; ADR-020's `ext4`
     release notes have the authoritative list.
   - `try_cast` → DuckDB-native `TRY_CAST(expr AS type)` — likely native, not
     extension; verify.

2. **Aggregate extension rows** in `emission.rs::render_aggregate`:
   - `sum(decimal)` → `spark_sum(...)` (correct decimal precision propagation)
   - `avg(decimal)` → `spark_avg(...)` (correct decimal precision propagation)
   - `stddev` / `stddev_samp` / `stddev_pop` — verify shapes against
     `FunctionRegistry`.
   - `variance` / `var_samp` / `var_pop` — same.
   - `skewness` → `spark_skewness(...)` (Spark's population semantics)
   - `kurtosis` — verify shape.
   - `corr` / `covar_samp` / `covar_pop` — verify shapes; may be native.
   - `percentile_approx` / `median` — verify shape; `agg-013` was Slice B's
     schema-only target — Slice D lands the full row-diff.
   - `regr_*` (regression aggregates) — verify shapes.
   - `try_sum` / `try_avg` — verify.
   - `count_if` — verify shape (may be native DuckDB `COUNT_IF`).
   - `histogram_numeric` — verify shape.

3. **Decimal-division correction** — anywhere Slice C.2 emits `a / b` where
   both operands are `Decimal(_,_)`, the correct Spark shape is
   `spark_decimal_div(a, b)`. Locate in `render_binary` (`Binary(Div, Decimal,
   Decimal)`) and route to the extension call. This closes the parity gap
   affecting `type-005`, `math-011`, and Div-in-`chain-*` cases.

4. **`extension_targets()`** — populate with the exact set of `spark_*` names
   the arms in #1-#3 now reference. This is the coverage set that INV6
   checks against `duckdb_functions()`.

5. **INV6 activation** — replace the stub body of
   `inv6_extension_targets_exist_in_loaded_extension` in `invariants.rs`
   with a real check:
   - The test opens a DuckDB connection with the `thdck_spark_funcs`
     extension loaded (via the existing runtime path).
   - Queries `duckdb_functions()` for the set of function names present.
   - Asserts every name in `extension_targets()` is in that set.
   - Fails loudly with the missing names on any mismatch.
   - Delete the `TODO INV6:` markers in `invariants.rs`.

## Acceptance

- Case IDs green on `core_v2` (via `tests/scripts/v2-progress.sh`):
  `hash-001..003` (3), `agg-007`, `agg-008`, `agg-009`, `agg-012`, `agg-013`,
  `math-016`, `cast-012`, `agg2-003`, `agg2-004`, `agg2-006`; plus decimal-
  division corrections in `type-005`, `math-011`, and any Div in `chain-*`.
- Progress signal: 134 → 145-160 (readiness map estimate +14; empirical
  range acknowledges cross-slice interactions).
- Quality gate (per `CLAUDE.md` §Quality Gate) green on all passes.
- `git grep 'TODO INV6'` returns empty crate-wide (INV6 fully activated).
- Legacy path (`THUNDERDUCK_TRANSPILER=legacy` default) unregressed:
  `./tests/scripts/run-differential-tests.sh tpch` remains 51/51.

## Out of scope (deferred to later slices per readiness map)

- **Slice E**: full join cluster (`join-*`), outer-join emission, chain
  cases needing joins, `type-019`/`type-020` set-op widening completion.
- **Slice F**: complex-type emission (arrays, maps, structs, HOFs).
- **Slice G**: verticals (Window / Interval / JSON / pivot / parsing).
- **Slice H**: command arm + lakehouse writes.
- **Slice I**: full INV1 activation (differential-harness).
- **Slice J**: INV2 escape-hatch dimension.

## Non-goals — do NOT do any of these

- Do NOT introduce new C++ extension functions in `thdck_spark_funcs`. The
  `ext4` release binaries are pinned; if a corpus case needs a function not
  yet in the extension, DEFER it to a future release-pinning slice per
  ADR-020's revisit-triggers.
- Do NOT reintroduce `use crate::functions::FunctionRegistry` or
  `use crate::generator::*` in `emission.rs`. INV3's tightened predicate
  (Slice C.2) stays load-bearing.
- Do NOT change legacy `FunctionRegistry`, `SqlGenerator`, or
  `TypeInferenceEngine` bodies. Legacy remains untouched.
- Do NOT skip INV6's activation. The whole slice's ADR value is contingent
  on INV6 having teeth.
- Do NOT edit the corpus or test harness.
- Do NOT run the differential suite between passes (methodology forbids it).

## Note to the architect

Slice D is small in code volume (~10-20 function arms, one `extension_targets()`
list, one INV6 test body) but load-bearing in ADR terms: it is the first slice
where `Extension(...)` rows in ADR-009's table materialize and INV6's mechanical
check becomes real. Spend design energy on (a) confirming which functions are
actually in the `ext4` release binary vs. native DuckDB (getting this wrong is
a build failure at INV6 activation, not a subtle bug), and (b) the decimal-
division routing decision (a fanout point where `Binary(Div, Decimal, Decimal)`
must route to `spark_decimal_div` — a real analyzer/emitter boundary decision).
Per §CV.7, if Slice D needs a sub-split (e.g. D.1 = hash/try functions, D.2 =
aggregate extensions + decimal-div), propose it.
