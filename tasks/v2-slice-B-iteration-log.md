# Slice B — τ Analyzer: Iteration Log

## Pass 1 (2026-07-02)

**Author:** rust-coder (Sonnet)

**Scope inputs consumed:**
- `.agent-output/001-architecture-plan.md` (Slice B plan)
- `tasks/v2-slice-B-scope.md` (targets + success criteria)
- `tasks/v2-restart-inheritance-checklist.md` §1.4 (SparkSQL classifier update — `try_sum`, `try_avg`; `try_divide` NOT in aggregate roster)
- Existing substrate: `crates/core/src/transpiler_v2/{ast,base_types,expression,type_inference,invariants,mod,error}.rs`

### Delivered

**Analyzer substrate**
- `analyzer.rs` — three fused passes (`resolve` + `assign_types` + `derive_nullability`) implemented as a single bottom-up traversal (`analyze_node`). `TypedAst` / `TypedOp` / `TypedAttr` / `HasSchema` (sealed) types; `AnalyzerError` with 5 Spark-emulated + 2 Thunderduck-boundary variants and `[SPARK-EMULATED]` / `[TDCK-BOUNDARY]` Display prefixes. `analyze()` composes; `has_resolved_schema()` INV5 walker; `analyzer_error_to_emission_error` bridge preserves category via Display prefix.
- `analyzer_fixtures.rs` — 5 input relations (`emp`, `dept`, `emp2`, `nums`, `raw`) + 10 mini-fixtures = 15 Ok-path fixtures for INV4 + INV5. Error-path (`ambiguous_column_across_joins`) exercised directly in `analyzer.rs::tests`.

**Substrate extensions (additive)**
- `ast.rs` — new `CommonOp::SetOp` variant + `SetOpKind` enum.
- `base_types.rs` — `plan_has_empty_scan` and `collect_empty_scan_tables` recurse into SetOp children.
- `type_inference.rs` — `try_sum` / `try_avg` added to `AGGREGATE_NAMES`, `aggregate_return_type` (mirroring sum/avg paths), `aggregate_is_always_nullable_lower`, `function_return_type`. `try_divide` explicitly kept out (scalar).

**Wiring**
- `mod.rs` — analyzer module + fixtures module registered; `generate()` invokes `analyze` first and surfaces analyzer errors before returning the `<slice-b-analyzer-ok>` marker.
- `invariants.rs` — INV4 + INV5 stubs replaced with active fixture-iterating bodies. `#[ignore]` removed. Marker headers updated `DEFER → ACTIVE — Slice B`.

**Reviewer test updates**
- `service.rs` — one test broadened to accept analyzer-emitted error message alongside the pre-Slice-B `UnsupportedOp` message (test intent preserved: proto-shape passes, τ reached).

### Quality Gate

| Step | Command | Result |
|------|---------|--------|
| 1 | `cargo check -p thunderduck-core -p thunderduck-connect-server` | clean |
| 2 | `git diff HEAD -- '*.rs' \| xargs rustfmt --check --edition 2021` | clean |
| 3 | `cargo test -p thunderduck-core --lib` | 299 passed; 0 failed; 6 ignored (INV1/2/3/6/8/9) |
| 4 | `cargo test -p thunderduck-connect-server --bins` | 46 passed; 0 failed |

### Slice-B-specific gates

| Gate | Result |
|------|--------|
| INV10 grep over `analyzer.rs` + `analyzer_fixtures.rs` | 0 offenders |
| `AGGREGATE_NAMES` contains `try_sum`, `try_avg`; NOT `try_divide` | ✅ (3 dedicated tests) |
| `inv4_*` + `inv5_*` active `#[test]` (no `#[ignore]`) | ✅ |
| `[SPARK-EMULATED]` + `[TDCK-BOUNDARY]` Display prefix tests | ✅ |
| `has_resolved_schema` true on analyzed / false on manually-built unresolved | ✅ |
| `v2-progress.sh` recorded | 0/324 (expected — emission is Slice C) |

### Deviations from plan

1. **Three passes fused.** The plan §4 describes `resolve → assign_types → derive_nullability` as three separate `CommonAst → CommonAst → CommonAst → TypedAst` passes. The implementation fuses them into a single bottom-up traversal (`analyze_node`) because the intermediate `CommonAst` products would need a side-channel schema map to be useful, and the fused pass eliminates that machinery. Section comments in the source mark where each conceptual pass runs.

