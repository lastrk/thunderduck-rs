# τ Simplification Pass Log (fresh start 2026-07-07)

Goal: iterative simplification of the τ transpiler (type inference, analyzer/lowering,
emission, both front-end converters): fewer special-case branches, more general and
compact code, no obscure abstractions, behavior preserved exactly (Spark parity).
Prior simplification work was deleted/ignored per step 0; this log is the new record.

Method per pass: parallel Fable 5 analysis agents → triage → parallel implementation on
disjoint files → quality gate (fmt scoped to changed files, cargo check, crate unit
tests) → DataFrame corpus (`tests/scripts/v2-progress.sh`) as the fitness gate.
Max 20 passes; stop when a pass yields no meaningful improvement. No commits without
user review (per CLAUDE.md); per-pass diffs snapshotted to /tmp/simplify-passN.patch.

Baseline (pass 0, working tree at fb274d1 + deleted old log):
- `cargo fmt --check` clean
- `cargo test -p thunderduck-core --lib --tests` green
- `cargo test -p thunderduck-connect-server --tests` green (91 passed)

---

## Pass 1 — analysis summary (9 agents)

Verified headroom by area (production code unless noted):
- **type_inference.rs**: agg metadata in 5 parallel in-file rosters (real drift found);
  paired type/nullability resolvers walking the same structure twice; duplicated
  integral→decimal table + dead else; near-identical array-rewrap arms; dead pub fns.
- **analyzer.rs** (production ends ~3858): two pre-`children()` hand walkers
  (`collect_referenced_columns`, `qualify_plan_id_refs`); set-op strict-by-name branch
  proved a special case of allow-missing; small exact dedups (pretty_literal,
  field_by_name, na_fill closure, unpivot double-validation, expr_field, projection
  expanders); dead pivot guard + stale docs.
- **emission.rs first half**: ~50 `render_function_call` arms share an
  arity-check/render/format skeleton (const-generic `exact_args::<N>` helper); 40+
  hand-rolled comma-join loops; `substitute_index_var` provably = `substitute_lambda_var`
  wrapper; join keyword/clause duplicated across `render_join_from`/`render_join`
  (SEMI/ANTI gotcha lives twice); set-op triple loop; make_interval trio; misc.
- **emission.rs second half**: production is only ~240 lines; duplicate DDL-grammar
  walker (typed vs DuckDB-JSON) collapsible to parse-once/render-from-DataType; rest is
  a 5,478-line test module with ~850–1,000 lines of pure builder boilerplate.
- **expression.rs**: verbose `map_children` (156 lines) → `children_mut` + generic loop;
  dead `ifnull` arm (differing body!); dead `is_arithmetic`; `as_string_literal` helper
  (8 in-file sites, ~64 more elsewhere); mergeable identical arms; arrays_zip name
  ladder duplicated with emission (comment-enforced parity).
- **v2_relation_converter.rs**: `lit()` ctor helper (~110 LOC); hand-rolled ~270-line
  JSON schema scanner → serde_json (~220 LOC, kills a bug class); require_proto sweep;
  convert_window extraction; small dedups. Confirmed NO function-name table duplication
  with the SQL front-end (different input domains).
- **v2_lowering.rs**: hand-rolled 120-line lambda rewriter → `map_children`;
  ctor micro-helpers; guard-then-`unreachable!` double matches; select_item_expr;
  interval slot table; lower_function split. Decimal precision/scale computation
  duplicated verbatim with connect-server (comment-enforced mirror).
- **cross-cutting**: 6 hand-rolled Expression walkers in 4 files (~650 lines total);
  `CommonOp` child-walk written 3× (base_types ×2 + service.rs) with plan walked 3× per
  request; Spark type-string parsing implemented 2×+1 mapping table (incl. a dead
  struct branch in `parse_type_str_to_struct` — DDL-string schema silently becomes
  empty); function return-type knowledge shadowed between expression.rs and
  type_inference.rs; bail_boundary macro quadruplication; alias-strip idiom ×9.

Reconciliation notes:
- Cross-cutting's "aggregate roster single-sourced" refers to external consumers of
  `AGGREGATE_NAMES`; the 5-roster problem is *within* type_inference.rs — both true.
- Cross-cutting proposed "add map_children" — it already exists; the actual work is
  rebuilding each hand walker on it.

