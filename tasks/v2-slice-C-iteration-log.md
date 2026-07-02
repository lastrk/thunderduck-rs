# Slice C Iteration Log

Slice C — τ emission substrate + operator arms + scalar/aggregate arms.

Baseline: commit `848ff0d` — 0/324 core_v2 (Slice B analyzer landed, no emission).

Cumulative sub-slice targets (per scope + readiness map):
- **C.1** ≈ 30-50 (operator arms + analyzer-only-passing cluster).
- **C.2** ≈ 120-150 (cumulative — adds scalar function arms).
- **C.3** ≈ 180-200 (cumulative — adds primitive aggregate arms).

Design authority: ADR-009 (declarative emission table — Approach A permanent),
ADR-021 (τ owns substrate), ADR-022 (τ is the only path; two error categories).
Inheritance discipline: `tasks/v2-restart-inheritance-checklist.md` §3.1 / §3.2 /
§4.2-first / §5.1 / §5.3 / §5.4 / §5.6 / §5.7.

Methodology: `tasks/v2-slice-iteration-methodology.md` (5-pass hard cap; sub-slice
passes count against the cap).

## Preflight (2026-07-02)

- `cargo check -p thunderduck-core -p thunderduck-connect-server` — clean.
- `tests/scripts/v2-progress.sh` — `0 passed / 324 failed / 324 total`
  (matches Slice B landing note).

## Pass 1 — 2026-07-02 — sub-slice C.1

Prompt: derived from `tasks/v2-slice-C-scope.md` §Sub-slice sketch → C.1.
Dispatched via `/new-feature`.

**Author:** rust-coder (Sonnet)

**Scope inputs consumed:**
- `.agent-output/001-architecture-plan.md` (Slice C.1 plan, §0–§10)
- `tasks/v2-restart-inheritance-checklist.md` §4.2 first item, §5.1, §5.3, §5.4, §5.6
- `tasks/v2-restart-open-decisions.md` Decision 13-A (six unwired renderers)
- Existing substrate: `crates/core/src/transpiler_v2/{ast,analyzer,base_types,expression,type_inference,error,invariants,mod}.rs`

### Delivered

**Emission substrate (new)**
- `emission.rs` — 1077 LOC (non-test 700, tests 377). Contains:
  - INV2 companion: `EMIT_TAP: AtomicU64` + `EMIT_TAP_MUTEX: Mutex<()>` (§5.3).
  - `EmittedSql` newtype (constructor private to file).
  - `dispatch_op` — Approach A hand-written match, one arm per real Slice-B `TypedOp` variant (SingleRow / TableScan / Values / LocalRelation / FileScan / Project / Filter / Sort / Limit wired; Aggregate / Join / SetOp / TableFunction / Unnest boundary-erred).
  - `render_expr` — exhaustive match over every `Expression` variant (28 variants).
  - `render_cast` — `try_cast` branch emits `TRY_CAST(expr AS ty)` (§4.2 first item).
  - Operator renderers: `render_project` (with `spark_return_cast` at projection slot), `render_filter`, `render_sort`, `render_limit`, `render_single_row`, `render_table_scan`, `render_values`, `render_local_relation`, `render_file_scan`.
  - Six unwired helpers under Decision 13-A (`render_tail` CTE per §5.4; `render_distinct`, `render_with_columns`, `render_drop_columns`, `render_aliased_relation`, `render_range_relation`) — all `#[allow(dead_code)]`.
  - `spark_return_cast` (wired) and `spark_aggregate_return_cast` (`#[allow(dead_code)]` — Slice C.3 wires it) — SEPARATE `fn` items (§5.1).
  - `quote_ident` — `Cow<'_, str>` fast path (§5.6). 79-word reserved-word list.
  - `render_data_type`, literal / atomic renderers, `extension_targets()` returning empty (§4.1 stub).
  - `render_function_call` stub → `UnsupportedFunction`; `render_aggregate` stub → `UnsupportedOp`.

**Empty rewrites shell (new)**
- `rewrites.rs` — module-level doc only, Slice G populates.

**Wiring**
- `mod.rs` — `pub mod emission;`, `pub mod rewrites;`. `generate()` now calls `analyze` → `dispatch_op`. The Slice-B marker `<slice-b-analyzer-ok>` retired; SingleRow test asserts real SQL (`"SELECT"`).
- `invariants.rs` — INV2 and INV3 activated (no more `#[ignore]` / `todo!()`). Active bodies exercise `EMIT_TAP` / grep barrier respectively.

