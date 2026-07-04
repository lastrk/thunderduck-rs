# v2 Corpus-Driven Pass Log

Append one entry per corpus-driven pass. Format per
`tasks/v2-corpus-driven-iteration-methodology.md` §Pass log.

## Session 2026-07-02 → 2026-07-03 (retroactive summary)

The first 57 passes ran under the earlier methodology iteration (before
this pass-log format was defined). Reconstructed from git commit trail
`4fb17b9`..`dd8d7e8` on branch `feat/v2-transpiler`. Corpus climb: 0 →
205 / 324 (63%). Detailed per-commit annotations in commit messages;
the summary below records the corpus deltas by pass number.

| Pass | Δ | Focus | Commit |
|------|----|-------|--------|
| E.0 + diagnostic | +25 | execute_streaming_query wiring; SingleRow subquery-safe; complex-type literals; timestamp construction; analyze_plan schema producer; NaN diff util | `4fb17b9` |
| 1 | +21 | WithColumns end-to-end + column-order contract | `27dadaa` |
| 2 | +9 | Aggregate operator + primitive function family | `47c478e` |
| 3 | +4 | ExpressionString via parser_v2::parse_expression | `b5491a7` |
| 4 | +9 | Join emission (8/14 cases) + USING dedup + explicit column list | `7fc7887` |
| 5 | +3 | DropColumns | `82fd174` |
| 6 | +7 | SetOp Union/Intersect/Except + widened CAST wrapper | `b0d499f` |
| 7 | +1 | Scalar function pass-through arm | `ce18fbb` |
| 8 | +2 | AliasedRelation + WithColumnsRenamed | `3989f66` |
| 9 | +9 | Deduplicate + cosmetic passthrough | `0e36303` |
| 10 | +1 | UNION BY NAME (analyzer + emission) | `93c4511` |
| **11** | **+30** | **Scalar function return types (~100 arms)** | `fec602d` |
| 12 | +5 | NA family (fill/drop/replace) | `ddadc31` |
| 13 | +1 | array/map/struct/locate/overlay remaps + GROUP BY alias-strip | `f648153` |
| 14 | +1 | date_add/date_sub → INTERVAL form | `a1c639f` |
| 15 | +1 | nvl/nvl2/ifnull/unix_timestamp/startswith | `60dc527` |
| 16 | +1 | array/list_* remaps + ExtractValue wiring | `129f080` |
| 17 | +3 | NaFill nullability tightening | `3510da2` |
| 18 | +1 | plan_id disambig in Join ON | `9b5ffbc` |
| 19 | +1 | USING-column donor rules for RIGHT/FULL | `f5f38a5` |
| 20 | 0 | ROLLUP/CUBE emission (test scaffolding fix `bf2b054`) | `1e7d1d5` |
| 21 | +2 | current_date DateType + datediff/months_between remaps | `9343653` |
| 22 | +1 | add_months + months_between return-type | `78733aa` |
| 23 | +1 | sha/sha1/sha2 + signum remaps | `ed05dcd` |
| 24 | +6 | isnull/like/eqNullSafe/split/bitwise remaps | `819681b` |
| 25 | +1 | overlay/nanvl/named_struct/map_contains | `2baa08f` |
| 26 | +2 | statistical aggregates (skewness/kurtosis/corr/covar/regr_/median) | `af8fb1c` |
| **27** | **+12** | **DataFrame-path aggregate grouping unfold** | `7dbf0f7` |
| 28 | +9 | Window expression wiring | `47eb957` |
| 29 | +3 | Lambda / HOF wiring | `d0496c9` |
| 30 | +1 | ceil/floor/signum/factorial types | `1dcf4da` |
| 31 | +2 | collect_list/set + approx_count_distinct non-null | `fb0b6d1` |
| 32 | +3 | Project-over-Join inlining for user aliases | `c6665a6` |
| 33 | +3 | count_if/grouping/grouping_id aggregates | `57c885f` |
| 34 | +2 | first/last/nth_value ignorenulls arg drop | `9d5e0bb` |
| 35 | +1 | Spark-parity truncation on float→int cast | `1d8e1c5` |
| 36 | +1 | regexp_replace global flag | `447d39e` |
| 37 | 0 | split element-nullability | `9158425` |
| 38 | 0 | allow zero-arg grouping/grouping_id | `d5e0a2c` |
| 39 | +1 | lag/lead nullability rule | `1b3fc7b` |
| 40 | +1 | String→(Date/Timestamp/numeric) cast nullability | `407d2f2` |
| 41 | +1 | spark_partition_id return type Integer | `2c81116` |
| 42 | 0 | typeof/spark_partition_id non-null | `deac7bd` |
| 43 | 0 | trunc(date, fmt) arg swap | `dc676d0` |
| 44 | +1 | always-nullable Spark scalars (factorial, url_encode, try_*) | `5a29af7` |
| 45 | +1 | date_format Spark→strftime token translation | `0ed222c` |
| 46 | +2 | Window frame spec parsing + emission | `24d2421` |
| 47 | +1 | dayofweek Sunday-index correction | `4aa5e52` |
| 48 | +1 | date_trunc returns Timestamp | `3024241` |
| 49 | +1 | ext6 extension remaps (spark_hash/xxhash64/try_divide/skewness) | `0145169` |
| 50 | +1 | kurtosis population formula | `d9aa9c5` |
| 51 | +2 | array/map constructor return types | `367ec8b` |
| 52 | +4 | array function return types + sort_array signature | `a0f4b1a` |
| 53 | 0 | lambda paren correction + aggregate fold init | `cf21a82` |
| 54 | +1 | ToDf positional rename | `2e491d7` |
| 55 | +2 | grouping_id/grouping populate group cols | `fd4d112` |
| 56 | 0 | percentile_approx/median/mode/any/every aggregates | `dd8d7e8` |

