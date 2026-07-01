# Slice D — Initial `/new-feature` prompt (pass 1)

Use this file verbatim as the `/new-feature` prompt for pass 1 of Slice D
under the iteration methodology in `tasks/v2-slice-iteration-methodology.md`.

---

Bring the v2 extension-dispatch tranche online (Slice D) per rearchitect
ADR-010. Populate `crates/core/src/transpiler_v2/emission.rs`'s function-
call arms with `spark_*` extension calls for Spark/DuckDB-divergent-
semantics corpus cases; populate
`crates/core/src/transpiler_v2/emission.rs::extension_targets()` (currently
returns an empty slice) with the `spark_*` names the emitter now references;
activate **INV6** (every emitted `Extension(...)` target corresponds to an
existing, loaded function in the `thdck_spark_funcs` C++ extension).

**Slice D is structured as two phases** per the 2026-07-01 up-front audit
(`tasks/v2-adr-readiness-map.md` §Slice D):

- **Phase 1 — ext4 wiring (this pipeline's immediate scope).** Wire the
  `spark_*` functions already in the pinned `ext4` release plus the native-
  DuckDB functions matching Spark. Progress signal delta: **134 → ~140-142**.
- **Phase 2 — post-`ext5` pin (blocked externally).** Wire the ~5-8
  functions requiring new C++ extension work per the pre-drafted specs in
  `tasks/duckdb-extension-specs/`. Blocked on the separate
  `thunderduck-duckdb-extension` project shipping `ext5` and this repo
  pinning it. Progress signal delta: **~142 → ~145-148**.

**Slice D terminates only when Phase 2 completes** (all 14 target case IDs
pass, `git grep 'TODO INV6'` empty over the full function surface). Between
phases, the iteration pauses with a "waiting for ext5 pin" state; a
follow-up `/goal` invocation resumes when ext5 is pinned.

The 10 spec files in `tasks/duckdb-extension-specs/` are **already committed**
as handoff artifacts for the extension project. Do NOT re-draft them; if
Pass 1 discovers additional missing functions beyond the audit's 10, follow
the reactive protocol in the spec directory README.

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

### Phase 1 (this pipeline's scope — ext4 wiring)

1. **Extension-function rows in `emission.rs::render_function_call`** (functions already in ext4):
   - `hash` → `spark_hash` (single arg or variadic, INT return).
   - `xxhash64` → `spark_xxhash64` (variadic, BIGINT return).
   - `skewness` → `spark_skewness` (already in ext4; add the arm).

2. **Native DuckDB function rows** (no extension needed):
   - `md5`, `sha1`, `sha2`, `crc32` → native DuckDB.
   - `stddev`, `stddev_samp`, `stddev_pop`, `variance`, `var_samp`, `var_pop` → native DuckDB (mostly already wired in Slice C.2; verify coverage).
   - `percentile_approx` / `median` → native `PERCENTILE_CONT` in DuckDB.

3. **Aggregate extension rows in `emission.rs::render_aggregate`** (routing existing ext4 functions):
   - `sum(decimal)` → `spark_sum(...)`. The `spark_aggregate_return_cast` helper (`emission.rs` ~line 1610) already has a TODO for this; wire it.
   - `avg(decimal)` → `spark_avg(...)`. Same pattern.

4. **Decimal-division correction** in `render_binary`:
   - `Binary(Div, Decimal(_,_), Decimal(_,_))` → `spark_decimal_div(a, b)`.
   - Function is already in ext4; just route it.
   - Closes parity gap in `type-005`, `math-011`, Div-in-`chain-*`.

5. **Verify-first arms** (DuckDB may match Spark; decide at Pass 1 implementation time):
   - `kurtosis` — test native `KURTOSIS_POP` against Spark's excess-kurtosis Fisher's-definition. If parity: wire native. Else: leave as pending `spark_kurtosis` per `tasks/duckdb-extension-specs/spark_kurtosis.md` and defer to Phase 2.
   - `count_if` — test native `COUNT_IF` NULL-as-FALSE semantics against Spark. If parity: wire native. Else: leave as pending `spark_count_if` per the spec file and defer to Phase 2.

6. **`extension_targets()`** — populate with the Phase 1 subset of `spark_*` names the arms in #1-#4 now reference: `spark_hash`, `spark_xxhash64`, `spark_skewness`, `spark_sum`, `spark_avg`, `spark_decimal_div`. This is Phase 1's coverage set for INV6.

7. **INV6 activation (Phase 1)** — replace the stub body of `inv6_extension_targets_exist_in_loaded_extension` in `invariants.rs` with a real check:
   - Test opens a DuckDB connection with `thdck_spark_funcs` loaded (existing runtime path).
   - Queries `duckdb_functions()` for the set of function names present.
   - Asserts every name in `extension_targets()` is in that set.
   - Fails loudly with the missing names on any mismatch.
   - Delete the `TODO INV6:` markers in `invariants.rs` — Phase 1 activates INV6 over the ext4 subset, and Phase 2 will extend the set without needing to re-activate.

### Phase 2 (deferred; blocked on `ext5` pin)

Post-`ext5` pin, a follow-up `/goal` invocation of Slice D adds:
- Extension-function arms for `try_divide`, `try_cast` (verify DuckDB `TRY_CAST` first), `corr`, `covar_samp`, `regr_slope`, `regr_r2`, `try_sum`, `try_avg`.
- Verify-first resolutions for `kurtosis` / `count_if` if they ended up in Phase 2.
- Extends `extension_targets()` with the newly-available names.
- Once complete, `git grep 'TODO INV6'` returns empty **and** all 14 Slice D target case IDs pass.

Between phases, when Pass 1 (or subsequent Phase-1 passes) close, the iteration log records "Phase 1 complete; awaiting ext5 pin" and the pipeline terminates. Slice D as a whole is NOT declared complete until Phase 2 lands.

## Acceptance

### Phase 1 acceptance (this pipeline)

- Case IDs green on `core_v2`: `hash-001..003`, `agg-007`, `agg-008`,
  `agg-013`, `math-011`, `type-005`, `agg-009 skewness` half (paired with
  the kurtosis half if verify-first resolved it). Plus decimal-division
  corrections in Div-in-`chain-*`.
- Phase 1 progress signal: **134 → ~140-142**.
- `git grep 'TODO INV6'` returns empty crate-wide (INV6 activated over
  the ext4 subset; extension_targets() populated; the real
  `duckdb_functions()` diff test passes).
- Quality gate green on all Phase 1 passes.
- Legacy path unregressed: TPC-H differential 51/51.
- Iteration log records "Phase 1 complete; awaiting ext5 pin" and lists
  the case IDs deferred to Phase 2 with their spec-file pointers.

### Phase 2 acceptance (follow-up /goal after ext5 pins)

- Case IDs green: `math-016`, `cast-012`, `agg-012`, `agg2-003`,
  `agg2-004`, `agg2-006` (if it ended up in Phase 2). Plus any verify-first
  cases that deferred.
- Cumulative Slice D progress signal: **134 → ~145-148**.
- All 14 original Slice D target case IDs pass.
- `git grep 'TODO INV6'` still empty (Phase 1 already made it so; Phase 2 just extends `extension_targets()`).
- Legacy path unregressed.
- Slice D officially terminates in the readiness map (§Slice D "landed" line).

## Out of scope (deferred to later slices per readiness map)

- **Slice E**: full join cluster (`join-*`), outer-join emission, chain
  cases needing joins, `type-019`/`type-020` set-op widening completion.
- **Slice F**: complex-type emission (arrays, maps, structs, HOFs).
- **Slice G**: verticals (Window / Interval / JSON / pivot / parsing).
- **Slice H**: command arm + lakehouse writes.
- **Slice I**: full INV1 activation (differential-harness).
- **Slice J**: INV2 escape-hatch dimension.

## Handling functions missing from `ext4`

The 2026-07-01 up-front audit already identified 10 functions that will need
`ext5` work; the specs are pre-drafted in `tasks/duckdb-extension-specs/`.
Slice D Pass 1 does NOT re-draft them.

**If Pass 1 discovers an ADDITIONAL function beyond the audit's 10** (one
that the audit missed, or that emerges from verify-first cases), the
reactive protocol per `tasks/duckdb-extension-specs/README.md` applies:

1. Write a new `spark_<name>.md` spec per the template in the README.
2. Add the function to the "Pending C++ extension work (`ext5`)" DEFER
   heading in the readiness map's §Slice D.
3. Continue Pass 1 on the functions that ARE in ext4.
4. Commit the new spec file in the same pass that identified it.

The pre-drafted specs assume `ext5` bundles the following (any deviation
from this list at ext5 release triggers a follow-up spec update):
`spark_try_divide`, `spark_try_cast` (or resolves to native `TRY_CAST`),
`spark_kurtosis` (verify-first — may resolve to native), `spark_corr`
(verify-first), `spark_covar_samp` (verify-first), `spark_regr_slope`
(verify-first), `spark_regr_r2` (verify-first), `spark_try_sum`,
`spark_try_avg`, `spark_count_if` (verify-first — may resolve to native).

## Non-goals — do NOT do any of these

- Do NOT modify the `thdck_spark_funcs` C++ extension binaries in this
  repo. The `ext4` release is pinned per ADR-020. New C++ implementations
  land in the separate `thunderduck-duckdb-extension` project via the
  specification handoff described above.
- Do NOT reintroduce `use crate::functions::FunctionRegistry` or
  `use crate::generator::*` in `emission.rs`. INV3's tightened predicate
  (Slice C.2) stays load-bearing.
- Do NOT change legacy `FunctionRegistry`, `SqlGenerator`, or
  `TypeInferenceEngine` bodies. Legacy remains untouched.
- Do NOT skip INV6's activation for the subset of functions that ARE in
  `ext4`. INV6 becomes load-bearing over that subset, not vacuously
  waived because some functions are pending.
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