**Reviewer/service-test updates**
- `service.rs` — two tests updated: `transpile_relation_sql_routes_to_parser_v2_not_converter` and `finalize_short_circuits_on_plans_without_empty_scan`. Both previously asserted τ errored; now assert τ successfully emits SQL for the Slice-C.1-wired arms (SingleRow, Project over literals over SingleRow). Routing intent preserved.

### Quality Gate

| Step | Command | Result |
|------|---------|--------|
| 1 | `cargo check -p thunderduck-core` | clean |
| 2 | `cargo check -p thunderduck-connect-server` | clean (31 pre-existing dead-code warnings on legacy converter/generator — untouched) |
| 3 | `git diff --name-only HEAD -- '*.rs' \| xargs -r rustfmt --check --edition 2024 --config skip_children=true` | clean |
| 4 | `cargo test -p thunderduck-core --lib` | **340 passed; 0 failed; 4 ignored** (INV1/6/8/9 remain deferred) |
| 5 | `cargo test -p thunderduck-connect-server --bins` | **46 passed; 0 failed** |

Notes on `--lib --tests`: the `runtime_integration` integration test binary has one failing case (`struct_field_name_case_is_preserved`) with an environment DuckDB extension version mismatch (`v1.5.4` vs `v1.5.1`). Confirmed pre-existing at baseline `848ff0d` (Slice B HEAD) — not introduced by C.1.

Clippy is out of gate per CLAUDE.md §Quality Gate.

### Slice-C-1-specific gates

| Gate | Result |
|------|--------|
| INV2 companion active — `EMIT_TAP` + `EMIT_TAP_MUTEX` in `emission.rs`; `inv2_dispatch_is_only_sql_writer` passes | ✅ |
| INV3 grep barrier — `git grep -E 'use crate::(generator\|functions)::' emission.rs` returns zero; `inv3_no_legacy_use_in_emission` + `inv3_emission_table_single_source_of_truth` pass | ✅ |
| `render_tail` uses CTE (§5.4) — asserted by `render_tail_uses_cte_not_double_embed` | ✅ (child SQL substring appears exactly once) |
| `spark_return_cast` and `spark_aggregate_return_cast` are distinct `fn` items (§5.1) — asserted by `spark_return_cast_and_aggregate_return_cast_are_distinct_fns` | ✅ (fn-pointer identity mismatch) |
| `quote_ident` fast path returns `Cow::Borrowed` on safe identifiers (§5.6) — asserted by `quote_ident_fast_path_returns_borrowed_for_unquoted_safe` | ✅ |
| Approach A dispatch — one hand-written arm per `TypedOp` variant | ✅ (13 arms — 9 wired + 4 boundary-erred) |
| `rewrites.rs` empty (Decision 6) | ✅ (single-line doc, nothing else) |
| `extension_targets()` returns empty (Slice D populates) | ✅ (`extension_targets_is_empty_at_slice_c1`) |
| `EmissionError::UnsupportedFunction` returned for scalar function names (ADR-022) | ✅ (`render_expr_function_call_stub_returns_unsupported_function`) |
| `Cast::try_cast` emits `TRY_CAST(expr AS ty)` (§4.2 first item) — asserted by `render_cast_try_cast_emits_try_cast` | ✅ |
| INV10 grep zero — `git grep -E 'use crate::(generator\|functions\|logical\|expression\|parser\|runtime\|converter)::' transpiler_v2/` returns zero | ✅ (INV10 walker + INV10 positive-shape test both pass) |

### Deviations from plan