### Pass 1 implementation slate

Wave 1 (parallel, disjoint files, byte-identical behavior only):
1. type_inference.rs: resolver pairs → (DataType,bool); decimal_form dedup + dead else;
   array-rewrap helper + arm merges; delete dead pub fns + no-op window branch.
2. expression.rs: children_mut + generic map_children; delete dead ifnull arm +
   is_arithmetic; as_string_literal (pub(super)); merge identical arms; dst_may_fail
   via is_numeric.
3. emission.rs: substitute_index_var wrapper; join kind/clause dedup; exact_args/
   min_args helpers; sql_join; string_literal_arg reuse; render_window extraction +
   render_sort_key reuse; strip_alias local; set-op single loop; make_interval helper;
   group-exprs builder; DDL walker parse-once dedup.
4. v2_lowering.rs: lambda rewriter on map_children; fn_call/str_lit/is_distinct
   helpers + wrap_not reuse; try_from single-elem matches; select_item_expr; interval
   slot; lower_function split.
5. v2_relation_converter.rs + relation_converter.rs: lit(); require_proto sweep;
   convert_window; NaFill direct convert_literal; plan-ids children table;
   arrow_field_to_struct_field; single_name_part; serde_json schema decode.
6. analyzer.rs: collect_referenced_columns via children(); widen_by_name extraction;
   pretty_literal merge; field_by_name sweep; dead pivot guard; na_fill closure merge;
   unpivot implicit-path dedup; expr_field; expand_projections driver; minor items.

Deferred to later passes (behavior-superset or cross-file, need dedicated gating):
- qualify_plan_id_refs recursion widening (corpus-gated behavior superset).
- Aggregate spec table (T1) + expression.rs roster delegation (touches 3 files).
- CommonOp::children (ast.rs + base_types.rs + service.rs).
- Expression::unaliased shared helper; arrays_zip name into struct_names.rs.
- Shared decimal precision/scale fn (core + connect-server).
- Spark DDL type parser unification (acceptance-widening; corpus-gated).
- bail_boundary macro + spark_errors fragments consolidation.
- Test-module boilerplate consolidation (analyzer ~600 LOC, emission ~900 LOC).

### Pass 1 implementation results

Wave 1 (6 parallel coders, disjoint files) + Wave 2 (2 coders, cross-file):

| File | Δ |
|---|---|
| type_inference.rs | −107 then ≈+30 (agg spec table replaces 5 rosters) |
| expression.rs | −171 then ≈+35 (shared decimal fn, unaliased) |
| emission.rs | −469 then ≈−10 |
| analyzer.rs (+fixtures) | −114 then −12 |
| v2_lowering.rs | −50 then −15 |
| converter (2 files + Cargo.toml) | −303 then −12 |
| ast.rs / base_types.rs / service.rs | −53 (CommonOp::children; plan walked 1× not 3×) |

Net: **−1,800 lines** (2,511 insertions / 4,311 deletions incl. this log).

Landed: all Wave-1 items from the slate; CommonOp::children()/children_mut
(exhaustive, no `_` arm — new variants fail to compile in one place);
BaseTypes::from_entries; aggregate spec table (drift transcribed verbatim and
commented: std, array_agg, approx_count_distinct, count_approx_distinct,
regr_count, nth_value, mode); arrays_zip name ladder → struct_names;
Expression::unaliased; shared decimal_value_precision_scale;
bail_boundary_kind! consolidation; spark_errors template dedup + canonical
escaping; flip_all_nullable dedup.

Documented equivalence caveats (reviewed, accepted):
- analyze_set_op strict-by-name: old code was internally inconsistent for
  non-ASCII-cased duplicate names (to_lowercase vs eq_ignore_ascii_case);
  new code marks such a field nullable. ASCII behavior identical.
- render_function_call converted arms now check arity BEFORE rendering args;
  only observable when a call is both wrong-arity AND has an unrenderable arg
  (error precedence between two errors). No test/corpus case observes it.
- Test rewrites confined to: type_inference/expression drift-police tests
  (now police the single table), base_types plan_has_empty_scan tests
  (rewritten to empty_scan_tables equivalents, intent preserved).
