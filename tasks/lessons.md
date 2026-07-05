# Lessons

Update after any user correction, review finding, or workflow-shape observation worth
generalizing. Terse; one bullet per lesson; cite the concrete instance.

> **Note on vocabulary.** Entries below dated 2026-07-01 through 2026-07-02 reference
> a retired phase-based methodology and the retired v1 transpiler; keep them as
> historical evidence but read them for the *lesson*, not the process framing. The
> v1 transpiler was deleted 2026-07-05. Current work is corpus-driven — see
> `tasks/v2-corpus-driven-goal-prompt-template.md`.

---

## Workflow shape

- **Substrate-only efforts are the right shape when the runway to the next unlock is long.**
  The v2 analyzer (2026-07-01) landed as substrate with an honest zero-delta on the
  differential progress signal (`v2_progress.md` stayed 12/324). Fighting for a fake `+N`
  before the emission-dispatch was wired would have forced adapter shortcuts. The `/new-feature`
  pipeline handled this cleanly — the pipeline's summary just reported "0 corpus cases;
  substrate for +5..+15 once emission lands." Do not force differential movement out of an
  effort whose ADRs (here ADR-005, ADR-006) explicitly own only typing, not emission.

- **Half-declarative is worse than fully-hand-written or fully-interpreted.** In the first
  emission iteration (2026-07-01), the coder built `EmissionRow` / `Template` / `SlotKind` /
  `EMISSION_TABLE` as declarative data with no interpreter — the actual emission was still
  hand-written `render_*` helpers, and the table was dead code. The reviewer flagged this as
  Critical (C1). Iteration 2 closed it by **deleting the scaffolding** rather than adding an
  interpreter. Rule: don't ship declarative data whose only reader is a `#[test]`. Either
  hand-write until you have real clients, or write the interpreter in the same pass. A later
  emission pass lands the declarative table when it has per-function rows that need it.

- **The up-front audit is worth the cost.** The extension-wiring architect's §0 (2026-07-01)
  audited the actual `emission.rs` state before proposing edits and found that `md5`,
  `sha1`/`sha2`, and the full stddev/variance family were already wired by an earlier emission
  pass — the initial prompt listed them as "wiring to add." The audit collapsed the
  planned edit surface from ~14 arms to 8 and saved multiple Pass-1 iterations that would
  have discovered the overlap during coder work (each `render_function_call` arm collision
  is a fresh compile error + re-review cycle). Rule: for any effort that touches a
  large-surface file already grown by a previous effort, spend a plan §0 pass diff'ing the
  actual substrate against the prompt's assumptions before enumerating deliverables.

- **Approach A is the honest choice when the interpreter is trivial.** The emission architect
  (2026-07-01) chose Approach A (hand-written per-variant / per-function `match` arms) over
  Approach B (declarative row substrate + interpreter) precisely because the ~50 non-trivial
  function shapes are 3-to-5-line `format!` strings — not enough interpreter substrate to
  justify a table. This is the *inverse* of the Pass-1 dead-data lesson at a different scale:
  once the substrate is real (the extension work adds `spark_*` extension rows, later τ work
  adds ~30 complex-type functions), the interpreter becomes non-trivial and the declarative form pays
  off. Rule: pick the shape that matches the interpreter's own weight — trivial interpreter
  → hand-written `match`; non-trivial interpreter → declarative rows.

## ADR-015 discipline (differential oracle > plan document)

- **The legacy `TypeInferenceEngine` is the oracle for Spark-parity coercion, not the plan.**
  In the analyzer work, `smoke_type_019` (`type-019`: `Decimal(5,0)` unionByName `Decimal(10,2)`) had a
  plan-document expected value of `Decimal(11,2)`. The coder ran the legacy
  `unify_decimal(5,0,10,2)` and got `Decimal(10,2)` — precision =
  `min(max(5-0, 10-2) + max(0,2), 38) = 10`. Fixture updated to match the oracle, not the plan.
  This is the ADR-015 pattern working as designed: LLM-extracted rules stay untrusted until
  the oracle validates. Reuse verbatim; do not re-derive.