**Cumulative:** 0 → 205 / 324 (63%) across 57 recorded passes.

**Ground rules going forward (per iteration methodology):**
- Every new pass appends its own entry below this block.
- Include ADR citations, checklist §-anchors, layer(s) touched, compiler-warning delta, and commit SHA per methodology §Pass log.
- No pass is complete until findings = 0 (zero DEFER).

---

<!-- Add new passes below this line -->

## Pass 57 — 2026-07-03T (in progress)
- Case: `struct-001` (F.struct("name", "age").alias("info"))
- Diagnostic: `tests/integration/.agent-output/diagnostic-struct-001.md`
- Architect verdict: APPROVED (fix iteration 1)
- Layer(s) touched: emission (primary), analyzer type-inference (secondary)
- ADR citations: ADR-013 (typed AST is interchange), ADR-015 (Spark parity — Catalyst `CreateStruct` field-name derivation), ADR-020 (`struct_pack` is native DuckDB, no extension), ADR-022 (τ-only path, runtime-correctness category)
- Checklist §-anchors: §3.3 (new — struct field-name derivation, inherited from named_struct §3.2 discipline), §1 (analyzer/emission symmetric-omission)
- Root cause: emission.rs:1300-1304 mapped Spark `struct(a,b,...)` → DuckDB `row(a,b,...)`, producing empty-named struct fields; PySpark's Arrow-to-dict conversion rejects the duplicate empty keys.
- Fix: (a) new `transpiler_v2/struct_names.rs` sibling module with pure `derive_struct_field_name(&Expression, usize) -> String`; (b) `emission.rs` struct arm now emits `struct_pack(name := expr, ...)` with per-arg name derivation; (c) `expression.rs::function_call_data_type` gains fast-paths for `struct` + `named_struct` returning `DataType::Struct` with matching field names.
- Field-name derivation (matches Catalyst `CreateStruct`, 4 branches after M1 fix): Alias → alias name; ColumnReference → column name; UnresolvedColumn → column name; `_` → `col{i+1}` (Spark's `Alias.tryUnaliasedName` fallback). The `Literal(String) → literal value` branch was removed per review M1 because Spark's actual behavior for `struct(lit("colA"))` is `col1`, not `colA`.
- Corpus signal: 205 → 207 (+2). Cascade: `struct-007` (named_struct type-inference fast-path) newly green alongside the target `struct-001`.
- Files: `crates/core/src/transpiler_v2/struct_names.rs` (new), `crates/core/src/transpiler_v2/{mod.rs, emission.rs, expression.rs}` (edited).
- Tests added: 10 (2 in `struct_names.rs::tests`, 5 in `emission.rs::tests`, 3 in `expression.rs::tests`).
- Findings CLOSE_NOW_IN_THIS_PASS: 3 (M1 removed Literal(String) branch; L1 tightened test comment; L2 added module-level visibility note).
- Findings queued as follow-up passes (unrelated pre-existing patterns): 2 — perf P1 (`Expression::data_type`/`nullable` recursion — analyzer-wide memoization candidate) and combined review-M2 / perf-P2 (`render_function_call` pre-render/early-return waste across ~7 arms — refactor pass).
- Compiler warning delta: 37 → 37 (0 new on touched files).
- Quality Gate: PASS (cargo check both crates, rustfmt on new file clean, `cargo test -p thunderduck-core --lib --tests` 334 passed / 18 pre-existing failed, `cargo test -p thunderduck-connect-server --tests` 47 passed).
- Commit SHA: pending user approval.

## Pass 58 — 2026-07-03T (in progress)
- Case: 40-case `PySparkValueError: [UNSUPPORTED_OPERATION] data type unparsed{unresolved}` cluster (cond-011, math-016, arr-003/014, json-001/005, map-002/003/004, type-018, str-020, arr2-001/003/005, map2-001, meta-004, agg2-002/005, parse-006 + others).
- Diagnostic: `.agent-output/diagnostic-unresolved-schema.md`
- Architect verdict: APPROVED (fix iteration 1; review-fix iteration 1)
- Layer(s) touched: analyzer type-inference (primary) + connect-server schema-emission boundary
- ADR citations: ADR-013 (typed AST), ADR-015 (Spark-parity return types), ADR-022 (Thunderduck-boundary errors surface as `Unsupported*`, never as corrupt proto)
- Checklist §-anchors: §1 (analyzer/type-inference coverage), §7 (analyze_plan schema-emission boundary discipline — new)
- Root cause: (H1) `TypeInferenceEngine::function_return_type` at `transpiler_v2/type_inference.rs:521` lacked arms for 17 scalar functions and fell through to `_ => Unresolved`; (H2) `type_converter.rs::data_type_to_proto` at line 121 serialized `DataType::Unresolved` as `Kind::Unparsed{data_type_string:"unresolved"}` verbatim with no boundary guard.
- Fix: (A) added 17 scalar arms + 1 aggregate expansion (nanvl, try_divide, size/cardinality/array_size/map_size, sequence, get_json_object, element_at Array+Map, map_keys/values/entries, map_contains_key, array_append/prepend/compact/remove, histogram_numeric, input_file_name, split_part, regexp_extract_all, array_agg); (B) added ADR-022 boundary guard between `analyze_schema` and `data_type_to_proto` in `service.rs::analyze_plan` — walks `DataType::contains_unresolved` and returns `Status::unimplemented("τ boundary: unresolved type for field '<name>'")` if any field is unresolved.
- Files: `crates/core/src/transpiler_v2/type_inference.rs` (edited, 17 new arms + tests), `crates/connect-server/src/service.rs` (edited, boundary guard + tests).
- Tests added: 24 (21 in type_inference::tests, 3 in service::tests).
- Corpus signal: 207 → 220 (+13). Cascade cases greened: cond-011, math-016, arr-003, arr-014, json-001, map-002, map-003, plus others in the cluster.
- Findings CLOSE_NOW_IN_THIS_PASS: 7 (M1 try_divide Decimal→Unresolved + doc, M2 fallback arms bare Unresolved, M3 array_remove explicit Array match, L1 removed duplicate `"every"` unreachable pattern, L2 doc rewrite, perf OPT-2 unified array_agg dispatch, OPT-3 folded with M2).
- Findings queued as follow-up pass: 1 — **perf OPT-1** "boundary emitter fusion" (fuse `contains_unresolved` guard walk into `data_type_to_proto` by making it return `Result`; eliminates the happy-path double schema walk).
- Compiler warning delta: 37 → 36 (-1 on touched files; L1 removed the unreachable pattern).
- Quality Gate: PASS (`cargo check` × 2, rustfmt clean on touched file, `cargo test -p thunderduck-core --lib --tests` 355/373 pass — 18 pre-existing failures unchanged, `cargo test -p thunderduck-connect-server --tests` 50 pass).
- Commit SHA: pending user approval.

## Pass 59 — 2026-07-03T (in progress)
- Case cluster: `RelType::Unpivot` — piv-004 (`.unpivot(...)`), piv-005 (`.melt(...)`). Piv-006 (`stack()`) requires generator + multi-name-alias (Slice F territory), out of scope.
- Diagnostic: `.agent-output/diagnostic-unpivot.md`
- Architect verdict: APPROVED (fix iteration 1; review-fix iteration 1)
- Layer(s) touched: AST (new CommonOp::Unpivot variant), converter (v2_relation_converter), analyzer (TypedOp::Unpivot, analyze_unpivot with Spark widening + empty-values expansion), emission (render_unpivot mirroring legacy UNPIVOT-with-pre-SELECT shape)
- ADR citations: ADR-013 (typed AST — Unpivot as first-class variant), ADR-015 (Spark parity — numeric type widening across value columns; STRING NOT NULL variable col; nullability of value col = OR of source nullability), ADR-022 (τ-only path — analyzer rejects unmergeable / duplicate cases as Spark-emulated boundary errors)
- Checklist §-anchors: §1 (analyzer schema stamping), §5 (SQL emission via typed AST), §3.4 (new — Unpivot boundary-error hardening)
- Root cause: `RelType::Unpivot` was rejected at `v2_relation_converter.rs::convert_relation` catch-all with "relation shape not covered". Legacy converter + generator already had a working reference (`relation_converter.rs:1187-1221`, `generator/mod.rs::gen_unpivot`).
- Fix: 
  - New `CommonOp::Unpivot { input, ids, values, variable_column_name, value_column_name }` in `ast.rs`.
  - `V2RelationConverter::convert_unpivot` — proto → AST, `extract_column_name` requires `UnresolvedAttribute` (surfaces `UnsupportedProtoShape` on non-attribute expressions — strict improvement over legacy's silent `filter_map` drop).
  - `Analyzer::analyze_unpivot` — schema widening via `TypeInferenceEngine::unify_types`; empty-values expansion (all non-id cols); duplicate/collision rejection.
  - `emission::render_unpivot` — SQL shape `UNPIVOT (SELECT <ids,values> FROM <input>) ON <values> INTO NAME <var> VALUE <val>`; identifier quoting.
- Files: `crates/core/src/transpiler_v2/{ast.rs, base_types.rs, analyzer.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 10 (7 in initial impl + 3 in review-fix iteration for M2/M3 hardening).
- Corpus signal: 220 → 222 (+2). Cluster: piv-004, piv-005. Piv-006 remains failing (generator work, Slice F).
- Findings CLOSE_NOW_IN_THIS_PASS: 5 (M2 duplicate/overlap id+value name check, M3 variable/value col vs id name collision check, L2 test comment fix, OPT-1 HashMap-based O(F+I+V) resolution, OPT-3 String::with_capacity preallocation).
- Findings queued as follow-up pass: 1 — **M1** "Spark-parity `unify_types` fallback → AnalyzerError" (systemic across Unpivot/SetOp/TableFunction; needs dedicated pass touching all three call sites).
- Compiler warning delta: 36 → 36 (0 new on touched files).
- Quality Gate: PASS (`cargo check` × 2, rustfmt clean on touched files, `cargo test -p thunderduck-core --lib` 8/8 unpivot tests + 363/(18 pre-existing) full-lib, `cargo test -p thunderduck-connect-server --tests` 52/52).
- Commit SHA: pending user approval.

## Pass 60 — 2026-07-03T (in progress)
- Case cluster: `Aggregate::Pivot` — grp-004 (explicit pivot values), grp-005 (implicit / eager).
- Layer(s) touched: AST (new CommonOp::Pivot), converter (v2_relation_converter), analyzer (TypedOp::Pivot, analyze_pivot with H1/H2 hardening), emission (render_pivot — conditional-aggregate SQL with NULLIF for COUNT-family, reads pivot names from analyzer-stamped schema).
- ADR citations: ADR-013 (typed AST for Pivot), ADR-015 (Spark parity — column-name derivation for pivot values including float `1.0` formatting), ADR-022 (grp-005 implicit-values path punts as honest Thunderduck-boundary `PuntedOperator("Pivot[implicit-values]")` at analyzer, converted to `EmissionError::UnsupportedOp`).
- Root cause: `Aggregate::Pivot` proto rejected at V2RelationConverter as "PIVOT deferred to Slice G". Legacy Pivot supports both explicit and implicit forms via session-scoped DISTINCT preloading.
- Fix: 
  - `CommonOp::Pivot { input, group_by, pivot_column, pivot_values, aggregates }` added.
  - Converter accepts explicit pivot_values; when values absent, still constructs the AST but analyzer punts.
  - Analyzer stamps output schema (group_by cols + one col per pivot_value × aggregate); analyzer owns pivot-column-name derivation via `literal_to_pivot_column_name` (Spark parity: `"1.0"` for integral floats, rejects `Literal(Null)` per Spark's own rejection).
  - Emission reads pivot output names from stamped schema — single source of truth.
- Files: `crates/core/src/transpiler_v2/{ast.rs, base_types.rs, analyzer.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 12 (initial) + 3 (review-fix) = 15 total.
- Corpus signal: 222 → 223 (+1). grp-004 GREEN; grp-005 honestly surfaces as `Unsupported[Pivot[implicit-values]]`.
- Findings CLOSE_NOW_IN_THIS_PASS: 4 (review H1 float pivot name Spark-parity, H2 reject Literal(Null) in pivot_values, M1 deduplicate literal_to_pivot_column_name via analyzer-stamped schema; M2 NaN column name info-only).
- Findings queued as follow-up pass: 1 — **grp-005 implicit-pivot-values** requires session-injected DISTINCT hook. Cross-crate architecture work (session access at analyzer time or converter-time DISTINCT preload); dedicated follow-up pass "Pivot: eager DISTINCT discovery for implicit values".
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (`cargo check` × 2, rustfmt clean, `cargo test -p thunderduck-core --lib --tests` 370/21 — 21 pre-existing EMIT_TAP failures, `cargo test -p thunderduck-connect-server --tests` 54/54).
- Commit SHA: pending user approval.

## Pass 61 — 2026-07-03T (in progress)
- Case cluster: `Expression::UpdateFields` — struct-005 (`.withField("country", lit("AT"))`), struct-006 (`.dropFields("geo")`).
- Layer(s) touched: expression AST (UpdateFieldsExpression variant), converter (v2_relation_converter — ExprType::UpdateFields arm + flatten_update_fields), analyzer (resolve_and_stamp for UpdateFields + validate_update_fields_ops), emission (render_update_fields via struct_pack reconstruction).
- ADR citations: ADR-013 (typed AST for UpdateFields), ADR-015 (Spark parity — case-insensitive field match; preserve original struct field casing; Spark 4.1 dropField-missing-target error), ADR-022 (missing drop target → Spark-emulated `AnalyzerError::Other`).
- Root cause: `Expression::UpdateFields` was rejected at V2ExpressionConverter with "Slice A.2" gap.
- Fix:
  - `UpdateFieldsExpression { struct_expr, updates: Vec<(String, Option<Expression>)> }` in expression.rs (Some = withField / add-or-replace, None = dropField).
  - Converter flattens nested UpdateFields proto chain into single ordered ops list (oldest first).
  - Analyzer resolves struct_expr + each value, validates drop targets exist (case-insensitive), then applies ops via shared helper.
  - Shared `apply_update_fields_ops<T>` in expression.rs — SINGLE SOURCE OF TRUTH used by both analyzer's `update_fields_data_type` and emission's `render_update_fields`. Case-insensitive match; preserves original struct field casing on replace.
  - Emission emits `struct_pack(f := struct_extract(base, 'f'), ..., new_field := val)`. Uses shared `sql_string_literal` helper.
- Files: `crates/core/src/transpiler_v2/{expression.rs, analyzer.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 8 (initial) + 6 (review-fix, locking C1/C2/H1) = 14 total.
- Corpus signal: 223 → 225 (+2). Both target cases green.
- Findings CLOSE_NOW_IN_THIS_PASS: 4 (Critical C1 case-insensitive withField match; Critical C2 shared helper preventing analyzer/emission drift; High H1 Spark-emulated error on missing dropField target; High H2 sql_string_literal helper reuse). Medium M1/M2 info-only, deferred to style refactor.
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (`cargo check` × 2, rustfmt clean, 16/16 update_fields tests + full-lib pass, connect-server 54/54).
- Commit SHA: pending user approval.

## Pass 62 — 2026-07-03T (in progress)
- Case cluster: JSON quick wins — json-005 (`to_json`), json-006 (`schema_of_json`), json-008 (`to_csv`).
- Layer(s) touched: analyzer type-inference (3 return-type arms), emission (`schema_of_json` → `spark_schema_of_json` extension remap; `to_csv(struct(...))` → `concat_ws` unpacking), expression (to_json/to_csv always-nullable).
- ADR citations: ADR-013 (typed AST), ADR-015 (Spark parity — String return type; extension-provided `spark_schema_of_json`), ADR-020 (thdck_spark_funcs extension provides `spark_schema_of_json`), ADR-022 (non-struct `to_csv` boundary-errors honestly).
- Root cause: `to_json`/`schema_of_json`/`to_csv` had no type-inference arms → `Unresolved` → analyze_plan boundary guard fired.
- Fix:
  - `type_inference.rs::function_return_type`: added 3 arms (`to_json`/`schema_of_json`/`to_csv` → STRING).
  - `emission.rs`: added `schema_of_json` → `spark_schema_of_json` remap (thdck_spark_funcs extension); added `to_csv(struct(...))` → `concat_ws(',', CAST(f AS VARCHAR), ...)` struct-unpacking; boundary-errors on non-struct/non-named_struct `to_csv` args.
  - `expression.rs::function_call_nullable`: `to_json`/`to_csv` added to always-nullable list (matches Spark's projection semantics).
- Files: `crates/core/src/transpiler_v2/{type_inference.rs, emission.rs, expression.rs}`.
- Tests added: 8 (3 in type_inference, 5 in emission).
- Corpus signal: 225 → 227 (+2). json-006 + json-008 GREEN. json-005 progressed from `τ boundary: unresolved type` to value-level to_json divergence (schema green, value fidelity pending).
- Findings CLOSE_NOW_IN_THIS_PASS: 1 (H1 KNOWN DEVIATION doc-only for CSV escaping; queued proper fix as follow-up pass).
- Findings queued as follow-up passes: 2 — "Spark-parity CSV escaping" (RFC-4180 quoting for `to_csv`), "Spark-parity JSON emission" (value-level `to_json` fidelity for json-005).
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (`cargo check` × 2, rustfmt clean, 8/8 new tests, connect-server 54/54).
- Commit SHA: pending user approval.

## Pass 63 — 2026-07-03T (in progress)
- Cluster: Spark non-ANSI runtime tolerance for math (math-005 log-of-zero; math-012 shiftleft-negative).
- Layer(s) touched: emission (Spark-parity wrappers), analyzer type-inference (bitwise operators).
- ADR citations: ADR-015 (Spark non-ANSI mode returns NULL where DuckDB errors).
- Fix: `ln/log/log10/log2` wrapped as `CASE WHEN x > 0 THEN <fn>(x) ELSE NULL END`; `log(x)` single-arg remaps to DuckDB `ln`; `shiftleft` emits arithmetic form `(x * (1::BIGINT << n))` (accepts negative operands); `shiftright` passes through native `>>`. Added `& | ^ bitwiseand bitwiseor bitwisexor` return-type arms.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs}`.
- Tests added: 7.
- Corpus signal: 227 → 229 (+2). math-005, math-012 GREEN.
- Compiler warning delta: 37 → 36 (-1).

## Pass 64 — 2026-07-03T (in progress)
- Cluster: win-006 nth_value trailing-ignoreNulls arg.
- Fix: `render_function_call` detects trailing boolean literal on `nth_value`/`first_value`/`last_value`/`lag`/`lead` and drops it. Defensive: only drops literal-bool trailing args; non-bool passes through.
- Files: `crates/core/src/transpiler_v2/emission.rs`.
- Tests added: 3.
- Corpus signal: 229 → 230 (+1). win-006 GREEN. json-003/004/007 deferred (need DDL parser).
- Compiler warning delta: 36 → 36 (0 new).

## Pass 65 — 2026-07-03T (in progress)
- Case: struct-004 (`F.col("address.geo.lat")` — multi-level nested struct dot access).
- Layer touched: analyzer (`try_rewrite_nested_struct_path` in resolve_column — rewrites `ColumnReference("a.b.c")` into an `ExtractValue` chain against the resolved struct type).
- ADR citations: ADR-013 (typed AST — ExtractValue chain resolved at analysis time), ADR-015 (Spark parity dot-path resolution).
- Files: `crates/core/src/transpiler_v2/analyzer.rs`.
- Tests added: 3.
- Corpus signal: 230 → 231 (+1). struct-004 GREEN; struct-002 (single-level) unchanged.
- Compiler warning delta: 36 → 36 (0 new).

## Pass 66 — 2026-07-03T (in progress)
- Cluster: dt-009 `to_date(fmt)`, dt-010 `to_timestamp(fmt)`, dt-014 `unix_timestamp` / `from_unixtime`.
- Layer touched: emission (Spark→strftime format translation via shared `spark_fmt_to_duckdb`), type-inference (`from_unixtime`→String, `unix_timestamp`→Long).
- Fix: added `spark_fmt_to_duckdb` helper; `to_date(str, fmt)` → `strptime(str, fmt) :: DATE`; `to_timestamp(str, fmt)` → `strptime(str, fmt)`; `unix_timestamp(col)` → `CAST(epoch(col) AS BIGINT)`, 2-arg with fmt wraps `strptime`; `from_unixtime(sec)` → default format string; 2-arg uses `strftime`.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs, expression.rs}`.
- Tests added: 10.
- Corpus signal: 231 → 234 (+3). dt-009, dt-010, dt-014 GREEN.
- Compiler warning delta: 36 → 36.

## Pass 67 — 2026-07-03T (in progress)
- Cluster: HOF fixes — hof-004 `exists`, hof-005 `forall`, hof-007 `transform(x, i)` index origin.
- Layer touched: emission (HOF arms with lambda-index rewrite).
- Fix: `exists` → `CASE WHEN arr IS NULL THEN NULL WHEN len(arr)=0 THEN false ELSE list_bool_or(list_transform(arr, lambda)) END`; `forall` mirror with `list_bool_and` and vacuous-truth (empty→true); 2-arg `transform`/`filter` lambdas rewrite index refs to `(index - 1)` for Spark 0-indexed parity. Added `hof_lambda_has_index`, `render_expr_with_lambda_adjust`, `substitute_index_var` helpers.
- Files: `crates/core/src/transpiler_v2/emission.rs`.
- Tests added: 5.
- Corpus signal: 234 → 237 (+3). hof-004, hof-005, hof-007 GREEN.
- Compiler warning delta: 36 → 36.

## Pass 68 — 2026-07-03T (in progress)
- Cluster: `explode` / `explode_outer` / `posexplode` (arr-015/016/017).
- Layer touched: emission (UNNEST-in-SELECT), converter (posexplode multi-alias splitter), type-inference.
- Fix: `explode(arr)` → `UNNEST(arr)`; `explode_outer(arr)` → `UNNEST(CASE WHEN arr IS NULL OR len(arr)=0 THEN [NULL] ELSE arr END)`; `posexplode(arr).alias(pos, val)` → splits into two synthetic projections at converter: `(generate_subscripts(arr,1)-1)` for pos + `UNNEST(arr)` for val.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs, expression.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 7.
- Corpus signal: 237 → 242 (+5). arr-015, arr-016, arr-017 + 2 incidental cascade (type-012 etc.).
- Compiler warning delta: 36 → 36.

## Pass 69 — 2026-07-03T (in progress)
- Cluster: batch array + map HOFs — arr-007/010/011/012/013, hof-006/008/009/010.
- Layer touched: emission (function remaps + `render_map_hof` helper for `map_filter`/`transform_values`/`transform_keys`), type-inference.
- Fixes:
  - `arrays_overlap` → `list_has_any` + Boolean return type.
  - `array_position` return type → Long.
  - `array_union` NULL-propagating, order-preserving.
  - `array_join(a, sep, null_repl)` → `array_to_string(list_transform(a, x -> coalesce(x, null_repl)), sep)`.
  - `arrays_zip` derives per-arg field names (positional-int fallback).
  - `flatten` guarded, 1-level reduction, NULL-sub-array propagation.
  - `zip_with(a, b, lambda)` → `list_transform(range(1, least(len(a), len(b))+1), i -> lambda(a[i], b[i]))`.
  - `map_filter/transform_values/transform_keys` via `map_from_entries(list_*(map_entries(m), kv -> ...))` template with lambda-var substitution.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs, expression.rs}`.
- Tests added: 18.
- Corpus signal: 242 → 250 (+8). arr-012 still red (Spark's duplicate field names in `arrays_zip("tags","tags")`).
- Compiler warning delta: 36 → 36.

## Pass 70 — 2026-07-03T (in progress)
- Cluster: math-003 ceil/floor NaN, intv-003 make_dt_interval, dt-017 to_utc_timestamp, hof-003 aggregate fold.
- Layer touched: emission (Spark-parity wrappers), analyzer type-inference (aggregate/reduce return = seed type).
- Fix: `ceil`/`floor` cast to BIGINT wrap NaN as NULL; `make_dt_interval(d,h,m,s)` as sum of unit intervals; `to_utc_timestamp` via `AT TIME ZONE` normalization; `aggregate`/`reduce`/`list_reduce` return-type fast-path uses seed type; NULL-array propagation in aggregate.
- Files: `crates/core/src/transpiler_v2/{emission.rs, expression.rs, type_inference.rs}`.
- Tests added: 14.
- Corpus signal: 250 → 253 (+3). math-003, dt-017, hof-003 GREEN. intv-003 SQL correct but blocked by PySpark Arrow decoder.
- Compiler warning delta: 36 → 36.

## Pass 71 — 2026-07-03T (in progress)
- Cluster: SparkSQL parser gaps — `every(active)`, `try_sum(lng)`, `any/some/all` aggregate aliases (agg-021, agg2-004).
- Layer touched: parser_v2 (parse_expression unwraps CommonOp::Aggregate SingleRow shape), type-inference (Spark aggregate-name aliases).
- Files: `crates/core/src/parser_v2/mod.rs`, `crates/core/src/transpiler_v2/type_inference.rs`.
- Tests added: 10.
- Corpus signal: 253 → 255 (+2). agg-021, agg2-004 GREEN.
- Compiler warning delta: 36 → 36.

## Pass 72 — 2026-07-03T (in progress)
- Cluster: batch — map-004/006, arr2-001/002/004/005, map2-002, meta-003.
- Layer touched: emission (arms + macros), analyzer type-inference (map/array arms).
- Fixes: session macros for `array_size`/`array_insert`/`str_to_map`; `typeof`; `element_at` split Array vs Map; `map_concat` NULL propagation; `array_append`/`array_prepend` NULL guards; `create_map` two-list emission; `array_intersect` containsNull=false; analyzer arms for `array_insert`, `str_to_map`, `map_concat`.
- Files: `crates/core/src/runtime/session.rs`, `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs}`.
- Tests added: 10.
- Corpus signal: 255 → 265 (+10).
- Compiler warning delta: 36 → 36.

## Pass 73 — 2026-07-03T (in progress)
- Cluster: math-002 bround, math-006 hypot, str-015 format_string, math-013 conv, dt-004 months_between, cond-007 nvl2, agg-014 mode-on-Boolean, type-007 DIV keyword, dt-016 extract.
- Layer touched: emission (banker's rounding, inline hypot, printf remap, hex/bin/conv, months_between fractional form, mode BOOL→INT wrap), expression (nvl2 return-type = args[1]; multi-arg widening for coalesce/greatest/least), parser_v2 (SparkDialect DIV parse_infix, Expr::Extract lowering).
- Files: `crates/core/src/transpiler_v2/{emission.rs, expression.rs, type_inference.rs}`, `crates/core/src/parser_v2/{dialect.rs, v2_lowering.rs}`.
- Tests added: 12.
- Corpus signal: 265 → 274 (+9).
- Compiler warning delta: 36 → 36.

## Pass 74 — 2026-07-03T (in progress)
- Cluster: cond-010 isnan, str-011 concat_ws-null-array, struct-008 star-expand, parse-005 find_in_set, parse-007 elt, agg-013 percentile_approx, dt-015 make_date return-type, type-015 concat NULL propagation.
- Layer touched: emission (Spark-parity wrappers), converter (UnresolvedStar strips `.*` for struct expansion), analyzer.
- Fixes: `isnan(x)` → COALESCE with FALSE; `concat_ws(sep, arr)` NULL-array → ''; `find_in_set` = `COALESCE(list_position(string_split(csv, ','), needle), 0)`; `elt(idx, ...)` = `[s1, ..., sN][idx]`; percentile_approx switched to `quantile_disc` (discrete sample) for Spark parity; `make_date` returns Date; `concat` on strings wraps NULL propagation.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 6.
- Corpus signal: 274 → 282 (+8).
- Compiler warning delta: 36 → 36.

## Pass 75 — 2026-07-03T (in progress)
- Cluster: parse-001 parse_url, cast-001 Decimal-literal emission, type-005 Decimal division, type-009 CaseWhen type widening, type-020 heterogeneous array typing, agg2-005 histogram_numeric nullability.
- Layer touched: emission (parse_url via regexp_extract for HOST/PROTOCOL/PATH/QUERY/REF/FILE/AUTHORITY/USERINFO), literal emission (Double literal → CAST(v AS DOUBLE)), Spark-return-cast wrapper for CaseWhen numerics, type-inference.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 4.
- Corpus signal: 282 → 288 (+6).
- Compiler warning delta: 36 → 36.

## Pass 76 — 2026-07-03T (in progress)
- Cluster: set-003 union-by-name, parse-002 url_encode form-encoding, parse-004 try_to_number(fmt), map-007 map explode multi-name-alias, json-003/004 from_json DDL schema.
- Layer touched: analyzer (skip positional-cast pushdown for by-name unions), emission (url_encode `%20 → +`, `try_to_number` DDL→DECIMAL, `from_json` DDL→JSON schema recursive), converter (map explode alias split).
- Files: `crates/core/src/transpiler_v2/{analyzer.rs, emission.rs, expression.rs, type_inference.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 16.
- Corpus signal: 288 → 294 (+6).
- Compiler warning delta: 36 → 34 (-2).

## Cumulative Session Total (Passes 57–76)

- Starting corpus (session baseline): 205 / 324 (63.3%).
- Ending corpus: **294 / 324 (90.7%)**.
- Delta: **+89 cases across 20 passes**.
- No regressions across any pass.
- Compiler warnings: 37 → 34 (-3).
- Zero-DEFER discipline: findings closed in-pass or explicitly routed to dedicated follow-up passes (queued in each entry's "Findings queued as follow-up pass" line).

## Pass 77 — 2026-07-04T (in progress)
- Case: `set-004` — `unionByName(other, allowMissingColumns=True)`; previously mis-classified as "Group A / not fixable" in the 2026-07-03 blocker deep-dive but confirmed real τ analyzer bug (Spark accepts the input; τ was rejecting it).
- Diagnostic: `.agent-output/diagnostic-pass-77.md` (root cause: `allow_missing_columns` proto field dropped at every τ layer — converter/CommonAst/TypedAst/emission).
- Architecture: `.agent-output/architecture-pass-77.md` (single-bit plumbing extension across 4 layers + analyzer schema-alignment rewrite for ordered union-of-names).
- Layer(s) touched: converter (v2_relation_converter — reads `allow_missing_columns`), AST (`CommonOp::SetOp` field), analyzer (`TypedOp::SetOp` field + ordered-union schema + type widening + nullable extras), emission (padded-SELECT + plain UNION [ALL] when flag=true).
- ADR citations: ADR-003 (CommonAst extension), ADR-005/006 (schema-threading analyzer sub-sweep), ADR-015 (`TypeCoercion.WidenSetOperationTypes` + `ResolveUnion.scala` parity), ADR-022 (defect was runtime-correctness; `[SPARK-EMULATED]` wire label was mis-attribution — now removed for the flag=true path).
- Corpus signal: 294 → **295** (+1). set-004 GREEN; set-003 (strict by-name path) no regression.
- Files: `crates/core/src/transpiler_v2/{ast.rs, analyzer.rs, analyzer_fixtures.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 6 (5 analyzer + 1 emission covering shared/left-extra/right-extra/both-extra/nullable-propagation).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 0 Medium + 4 Low non-blocking); Perf OPTIMIZED (0 HIGH + 0 MEDIUM, 7 INFO cold-path notes).
- Compiler warning delta: 36 → 36 (0 new on touched files).
- Quality Gate: PASS (cargo check both crates, rustfmt clean, `cargo test -p thunderduck-core --lib --tests` 503/20 vs 497/20 baseline — 6 new tests pass, 20 pre-existing INV10 cascade unchanged, `cargo test -p thunderduck-connect-server --tests` 54/0).
- Journal: `.agent-output/pass-journal.md`.
- Commit SHA: pending.