1. **Test count is 36, not "~32".** The plan's §8 minimum set is 32; I landed 34 core plan-anchored tests plus 2 extra (`emit_tap_increments_on_ok_dispatch`, `emit_tap_does_not_increment_on_err_dispatch`, `render_interval_emits_interval_literal`, `render_column_reference_qualified`) that anchor INV2 semantics beyond what §8 required.
2. **Self-referential test literal fix.** Plan §6 shows INV3's grep test using literal `"use crate::generator::"` as a needle. This literal appears inside `emission.rs` itself once the test body is added, self-triggering. Fix: scan only the pre-`#[cfg(test)]` region of `emission.rs`, and build the needle strings at runtime from a `base` variable so the test source contains no offending literal. Applied to both `emission::tests::inv3_no_legacy_use_in_emission` and `invariants::inv3_emission_table_single_source_of_truth`. Same treatment for the INV10 positive-shape test — test code legitimately imports from `crate::transpiler_v2::…`, so only the non-test region is checked.
3. **`SingleRow` renders as bare `"SELECT"`.** The plan's §2 skeleton did not specify the exact string. DuckDB parses `SELECT` (no columns) as a one-row zero-column relation; wrapping to `SELECT * FROM (SELECT) t` isn't necessary at the leaf. Callers that consume `SingleRow` as an input (e.g. `Project`) already wrap the child as `FROM (<sql>) AS __td_proj` — DuckDB accepts this shape.
4. **Two consumer tests in `service.rs` updated.** Two `#[test]`s in `crates/connect-server/src/service.rs` (`transpile_relation_sql_routes_to_parser_v2_not_converter`, `finalize_short_circuits_on_plans_without_empty_scan`) previously asserted that τ *errored* at A.3. Now that C.1 wires SingleRow + literal Projects, τ succeeds for both. Their routing / short-circuit intent is preserved (the update asserts τ emits SQL rather than errors); the tests continue to serve as anti-regression witnesses.
5. **`render_data_type(DataType::Null)` returns `"INTEGER"`.** Best-effort — DuckDB doesn't have a first-class NULL scalar type-string in `CAST(... AS <type>)`. Legacy uses `INTEGER` as the fallback cast target for typed-NULL projections. This may need revisiting when complex-type LocalRelation cases start flowing through in Slice F.
6. **Timestamp/Date literal rendering uses `epoch_us` / `epoch_ms`.** DuckDB's temporal-literal syntax is unusual for micros-since-epoch encoding; my `render_literal` uses `epoch_ms(days * 86400000)` and `epoch_us(micros)` respectively. This is untested against the corpus at C.1 because LocalRelation rendering hits ArrayLiteral (Slice F) before reaching a temporal literal.

### v2-progress signal

```
recorded: 0 passed / 324 failed / 324 total  (Δ n/a)
```

**Signal is 0 — well below the plan's 30-50 target. Per prompt: STOP-and-report.**

Root cause is *not* an emission or substrate defect. The differential harness requires end-to-end query execution (Spark reference vs Thunderduck) and result comparison. τ successfully emits SQL for Slice-C.1-wired arms — I verified `SELECT 1` (SQL text via parser_v2) round-trips through τ successfully — but the connect-server dispatch path errors with `Slice E: streaming query execution over CommonAst` before reaching DuckDB. See `crates/connect-server/src/service.rs::execute_streaming_query` — it errors unconditionally per the readiness map (Slice E owns query execution).

Additionally, the DataFrame corpus's input tables (`emp`, `dept`, etc.) are `LocalRelation` payloads containing Array/Map/Struct columns (`{"team": ...}`, tag lists, nested structs). Even for scalar-only projections, τ must render the *entire* LocalRelation payload before projecting; the ArrayLiteral / MapLiteral / StructLiteral variants correctly boundary-error as `UnsupportedExpression{ shape: "ArrayLiteral", reason: "complex-type emission lands in Slice F" }` — expected per plan §4 table.

**Recommendation:** the plan §7 progress target ("≥30 corpus passing cases at C.1") is not achievable at Slice C.1 in isolation. Achieving corpus passes requires the *conjunction* of (a) C.1's wired arms **plus** (b) Slice E's streaming execution wiring **plus** (c) Slice F's complex-type LocalRelation rendering. Suggested action for the /goal driver: either land Slice E execution as an immediate follow-on, or use `cargo test -p thunderduck-core --lib` as the C.1 fitness function until Slice E lands.

### Handoff to reviewer

Reviewer should verify (per plan §7 success criteria):
1. INV2 active — `EMIT_TAP` in emission.rs; `inv2_dispatch_is_only_sql_writer` passes ✅
2. INV3 barrier — emission.rs contains no `use crate::generator::` / `use crate::functions::`; INV3 test active ✅
3. `render_tail` CTE (§5.4) — worked shape asserted; child SQL not double-embedded ✅
4. `spark_return_cast` and `spark_aggregate_return_cast` are two distinct `fn` items ✅
5. `quote_ident` fast path returns `Cow::Borrowed` on `[A-Za-z_][A-Za-z0-9_]*` non-reserved ✅
6. Approach A — one match arm per `TypedOp` variant (13 arms) ✅
7. `rewrites.rs` empty ✅
8. `extension_targets()` returns empty ✅
9. `UnsupportedFunction` for scalar function names (ADR-022 boundary) ✅
10. `Cast::try_cast` emits `TRY_CAST` (§4.2 first item) ✅
11. INV10 grep zero — no disallowed imports under τ tree ✅
12. Quality Gate green ✅ (with pre-existing `runtime_integration` env issue noted)