- ast.rs uses one local macro (common_op_children!) to generate the &/&mut
  accessor pair from a single exhaustive match — accepted as the standard
  Rust idiom for mut/immut accessor pairs (logic visible in one place).

Verification: `cargo fmt --check` clean; core 686 passed / cs 91 passed;
invariants 10/10; DataFrame corpus **327/327 (= baseline)**; SQL corpus
**237 passed / 25 failed (= baseline exactly)**. Diff snapshot:
/tmp/simplify-pass1-full.patch. Not committed (awaiting user review).

---

## Pass 2 — deferred behavior-superset + larger structural items

Slate (from pass-1 analysis, deferred for dedicated gating):
- A1 analyzer.rs: qualify_plan_id_refs rebuilt on map_children — recursion-set
  superset (plan_id refs inside Between/InList/Like/etc. now qualified);
  corpus-gated. Also: delete dead TypedAttr (zero constructors workspace-wide;
  flagged for user review here).
- A2 emission.rs: substitute_lambda_var rebuilt on children_mut/map_children —
  same superset class (lambda vars inside Between/InList currently silently
  unsubstituted = latent wrong-SQL bug); corpus-gated.
- A3 expression.rs + type_inference.rs: function_return_type takes the
  argument types; move the whole-arg-list widening arms out of
  expression.rs's pre-empt block; delete the shadowed weaker duplicates.
- B1 Spark type-string parsing unification: one DDL/type-string parser in
  core (types/spark_ddl.rs) shared by type_converter.parse_type_str and
  emission's DDL parser; fixes the dead Struct branch in
  parse_type_str_to_struct (DDL-string schema currently silently → empty);
  acceptance-widening, corpus-gated.
- B2 analyzer.rs test-module builder consolidation (~500–700 LOC, zero risk).
- C1 emission.rs test-module builder consolidation (~850–1,000 LOC, zero risk).

### Pass 2 implementation results

All six slate items landed (one agent needed two relaunches due to transient
API errors/rate limits; the finisher verified byte-identical end state):
- A1 qualify_plan_id_refs → map_children (61→29 lines) + dead TypedAttr
  deleted (−54 total). No test pinned the old narrower recursion.
- A2 substitute_lambda_var → map_children (−75); MapLiteral/StructLiteral/
  RowConstructor/UpdateFields now recursed (latent wrong-SQL bug fixed);
  subquery/Window opacity explicitly preserved with rationale comment.
- A3 function_return_type(name, &[DataType]) is the single home for
  per-function return typing; widening arms moved from expression.rs
  (coalesce family, nvl2/if/iif, aggregate/reduce); shadowed weak arm reduced
  to abs|nullif; zero-args edges documented in comments.
- B1 types/spark_ddl.rs: ONE Spark DDL/type-string parser (strict + lenient
  entries), union grammar documented; emission −155, type_converter −49;
  latent bug fixed: parse_type_str_to_struct's DDL fallback was dead and
  DDL-string schemas silently became empty — now live, +17 tests. Acceptance
  changes strictly additive (verified; corpus unchanged).
- B2 analyzer test module: 4,038 → 3,059 lines (−979) via shared builders;
  115/115 module tests, name list byte-identical.
- C1 emission test module: 5,481 → 4,801 (−680); fcall/render_fn/
  expect_unsupported/scan helpers; 217/217 tests, name list byte-identical;
  tap_guard placements untouched.

Cumulative (passes 1+2, vs HEAD fb274d1): **−3,580 lines** across 25 code
files (3,942 insertions / 7,522 deletions incl. logs). Diff snapshot:
/tmp/simplify-pass2-full.patch.

Verification: fmt clean; core 700 passed / cs 94 passed (both grew by new
tests); DataFrame corpus **327/327**; SQL corpus **237/25** — both identical
to baseline; the two walker behavior-supersets and the DDL widening caused
zero corpus movement.