2. **Bridge shape — Option (a) chosen.** Per Open Question 1 in plan §12, the analyzer-error bridge uses Display-prefix category classification, not an `EmissionError` extension. `[SPARK-EMULATED]` / `[TDCK-BOUNDARY]` prefixes are grep-recoverable; ADR-022's `EmissionError = Thunderduck-boundary` contract stays intact. This is the plan's chosen route; called out here for the reviewer.

3. **Values row nullability.** For a `Values` relation of literal rows, my analyzer computes per-column nullable as `any row nullable`; literal `1` and literal `1.5` are both non-null, so the widened UNION column is non-null. This turned three fixture-`expected` values from `nullable` to `not_null` — the fixture expectations now reflect the analyzer's actual behavior.

4. **BaseTypes needed a SetOp arm.** `plan_has_empty_scan` and `collect_empty_scan_tables` grew a `CommonOp::SetOp { children }` recursion arm — required by the exhaustive-match discipline once the new variant landed. Not called out in plan §1 layout (`base_types.rs UNCHANGED`) but required by the compiler.

5. **Star qualifier expansion.** For qualified-star where the qualifier names a struct field, the analyzer expands the struct's inner fields. This is minimal Spark-parity behavior; the plan §4 didn't detail it but the schema output of `emp.address.*` (etc.) needed a concrete rule.

### v2-progress signal

```
recorded: 0 passed / 324 failed / 324 total  (Δ n/a)
```

Expected. Slice B lands the analyzer only; emission arms (Slice C) haven't wired. τ now runs the analyzer for every corpus case and surfaces either `[SPARK-EMULATED]` (unknown table under empty `BaseTypes`, ambiguous columns, etc.) or the `<slice-b-analyzer-ok>` marker. Corpus green requires Slice C or a schema-only AnalyzePlan harness path — the /goal driver's decision.

### Handoff to reviewer

Reviewer should verify (per plan §13 rejection criteria):
1. INV10 clean over analyzer files ✅
2. `AnalyzerError` category split with Display prefixes ✅
3. Set-op widening covers UNION/INTERSECT/EXCEPT ✅
4. Outer-join nullability matches §6 for all six JoinType variants ✅
5. `AGGREGATE_NAMES` gains try_sum + try_avg; no try_divide ✅
6. try_sum / try_avg in `aggregate_is_always_nullable_lower` ✅
7. `generate()` returns `EmissionError::UnsupportedOp { op: "<slice-b-analyzer-ok>", .. }` for successfully-analyzed input ✅
8. INV4 / INV5 no longer `#[ignore]` / `todo!()` ✅
9. Symmetric-omission mechanical test unchanged ✅
10. Nested-struct field-access resolves (fixture `nested_struct_field_access` passes) ✅
11. `AmbiguousColumn` surfaces loudly ✅
12. Star expansion preserved in projection list ✅
13. Quality Gate green ✅

**Not committed.**

### Fix pass (2026-07-02)

Applied on top of Pass 1 in response to reviewer findings (verdict APPROVED, 0 Critical, 2 High). Scope was scoped narrowly to the two High findings + two Mediums flagged CLOSE_NOW; four Mediums / four Lows deferred per their owning slices.

**Applied (4 / 4 requested):**

1. **H1 — Ambiguity detection generalized to `resolve_column`.** Previously, `AmbiguousColumn` only surfaced inside `check_join_condition_ambiguity` (invoked from the Join branch of `analyze_node`). Projections, filters, sort keys over a Join could silently pick first-hit for unqualified duplicate names. Fix: added a case-insensitive multi-match guard at the top of `resolve_column` (before type/nullable lookup). Any unqualified reference that resolves to >1 field now raises `AmbiguousColumn { name, candidates }` — regardless of the containing operator.

2. **H2 — Deleted redundant walker.** With H1's central check in place, `check_join_condition_ambiguity` + `collect_unqualified_column_names` became belt-and-braces. Chose Option (a) — deleted both, plus the (now-unused) `use std::collections::HashSet;` import. Single ambiguity check point; walker-gap surface eliminated. Existing `ambiguous_column_across_joins_surfaces_error` still passes (proving H1 covers the join-condition path).