## Design tactics learned in the emission work

- **Legacy-verbatim shape hard-copying is the honest cost of a contamination barrier.**
  The emission work (2026-07-01) needed the shape of ~50 Spark scalar functions inside `emission.rs`
  but INV3 forbids importing `crate::functions::FunctionRegistry`. The architect considered a
  narrow accessor (Approach C in ADR-009's refinement) and rejected it: duplication of a
  bounded, stable surface (~50 function shapes) is honest cost, whereas an accessor becomes a
  permanent second-place-to-update. Rule: when a load-bearing contamination barrier (INV3-class)
  forbids importing a substrate, prefer copying the substrate's shape over building a narrower
  accessor — unless the copied surface would grow unbounded or the accessor's maintenance
  surface would demonstrably be smaller.

- **The `spark_return_cast` / `spark_aggregate_return_cast` separation prevents double-cast.**
  In the emission work, projection-level and aggregate-level return-type parity live in different
  renderers (`render_projection_slot` vs. `render_aggregate`). Sharing a single
  `spark_return_cast` helper across both would double-cast aggregate output. Rule: the CAST
  that pins Spark's return type belongs at exactly one call site per emission decision. If a
  helper needs to cover multiple emission contexts, split it by context; do not chain.

- **Silently absorbed performance wins are a real refactoring pattern.** The emission work's
  `SqlGenerator::gen_expr` seam drain (an architectural change) eliminated the earlier emission
  iteration's per-expression `SqlGenerator::new()` allocation (Pass-1 perf finding OPT-M2 and OPT-L1)
  without a labeled perf change. When a refactor subsumes a perf concern, the perf backlog is
  drained *by* the refactor — no separate perf commit needed. But it should be *named* in the
  Pass N summary so the perf-agent's audit trail stays coherent. Rule: if you're about to
  restructure code the perf backlog references, close the backlog items in your summary as
  "subsumed by <refactor>" rather than letting them go silent.

- **One allowed cross-module edit per effort, flagged and documented.** The first emission iteration needed
  `pub fn with_schema_for_v2` in the legacy `generator/mod.rs` to make the analyzer-side schema
  threadable into the emission-time renderer. The plan's §Scope allowed exactly one such
  cross-module edit; the coder's implementation log flagged it as a deviation; the reviewer
  verified it was minimal and non-behavioral. Rule: cross-module edits outside the effort's
  designated files are permitted only if (a) the plan sanctions them explicitly, (b) the coder
  discloses them as a named deviation, and (c) the reviewer verifies both scope and
  minimality. Silent cross-module edits are a Critical review finding.

## Bug-fix diagnostics

- **Diagnostician-overturned bug reports are a strong signal — trust the
  falsification.** The div-routing bug-fix (2026-07-01) was scoped by the initial prompt as a
  Div-routing bug inside `emission.rs::render_binary` / `render_spark_decimal_div`
  (v2 substrate). The diagnostician's multi-hypothesis pass proved the v2 path
  was byte-correct and traced the failure two crates over to
  `crates/connect-server/src/converter/relation_converter.rs:2513` — a
  silent-NULL catch-all in `local_relation_to_values_sql::val()`. Confirming
  evidence: `type-005` also failed under `THUNDERDUCK_TRANSPILER=legacy` with
  identical symptoms, proving the bug lived upstream of transpiler selection.
  Rule: when a diagnostician says the initial-prompt scope is wrong, don't
  argue — follow the falsification. `/fix-bug`'s diagnostic-first pipeline
  shape is specifically designed to surface this kind of scope overturn before
  the coder starts; `/new-feature`'s architect-first shape assumes the scope
  is roughly right, which is safe only when there's no bug to reproduce yet.

- **Symmetric-omission bug pattern — audit for `count_if` whenever the count family is enumerated.**
  A 2026-07-01 fix closed `agg-020`/`agg2-006` by adding `count_if` in
  **two** independent files that both enumerated the count family and both
  omitted it: `TypeInferenceEngine::aggregate_return_type` at
  `types/type_inference.rs:326` (arg-type fall-through returned Boolean
  instead of Long) and `Expression::FunctionCall::nullable` at
  `expression/mod.rs:1051` (default `_` arm marked it nullable). The
  scalar-context helper at `type_inference.rs:797` already handled
  `count_if => Long` correctly — so the omission was site-local, not
  design-wide. The Java reference has the same latent gap. Rule: when a
  region enumerates the count family (`count`, `count_distinct`,
  `grouping`, `grouping_id`), always audit for `count_if` inclusion; the
  omission travels together across sibling code paths.

- **Corpus-first reading beats prompt speculation.** The `count_if` fix's initial
  /fix-bug prompt speculated the `salary > 90000` predicate inside
  `count_if` was being routed as Decimal. Reading the corpus fixtures
  verbatim (`agg-020` uses `F.count_if(F.col("active"))` — argument is a
  Boolean *column*; `agg2-006` uses `count_if(salary > 90000)` where the
  comparison result is Boolean, not Decimal) immediately narrowed the
  hypothesis space to "return-type of `count_if(Boolean)` is wrong."
  Rule: whenever a bug report speculates about the input shape, read the
  corpus fixture verbatim before enumerating hypotheses — cheaper than
  probing the runtime.

- **The diagnostician's "rerun first" preflight can collapse a `/fix-bug` pipeline
  to a verify-only pass.** A 2026-07-01 fix was scoped as a `sum(decimal)`
  routing fix in `emission.rs::render_aggregate` / `spark_aggregate_rewrite`. The
  diagnostician ran the reproducer (`agg-007` under `THUNDERDUCK_TRANSPILER=v2`)
  twice before staging any hypotheses — both PASSED deterministically. The bug
  report predated the silent-NULL landing that unmasked `Decimal128` `LocalRelation`
  payloads; combined with the extension work's existing `spark_aggregate_rewrite`
  helper, the case was already GREEN. The pipeline landed as verify-only: two
  regression tests locking in the routing invariant, no production code change.
  Same shape as the silent-NULL scope-overturn (diagnostician-first pipelines catch
  stale-scope bugs), but from a different angle: here the bug report itself
  had already been closed by subsequent landings. Rule: when a `/fix-bug`
  prompt was drafted before recent landings, the diagnostician's Phase-1
  reproducer step doubles as a preflight; if the reproducer is green, the
  pipeline correctly collapses to a test-lock-in verify-only pass — do not
  fabricate a fix for a bug that no longer reproduces.

- **Dormant v2 fixes are legitimate outcomes when the runtime routes through legacy
  for a corpus case that the extension/execution work has not yet wired.** A 2026-07-01 fix
  landed the correct sha arg-strip fix in `crates/core/src/transpiler_v2/emission.rs`
  plus a regression test, but the target case `hash-002` stayed RED because the
  runtime path for `spark.createDataFrame(...)` DataFrames routes through the
  legacy `SqlRelation` fallback (`AnalyzerError::PuntedOperator` — raw-SQL
  sub-relations are out of the analyzer's common-AST surface), and legacy's
  `FunctionRegistry` maps `sha2 → SHA256` name-only and has the same bug. Non-goals
  forbid touching legacy `FunctionRegistry`. Two shapes were available: (a)
  silently rewrite legacy to satisfy the corpus, or (b) land the v2 fix + test
  and halt-and-flag the corpus case as blocked on future extension/execution `SqlRelation`
  handling. The coder picked (b). Complements the silent-NULL scope-overturn lesson
  from a different angle: there the fix belonged upstream of the v2 substrate;
  here the fix belongs on the v2 substrate but the runtime hasn't yet reached
  it. Rule: when a v2 fix is correct but the corpus case runs through legacy
  fallback for a plan shape the current effort doesn't own, land the v2 fix +
  regression test as dormant; halt-and-flag the corpus case with an explicit
  "blocked on future τ work" note. The regression test locks the fix in for
  the moment the routing changes, and no non-goal edits sneak in.

- **Silent-NULL catch-alls in typed dispatch are data-corruption anti-patterns.**
  The silent-NULL root cause was a single-line `_ => Ok("NULL".to_string())`
  at `relation_converter.rs:2513`. It silently mapped every unhandled Arrow
  type — including `Decimal128`, `Decimal256`, `Interval*`, `Duration*`,
  `Binary`, etc. — to SQL literal `"NULL"`, corrupting every affected column
  in `createDataFrame` payloads while preserving the schema. Replacing it
  with `_ => Err(...)` immediately turned the marshalling gap into a visible
  failure at first use, and the fix delivered +15 corpus cases (134 → 149) —
  far above the diagnostician's minimum +3 prediction from `type-003/004/005`,
  because the halt-and-flag audit had no visibility into the other
  silently-corrupted decimal-payload cases. Rule: in `crates/connect-server/src/converter/`
  (and any encoder translating typed data into a downstream SQL/wire
  representation), no catch-all `Ok` fallbacks for typed dispatch. Every
  unhandled type surfaces as a loud error. Silent NULL substitution is
  worse than a loud panic in every case.

## Progress-signal calibration

- **Per-effort progress-signal estimates are lagging indicators; recalibrate after each effort
  lands.** The analyzer predicted `[+5, +15]` on `v2_progress.md`, actual delta was 0 (the analyzer
  alone can't move differential counts without dispatch). The emission work predicted `12 → 180-200`,
  actual `12 → 134` (the 46-case gap is the honest DEFER carryover to the extension, execution, and
  later τ work). The estimates aren't wrong in principle; they're wrong because the case-ID target
  lists overcounted what the effort alone could unblock without extension functions,
  the full join cluster, complex types, or verticals.
  Corollary: `tests/scripts/v2-progress.sh` is a measurement, not a completion gate. Use it to
  recalibrate the per-effort deltas *after* the effort lands, not to score the
  effort's completion during termination.

## Extension-spec discipline

- **Verify-native-first before drafting extension specs.** The ext5 spec set pre-drafted 10 `spark_*`
  functions; when ext6 shipped (2026-07-01 audit), 7 of them turned out to be unnecessary because
  native DuckDB matches Spark semantics — `TRY_CAST`, `CORR`, `COVAR_SAMP`, `REGR_SLOPE`, `REGR_R2`,
  `KURTOSIS_POP`, `COUNT_IF`. Only 3 of the 10 (`spark_try_divide`, `spark_try_sum`, `spark_try_avg`)
  actually shipped as extension functions because they add real gap-filling value.
  Rule: **before writing a `spark_<fn>` spec, run `SELECT * FROM duckdb_functions() WHERE
  function_name = '<fn>'` in a live DuckDB session** and check whether the native function's
  value + type behavior matches Spark for the corpus test range. Only spec an extension arm when
  the native path demonstrably diverges. This is ADR-010's cast-preferred discipline applied at
  the spec-drafting stage, not just the routing stage — and it prevents 7-of-10 wasted-work rates
  in the sibling C++ project.

- **`extension_targets()` allow-list is not authoritative for what the extension provides.** The
  first extension-Phase-2 `/goal` preflight (2026-07-01) halted on a `grep` of `extension_targets()`
  that returned no Phase-2 `spark_*` names — but the ext6 binary DID register `spark_try_divide`,
  `spark_try_sum`, `spark_try_avg`. The allow-list simply hadn't been extended yet. Rule: for "does
  the extension provide function X" questions, **query `duckdb_functions()` on a live session**,
  not the source-side allow-list. INV6 checks the allow-list ⊆ `duckdb_functions()` direction; it
  does not check the reverse — extensions can register functions the allow-list hasn't caught up
  with. A preflight that checks the allow-list is testing "did *this repo* wire the arm," not
  "does the extension provide the function." Design preflights for what they actually measure.
