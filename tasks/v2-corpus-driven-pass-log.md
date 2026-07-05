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
- Commit SHA: 383b90c.

## Pass 78 — 2026-07-04T (in progress)
- Case: `cond-004` — coalesce Decimal precision mismatch. Spark returns Decimal(10,2); τ returned Decimal(9,2). Prior deep-dive mis-classified as "τ analyzer bug" — actual root cause was one layer earlier at the converter.
- Diagnostic: `.agent-output/diagnostic-pass-78.md` (`normalize_decimal_literal` at `v2_relation_converter.rs:60-102` used tight-fit case-analysis; Spark's `LiteralValueProtoConverter.scala:555-571` uses `max(value-derived, wire-supplied)`).
- Architecture: `.agent-output/architecture-pass-78.md` (single-function body rewrite, signature unchanged, doc corrected, 5 unit tests).
- Layer(s) touched: converter only. Downstream widening in `type_inference.rs::unify_decimal` (:853-857) and analyzer coalesce arm (`expression.rs:764-771`) were already Spark-correct — fed the right literal type they produce the right result.
- ADR citations: ADR-015 (differential oracle: τ boundary mirrors Spark byte-for-byte), ADR-016 (Spark 4.1.1 ANSI mode pin), ADR-022 (τ-only path; no changes outside τ).
- Corpus signal: 295 → **296** (+1). cond-004 GREEN; cond-005 no regression.
- Files: `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 5 (Spark-anchor for cond-004, wire-smaller, wire-absent, zero-value, invariant clamp).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 3 Low non-blocking, all in the malformed-wire regime the plan explicitly flagged as out-of-scope); Perf OPTIMIZED (0 HIGH + 0 MEDIUM, 4 INFO cold-path notes).
- Findings queued as follow-up: 1 — malformed-wire error-class parity (Spark throws for `p_wire < s_wire`; τ clamps). Open question 1 in the plan; ADR-016 error-emulation contract candidate.
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (cargo check both crates clean, rustfmt clean on touched file, `cargo test -p thunderduck-connect-server --tests` 59/0).
- Commit SHA: 47e5d51.

## Pass 79 — 2026-07-04T (in progress)
- Case: `hash-001` — `F.crc32(name.cast('binary'))`. Failed with `DuckDB error: Catalog Error: Scalar Function with name crc32 does not exist`.
- Diagnostic: `.agent-output/diagnostic-pass-79.md` (`crc32` had no remap arm in emission; generic passthrough at `:3728` leaked to DuckDB; type inference at `type_inference.rs:625-629` already returned `Long` correctly with a comment flagging the emission gap).
- Architecture: `.agent-output/architecture-pass-79.md` (session-macro emulation of Spark's `java.util.zip.CRC32` — CRC-32-IEEE, poly `0xedb88320`, init/final XOR `0xFFFFFFFF` — via 256-entry lookup table + `list_reduce`).
- Layer(s) touched: runtime (`SPARK_CRC32_MACRO_SQL` const + registration in `DuckDbSession::spawn`), emission (`"crc32" => "spark_crc32"` dispatch arm), type-inference (docs-only comment refresh).
- ADR citations: ADR-015 (Spark parity — bit-exact against `java.util.zip.CRC32`), ADR-020 (`thdck_spark_funcs` ext6 does not export `spark_crc32`; session macro emulates until extension update), ADR-022 (τ-only, no changes outside τ).
- Corpus signal: 296 → **297** (+1). hash-001 GREEN.
- Files: `crates/core/src/runtime/session.rs`, `crates/core/src/transpiler_v2/emission.rs`, `crates/core/src/transpiler_v2/type_inference.rs`.
- Tests added: 4 (3 runtime — bit-exact vs Python `binascii.crc32` for `test`/`Spark`/empty/NULL — plus 1 emission dispatch-shape).
- Deviations from plan (all documented in-source):
  1. 2-arg `list_reduce` with `list_prepend(init, ...)` — plan's fallback option (DuckDB 1.5.x prefers this form).
  2. Decimal literals for the 256-entry table (DuckDB parser rejected `0xNN::UINTEGER` inside a list initializer).
  3. Byte extraction via `('0x' || substr(hex(b), i*2+1, 2))::INTEGER` — DuckDB lacks `get_byte(BLOB, i)`; project shadows built-in `octet_length` with a bit_length-based macro that fails on BLOB.
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 0 Medium + 5 Low, all docs/test-coverage-extension); Perf 0 HIGH + 2 MEDIUM (both explicitly routed by perf agent to follow-up — "cold-path status confirmed, correct long-term fix is C++ extension").
- Findings queued as follow-up passes:
  1. **Perf MEDIUM-1**: `_spark_crc32_table()` hoisting risk. Requires `EXPLAIN ANALYZE` verification + long-term C++ extension implementation of `spark_crc32` in `thdck_spark_funcs`. Cross-repo.
  2. **Perf MEDIUM-2**: `hex(b)` evaluated twice — try `octet_length(BLOB)` if unshadowed. Single-line fix; can bundle with the extension-migration pass.
- Compiler warning delta: 36 → 36 (0 new on touched files).
- Quality Gate: PASS (cargo check both crates, rustfmt on touched files, `cargo test -- spark_crc32` 4/4, `cargo test -p thunderduck-connect-server --tests` 59/0).
- Commit SHA: 5adc30d.

## Pass 80 — 2026-07-04T (in progress)
- Case cluster: **BUNDLED** — `misc-001` (`df.describe(cols)`) + `misc-002` (`df.summary(stats)`). Identical shape; 5-file substrate slice; corpus +2 vs +1 for the same touch set.
- Diagnostic: `.agent-output/diagnostic-pass-80.md` (full substrate gap — no `CommonOp` variant, no converter arm, no analyzer, no emission; legacy port available at `generator/mod.rs::gen_describe`+`gen_stats_union`+`stat_to_agg_expr` and `relation_converter.rs::convert_describe`/`convert_summary`).
- Architecture: `.agent-output/architecture-pass-80.md` (Option A: two `CommonOp` variants + two `TypedOp` variants + shared `render_stats_union` emission helper; `DEFAULT_SUMMARY_STATS` constant for empty-stats expansion; analyzer applies expansion at the analyze step per Unpivot precedent).
- Layer(s) touched: converter (v2_relation_converter — `convert_describe`, `convert_summary`, 2 `RelType` arms), AST (`CommonOp::{Describe,Summary}`), analyzer (`TypedOp::{Describe,Summary}`, `analyze_describe`, `analyze_summary`, `DEFAULT_SUMMARY_STATS`, `build_stats_output_schema`, `materialise_stats_cols`, 2 arms in `analyze_node`, 2 in `has_resolved_schema`), base_types (2 arms in each of two walkers), emission (`render_describe`, `render_summary`, `render_stats_union` with `AS MATERIALIZED` CTE, `stat_to_agg_expr` with 9 branches including `quantile_disc(TRY_CAST(col AS DOUBLE), q)` for percentile, 2 arms in `dispatch_op`).
- ADR citations: ADR-003 (CommonAst incremental extension — task text said ADR-013 which was a typo; ADR-013 covers external/lakehouse tables), ADR-015 (differential + AnalyzePlan schema oracle — reviewer verified `summary` column is nullable per Spark), ADR-022 (Thunderduck-boundary → Spark-emulated after this pass).
- Corpus signal: 297 → **299** (+2). misc-001 GREEN, misc-002 GREEN.
- Files: `crates/core/src/transpiler_v2/{ast.rs, analyzer.rs, base_types.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 14 (2 ast, 5 analyzer, 2 emission [initial] + 2 emission-shape adjustments for MATERIALIZED, 5 converter).
- Deviations from plan (documented in-source):
  1. `summary` column changed from NOT NULL to nullable — reviewer verified against differential oracle; Reference=True. Spark-parity per ADR-015. Test assertions inverted.
- Findings CLOSE_NOW_IN_THIS_PASS: 1 (perf M1: `AS MATERIALIZED` on the CTE — prevents DuckDB from inlining the child 5-8× per UNION branch; one-word emission change + 2 test updates).
- Findings not blocking: reviewer 2 Low (HashMap-vs-HashSet idiom; caller-casing preservation in materialised cols — non-corpus-witnessed), perf 1 LOW + 3 INFO (all cold-path style notes; not prescribed).
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (cargo check both crates, rustfmt clean, connect-server 64/0, core 9 new τ tests pass individually).
- Commit SHA: 8f4d8ec.

## Pass 81 — 2026-07-04 tech-debt sweep
- Trigger: 5th pass in /goal invocation (every-5th cadence).
- Sweep verdict: mostly CLEAN. 1 cheap action applied: `materialise_stats_cols` uses `HashSet<String>` instead of `HashMap<String, ()>`.
- Compiler warnings: 36 → 36 (baseline; zero new across passes 77-80).
- No TODO/FIXME/dbg!/println!/HACK markers in 4-pass diff.
- INV3/INV10 clean.
- Queued follow-ups stayed queued (all require architectural or cross-repo work).
- Corpus: 299 → 299 (behaviour-neutral).
- Commit SHA: e41fcdc.

## Pass 82 — 2026-07-04T (in progress)
- Case cluster: **BUNDLED** — misc-007 (`freqItems`) implement + misc-006 (`crosstab`) punt.
- Diagnostic: `.agent-output/diagnostic-pass-82.md` (split along ADR-022: freqItems has static schema — portable; crosstab schema depends on DISTINCT of col2 at runtime — needs Slice-G session-injected DISTINCT hook, same blocker as Pivot[implicit-values]).
- Architecture: `.agent-output/architecture-pass-82.md` (Pass-80 shape port for freqItems; `TypedOp::Crosstab` intentionally omitted since analyzer punts before construction — no dead arms).
- Layer(s) touched: AST (`CommonOp::{FreqItems,Crosstab}` variants), converter (`convert_freq_items` with default support=0.01, `convert_crosstab`), analyzer (`TypedOp::FreqItems`, `analyze_freq_items` — per-col `Array<source_type>` with `contains_null=true` matching Spark's `ArrayType(t)` default; `CommonOp::Crosstab` arm punts with `PuntedOperator("Crosstab[dynamic-values]", ...)`), base_types (2 walker arms), emission (`render_freq_items` — WITH __freq_input__ AS MATERIALIZED, per-col LIST subquery with `HAVING COUNT(*) >= support * total`, defensive empty-cols guard).
- ADR citations: ADR-003 (CommonAst extension), ADR-015 (Spark parity — Array<source_type> element type; ArrayType(t) default contains_null=true; fixes legacy `Array<String>` hardcode bug), ADR-020 (stock DuckDB — no extension needed), ADR-022 (Crosstab punt is correct boundary posture).
- Corpus signal: 299 → **300** (+1). misc-007 GREEN; misc-006 stays RED but with correct `[TDCK-BOUNDARY]` shape (`Crosstab[dynamic-values]`).
- Files: `crates/core/src/transpiler_v2/{ast.rs, analyzer.rs, base_types.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: 12 (2 ast, 4 analyzer including per-element-type array test defending against legacy hardcode, 3 emission, 3 converter).
- Deviations from plan:
  1. Outer `freqItems` output column stamped `NOT NULL` (initial plan said nullable) — corpus signaled Reference=False; `LIST(...)` never returns NULL. Documented in-source.
- Findings CLOSE_NOW_IN_THIS_PASS: 2 (review M: inner Array `contains_null=true` matching Spark's `ArrayType(t)` default rather than mirror source nullability — one-line fix + doc comment rewrite; review L: emission f64 comment mischaracterised {:?} vs {} behavior — comment rewrite).
- Findings queued as follow-up: none new this pass.
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (cargo check both crates, rustfmt clean, freq_items tests 7/7 after M-fix, connect-server 67/67, corpus 300).
- Commit SHA: c1dd16b.

## Pass 83 — 2026-07-04T (in progress)
- Case cluster: **BUNDLED** — samp-001 (`Sample`) + samp-002 (`SampleBy`). Both `nondeterministic` (schema-only comparison per harness).
- Diagnostic: `.agent-output/diagnostic-pass-83.md` — full 4-layer substrate gap in τ for both `RelType::Sample` and `RelType::SampleBy`.
- Architecture: `.agent-output/architecture-pass-83.md` (Pass-80/82 template).
- Layer(s) touched: AST (2 CommonOp variants), converter (2 arms + helpers), analyzer (2 TypedOp variants, schema-passthrough, 2 arms in `analyze_node` + 2 in `has_resolved_schema`), base_types (2 arms in each walker), emission (2 dispatch arms, `render_sample` with `TABLESAMPLE BERNOULLI(pct PERCENT) REPEATABLE(seed)`, `render_sample_by` with OR-chain and `setseed()`-hoist seed handling — legacy port).
- ADR citations: ADR-003 (CommonAst extension), ADR-015 (Spark-parity schema-passthrough), ADR-020 (stock DuckDB — TABLESAMPLE + RANDOM()), ADR-022 (τ-only end-to-end; `Sample::with_replacement=true` correctly surfaces as `EmissionError::UnsupportedOp` — DuckDB has no row-level replacement sampling).
- Corpus signal: **+0** at the harness level (300→301 counted for Pass 82's misc-007; corpus stays 301). BUT: both cases now flow end-to-end through τ, correcting a prior harness-loophole where `nondeterministic` schema-only comparison masked τ's ingress rejection with `UnsupportedProtoShape`. Real ADR-022 correctness fix; no visible corpus delta due to harness-metric limitation for `nondeterministic` cases.
- Files: `crates/core/src/transpiler_v2/{ast.rs, analyzer.rs, base_types.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`, `crates/connect-server/src/service.rs` (repointed unsupported-shape fixture Sample → ShowString).
- Tests added: 10 new + 1 updated fixture (2 ast, 2 converter, 3 analyzer, 5 emission).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 0 Medium + 2 Low: L1 randomSplit range-partition consistency limitation matching legacy, L2 harness loophole for nondeterministic-flag cases). Perf OPTIMIZED (0 HIGH + 0 MEDIUM + 0 LOW + 3 INFO cold-path notes).
- Findings queued as follow-up passes:
  1. Harness-hardening pass: differential harness should exercise `nondeterministic`-flagged cases end-to-end (currently PySpark-local schema check bypasses τ), preventing future harness-loophole regressions.
- Compiler warning delta: 36 → 36.
- Quality Gate: PASS (cargo check both crates, rustfmt clean, 10 sample-related tests pass, connect-server 69/69, samp-001+samp-002 both PASSED individually).
- Commit SHA: 5a603eb.

## Pass 84 — 2026-07-04T (in progress)
- Case: `struc-006` — `F.expr("reduce(tags, '', (acc, x) -> concat(acc, x))")`. SparkSQL parser lambda support gap.
- Diagnostic: `.agent-output/diagnostic-pass-84.md` — `parser_v2/v2_lowering.rs:709` had no `Expr::Lambda` arm; falls through catch-all with mis-named `sql::expr::other` shape. Legacy has the arm at `parser/sql_converter.rs:1193-1200`.
- Architecture: `.agent-output/architecture-pass-84.md` — single-file port; add `Expr::Lambda` arm + `expr_kind` update + `LambdaExpression` import.
- Layer(s) touched: parser_v2 (`v2_lowering.rs`).
- ADR citations: ADR-003 (front-end symmetry — protobuf front-end already emits `Expression::Lambda`; SparkSQL was missing), ADR-015 (Spark HOF `->` parity), ADR-022 (correctness — remove mis-named "sql::expr::other" wire error).
- Corpus signal: 301 → **302** (+1). struc-006 GREEN. hof-003 (independent protobuf path) unchanged. Also unlocks all SparkSQL HOFs via `F.expr(...)` / `selectExpr(...)` — no other corpus witnesses today, but latent capability opened.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 2 (single-arg + multi-arg lambda lowering).
- **Deviation from plan**: plan claimed downstream was fully wired for `Expression::Lambda`. In practice, the analyzer treats `Lambda` opaquely, so `UnresolvedColumn(param)` refs inside the lambda body reach emission unresolved → `unsupported expression UnresolvedColumn`. Coder added a `rewrite_lambda_params_to_vars` helper (one function in the same file) that walks the body and rewrites `UnresolvedColumn(name)` → `LambdaVariable` when `name` matches a lambda param. Supports nested-lambda shadowing via `remaining = params \ inner.params` computation at each nested-Lambda descent. Legacy avoided this because its SqlGenerator was permissive enough to emit bare `UnresolvedColumn(name)` — τ's stricter contract requires the rewrite.
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 0 Medium + 3 Low: L1 unit tests don't verify rewrite fired — pure body-shape asserts; L2 no test for nested-lambda shadowing; L3 mixed inline vs top-of-file import style). Perf OPTIMIZED (0 HIGH + 0 MEDIUM; per-child recursion allocs are intrinsic to owned-value rewrite shape; cold path).
- Findings queued as follow-up: 1 — add corpus-invariant tests that verify `rewrite_lambda_params_to_vars` fires (assert `LambdaVariable` in body) + nested-lambda shadowing test. Cheap; can bundle with next parser pass.
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (cargo check both crates, rustfmt clean on touched file, `cargo test -- lambda` 6/6, corpus 302, no regression on hof-003).
- Commit SHA: 2a1c580.

## Pass 85 — 2026-07-04T (in progress)
- Case: `struc-002` — `df.colRegex("\`.*_id\`")` — regex-based column selection.
- Diagnostic: `.agent-output/diagnostic-pass-85.md` — `V2ExpressionConverter::convert` had no `ExprType::UnresolvedRegex` arm; no τ `Expression` variant; expansion is schema-dependent so must happen in analyzer.
- Architecture: `.agent-output/architecture-pass-85.md` — analyzer-driven expansion (mirrors `Expression::Star`); reject Java's `COLUMNS('pattern')` shortcut per ADR-015 (needs real schema in AnalyzePlan).
- Layer(s) touched: expression AST (new `Expression::UnresolvedRegex` variant + defensive `data_type`/`nullable` arms), analyzer (`expand_regex_projections` pre-pass in `CommonOp::Project`, opaque `resolve_and_stamp` arm, `expression_is_fully_resolved` returns false), emission (defensive `UnsupportedExpression` arm — never fires in happy path), converter (`convert_unresolved_regex` + `strip_regex_backticks`), Cargo.toml (new `regex = "1"` workspace dep).
- ADR citations: ADR-003 (CommonAst extension), ADR-015 (analyzer expansion → real schema in AnalyzePlan), ADR-022 (retires 1 Thunderduck-boundary; introduces 2 Spark-emulated errors: invalid regex, no-match).
- Corpus signal: 302 → **303** (+1). struc-002 GREEN.
- Files: `crates/core/src/transpiler_v2/{expression.rs, analyzer.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs`, `Cargo.toml`, `crates/core/Cargo.toml`.
- Tests added: 10 (2 expression, 5 analyzer, 1 emission, 2 converter).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer APPROVED (0 Critical + 0 High + 0 Medium + 2 Low/informational: N1 Rust `is_match` is partial-match vs Spark `Pattern.matcher.matches()` full-match — not corpus-witnessed, consistent with Java Thunderduck reference which used DuckDB COLUMNS() also partial; N2 backtick-strip requires both — deliberate). Perf OPTIMIZED (0 HIGH + 0 MEDIUM + 0 LOW + 6 INFO; all analysis-time cold-path).
- Findings queued as follow-up: 1 — N1 partial-vs-full match latent divergence; anchor to Spark parity when a witness surfaces.
- Compiler warning delta: 36 → 36 (0 new).
- Quality Gate: PASS (cargo check both crates clean, rustfmt on touched files, `cargo test -- regex` passes, release build 36 warnings, struc-002 PASSED).
- Commit SHA: 6f18370.

## Pass 86 — 2026-07-04 tech-debt sweep (10th /goal pass)
- Trigger: 10th pass in /goal invocation (every-5th cadence). Second sweep of this /goal; first was Pass 81 at commit `e41fcdc`.
- Diff scope: 4 corpus passes (82-85) since Pass 81.
- Sweep verdict: **ACTIONS_QUEUED (2 cheap fixes) applied in-pass; all else stays queued**.
- Compiler warnings: 36 → 36 (zero new across Passes 82-85).
- INV3/INV10: clean — no legacy imports.
- No new TODO/FIXME/dbg!/println! markers.
- Cargo.toml diff limited to `regex = "1"` (Pass 85, correctly scoped to thunderduck-core).
- Cross-pass duplication: `WITH __*_input__ AS MATERIALIZED (...)` pattern at 2 sites (Pass 80 stats_union + Pass 82 freq_items). Below rule-of-three; extraction deferred until Pass 87+ adds a third.
- Cross-pass invariant surfacing: "schema-transforming operators materialise expansion in the analyzer, not the converter" now has 5 examples (Unpivot, Pivot, Describe/Summary, FreqItems, UnresolvedRegex). Candidate for ADR-023 or ADR-003 note in future docs pass.
- **Actions applied this pass:**
  1. Pass 84 L1: strengthened `single_arg_lambda_lowers_to_lambda_expression` and `multi_arg_lambda_lowers_to_lambda_expression` — now unwrap the lambda body's FunctionCall and assert arguments are `Expression::LambdaVariable`, not `UnresolvedColumn`. Previously identity-function `rewrite_lambda_params_to_vars` would have passed.
  2. Pass 84 L2: new `nested_lambda_shadowing_preserved` test — asserts outer `x` reaches through inner-lambda body as `LambdaVariable("x")` (surviving `params \ inner.params = ["x"] \ ["y"] = ["x"]`) while inner `y` is `LambdaVariable("y")`. Locks the shadow-filter's set-subtraction against naive clear-on-inner regressions.
- **Follow-ups that stay queued** (all architectural / cross-repo / awaiting Spark witness):
  1. Pass 77 M1: `unify_types` String-fallback → AnalyzerError (systemic across Unpivot/SetOp/TableFunction).
  2. Pass 78 open Q1: malformed-wire error-class parity (ADR-016 error-emulation extension).
  3. Pass 79 Perf M1/M2: `spark_crc32` C++ extension migration; `hex(b)` double-eval (cross-repo).
  4. Pass 80 perf INFO: `stat_to_agg_expr` allocation (cold-path only).
  5. Pass 83 harness-hardening: nondeterministic-flag cases should exercise τ end-to-end (harness gap).
  6. Pass 85 N1: Rust `is_match` partial vs Spark `Pattern.matches()` full-match parity.
- Files: `crates/core/src/parser_v2/v2_lowering.rs` (test-only additions).
- Tests added: 1 new + 2 strengthened.
- Corpus: 303 → 303 (behaviour-neutral).
- Warning delta: 0.
- Commit SHA: 3461e4c.

## Pass 87 — 2026-07-04T (in progress)
- Case: `json-007` — `F.from_csv(str, "qty INT, label STRING, price DOUBLE")`.
- Approach: port Pass 76's from_json DDL parser to from_csv. Manual `split_part(csv, ',', i)::type` per field wrapped in `struct_pack`.
- Layer(s) touched: emission (new `from_csv` arm + `from_csv_ddl_to_struct` helper — flat-DDL only, rejects composite types), expression (type-inference arm returning `DataType::Struct` from literal DDL).
- ADR citations: ADR-013 (typed AST), ADR-015 (Spark-parity: `try_cast + nullif` mirrors Spark permissive mode default), ADR-022 (rejects composite DDL + non-literal schema arg + 3-arg options-map form as Thunderduck-boundary).
- Corpus signal: 303 → **304** (+1). json-007 GREEN.
- Files: `crates/core/src/transpiler_v2/{emission.rs, expression.rs}`.
- Tests added: 3 initial + 2 review-fix (5 total): render shape, DDL parser resolves flat struct, non-literal schema boundary, from_csv 3-arg boundary, from_json 3-arg boundary.
- Findings CLOSE_NOW_IN_THIS_PASS: 1 (review M2 — from_csv AND from_json had misleading "3-arg is boundary" comments while actual guard was `len==2`, silently falling through to DuckDB. Added defensive `!= 2` arms returning `EmissionError::UnsupportedFunction` with honest boundary reason).
- Findings not blocking: reviewer 1 Medium (csv_str evaluated N+1 times — no corpus impact for simple col refs), 2 Low (whitespace trimming; DATE/TIMESTAMP inherit DuckDB parsing vs Spark dateFormat). Perf 0 prescriptions (per-row `split_part` re-scan is theoretical; needs benchmark to justify alternate emission).
- Known deviation in-source: manual split ignores CSV quoting rules (embedded commas, quoted strings, escapes). Corpus witness uses simple unquoted values; queued.
- Compiler warning delta: 36 → 36.
- Quality Gate: PASS (cargo check both crates, rustfmt clean on additions, `cargo test -- from_csv from_json` 9/9, release build 36 warnings, json-007 PASSED).
- Commit SHA: 27b9211.

## Pass 88 — 2026-07-04T (in progress)
- Case: `arr-012` — `F.arrays_zip("tags", "tags")` — Spark preserves input column names in the struct, allowing duplicates (`Array<Struct<tags,tags>>`); τ was surfacing DuckDB's substrate positional names (`Array<Struct<tags_0,tags_1>>`).
- Diagnostic: `.agent-output/diagnostic-pass-88.md` — τ analyzer and emission both correct individually; the gap is at the service boundary. ExecutePlan streams DuckDB Arrow schema verbatim; PySpark's `createDataFrame(rows, df.schema)` disambiguates the mismatch.
- Architecture: `.agent-output/architecture-pass-88.md` — new metadata-only Arrow-schema stamp between `session.execute` and `batches_to_responses`, driven by τ's `resolved_schema`. Rejects boundary-error alternative per ADR-022 (input is supported) and per-function schema-rewrite arms per ADR-003 (once-only boundary transform).
- Layer(s) touched: connect-server (NEW `arrow_schema_stamp.rs` — recursive Field-rename walk with debug_assert + release soft-fallback), converter (depth-aware JSON schema parser closing dormant nested-nullable bug), τ core (`generate_with_schema` fused entry point sharing one analyzer run; `render_data_type::Struct` dedup helper).
- ADR citations: ADR-003 (`resolved_schema` is authoritative), ADR-005 (τ owns Spark type inference), ADR-015 (differential oracle), ADR-020 (`struct_pack("0" :=..)` remains only DuckDB-legal substrate for duplicate Spark field names), ADR-022 (τ-only path, boundary hygiene not a boundary error). Candidate new invariant INV11 flagged for future ADR-023 ("Arrow schema returned by connect server IS τ's resolved_schema view").
- Corpus signal: 304 → **305** (+1). arr-012 GREEN.
- Files: NEW `crates/connect-server/src/arrow_schema_stamp.rs`; MOD `crates/connect-server/src/{main.rs, service.rs, converter/relation_converter.rs, converter/v2_relation_converter.rs}`; MOD `crates/core/src/transpiler_v2/{mod.rs, emission.rs, expression.rs}` (last is rustfmt reflow only).
- Tests added: 15 total — 10 stamp unit tests (primitive passthrough / struct rename / arr-012 duplicate-name / triple-nested list-struct / map traversal / two shape-mismatch probes / debug-assert `#[should_panic]` / data-buffer preservation / empty-input); 3 emission tests (struct dedup / unique-name no-op / array-of-struct dedup); 2 relation_converter tests (outer-vs-inner nullable + PySpark key-order round-trip).
- Findings CLOSE_NOW_IN_THIS_PASS: 2 (review High #1 — depth-blind JSON nullable lookup pre-existing but activated by Pass 88's inbound JSON-schema preference, fixed via depth-aware `top_level_bool_value`/`top_level_string_value`; perf HIGH #1 — double `analyze()` per ExecutePlan, fixed via fused `generate_with_schema(plan, base_types) -> (String, StructType)` and removal of dead `finalize_with_schema` helper).
- Findings not blocking: review 5 Medium + 3 Low (dedup helper duplicated across arrow_schema_stamp/emission — belongs-at-crate-boundary, acceptable; various style/doc gaps). Perf 3 Low + 4 Info (per-batch `Arc::clone` acceptable; per-row `render_data_type` in `render_local_relation` is pre-existing, defer; JSON parser micro-alloc; all sub-µs on corpus workloads).
- Findings queued as follow-up: unify `dedup_names` into a shared helper module (if a third caller appears); `render_local_relation` per-row `render_data_type` optimization.
- Compiler warning delta: baseline preserved (no new warnings on touched files).
- Quality Gate: PASS (cargo check both crates clean, rustfmt clean on all touched files, `cargo test -p thunderduck-connect-server --tests` 83/0, `cargo test -p thunderduck-core --lib` 545/29 identical to baseline, `v2-progress.sh` 305/19/324 with arr-012 PASSED and no regressions).
- Commit SHA: 8eb3d69.

## Pass 89 — 2026-07-04T
- Case: `json-005` — `F.to_json(F.struct("name","age","address"))` — Spark's `to_json` uses `ignoreNullFields=true` by default (`JacksonGenerator` in `SQLConf.scala:3872-3880`); DuckDB's native `to_json` keeps null keys. Corpus witness: nested `address: Struct<city, zip, geo>` where individual fields can be NULL — Spark emits `{"name":"Carol",…,"address":{"city":"Vienna","geo":{…}}}` (zip omitted); τ was emitting the full skeleton with `"zip":null`.
- Diagnostic: `.agent-output/diagnostic-pass-89.md` — one-arm gap in emission; type-inference and expression layers already correct (return type `String`, `nullable=true`).
- Architecture: `.agent-output/architecture-pass-89.md` — wrap `to_json(x)` → `json_strip_nulls(to_json(x))`. Handle 2-arg options-map form (`ignoreNullFields=false` bypass; unknown options → ADR-022 boundary reject). Initially planned to use DuckDB's native `json_strip_nulls`.
- Substrate pivot: DuckDB v1.5.1 (bundled) has NO `json_strip_nulls` function (added in later upstream). First implementation used a two-pass regex SQL macro in session startup — reviewer flagged as HIGH: regex fires inside quoted string values containing `\":null` (silently corrupts user data). Pivoted to Rust UDF via `duckdb` crate's `vscalar` feature — `JsonStripNulls` implementing `VScalar`, using `serde_json` with `preserve_order` to keep field-insertion order. Fallback path (`json_object`-based emission) verified unnecessary — DuckDB v1.5.1's `json_object` skips null-KEY entries but keeps null-VALUE entries, so it would not have solved the gap.
- Layer(s) touched: τ core emission (`to_json` arm at `emission.rs:2487`, helper `parse_to_json_ignore_null_fields`), runtime session (`JsonStripNulls` UDF registration in `session.rs`), workspace dependencies (`serde_json` with `preserve_order`, `duckdb` `vscalar` feature).
- ADR citations: ADR-015 (Spark parity wins), ADR-020 (extension mandatory — `thdck_spark_funcs` is C++ yyjson-based; adding cross-repo work here would be disproportionate — Rust UDF fits the boundary), ADR-022 (options-map non-witnesses = Thunderduck-boundary).
- Corpus signal: 305 → **306** (+1). json-005 GREEN.
- Files: `crates/core/src/transpiler_v2/emission.rs`, `crates/core/src/runtime/session.rs`, `Cargo.toml`, `crates/core/Cargo.toml`.
- Tests added: 11 (4 emission unit tests — wraps with strip / nested still wraps once / options-map bypass / unsupported option boundary; 6 UDF-semantics tests — round-trip identity / recursive nested-null strip / empty-object preservation / array-null preservation / escape-quote regression pin `foo\\":null,bar` byte-identical / malformed input passthrough; 1 renamed).
- Findings CLOSE_NOW_IN_THIS_PASS: 1 (review HIGH — regex fragility on quoted strings containing `\":null`). No perf blockers.
- Findings not blocking: review 1 Medium (case-sensitivity on `ignoreNullFields` key/value), 1 Low (unconditional wrap on scalar `to_json` args). Perf 3 Low + 6 Info — batched UDF (per-DataChunk, not per-row FFI), in-place strip micro-opts available but not corpus-witnessed at scale.
- Findings queued as follow-up: case-insensitive options parsing (Spark accepts `True`/`TRUE`/`true`); `serde_json` in-place `Map::retain` if hot in future.
- Compiler warning delta: baseline preserved.
- Quality Gate: PASS (cargo check clean, rustfmt clean on touched files, `cargo test -p thunderduck-core --lib` 554/29 — +9 net passing from baseline 545/29, no new failures, `v2-progress.sh` 306/18/324 with json-005 PASSED, no regressions).
- Commit SHA: 71947c6.

## Pass 90 — 2026-07-04T
- Case: `inl-001` + `inl-002` cluster — `F.inline(array<struct>)` / `F.inline_outer(...)`. Table generator that fans one array-of-struct row into N-field rows (one per array element); outer variant emits one all-NULL row for NULL/empty arrays.
- Diagnostic: `.agent-output/diagnostic-pass-90.md` — three-arm gap mirroring posexplode: (a) analyzer Project pre-pass expands `inline(arr)` into N synthetic per-field projections, (b) type_inference/nullable arms for the synthetic name, (c) emission arm rendering `UNNEST(arr).<field>` (inner) or sentinel-guarded UNNEST (outer). τ was emitting `inline(...)` verbatim → DuckDB catalog error.
- Architecture: `.agent-output/architecture-pass-90.md` — synthetic `Alias(inline_field(arr, "<name>"), "<name>")` projections; outer wraps arg in `CASE WHEN arr IS NULL OR LEN(arr)=0 THEN [struct_pack(f1 := CAST(NULL AS T1), ...)] ELSE arr END`; non-`Array<Struct>` arg → Thunderduck-boundary. Empirical DuckDB verification confirmed sibling `UNNEST(arr).f` calls with identical arg fold into one row-multiplier (no N-square blowup).
- Layer(s) touched: analyzer (`expand_inline_projections` + Project wire-up), expression (type/nullability arms for `inline_field`/`inline_outer_field`), emission (`render_function_call` new arms), converter (code comment only near `try_convert_posexplode_multi_alias` documenting analyzer-side expansion boundary).
- ADR citations: ADR-003 (CommonAst extension via synthetic per-field FunctionCall), ADR-005 (τ owns Spark type inference — outer's always-nullable rule), ADR-015 (Spark parity: sentinel guard emits exactly one all-NULL row like Spark's inline_outer), ADR-022 (non-Array<Struct> arg and unresolved-element-type arg surface `AnalyzerError::UnsupportedRule` with `[TDCK-BOUNDARY]` Display tag — category-2 τ-not-implemented).
- Corpus signal: 306 → **308** (+2). inl-001 + inl-002 GREEN.
- Files: `crates/core/src/transpiler_v2/{analyzer.rs, expression.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs` (comment).
- Tests added: 13 (5 analyzer expansion + 4 type/nullability + 3 emission + 1 sibling boundary for inline_outer).
- Findings CLOSE_NOW_IN_THIS_PASS: 1 (review HIGH — `AnalyzerError::Other` prefixes `[SPARK-EMULATED]`, should be `[TDCK-BOUNDARY]` per ADR-022. Fixed both boundary sites to `AnalyzerError::UnsupportedRule { rule: "inline{,_outer}-expansion", reason }`; new/updated tests positively assert `[TDCK-BOUNDARY]` prefix).
- Findings not blocking: review 3 Medium (inline in non-Project contexts falls through to DuckDB catalog error; unresolved column arg misclassified as `[TAU-UNIMPLEMENTED]` because pre-pass runs before `resolve_and_stamp`; one other). Perf 0 HIGH/MEDIUM (2 LOW DEFER: CTE-hoist for computed arr in outer sentinel + `String::with_capacity` micro-opt — no witness).
- Findings queued as follow-up: non-Project-context guard for `inline` (withColumn/agg/join.on/filter → boundary reject); pre-pass ordering so unresolved-column errors classify as SPARK-EMULATED; outer sentinel N-copy blowup on computed `arr` args.
- Compiler warning delta: baseline preserved.
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 567/29 — +13 net passing from baseline 554/29, no new failures, `cargo test -p thunderduck-connect-server --tests` 83/0, `v2-progress.sh` 308/16/324 with inl-001 + inl-002 PASSED and no regressions).
- Commit SHA: 1eb5770.

## Pass 91 — 2026-07-04T
- Case: `json-002` — `F.json_tuple("json_str", "a", "e")` — Spark table generator extracting N fields from a JSON string into N columns (default positional names `c0, c1, ..., cN-1`).
- Diagnostic: `.agent-output/diagnostic-pass-91.md` — same-shape as Pass 90 inline generator; τ had no dispatch for `json_tuple` at any layer.
- Architecture: `.agent-output/architecture-pass-91.md` — three-arm change mirroring Pass 90: analyzer pre-pass expands `json_tuple(j, k1..kN)` into N `Alias(json_tuple_field(j, "<ki>"), "c<i>")` (POSITIONAL names, verified via PySpark docstring), type-inference returns always-nullable `String`, emission renders `json_extract_string(<j>, '$.<key>')` using the existing JSON extension substrate (same as `get_json_object` session macro).
- Layer(s) touched: analyzer (`expand_json_tuple_projections` + Project wire-up after `expand_inline_projections`), expression (type/nullability arms for `json_tuple_field`), emission (`render_function_call` new arm with unsafe-char boundary reject), converter (code comment only).
- ADR citations: ADR-003 (synthetic per-field FunctionCall), ADR-005 (always-nullable String matches Spark semantics — NULL for both missing key and JSON-null value), ADR-015 (positional `c0, c1` names match Spark default), ADR-022 (unsafe key chars `'`, `"`, `\`, `.`, `[`, `]`, LF/CR/ASCII-control → `AnalyzerError::UnsupportedRule` with `[TDCK-BOUNDARY]` prefix; caller-side type errors → `AnalyzerError::TypeMismatch` with `[SPARK-EMULATED]`).
- Corpus signal: 308 → **309** (+1). json-002 GREEN.
- Files: `crates/core/src/transpiler_v2/{analyzer.rs, expression.rs, emission.rs}`, `crates/connect-server/src/converter/v2_relation_converter.rs` (comment only).
- Tests added: 9 (5 analyzer expansion + 2 type/nullability + 2 emission — including positive `[TDCK-BOUNDARY]` prefix assertion, unsafe-key rejection).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer 0 Critical + 0 High + 0 Medium + 4 Low (all queued: empty-string key edge, `TypeMismatch.actual` shows `Unresolved`, duplicated unsafe-char predicate between analyzer/emission, plan `_input_schema` param dropped). Perf 0 HIGH/MEDIUM + 2 LOW DEFER + 6 INFO.
- Findings queued as follow-up: analyze/emission unsafe-char predicate deduplication; per-row `json_extract_string` re-parses JSON for each key (Focus 3 — pre-existing pattern, not corpus-witnessed at scale); non-Project context guard (inherited from Pass 90).
- Compiler warning delta: baseline preserved.
- Quality Gate: PASS (cargo check clean, rustfmt clean, thunderduck-core lib tests + connect-server tests clean, `v2-progress.sh` 309/15/324 with json-002 PASSED and no regressions).
- Commit SHA: 1170856.

## Pass 92 — 2026-07-04 tech-debt sweep (5th /goal pass)
- Trigger: 5th pass in /goal invocation (every-5th cadence). First sweep of this /goal.
- Diff scope: 4 corpus passes (88-91) since PIPELINE_START_SHA `27b9211`; corpus 304 → 309 (+5 cases: arr-012, json-005, inl-001, inl-002, json-002).
- Sweep verdict: **NO_ACTIONS**. Zero new compiler warnings, zero new TODO/FIXME/dbg!/println!, zero legacy imports, no source edits needed.
- Compiler warnings: 36 → 31 (net -5 incidental improvements, zero new introductions on files touched by Passes 88-91).
- INV3/INV10: clean — no `use crate::legacy` or cross-module imports breaking τ-only-path.
- Cargo.toml drift: `duckdb` `vscalar` feature + `serde_json` (with `preserve_order`) added in Pass 89, minimally scoped to `crates/core/src/runtime/session.rs`; no duplicate declarations, no version conflicts.
- Two new `.expect()` calls introduced in Passes 88-91, both with invariant comments referencing upstream length/type guards; no new raw `.unwrap()`.
- Cross-pass duplication (all rule-of-two, queued):
  1. `dedup_names` PySpark-parity helper at `crates/connect-server/src/arrow_schema_stamp.rs:50` (Pass 88) + `crates/core/src/transpiler_v2/emission.rs:5596` (Pass 88 gap 3). Two callers, one in each crate — shared helper would require a common crate; below threshold.
  2. Unsafe-char predicate at `crates/core/src/transpiler_v2/analyzer.rs:1781` (Pass 91 analyzer) + `crates/core/src/transpiler_v2/emission.rs:2598` (Pass 91 emission). Same crate, two callers — trivial to extract when the third emerges.
  3. Non-Project generator context guard — `inline` (Pass 90) + `json_tuple` (Pass 91) both silently pass through to DuckDB catalog error outside a Project. Two witnesses; a third from `stack`/`explode_generic` would trigger the visitor.
- Cross-pass invariant candidates surfaced:
  1. **ADR-023 candidate — "analyzer materialises schema-transforming operators".** 7 witnesses (Unpivot, Pivot, Describe/Summary, FreqItems, UnresolvedRegex, inline, json_tuple). Well past rule of three. Docs-only.
  2. Pre-pass ordering skews Spark error-class parity across 3 pre-passes (regex, inline, json_tuple). Cross-cutting architectural refactor candidate.
  3. Methodology note: "τ carries Spark-visible information the DuckDB substrate lacks" (arr-012 arrow-schema stamp, json-005 UDF for ignoreNullFields, inline UNNEST with sentinel, json_tuple positional names).
- Actions applied this pass: none.
- Follow-ups that stay queued:
  1. ADR-023 authoring — "analyzer is the schema-expansion boundary" (7 witnesses).
  2. Pre-pass ordering fix for Spark error-class parity (Pass 90 reviewer M2).
  3. Non-Project-context generator visitor guard (Pass 90 M1, Pass 91).
  4. Dedup `dedup_names` and unsafe-char predicate when rule of three lands.
  5. Case-sensitivity in `parse_to_json_ignore_null_fields` (Pass 89 M).
  6. Per-row `json_extract_string` re-parse cost (Pass 91 perf INFO).
  7. Wide-struct per-row `render_data_type` in `render_local_relation` (Pass 88 LOW).
  8. Empty-string key edge case in `expand_json_tuple_projections` (Pass 91 L).
- Files: tasks/v2-corpus-driven-pass-log.md (this entry), .agent-output/tech-debt-sweep-pass-92.md (full report).
- Tests added: 0.
- Corpus: 309 → 309 (behaviour-neutral).
- Warning delta: 0 new.
- Commit SHA: 8b35361.

## Pass 93 — 2026-07-04T
- Case: `win2-002` — `F.window("last_login", "1 day")` in a groupBy — tumbling time-window aggregate. Spark returns `Struct<start: Timestamp, end: Timestamp>` non-nullable (via `CreateNamedStruct`); τ was emitting `window(...)` verbatim → DuckDB parser error because `WINDOW` is a reserved keyword.
- Diagnostic: `.agent-output/diagnostic-pass-93.md` — no dispatch at any layer; single emission arm + type-inference arm + non-nullable-list addition close the gap.
- Architecture: `.agent-output/architecture-pass-93.md` — substrate `struct_pack(start := time_bucket(INTERVAL 'N unit', ts), "end" := time_bucket(INTERVAL 'N unit', ts) + INTERVAL 'N unit')`; `"end"` quoted via existing `quote_ident` helper (DuckDB reserved); Pass 88 arrow-schema stamp handles Spark-visible name restoration on the wire. Duration parser accepts `{second,minute,hour,day,week}` singular/plural case-insensitive; rejects compound/month/year/fractional/signed/non-literal via `EmissionError::UnsupportedFunction` with `[TDCK-BOUNDARY]` prefix.
- Layer(s) touched: τ core emission (`render_function_call` new `window` arm + `parse_window_duration_literal` helper), type-inference (`function_return_type` arm returning `Struct<start,end: Timestamp>`), expression (`window` added to non-nullable-literal list per Spark `CreateNamedStruct.nullable = false` invariant — plan deviation warranted by corpus witness). No converter or analyzer changes.
- ADR citations: ADR-005 (τ owns Spark type inference — struct return type), ADR-015 (Spark parity — non-nullable per `CreateNamedStruct`), ADR-020 (uses existing DuckDB `time_bucket` — no extension work needed), ADR-022 (3+ arg sliding/offset forms, month/year buckets, compound/fractional/signed durations, non-literal args → boundary reject `[TDCK-BOUNDARY]`).
- Corpus signal: 309 → **310** (+1). win2-002 GREEN.
- Files: `crates/core/src/transpiler_v2/{emission.rs, type_inference.rs, expression.rs}`.
- Tests added: 11 (9 emission — including a DuckDB-embedded smoke test locking substrate against version bumps + duration parser negative tests; 1 type-inference; 1 nullability).
- Findings CLOSE_NOW_IN_THIS_PASS: 0 blocking. Reviewer 0 Critical + 0 High + 0 Medium + 2 Low (stale comment reference / latent null-ts row divergence — corpus witness pre-filters). Perf 0 HIGH/MEDIUM.
- Findings queued as follow-up: null-ts row divergence (τ does not filter null timestamps before struct rewrite; Spark `TimeWindowing` optimizer rule does — not witnessed).
- Compiler warning delta: baseline preserved.
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 587/28 — +10 net passing from prior baseline 577/28, `cargo test -p thunderduck-connect-server --tests` 83/0, `v2-progress.sh` 310/14/324 with win2-002 PASSED and no regressions).
- Commit SHA: 1484604.

## Pass 94 — 2026-07-04 — τ ANSI divide/mod-by-zero throw (math-010 + math-011) — integrated via merge
- Cases: `math-010` (`%` and `pmod`) + `math-011` (`/` int/int) — both cases tagged with `expected_error` in the corpus (REMAINDER_BY_ZERO / DIVIDE_BY_ZERO) by commit `7043acf` (tri-state harness comparator, landed mid-/goal). This pass integrates commit `933908c` (from branch `feat/v2-tau-ansi-throw`) which supplies τ's ANSI-throw substrate — Option-B design from the reused `.agent-output/{diagnostic,architecture}-pass-94.md` (originally Pass 88 math-DEFERRED).
- Layer(s) touched (per merged commit): τ core emission (ansi_zero_guard + Spark-verbatim message constants + is_nonzero_literal skip + wire into render_binary Div/IntDiv/Mod + render_function_call pmod/mod arms), τ core error (new `ThunderduckError::SparkRuntime{class,message}` variant + `classify_spark_runtime_error` [TOKEN]-extractor + `reclassified_spark_runtime`), runtime session (`DuckDbSession::execute` re-wraps DuckDB errors into SparkRuntime), connect-server (manual `From<ThunderduckError>` routes SparkRuntime to a status whose message leads with `[CLASS]`), differential harness (`spark_error_class` reads the gRPC rendezvous details for τ's re-wrapped errors).
- ADR citations: ADR-006 (error emulation contract — first runtime data-dependent Spark-emulated error), ADR-015 (Spark parity — byte-identical message text), ADR-020 (long-term route: `spark_div`/`spark_pmod` extension fns, documented as follow-up), ADR-022 (τ-only path — this is category-1 Spark-emulated).
- Corpus signal: 310 → **312** (+2). math-010 + math-011 GREEN via error-parity.
- Files: `crates/core/src/transpiler_v2/emission.rs`, `crates/core/src/error.rs`, `crates/connect-server/src/error.rs`, `crates/core/src/runtime/session.rs`, `tests/integration/utils/dataframe_diff.py`.
- Tests added (per merged commit): emission guard shapes, classifier/reclassify unit tests, runtime-error-surfacing contract test.
- Integration mechanics: fast-forward merge of `feat/v2-tau-ansi-throw` into `feat/v2-transpiler`. My own aborted Pass 94 coder-session work on emission.rs::pmod was stashed and dropped (superseded). Release binary rebuilt post-merge to observe corpus flip. Final report at `.agent-output/final-report.md` supersedes as the pipeline is now known-not-terminated.
- Follow-up recorded on merged commit: `spark_div`/`spark_pmod` in `thdck_spark_funcs` for throw-at-source semantics; decimal-div left as TODO(ADR-006).
- Quality Gate: PASS (cargo build --release clean, `v2-progress.sh` 312/12/324 with math-010 + math-011 GREEN via error-parity, no regressions on prior 310 green cases).
- Commit SHA: 933908c (integrated by fast-forward merge into feat/v2-transpiler).






## Pass 95 — 2026-07-05 — SQL corpus: spark.sql() SqlCommand execution (sel-007)
- **Corpus: SQL front-end** (`differential/sql_corpus.py`, 262 cases via `spark.sql`, tracked by `tests/scripts/v2-sql-progress.sh` → `tests/integration/v2_sql_progress.md`). This is the first pass of the SQL-corpus pipeline (parallel to the DataFrame `core_v2` corpus).
- Case: `sel-007` — `SELECT 1 AS one, 'x' AS s, true AS b` (catalog-free literal projection, no FROM). PySpark `spark.sql()` sends the query as a Spark Connect `SqlCommand` (whose `.input` is a `RelType::Sql` relation); `service.rs::handle_sql_command` was an `unimplemented("Slice C.1")` stub, so every `spark.sql()` failed before execution.
- Diagnostic: `.agent-output/diagnostic-pass-95.md` — owning layer = connect-server service layer; the Root/relation path (`transpile_relation → execute_streaming_query`) already works; only the SqlCommand command arm was stubbed.
- Architecture: `.agent-output/architecture-pass-95.md` — **lazy echo**: the command arm returns `SqlCommandResult { relation: Some(<input Sql relation>) }` + `ResultComplete`; PySpark's `CachedRelation` re-executes it as a Root plan on `.collect()` (zero new substrate, no Arrow→proto LocalRelation encoding). An eager `transpile_relation(&input_rel)?` in the arm forces parse+analyze at `sql()` time for Spark-parity `AnalysisException`. Deprecated `SqlCommand.sql` text branch synthesizes a `RelType::Sql`.
- Layer(s) touched: connect-server service layer only (`service.rs`): `handle_sql_command` (stub → two-frame echo), `sql_command_result_response` (gains `relation` arg, drops `#[allow(dead_code)]`), the `SqlCommand` dispatch arm (obtain/synthesize input relation + eager validate). Removed the now-orphaned `transpile_raw_sql` helper. No τ core / analyzer / emission changes.
- ADR citations: ADR-021 (typed `RelType::Sql` relation kept end-to-end — no SQL-string shortcut), ADR-022 (τ is the only path; eager validation routes both error categories through τ at command time), ADR-011 (fixes the command-vs-relation `ExecutePlanResponse` shape: `SqlCommandResult`+`ResultComplete`, not an ArrowBatch stream).
- Corpus signal: 0 → **2** (+2). `sel-007` GREEN (target); the SqlCommand execution fix also unblocked one further catalog-free case (literal/VALUES). All `FROM <table>` cases remain red on the separate Slice-B catalog blocker ("table not found") — next pass.
- Files: `crates/connect-server/src/service.rs`.
- Tests added: 2 (`sql_command_select_literals_returns_echoed_relation` — modern-path echo fidelity + ResultComplete + no ArrowBatch; `sql_command_deprecated_text_synthesizes_sql_relation` — deprecated-text synthesis).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (+ 4 Low/informational: intended double-transpile, DDL-seam bounded by TODO, pre-existing parameterized-query gap, optional negative test). Perf 0 HIGH + 0 MEDIUM (LOW-1 needless `String` clone on the deprecated branch — fixed: clone → move).
- Findings queued as follow-up: eager DDL/DML side effects (`spark.sql("CREATE VIEW ...")`) and non-deterministic re-evaluation require eager execution to a `LocalRelation` (marked `TODO Slice C.1:` in code); Slice-B temp-view registration + catalog bridge for `FROM <table>` cases.
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean on touched file, `cargo test -p thunderduck-connect-server --bins` 71/0 serially [pre-existing `/tmp` extension parallel flake], `v2-sql-progress.sh` 2/260/262 with sel-007 GREEN, no regressions — baseline was 0 green).
- Commit SHA: (this commit).

## Pass 96 — 2026-07-05 — SQL corpus: Slice-B temp-view registration + catalog bridge (sel-002)
- **Corpus: SQL front-end** (`differential/sql_corpus.py`). Target: `sel-002` — `SELECT * FROM emp`. The corpus registers 5 views via `createDataFrame(...).createOrReplaceTempView(...)` then queries them by name; every `FROM <table>` case failed with `[SPARK-EMULATED] table not found`.
- Diagnostic: `.agent-output/diagnostic-pass-96.md` — TWO compounding stubs: (1) `service.rs::handle_create_dataframe_view` returned `unimplemented("Slice B")` (never registered the view), (2) `build_base_types` catalog closure was `|_| None` (analyzer could never resolve a table name). Session machinery `create_temp_view_with_schema` / `get_view_schema` already existed but was uncalled.
- Architecture: `.agent-output/architecture-pass-96.md` — **pre-fetch map + sync closure** (blessed by ADR-012 line-318 short-circuit). `build_base_types` enumerates empty-scan table names, `await`s `session.get_view_schema` for each into a `HashMap`, then feeds `BaseTypes::build_from_plan` a sync `|name| map.get(name).cloned()` closure. Rejected `block_in_place`/channel-in-closure (deadlock risk — `duckdb::Connection` is `!Send`) and full-catalog snapshot (more machinery). Keeps the closure the sole INV10 runtime→analyzer bridge; no `transpiler_v2/` behavior change.
- Layer(s) touched: connect-server service layer (`service.rs`): `handle_create_dataframe_view` (stub → `create_temp_view_with_schema` + ResultComplete), `build_base_types`/`finalize`/`analyze_schema`/`transpile_relation` became `async` + take `&Arc<DuckDbSession>`, catalog closure pre-fetches from the session. τ core: `base_types.rs` gained additive `pub fn empty_scan_tables` (wrapper over the private walker). No analyzer/emission change.
- ADR citations: ADR-012 (narrow catalog overlay — commands write via `create_temp_view_with_schema`, resolution reads via the seeded closure; +line-318 `plan_has_empty_scan` short-circuit honored), ADR-011 (CreateDataframeView → `ResultComplete` only), ADR-021/INV10 (closure sole bridge; runtime types fully-qualified, no new `use` under τ), ADR-022 (`UnknownTable` stays Spark-emulated for unregistered names; registration failures → Thunderduck-boundary), ADR-005/006 (analyzer resolves TableScan from BaseTypes).
- Corpus signal: 2 → **108** (+106). `sel-002` GREEN; unlocked the entire `FROM emp/dept/emp2/nums/raw` batch — sel/whr/agg/ord/cnd/fn/num clusters + scattered join/cx/sq/pr/tbl cases. No regression (sel-007 catalog-free still green — `plan_has_empty_scan == false` short-circuits with zero session round-trips).
- Files: `crates/connect-server/src/service.rs`, `crates/core/src/transpiler_v2/base_types.rs`.
- Tests added: 3 (`catalog_bridge_resolves_registered_view` — register view + SELECT resolves, ArrowBatch+ResultComplete, stamped field names; `create_view_command_returns_result_complete` — command round-trip; `select_literal_makes_no_catalog_call_short_circuit` — regression guard). Plus 5 existing transpile tests converted to `#[tokio::test]` with a warmed session.
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (+ 4 Low: pre-existing raw-DuckDB error mapping on registration failure, negative-lookup not memoized, double plan traversal, is_global untested). Perf 0 HIGH + 0 MEDIUM (4 Low micro-costs — all once-per-query, dominated by the session round-trip; deferred to tech-debt sweep).
- Findings queued for tech-debt sweep (pass 99): LOW-2 memoize negative `get_view_schema` lookups; LOW-4 `map.remove` to drop the extra schema clone.
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean both crates, rustfmt clean, `cargo test -p thunderduck-connect-server --bins` 74/0 serially [pre-existing `/tmp` extension parallel flake], `sql_v2` differential 108/154/262 with sel-002 + FROM-table batch GREEN, no regressions).
- Commit SHA: (this commit).

## Pass 97 — 2026-07-05 — SQL corpus: set operations lowering (set-001 cluster)
- **Corpus: SQL front-end.** Target: same-shape cluster `set-001..010` (UNION/INTERSECT/EXCEPT/MINUS × ALL/DISTINCT, n-ary, +ORDER BY). All 10 failed at one seam: `parser_v2` rejected set ops (`sql::set_operation::* deferred past Slice A.2`).
- Diagnostic: `.agent-output/diagnostic-pass-97.md` — LOWERING-ONLY. τ's `CommonOp::SetOp`/`SetOpKind`, the analyzer's set-op widening (ADR-006), and emission's `render_set_op` already exist and are green via the DataFrame path (`convert_set_op` builds the identical op). Only `v2_lowering::lower_set_expr` rejected `SetExpr::SetOperation`.
- Architecture: `.agent-output/architecture-pass-97.md` — replace the reject arm with a mapping + binary recursion building `CommonOp::SetOp { kind, all, by_name:false, allow_missing_columns:false, children:[left,right] }`. `Except|Minus→Except`; `all` only on explicit `ALL`. n-ary via recursion (no flatten); ORDER BY already wrapped by `wrap_with_sort_limit`.
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`, one arm). No analyzer/emission/ast change.
- ADR citations: ADR-004 (SQL + DataFrame lower to the SAME common AST — closes the SQL front-end onto the existing SetOp substrate), ADR-006 (set-op widening, unchanged), ADR-022 (τ-only; `UNION BY NAME` rejected as Thunderduck-boundary rather than silently mis-lowered — reviewer MEDIUM, closed in-pass).
- Corpus signal: 108 → **114** (+6). `set-001/002/003/005/007/010` GREEN (UNION distinct/all, UNION DISTINCT, INTERSECT ALL, EXCEPT ALL, set-op+ORDER BY). No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 8 (7 lowering shape tests: UNION ALL/bare, INTERSECT, EXCEPT, MINUS→Except, 3-way nested, ORDER-BY-over-SetOp; +1 negative: `UNION BY NAME` rejected).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High + **1 Medium CLOSED** (`UNION BY NAME`/`AllByName`/`DistinctByName` silently lowered positional → now returns `UnsupportedProtoShape(sql::set_operation::by_name)`; `all` narrowed to `SetQuantifier::All` only). Perf N/A (parse-time AST construction, no hot path).
- Findings queued as follow-up (DIFFERENT root cause — not the lowering): `set-004`/`set-006`/`set-008` fail on **analyzer set-op nullability** for INTERSECT/EXCEPT-DISTINCT with mixed-nullability inputs (Spark output nullability = AND of inputs; `dept_id` Reference=False vs τ Test=True); `set-009` (3-way UNION ALL over Long+Long+Integer) fails with an execution error (mixed-type widening at emission). Next-pass candidates.
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 628/0 [+7 net], `sql_v2` differential 114/148/262 with 6 new set-op cases GREEN, no regressions).
- Commit SHA: (this commit).

## Pass 98 — 2026-07-05 — SQL corpus: window functions + interval literals (win-001 cluster)
- **Corpus: SQL front-end.** Target: `win-001..016` (ROW_NUMBER/RANK/DENSE_RANK/PERCENT_RANK/CUME_DIST/NTILE/LAG/LEAD/FIRST_VALUE/LAST_VALUE/NTH_VALUE, running SUM, moving AVG, RANGE BETWEEN, no-order partition, named WINDOW, top-per-group, multiple windows, window-over-expr, RANGE BETWEEN INTERVAL). All 16 failed at `sql::window_function` reject.
- Diagnostic: `.agent-output/diagnostic-pass-98.md` — LOWERING-ONLY (15/16). τ's `Expression::Window(WindowFunction{func,partition_by,order_by,frame})`, analyzer window typing, and `OVER(...)` emission are done + green via the DataFrame corpus (`v2_relation_converter.rs:916` builds the identical type). win-016 additionally needed a reusable `Expr::Interval` lowering arm.
- Architecture: `.agent-output/architecture-pass-98.md` — replace the `f.over.is_some()` reject in `lower_function` with window lowering (partition_by via lower_expr, order_by via existing lower_order_by_expr, frame mapping); add `Expr::Interval`→`IntervalExpression` arm; named-window pre-pass in `lower_select` inlining `OVER w` → resolved `WindowSpec`. Frame bounds mirror the DataFrame path (offset = absolute magnitude, direction in the variant; Groups → reject; end_bound None → CurrentRow; frame None when absent).
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No analyzer/emission/ast/expression-type change.
- ADR citations: ADR-004 (SQL + DataFrame → same common AST; closes the SQL front-end onto the existing Window/Interval substrate), ADR-005/006 (analyzer window typing/nullability, unchanged), ADR-022 (Groups frame units, compound/non-literal/unrepresentable intervals, unresolved named windows → Thunderduck-boundary reject, not silent RawSql fallback).
- Corpus signal: 114 → **132** (+18). All 16 win-* GREEN + collateral lit-003 (`INTERVAL '90' DAY`) and lit-010 (`INTERVAL '30' DAY` in WHERE). No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 7 (window_partition_order_no_frame, window_rows_unbounded_preceding_to_current_row, window_rows_between_one_preceding_and_one_following, window_named_window_is_inlined, window_groups_frame_is_rejected, unknown_named_window_is_rejected, interval_literal_day_lowers_to_interval_expression).
- Process note: the FIRST coder attempt reported success but its edits never persisted to disk (`git diff HEAD` empty, reject intact); caught by re-running the differential (no movement). Re-dispatched with a mandatory `git diff --stat HEAD` persistence check → 386 insertions confirmed. Lesson: verify subagent edits landed via git diff before trusting a green self-report.
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (+ 3 Low: named-window recursion doesn't descend CASE/args/ORDER-BY — but loud-rejects rather than silently mis-resolves; interval unit gaps WEEK/QUARTER/fractional → loud reject; one test doesn't assert magnitude==1 [covered end-to-end by win-009 differential]). Perf N/A (parse-time; named-window walk gated on `named_window` non-empty).
- Findings queued as follow-up: named-window resolution in CASE/function-args/ORDER-BY; interval WEEK/QUARTER/fractional-second/year-month/day-second compound (lit-004/005/006); DATE/TIMESTAMP literals (lit-001/002).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 636/0, `sql_v2` differential 132/130/262 with all 16 win-* + 2 interval cases GREEN, no regressions).
- Commit SHA: (this commit).

## Pass 99 — 2026-07-05 — SQL corpus: tech-debt sweep (passes 95–98)
- **Tech-debt sweep** (every-5th-pass): rust-reviewer over `git diff 096c55d..HEAD` (SqlCommand exec, temp-view+catalog, set-op + window/interval lowering). No Critical/High debt; 0 compiler warnings pre-sweep.
- Fixed: **M1** — extracted `relation_to_common_ast(relation) -> Result<CommonAst, Status>` (the RelType::Sql-vs-V2RelationConverter dispatch was duplicated verbatim in `transpile_relation` and the `AnalyzePlan(Schema)` arm; two copies would drift). **M2** — corrected 3 stale Slice-A.3 comment blocks that still claimed "`finalize()` errors unconditionally / τ returns UnsupportedOp for every input" (false since passes 95-96 — `execute_streaming_query` is live). **L1** — `build_base_types` dropped the redundant `plan_has_empty_scan` walk (now short-circuits on `empty_scan_tables(...).is_empty()`), and removed the resulting unused `plan_has_empty_scan` import (new warning fixed on the touched file).
- Dismissed (reviewer-recommended): L3 (`map.remove` over `.cloned()` — trades a fragile single-invocation coupling for a negligible clone; keep `.cloned()`). Deferred (Low): L2 negative-lookup memoization, L4 `bool_batch_responses` dead scaffolding (intentional Slice C.1), L5 named-window errors should be Spark-emulated class (message-quality only), L6 error-shape prefix nit.
- Files: `crates/connect-server/src/service.rs`. No behavior change (refactor); corpus stays 132.
- Quality Gate: PASS (cargo check --workspace --all-targets 0 warnings, rustfmt clean, `cargo test -p thunderduck-connect-server --bins` 74/0 serially).
- Commit SHA: (this commit).

## Pass 100 — 2026-07-05 — SQL corpus: ROLLUP / CUBE lowering (gx cluster) [final feature pass]
- **Corpus: SQL front-end.** Target: `gx-*` GROUP BY extensions. Diagnostic split the cluster three ways: ROLLUP/CUBE (lowering-only, substrate green via DataFrame path), GROUPING SETS (needs set-membership emission — Slice G), `WITH ROLLUP` Hive suffix (dialect gap).
- Diagnostic: `.agent-output/diagnostic-pass-100.md` — reject at `v2_lowering.rs` `lower_aggregate_select`; `GroupingKind::{Rollup,Cube}` + native emission (emission.rs:4678) + analyzer + `grouping()`/`grouping_id()` rewrite all already done and green via DataFrame grp-001/002/003/006.
- Architecture: `.agent-output/architecture-pass-100.md` — flatten `Expr::Rollup/Cube` (`Vec<Vec<Expr>>`) into τ's flat `grouping` list + set `grouping_kind`, mirroring `v2_relation_converter.rs:642`. Keep rejecting GROUPING SETS.
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No analyzer/emission/ast change.
- ADR citations: ADR-004 (SQL onto existing Rollup/Cube substrate), ADR-005/006 (analyzer grouping, unchanged), ADR-022 (GROUPING SETS, nested ROLLUP/CUBE terms, WITH ROLLUP → loud boundary reject).
- Corpus signal: 132 → **137** (+5). `gx-001/002/005/006/009` GREEN (ROLLUP, CUBE, grouping(), grouping_id(), ROLLUP+ORDER-BY). No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 4 (rollup, cube, GROUPING-SETS reject, nested-ROLLUP-term reject).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + **1 High CLOSED** (H1: nested grouping terms `ROLLUP((a,b),c)` were silently flattened to `ROLLUP(a,b,c)` — different grouping sets, silent wrong result on valid Spark input; now loud-rejects `sql::grouping_sets` + regression test). Perf negligible (parse-time).
- Findings queued as follow-up: **gx-007/gx-008 fail on an ANALYZER-nullability gap** — Spark marks GROUP-BY-ROLLUP/CUBE grouping columns nullable (subtotal rows carry NULL); τ's analyzer keeps them non-nullable (schema mismatch). Same class as the set-op nullability gap (set-004/006/008). gx-003/004 GROUPING SETS (set-membership emission, Slice G); gx-010 `WITH ROLLUP` (SparkDialect `supports_group_by_with_modifier` override + modifier lowering).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 640/0, `sql_v2` differential 137/125/262 with 5 gx cases GREEN, no regressions).
- Commit SHA: (this commit).

## Pass 101 — 2026-07-05 — SQL corpus: CTEs via inlining (cte cluster) [run 2]
- **Corpus: SQL front-end.** Target: `cte-001`..`cte-010` (WITH clauses). All failed at `sql::cte` reject.
- Diagnostic: `.agent-output/diagnostic-pass-101.md` — τ has NO WithCte CommonOp; chose **INLINE** (reuse `AliasedRelation` + `ToDf` + the derived-table path — all green). No analyzer/emission change.
- Architecture: `.agent-output/architecture-pass-101.md` — `lower_query` builds a `CteScope` (name→lowered body, incremental so nested CTEs see predecessors; explicit column list → `ToDf`); thread `&CteScope` through lower_query→lower_set_expr→lower_select→lower_from→…→lower_table_factor; at the FROM Table arm a CTE-named table becomes `AliasedRelation{clone(body), alias}` (reference alias wins). WITH RECURSIVE → loud reject.
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No analyzer/emission/ast change.
- ADR citations: ADR-004 (SQL → common AST; CTE inlines onto existing AliasedRelation/ToDf substrate — no new op), ADR-001 (transliterate — inlining IS the literal non-recursive-CTE semantics), ADR-022 (WITH RECURSIVE → `sql::recursive_cte` boundary reject).
- Corpus signal: 137 → **142** (+5). `cte-002/003/004/005/007/008`… → GREEN: cte-002/003/004/007/008 (multiple, twice, nested, +window, unioned). No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 4 (single-ref inlines as AliasedRelation, explicit-columns→ToDf, referenced-twice→2 clones, WITH RECURSIVE reject).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (2 Low: forward-ref error class = Thunderduck-boundary not Spark-emulated; adversarial nested-CTE exponential clone — inherent to ADR-004 inline, benign for corpus). Perf: per-reference deep clone, O(refs×body), corpus-bounded — no HIGH/MEDIUM.
- Findings queued as follow-up (DIFFERENT layer — emission): **cte-001** `Binder Error: Referenced table "e"` — a plain aliased table (`emp e`) JOINed with an inlined subquery (AliasedRelation) loses its alias in join emission; **cte-005** `Binder Error: Referenced column "count"` — `ToDf` positional rename over an auto-named `count(*)` doesn't rename in emission. Both are emission-layer gaps exposed by CTE inlining. cte-006 needs `Expr::InSubquery` (separate); cte-009/010 recursive (rejected).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 644/0, `sql_v2` differential 142/120/262 with 5 cte cases GREEN, no regressions).
- Commit SHA: (this commit).

## Pass 102 — 2026-07-05 — SQL corpus: SQL-syntax function forms (fn-003 cluster) [run 2]
- **Corpus: SQL front-end.** Target: fn-003 (`SUBSTRING(s FROM p FOR n)`), fn-004 (`substr`), fn-005 (`TRIM(BOTH x FROM s)`), fn-006 (`POSITION(sub IN s)`), fn-007 (`OVERLAY(s PLACING r FROM p FOR n)`). All hit `sql::expr::other` — sqlparser routes these special syntaxes to dedicated `Expr` variants that `lower_expr`'s catch-all rejected; the underlying functions already emit.
- Diagnostic: `.agent-output/diagnostic-pass-102.md` — pure lowering; add 4 `lower_expr` arms → `Expression::FunctionCall`. Per-variant → function + arg-order mapping table; Position must map to `locate` (DuckDB has no `position` scalar). fn-004 `substr` also parses to `Expr::Substring` (shorthand) — same arm.
- Architecture: `.agent-output/architecture-pass-102.md` — mirror the `Expr::Extract` arm; verify arg order against emission.
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No emission/analyzer/type_inference/ast change.
- ADR citations: ADR-004 (SQL onto existing function substrate), ADR-015 (Spark parity — verified arg order/name for each: trim `[str, chars]`, position→`locate` `[needle, str]`→`strpos(str,needle)` 1-based, overlay `[input, replace, pos, len]`, substring `[str, start, len]`), ADR-022 (now supported).
- Corpus signal: 147 (142 → **147**, +5). fn-003/004/005/006/007 GREEN. No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 8 (substring FROM/FOR + substr shorthand, trim BOTH/LEADING/TRAILING + bare trim, position→locate, overlay 4-arg) — with positional arg-order assertions for trim + position.
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (all arg orders verified end-to-end against emission arms; 1 Low: explicit `Both|None` match arm). Perf negligible (parse-time).
- Findings queued as follow-up: fn-014 (date_add/datediff), fn-017 (round/abs/ceil/floor), fn-018 (int/int div) — schema/value diffs, separate; fn-020 (`X'..'` hex string literal — `sql::value::HexStringLiteral`).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 652/0, `sql_v2` differential 147/115/262, no regressions).
- Commit SHA: (this commit).

## Pass 103 — 2026-07-05 — SQL corpus: IS [NOT] DISTINCT FROM + `<=>` (pr-001 cluster) [run 2]
- **Corpus: SQL front-end.** Target: pr-001 (`IS DISTINCT FROM`), pr-002 (`IS NOT DISTINCT FROM`), whr-015 (`<=>` null-safe equality). Failed as `sql::expr::other` / `sql::binary_op::Spaceship`.
- Diagnostic: `.agent-output/diagnostic-pass-103.md` — pure lowering; τ `Expression::IsDistinctFrom(IsDistinctFromExpression{left,right,negated})` already analyzer-typed (Boolean) + emission-rendered (`IS [NOT] DISTINCT FROM`). SQL front-end didn't lower the forms.
- Architecture: `.agent-output/architecture-pass-103.md` — 2 lower_expr arms (IsDistinctFrom negated:false, IsNotDistinctFrom negated:true) + Spaceship short-circuit in the Expr::BinaryOp handler → negated:true (Spark `<=>` = null-safe equal = NOT DISTINCT FROM).
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No emission/analyzer/ast change.
- ADR citations: ADR-004 (SQL onto existing IsDistinctFrom substrate), ADR-015 (`<=>` = NOT DISTINCT FROM; NULL<=>NULL true), ADR-022 (now supported).
- Corpus signal: 147 → **150** (+3). pr-001/pr-002/whr-015 GREEN. No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 3 (with `negated` polarity assertions).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (polarity verified against emission — negated:true renders IS NOT DISTINCT FROM; Spaceship short-circuit correctly placed before lower_binary_op's catch-all; 1 Low: cosmetic arm merge). Perf negligible.
- Findings queued as follow-up: more `sql::expr::other` — LIKE ANY/ALL (pr-003/004), IS TRUE/FALSE (pr-006), multi-col IN tuple (pr-005), array/map literals (cx-001/002), DATE/TIMESTAMP literals (lit-001/002).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 655/0, `sql_v2` differential 150/112/262, no regressions).
- Commit SHA: (this commit).

## Pass 104 — 2026-07-05 — SQL corpus: set-op output nullability (set-004 cluster) [run 2]
- **Corpus: SQL front-end.** Target: set-004 (INTERSECT), set-006 (EXCEPT), set-008 (MINUS) — EXECUTE but fail on schema nullable-mismatch (Reference=False, Test=True). ANALYZER layer (not lowering).
- Diagnostic: `.agent-output/diagnostic-pass-104.md` — `CommonOp::SetOp` per-column nullability fold (analyzer.rs:1544) ORed across all children regardless of operator. Spark rule (verified vs `basicLogicalOperators.scala`): Union=OR, Intersect=AND (`left.nullable && right.nullable`), Except/Minus=left child only (`output=left.output`). τ used OR for all → bug for Intersect/Except.
- Architecture: `.agent-output/architecture-pass-104.md` — split the fold: type widening (unify_types, ADR-006) unchanged; nullability computed per `kind` (Union `.any`, Intersect `.all`, Except children[0]).
- Layer(s) touched: τ analyzer (`analyzer.rs` SetOp arm). No lowering/emission change.
- ADR citations: ADR-006 (type widening — type half untouched), ADR-015 (Spark parity — per-operator nullability), ADR-005 (analyzer owns nullability).
- Corpus signal: 150 → **153** (+3). set-004/006/008 GREEN (all set-* now green except set-009 mixed-type union). No regressions. **core_v2 DataFrame suite 312 → 313** (the operator-aware rule also fixed a DataFrame set-op case; shared analyzer, no regression).
- Files: `crates/core/src/transpiler_v2/analyzer.rs`.
- Tests added: 4 (Intersect AND, Except left-only both directions [guards first/last index flip], Union OR regression guard).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (polarity + n-ary + no-panic verified; exhaustive `match kind` forces recompile on new variants). Perf negligible.
- Findings queued as follow-up: gx-007/008 ROLLUP/CUBE grouping-key nullability (analogous analyzer fix — grouping cols nullable under rollup/cube); set-009 mixed-type 3-way union (emission).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 659/0, `sql_v2` 153/109/262, `core_v2` 313/324 no regression).
- Commit SHA: (this commit).

## Pass 105 — 2026-07-05 — SQL corpus: tech-debt sweep (passes 101–104) [run 2]
- **Tech-debt sweep** (every-5th-pass): rust-reviewer over `git diff 7b85190..HEAD` (CTE inlining + CteScope threading, SQL-syntax function forms, IS DISTINCT FROM/`<=>`, set-op operator-aware nullability). Nothing Critical/High/Medium; 0 compiler warnings.
- Fixed: **L1** — stale comment on the `TableFactor::Derived` arm claiming `AliasedRelation` is a "deferred Slice C.1 variant" (false since pass 101 made it live for CTE references); reworded to note the derived-table alias is still dropped at Slice A.2 scope and preserving it (via AliasedRelation, now proven live) is a follow-up that would green tbl-010.
- Verified clean (reviewer): CteScope threading complete/consistent (lower_expr correctly does NOT take the scope — expression-level subqueries reject wholesale, so no silent mis-resolution); entry-point `&CteScope::new()` cheap; shape-string naming consistent; analyzer set-op index accesses arity-guarded (no panic); by-name branch OR-fold correct (unionByName-only). Deferred L2 (minor lower_expr arm duplication — marginal churn, defensible to leave).
- Files: `crates/core/src/parser_v2/v2_lowering.rs` (comment only; behavior-preserving; corpus stays 153).
- Quality Gate: PASS (cargo check clean, rustfmt clean, 0 warnings).
- Commit SHA: (this commit).

## Pass 106 — 2026-07-05 — SQL corpus: uncorrelated scalar/IN/EXISTS subqueries (sq-001 cluster) [run 2]
- **Corpus: SQL front-end.** Target: uncorrelated subqueries sq-001 (scalar in SELECT), sq-002 (scalar in WHERE), sq-005 (EXISTS), sq-008 (IN), sq-009 (NOT IN + 3VL), sq-020 (scalar in CASE), sq-021 (IN over GROUP BY/HAVING). The biggest single bucket. NOT lowering-only.
- Diagnostic: `.agent-output/diagnostic-pass-106.md` — the 3 τ subquery Expression variants exist but were STUBBED end-to-end (lowering catch-all rejects; analyzer opaque; emission UnsupportedExpression). FOUR seams needed. Correlated (10) need Slice-F outer-column staging → deferred; quantified ALL/ANY need a new variant → deferred.
- Architecture: `.agent-output/architecture-pass-106.md` — option (a), ADR-007 A (analyzer annotates, emission node-local): `SubqueryPlan { Unanalyzed(Box<CommonAst>), Analyzed(Box<TypedAst>) }` on the 3 variants; lowering builds Unanalyzed; analyzer recurses `analyze(inner, base_types)` → Analyzed + types (scalar→inner col-0 nullable; IN→Bool 3VL; EXISTS→Bool); **Seam D** base_types descends into subqueries for catalog pre-fetch; emission renders node-local via dispatch_op.
- Layer(s) touched: τ lowering + analyzer + base_types + emission + expression (5 files). Correlated → honest UnknownColumn boundary error.
- ADR citations: ADR-007 A (analyzer annotates the node; emission renders node-local — SubqueryPlan carries the analyzed inner plan), ADR-008 (subqueries; correlated deferred to Slice F), ADR-022 (correlated/quantified → boundary), ADR-016 (3VL NOT IN — native emission preserves Spark/DuckDB parity).
- Corpus signal: 153 → **160** (+7). sq-001/002/005/008/009/020 GREEN + 1 collateral. **core_v2 held at 313 (no regression** — base_types threading is inert for non-subquery expressions). sq-021 (GROUP BY/HAVING in subquery) still red.
- Files: `crates/core/src/transpiler_v2/{expression,analyzer,base_types,emission}.rs`, `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: ~15 across the 5 files (SubqueryPlan states, lowering arms, analyzer typing + single-column boundary + correlated→boundary, Seam D collection, emission shapes, + `subquery_sees_outer_cte_scope`).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High. **M1 (Medium) CLOSED in-pass** — subquery arms initially lowered the inner query with an empty CteScope (silent-shadow risk: an outer CTE invisible in a subquery could read a same-named base table); fixed by threading `cte_scope` into `lower_expr` (13 signatures, ~45 call sites) so subqueries see outer CTEs (correct Spark shadowing). **L1 CLOSED** — `for_each_node_expr` `_ => {}` → exhaustive match (gotcha #9; future expr-bearing CommonOp now fails to compile until wired). Perf (reviewer): inner plan analyzed exactly once (idempotent); Seam D redundant-walk negligible at corpus scale (Low).
- Findings queued as follow-up: correlated subqueries (10 cases, Slice F outer-col staging); quantified ALL/ANY (sq-011/012/013 — new variant); sq-021 (aggregate/HAVING-in-subquery diff); cte-006 (WITH-inside-subquery — separate); L2 (InSubquery nullability hard-true — projected-IN only); L3 (multi-col scalar error class); Seam D perf (bool short-circuit variant of plan_has_empty_scan).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 677/0, `sql_v2` 160/102/262, `core_v2` 313/324 no regression).
- Commit SHA: (this commit).

## Pass 107 — 2026-07-05 — SQL corpus: PIVOT / UNPIVOT (pv-001 cluster) [run 2]
- **Corpus: SQL front-end.** Target: pv-001/002/003/005 (PIVOT single/count/multi-agg/numeric-IN), pv-004 (UNPIVOT). pv-006 (stack()) out. NOT lowering-only — 3 gaps beyond the table-factor lowering.
- Diagnostic: `.agent-output/diagnostic-pass-107.md` — τ has `CommonOp::Pivot`/`Unpivot` + analyzer + emission (green via DataFrame path), but: G1 PIVOT implicit grouping (SQL has no grouping list; = input − pivot − agg-refs), G2 UNPIVOT implicit ids (= input − values), G3 PIVOT value-alias emission strip.
- Architecture: `.agent-output/architecture-pass-107.md` — two closed-set AST markers `PivotGrouping{Explicit,Implicit}` / `UnpivotIds{Explicit,Implicit}` (empty≠implicit); SQL lowering sets Implicit, DataFrame `convert_pivot`/`convert_unpivot` set Explicit (behavior-neutral); analyzer computes implicit sets from resolved input schema; emission G3 one-line alias strip.
- Layer(s) touched: τ ast + lowering + analyzer + emission + base_types + connect-server converter (6 files). ADR-005/006 (analyzer), ADR-022 (dynamic PIVOT values / INCLUDE NULLS / aliased unpivot cols → loud boundary reject), enums-for-closed-sets idiom.
- Corpus signal: 160 → **164** (+4). pv-001/003/004/005 GREEN. **core_v2 held at 313 (no regression** — AST enum behavior-neutral for the Explicit/DataFrame path). pv-002 (count(*) pivot value diff) + pv-006 (stack generator) red.
- Files: `crates/core/src/transpiler_v2/{ast,analyzer,emission,base_types}.rs`, `crates/core/src/parser_v2/v2_lowering.rs`, `crates/connect-server/src/converter/v2_relation_converter.rs`.
- Tests added: ~13 (lowering PIVOT/UNPIVOT→Implicit + aliased values + dynamic-value reject; analyzer implicit grouping/ids; emission G3; + M1/M2 fix tests).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High. **M1 + M2 (Medium) CLOSED in-pass** — implicit-grouping reference collection was name-based (M1) and had a `_ => {}` catch-all silently dropping refs inside Between/InList/Window/etc. (M2, gotcha #3/#9 silent-wrong-result class). Fixed: `collect_referenced_columns` made EXHAUSTIVE over all 28 Expression variants (future variant now fails to compile); pivot column excluded structurally via it. Perf negligible.
- Findings queued as follow-up: pv-002 (count(*) pivot value/schema diff); pv-006 stack() (generator, separate); L2/L3 (redundant unpivot validation; pivot-result alias qualification).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean both crates, rustfmt clean, `cargo test -p thunderduck-core --lib` 688/0, `cargo test -p thunderduck-connect-server --bins` 74/0, `sql_v2` 164/98/262, `core_v2` 313/324 no regression).
- Commit SHA: (this commit).

## Pass 108 — 2026-07-05 — SQL corpus: typed literals DATE/TIMESTAMP/decimal (lit-001 cluster) [run 2]
- **Corpus: SQL front-end.** Target: lit-001 (DATE literal), lit-002 (TIMESTAMP literal), lit-007 (decimal literal arithmetic). All `sql::expr::other`. Lowering-only.
- Diagnostic: `.agent-output/diagnostic-pass-108.md` — `lower_expr` has no `Expr::TypedString` arm; `lower_value` handles Number as i32/i64/f64 only (no decimal). τ has `LiteralValue::Date(i32)`/`Timestamp(i64)`/`Decimal{value,precision,scale}` — all render in emission. No string→epoch parser exists (no chrono dep).
- Architecture: `.agent-output/architecture-pass-108.md` — TypedString→Date/Timestamp literal; decimal arm in lower_value. (Initial CAST approach abandoned — CAST gives nullable=true; Spark DATE/TIMESTAMP literals are non-null.)
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). Self-contained date/timestamp string parsers (Hinnant days_from_civil; no deps). No emission/analyzer/ast change.
- ADR citations: ADR-004 (SQL literals → common AST), ADR-015 (Spark parity — non-null DATE/TIMESTAMP literals; decimal precision/scale + ADR-006 widening), ADR-016 (ANSI — invalid calendar dates/out-of-range years → boundary error, not silent rollover).
- Corpus signal: 164 → **168** (+4). lit-001/002/007 GREEN + 1 collateral. No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: ~13 (TypedString→non-null Date(20468)/Timestamp; decimal precision/scale 100.25→(5,2), 3.142→(4,3); + review-fix tests: invalid calendar dates, leap day, out-of-range year, >38-digit clamp).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + **1 High CLOSED** (H1: invalid calendar dates `2026-02-30`/non-leap `2023-02-29` silently rolled over to a wrong date — Spark ANSI throws; fixed with leap-aware day-of-month validation → boundary error). **2 Medium CLOSED** (M1: unbounded year → i64 overflow panic/wrap — bounded to Spark's [1,9999] DATE domain; M2: decimal precision missing `.min(38)` clamp — added to match `normalize_decimal_literal`). Also fixed the nullability bug (CAST→non-null literal). Perf negligible.
- Findings queued as follow-up: lit-008 (TIMESTAMP−TIMESTAMP→interval type inference); lit-004/005 (compound INTERVAL X TO Y); lit-006 (make_interval); lit-009 (string escape); large integer literal → Decimal(n,0) (L3).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 701/0, `sql_v2` 168/94/262, no regressions).
- Commit SHA: (this commit).

## Pass 109 — 2026-07-05 — SQL corpus: IS [NOT] TRUE / IS [NOT] FALSE (pr-006) [run 2]
- **Corpus: SQL front-end.** Target: pr-006 `... WHERE active IS TRUE AND (age > 100) IS FALSE` (`sql::expr::other`). Lowering-only, reuses the pass-103 `IsDistinctFrom` substrate.
- (Note: this pass initially targeted gx-008 [duplicated grouping columns under CUBE with a computed key]. Diagnosis found the real bug is the `already_folded` name-based fold detection; two coder attempts at a structural/pre-resolution fix passed unit tests but did NOT green gx-008 — the raw grouping vs aggregate exprs differ in a field (qualifier/plan_id) even pre-resolution, so the fold match fails for the real SQL path. Reverted [no regression] and deferred gx-008; gx-007 is a separate HAVING-with-aggregate emission bug [`WHERE clause cannot contain aggregates`]. Retargeted pr-006.)
- Diagnostic/Architecture: `.agent-output/architecture-pass-109.md` — `x IS TRUE` ⟺ `x IS NOT DISTINCT FROM TRUE` (3VL: NULL IS TRUE = false). 4 arms → IsDistinctFrom with a non-null Boolean literal and the right `negated` polarity.
- Layer(s) touched: τ SQL front-end only (`parser_v2/v2_lowering.rs`). No emission/analyzer/ast change.
- ADR citations: ADR-004 (reuse IsDistinctFrom substrate), ADR-015 (3VL parity — verified truth table), ADR-022 (now supported).
- Corpus signal: 168 → **169** (+1). pr-006 GREEN. No regressions.
- Files: `crates/core/src/parser_v2/v2_lowering.rs`.
- Tests added: 4 ((literal, negated) polarity assertions for IS TRUE/NOT TRUE/FALSE/NOT FALSE).
- Findings CLOSE_NOW_IN_THIS_PASS: Reviewer 0 Critical + 0 High (3VL polarity verified via full truth table incl. NULL cells). Perf negligible.
- Findings queued as follow-up: gx-008 (fold-detection — needs the raw-expr mismatch root cause understood, or lowering to declare the fold explicitly); gx-007 (HAVING-with-aggregate → HAVING/QUALIFY emission, not WHERE); pr-003/004 (LIKE ANY/ALL — tuples), pr-005 (multi-col IN — `sql::expr::tuple` row-values), pr-007 (lateral column alias — analyzer).
- Compiler warning delta: baseline preserved (0 new).
- Quality Gate: PASS (cargo check clean, rustfmt clean, `cargo test -p thunderduck-core --lib` 705/0, `sql_v2` 169/93/262, no regressions).
- Commit SHA: (this commit).