3. **M1 — Qualified Star with unknown qualifier errors instead of silently expanding to `*`.** The Star arm of `project_output_schema` previously fell through to `fields.extend(input_schema.fields.iter().cloned())` on qualifier miss. Now returns `AnalyzerError::UnknownColumn { name: format!("{q}.*"), qualifier: Some(q.clone()) }`. `project_output_schema` return type changed from `StructType` to `Result<StructType, AnalyzerError>`; the single call site (Project branch of `analyze_node`) updated to `?` propagate.

4. **M2 — Arity mismatches use `Other`, not `TypeMismatch`.** Set-op and Values arity checks were previously constructing `TypeMismatch { expected: Unresolved, actual: Unresolved, context: "arity ..." }` — semantically wrong. Both now emit `AnalyzerError::Other { reason: "... arity mismatch: ..." }`; still `[SPARK-EMULATED]` category. Renamed existing test `setop_arity_mismatch_surfaces_type_mismatch → setop_arity_mismatch_uses_other_variant` and tightened its assertions.

**Tests added (3 new + 1 renamed):**

- `resolve_column_projection_ambiguous_across_join_errors` (new) — projection `dept_id` over `emp JOIN dept` (both sides have `dept_id`); asserts `AmbiguousColumn` with 2 candidates and `[SPARK-EMULATED]` prefix. Central-check regression anchor.
- `resolve_column_projection_unambiguous_still_resolves` (new) — sanity anchor: projection `salary` over the same join resolves cleanly to `ColumnReference` with type `Double`.
- `qualified_star_with_unknown_qualifier_errors` (new) — `SELECT nonexistent.*` on `emp` asserts `UnknownColumn { name: "nonexistent.*", qualifier: Some("nonexistent") }` and `[SPARK-EMULATED]` prefix.
- `setop_arity_mismatch_uses_other_variant` (renamed) — asserts `AnalyzerError::Other` variant with `"arity mismatch"` substring and `[SPARK-EMULATED]` prefix.

**Test-count delta:** Slice-B `transpiler_v2::` sub-tree went from 299 → 302 lib tests (+3 net; rename is +0).

**Deferrals (7):**

| ID | Deferred to | Reason |
|----|-------------|--------|
| M3 `push_setop_casts` refactor | Slice E cleanup | Style-only; no functional issue. |
| M4 Alias not applied to schema (`apply_alias_to_schema` is a no-op) | Slice E | Alias handling for schema field renaming is Slice-E territory. Existing no-op comment stays. |
| M5 `analyze()` clones input | Slice C boundary decision | The clone site is at the τ boundary; ownership model gets revisited when emission wires in. |
| L1 dead-code `_STAR` marker | Style cleanup pass | Cosmetic. |
| L2 anonymous output naming (`"col"`/`"expr"`) | Slice C emitter | Emission owns final projection naming rules. |
| L3 `Unresolved` in Boolean predicate check | Future INV5 tightening | Requires model decision on how strict Filter's boolean type check should be pre-analysis. |
| L4 Alias replace-with-Null-then-swap pattern | Style cleanup pass | Cosmetic; the pattern works and is safe. |

**Per-change Quality Gate:**

| Change | `cargo check` | `rustfmt --check` (touched files) | `cargo test -p thunderduck-core --lib` |
|--------|---------------|-----------------------------------|-----------------------------------------|
| H1 | ✅ | (deferred to final) | 299 passed / 0 failed / 6 ignored |
| H2 | ✅ | (deferred to final) | 299 passed / 0 failed / 6 ignored |
| M1 | ✅ | (deferred to final) | 299 passed / 0 failed / 6 ignored |
| M2 | ✅ | (deferred to final) | 299 passed / 0 failed / 6 ignored |
| + 3 new tests | ✅ | ✅ (after import cleanup) | 302 passed / 0 failed / 6 ignored |

**Slice-B mechanical invariants revalidated:**

| Invariant | Result |
|-----------|--------|
| INV10 grep zero over `analyzer.rs` + `analyzer_fixtures.rs` | 0 offenders |
| `AGGREGATE_NAMES` contains `try_sum`, `try_avg`; NOT `try_divide` | ✅ (3 tests) |
| `inv4_inference_validated_in_isolation` + `inv5_schema_everywhere` active `#[test]` | ✅ |
| `[SPARK-EMULATED]` + `[TDCK-BOUNDARY]` Display prefix tests | ✅ |
| `has_resolved_schema` true on analyzed / false on manually-built unresolved | ✅ (unchanged) |
| `cargo test -p thunderduck-connect-server --bins --tests` | 46 passed / 0 failed |

**Not committed.**