Process note: .claude/agents/*.md all pinned `model: opus`; per user
instruction all six now pin `model: fable` (explicit per-launch overrides
were already Fable; resume-after-crash fell back to frontmatter — now safe).

---

## Pass 3 — fresh analysis on the post-pass-2 code

Four analysis agents re-read the updated tree. Consensus: production code is
approaching its natural minimum everywhere; remaining verified headroom is
~430–490 production lines (mostly mechanical: sweep-missed separator loops,
one to_number/try_to_number dup, bool-literal matcher ×4, dead code
incl. service.rs's retired collect-then-stream path (~55), the
collect_scan_tables_in_expr walker (~85, feasibility verified arm-by-arm),
the na.fill selection rule duplicated analyzer↔emission (the exact gotcha-12
desync hazard), dead HasSchema trait, children/children_mut macro per the
ast.rs precedent) plus ~800–1,000 test-module lines (type_inference,
expression, v2_lowering, converter, service test modules — never
consolidated).

Explicitly REJECTED after evaluation: splitting render_function_call into
domain modules (moves branches, breaks the O(1) grep-the-name navigation,
and the INV3 barrier is enforced by two file-hardcoded scans); sqlparser
visitor for expr_has_aggregate (descends into subqueries = behavior change);
Display↔spark_ddl name-table unification (format vs parse direction,
deliberately different vocabularies); CommonAst wrapper flattening
(high churn, low value).

### Correctness flags recorded during pass 3 (NOT fixed — need corpus
witnesses first; candidates for regular /fix-bug or corpus passes)
1. v2_lowering `expr_has_aggregate` does not descend into function args:
   `SELECT abs(count(x)) FROM t` classifies non-aggregate and mis-lowers to
   a plain Project (should be aggregate or an honest boundary error).
2. v2_lowering `resolve_named_windows_in_expr` descends only through
   Nested/UnaryOp/Cast/BinaryOp — a named-window ref inside CASE/IN/BETWEEN
   hits the "not defined in WINDOW clause" error despite being defined.
3. v2_lowering TableFactor::Function / TableFactor::TableFunction arms
   silently drop a user alias (`alias: _`), unlike the Table arm which
   routes through apply_table_alias — same silent-drop class gotcha 9 bans.
4. type_converter proto Kind::CalendarInterval → DataType::YearMonthInterval
   ("best-effort" written before DataType::Interval existed) — lossy
   round-trip vs data_type_to_proto's Interval → CalendarInterval.
5. Sort null-ordering divergence between front-ends (proto Unspecified →
   NullsFirst unconditionally vs SQL deriving from direction) — currently
   unobservable (PySpark always stamps explicit ordering); documented via
   comment in pass 3, alignment deferred until a wire shape witnesses it.
6. emission `rewrite_grouping_id` deliberately recurses only into
   FunctionCall/Alias/Cast/CaseWhen — `grouping_id() + 1` passes through
   un-rewritten (latent; conversion to map_children would be a behavior
   change, deferred).

### Pass 3 implementation results

Five parallel coders (disjoint files) + a 2-item sequential mini-wave:

| Area | Δ | Highlights |
|---|---|---|
| emission.rs | −85 | 7 sweep-missed loops → sql_join; to_number/try_to_number merged; bool_literal; render_distinct deleted; stale Decision-13-A docs fixed; TD_JOIN_* consts imported from analyzer (coupling fix); rendered_args adoption |
| analyzer.rs + base_types.rs | −134 | collect_scan_tables_in_expr → children() (last big hand walker); na_fill_value_for extracted (pub(super)); passthrough helpers merged; dead HasSchema deleted; qualify_plan_id_refs → in-place &mut |
| expression.rs + type_inference.rs + types/ | −359 | expression_children! macro (ast.rs precedent); dead API deleted (is_floating_point, is_decimal, field_index); constant-arm folds in function_call_nullable; from_json/from_csv merged; decimal_bounds; stale to_number comment fixed; both test modules consolidated |
| v2_lowering.rs | −104 | table_function_node; lower_function_args returns (distinct, args); boundary_shape test helper — assertions strengthened to kind+name; one masked misclassification pinned (in_row family) |
| connect-server (3 files) | −206 | dead collect-then-stream path deleted (~55); convert_all + import hoist; sort-divergence comment; test helpers; make-work test deleted |
| mini-wave (emission) | −28 | render_na_fill's value_for → shared na_fill_value_for (gotcha-12 hazard closed); literal_string_arg delegates to as_string_literal |

Pass 3 net: ≈ −915 lines. Documented deviations: conv() overflowing-Long
to_base now errors instead of silently wrapping (pathological,
corpus-unwitnessed, noted in code); v2_lowering boundary tests strengthened
(kind-only → kind+name); field_index test deleted with its function.

Verification: fmt clean; core 699 passed (−1 = deleted field_index test) /
cs 93 passed (−1 = deleted make-work test); DataFrame corpus **327/327**;
SQL corpus **237/25** — both identical to baseline. Diff snapshot:
/tmp/simplify-pass3-full.patch.

---

## Pass 4 — adversarial convergence check + residue implementation

Two adversarial agents tried to disprove the passes' "natural minimum" claims.

**Split verdict.** The transpiler proper (transpiler_v2/ + parser_v2/ + types/)
is converged: parser_v2 audit found nothing above bar (helper adoption
complete, no drift, no dead code); expression/type_inference audit likewise.
Only two in-scope core items remained (WithColumns slot-matching maintained
in analyzer AND emission — a documented wire-schema drift hazard; and
render_aggregate's dead nth_value arm + the first/last ignoreNulls trim rule
implemented twice with already-divergent guards).

**The claims failed in the never-analyzed adjacencies:**
- core runtime/session.rs: ~180 lines of zero-caller v1-era code (dead
  SessionCommand variants ExecDdl/ViewExists/CacheViewSchema + the LIMIT-0
  schema-probe path SchemaOf/schema_of/find_trailing_limit).
- core error.rs: five never-constructed ThunderduckError variants.
- runtime/config.rs: StreamingConfig threaded end-to-end but inert
  (from_env has zero callers; spawn takes `_config`).
- connect-server: stamp_batch_schemas/stamp_one production-dead + stale doc
  refs; classify_plan constant-returns Query (fake DDL dispatch, ADR-checked
  during implementation); StreamingState.pending_error never written;
  record_batches_to_arrow_batches serves one single-batch caller (with a
  production .expect); infallible-but-Result-typed converters incl. a
  misleading `if let Ok` fallback that can never fall through; dead dashmap
  dependency.

Rejected candidates recorded: toDF/WithColumnsRenamed merge (positional vs
name-map — diverges on duplicate names); ADR-anchored #[allow(dead_code)]
retention (render_tail, spark_aggregate_return_cast, extension_targets —
policy, not cleanup); StreamingConfig full de-threading (cross-crate churn,
deferred; minimal from_env deletion only).

Doc drift flagged (not auto-fixed, for user review): CLAUDE.md's crate map
lists `crates/connect-server/src/session/` — that module no longer exists
(SessionManager lives in thunderduck_core::runtime).

### Pass 4 implementation results

Core (−228 exact in runtime/error/config + transpiler items):
- session.rs +3/−201: dead SessionCommand surface (ExecDdl/ViewExists/
  CacheViewSchema) and the LIMIT-0 schema-probe path deleted; the one
  #[ignore]d integration test repointed, not deleted.
- error.rs −15: five never-constructed v1-era variants deleted.
- config.rs −7: from_env deleted; StreamingConfig documented as inert
  (full de-threading deferred — cross-crate churn).
- WithColumns slot-matching single-homed: WithColumnsPlan/with_columns_plan
  in analyzer.rs (na_fill_value_for style), consumed by both
  analyze_with_columns and render_with_columns — wire-schema drift hazard
  closed, byte-identical by construction.
- render_aggregate's dead nth_value arm deleted (classifier=false makes it
  unreachable); trailing-ignoreNulls trim arity table single-homed with the
  two paths' divergent guards PRESERVED and documented ("do NOT unify
  without a corpus witness").
- Stale docs fixed in transpiler_v2/mod.rs and ast.rs (ops described as
  unwired/punted that are implemented).

Connect-server (−109):
- classify_plan/execute_ddl/PlanKind fake dispatch deleted — ADR evidence:
  zero mentions in the rearchitect ADRs; ADR-011/ADR-004 route command
  discrimination at the parse root (the live handle_command), not by
  post-conversion classification.
- stamp_batch_schemas/stamp_one deleted (test-local helper keeps the 7
  rewrite-path tests' coverage); stale service.rs doc refs fixed.
- StreamingState.pending_error (never written) deleted with its dead branch.
- record_batch_to_arrow_batch single-batch API (removes a production
  .expect()).
- proto_to_data_type/proto_struct_to_struct_type/parse_json_schema made
  infallible; convert_cast's never-run map_err gone; the misleading
  `if let Ok` JSON-fallback expressed honestly (behavior re-derived and
  preserved exactly: a `{`-leading schema never reaches the DDL parser).
- dashmap dead dependency removed.

Verification: fmt clean; core 699 / cs 93; DataFrame corpus **327/327**;
SQL corpus **237/25** — baseline preserved through all four passes. Diff
snapshot: /tmp/simplify-pass4-full.patch.

---

## Pass 5 — final dry-check + last removals

The dry-check verified pass 4 left no orphans (compiler-verified, plus
caller-graph greps for every deleted symbol), read the last never-audited
corners (session_manager.rs, extension_loader.rs, runtime/mod.rs, lib.rs,
main.rs, connect-server error.rs), and found exactly two remaining items
above the bar — both implemented:
- F1 session.rs: the `spark_names` rename machinery in the streaming path
  was provably dead (sole caller passes None; column naming moved to
  connect-server's build_stamped_schema in the streaming rewrite). −49.
- F2 connect-server error.rs: three zero-constructor ConnectError variants
  (PlanConversion, Unsupported, Session) — the unfinished mirror of pass 4's
  core ThunderduckError purge. −12.
Plus an 8-item polish batch (stale comments referencing deleted v1 symbols
and pre-rewrite architecture; a misattached doc block; all verified against
the live emission arms).

Verification: fmt clean; zero cargo warnings workspace-wide; core 699 /
cs 93; DataFrame corpus **327/327**; SQL corpus **237/25** — baseline
preserved. Snapshot: /tmp/simplify-final.patch.

---

## CONVERGENCE DECLARATION (effort complete after 5 passes of 20 allowed)

**Stopping determination:** pass 4's adversarial audits declared the
transpiler proper converged (parser_v2: nothing above bar; expression/
type_inference: nothing above bar; transpiler_v2 core: two items, fixed);
pass 5 verified pass 4's own changes left no residue, audited the last
corners, found only F1/F2 (fixed above), and concluded "a pass 6 would not
pay for itself." No new meaningful improvements remain — remaining
compression would trade per-arm Spark-parity legibility (corpus-witnessed
rules with load-bearing comments) for line count, which the constraints
forbid.

**Totals (working tree vs HEAD fb274d1):** 37 files, 5,436 insertions /
10,074 deletions — net **≈ −4,600 lines** (code-only ≈ −4,300 excluding
this log and progress-row files), with every gate identical to baseline:
- `cargo fmt --check` clean; `cargo check --workspace --all-targets` zero
  warnings; no NEW clippy findings on any touched file (workspace clippy
  has pre-existing baseline issues excluded from the gate per CLAUDE.md).
- Unit tests: core 699 passed (was 685 — net +14 from new spark_ddl/
  converter tests minus 2 deliberately deleted with their dead subjects),
  connect-server 93 (was 91: +3 new, −1 make-work).
- DataFrame corpus 327/327 and SQL corpus 237 passed / 25 failed —
  **identical to baseline after every single pass**.

**What structurally changed:** traversal machinery single-homed
(Expression::children/children_mut/map_children + CommonOp::children — six
hand walkers and both MAINTENANCE CONTRACT hazards eliminated); aggregate
metadata in ONE spec table (was 5 rosters + cross-file copy); function
return typing single-homed in type_inference; ONE Spark DDL type parser
(was 2 grammars + a mapping table, with a dead fallback made live); emission
arity/join/separator/interval/set-op boilerplate collapsed into shared
helpers with byte-identical output; analyzer↔emission shared rules
(na.fill selection, WithColumns slot plan, grouping-fold, join-alias
consts) made structural instead of comment-enforced; ~600 lines of v1-era
dead code purged from runtime/session, error enums, and service.rs; four
test modules consolidated (~2,900 lines) with byte-identical test-name
lists.

**Left deliberately (recorded above, needs decisions/witnesses):** the six
correctness flags; ADR-anchored dead-code trio; StreamingConfig full
de-threading; CLAUDE.md crate-map drift (connect-server session/ no longer
exists); `parse_spark_type_strict` kept as documented strict entry point.

**Nothing committed** — the entire effort awaits user review per the git
workflow rule. Per-pass diff snapshots: /tmp/simplify-pass{1,2,3,4}-full.patch,
/tmp/simplify-final.patch (cumulative).