**Not committed.**

### Fix Iteration 1 (2026-07-02) — reviewer findings

Applied on top of Pass 1 in response to Stage 3 reviewer verdict (APPROVED, 0 Critical, 1 High). Scope: H1 + M2 + M5 closed (H1 is High = CLOSE_NOW per methodology; M2/M5 are CLOSE_NOW_HYGIENE). Four deferred to owning slices per readiness map.

**Applied (3 / 3 CLOSE_NOW):**

1. **H1 — Deleted dead `EmittedSql` newtype.** The type carried a docstring claiming *"the sole constructor is co-located with EMIT_TAP inside dispatch_op, so every EmittedSql witnesses a tap event"* — a claim not enforced by the shipped code (`dispatch_op` returned `Result<String, EmissionError>`, never constructed an `EmittedSql`). The tap-once invariant is enforced instead by `dispatch_op` being the only `fetch_add` writer to `EMIT_TAP`. Deleted the struct + impl block + section-header comment. Zero external consumers.

2. **M2 — Extended `DUCKDB_RESERVED` with 20 keywords.** Added `cross`, `full`, `groups`, `inner`, `join`, `left`, `list`, `map`, `natural`, `outer`, `over`, `partition`, `pivot`, `qualify`, `range`, `right`, `rows`, `sample`, `struct`, `unpivot` (alphabetically inserted; ascending order preserved). Rationale: reviewer flagged these as frequent Spark corpus column-name collisions (e.g. `df.select("left")` would emit `SELECT left FROM ...`, which DuckDB rejects). Bites once Slice E execution lands.

3. **M5 — Renamed test.** `dispatch_op_single_row_emits_select_from_empty` → `dispatch_op_single_row_emits_bare_select` (assertion is `assert_eq!(sql, "SELECT")` — no FROM clause; name now matches).

**Deferrals (4):**

| ID | Sev | Deferred to | Reason |
|----|-----|-------------|--------|
| M1 `render_local_relation` panic on width mismatch | Medium | Slice F | Defense-in-depth; V2RelationConverter guarantees row×schema width today. When Slice F wires complex-type LocalRelation payloads, this becomes reachable and gets a length-check upfront. |
| M3 `render_data_type(DataType::Null) = "INTEGER"` | Medium | Slice F | Complex-type LocalRelation cases are the ones that surface DataType::Null. Slice F owns complex-type emission. |
| L2 `render_projection_slot` doesn't peel nested Alias(Alias) | Low | Slice F | Rare Spark idiom; complex-type slice owns. |
| L3 `spark_return_cast` doesn't recurse into nested Div | Low | Slice C.2 | Planned per plan §5.1 — extends when scalar function arms wire. |

**Quality Gate (post fixes):** all steps PASS. `cargo test -p thunderduck-core --lib` = 340 passed / 0 failed / 4 ignored. `cargo test -p thunderduck-connect-server --bins` = 46 passed / 0 failed.

**Not committed.**

### Performance Optimizations (2026-07-02) — Stage 4 findings

Applied on top of Fix Iteration 1 in response to Stage 4 perf review (HAS_OPPORTUNITIES, 1 HIGH + 3 MEDIUM, all CLOSE_NOW / CLOSE_NOW_HYGIENE).

**Applied (4 / 4):**

1. **H1 — `is_safe_identifier` zero-alloc fast path.** The `Cow::Borrowed` fast path in `quote_ident` (§5.6) was silently defeated by `is_safe_identifier` calling `.to_lowercase()` on every input — allocating a fresh String even on the "safe identifier" case. Since `DUCKDB_RESERVED` is all-lowercase ASCII and safe-identifier inputs are restricted to ASCII by the character-class check, replaced the lowercased-String reserved-word test with a byte-level `ascii_ci_cmp` comparator. The fast path now allocates zero heap memory; the §5.6 anchor is honest.

2. **M1 — `DUCKDB_RESERVED` binary search.** The list is 91 words after Fix Iteration 1's M2. Replaced `.contains(&lowered.as_str())` linear scan with `binary_search_by(|w| ascii_ci_cmp(w.as_bytes(), name.as_bytes()))`. Added two invariant guards asserting `DUCKDB_RESERVED` remains lowercase-ASCII and ascending-sorted so future edits can't silently invalidate the binary-search precondition.

3. **M2 — List-building renderers: Vec→String buffer.** Rewrote seven renderers (`render_values`, `render_local_relation`, `render_file_scan`, `render_project`, `render_sort`, `render_with_columns`, `render_drop_columns`) to build the SQL directly into a pre-allocated `String` buffer instead of an intermediate `Vec<String>` + `.join(", ")`. Byte-identical SQL confirmed by the assertion-based unit tests (no drift). Eliminates one Vec allocation + N String allocations + their Drops per call.

4. **M3 — `is_aggregate_name` ASCII case-insensitive.** Replaced `AGGREGATE_NAMES.contains(&f.name.to_lowercase().as_str())` allocation with `AGGREGATE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(&f.name))`. `AGGREGATE_NAMES` in `type_inference.rs` is all-lowercase ASCII (verified) so this is semantically identical and zero-alloc.

**Test-count delta**: 340 → 342 (M1 added two invariant guards).

**Quality Gate (post optimizations):** all steps PASS.

| Step | Command | Result |
|------|---------|--------|
| 1 | `cargo check -p thunderduck-core` | clean |
| 2 | `cargo check -p thunderduck-connect-server` | clean (31 pre-existing dead-code warnings on legacy converter/generator — untouched) |
| 3 | `rustfmt --edition 2024 --check crates/core/src/transpiler_v2/emission.rs` | clean |
| 4 | `cargo test -p thunderduck-core --lib` | **342 passed** / 0 failed / 4 ignored |
| 5 | `cargo test -p thunderduck-connect-server --bins` | **46 passed** / 0 failed |

**Not committed.**

## Termination — 2026-07-02

**Passes counted against the 5-pass cap:** 1 (Pass 1 landed all of C.1's substrate + reviewer fixes + perf optimizations in a single architect→coder→reviewer→coder→perf→coder chain).

**Verdict:** Slice C.1 landed. Slice C.2 and C.3 REMAIN OPEN and are escalated back to the user per HALT-AND-FLAG trigger #2 (upstream substrate missing).

**Reason for escalation.** The `/goal` template's cumulative corpus targets (C.1 ≈ 30-50, C.2 ≈ 120-150, C.3 ≈ 180-200) are architecturally unattainable at Slice C alone. The differential harness invokes end-to-end Spark Connect query execution via `crates/connect-server/src/service.rs::execute_streaming_query` (line 407), which unconditionally errors `Status::unimplemented("Slice E: streaming query execution over CommonAst")`. τ emits correct SQL for the C.1-wired arms — verified by the two updated `service.rs` tests + the 342 core lib tests — but the SQL never reaches DuckDB because execution is Slice E's job. **The corpus signal cannot move until Slice E lands.** User's directive (2026-07-02): "Terminate Slice C.1 only; escalate."

**C.1's realized fitness function (accepted in lieu of corpus signal):**

| Signal | Baseline (post Slice B) | Post C.1 |
|--------|-------------------------|----------|
| `cargo test -p thunderduck-core --lib` | 302 passing | **342 passing** (+40) |
| `cargo test -p thunderduck-connect-server --bins` | 46 passing | 46 passing (unchanged) |
| INV2 companion active | DEFER stub | ✅ ACTIVE (`inv2_dispatch_is_only_sql_writer`) |
| INV3 grep barrier | DEFER stub | ✅ ACTIVE (`inv3_emission_table_single_source_of_truth`) |
| INV4 / INV5 / INV10 | Active from Slice B | ✅ Preserved (all still active) |
| `v2-progress.sh` | 0 / 324 | 0 / 324 (unchanged — blocked by Slice E per above) |

**Deferred to future slices (documented in readiness map §Slice C landing note):**

| ID | Slice | Reason |
|----|-------|--------|
| M1 LocalRelation panic path | Slice F | complex-type LocalRelation payloads |
| M3 DataType::Null | Slice F | complex-type LocalRelation |
| L2 nested Alias peel | Slice F | rare Spark idiom |
| L3 nested-Div return_cast recursion | Slice C.2 | scalar-arm territory |
| Six `#[allow(dead_code)]` unwired renderers | future substrate slice(s) that add `TypedOp::Tail` / `Distinct` / `WithColumns` / `DropColumns` / `AliasedRelation` / `Range` | Decision 13-A |

**Not committed.** User reviews before commit.
