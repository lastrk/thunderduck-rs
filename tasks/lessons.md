# Lessons

Update after any user correction, review finding, or workflow-shape observation worth
generalizing. Terse; one bullet per lesson; cite the concrete instance.

---

## Workflow shape

- **Substrate-only slices are the right shape when the runway to the next unlock is long.**
  Slice B (v2 analyzer, 2026-07-01) landed as substrate with an honest zero-delta on the
  differential progress signal (`v2_progress.md` stayed 12/324). Fighting for a fake `+N`
  before Slice C's dispatch was wired would have forced adapter shortcuts. The `/new-feature`
  pipeline handled this cleanly — the pipeline's summary just reported "0 corpus cases;
  substrate for +5..+15 once Slice C lands." Do not force differential movement out of a
  slice whose ADRs (here ADR-005, ADR-006) explicitly own only typing, not emission.

- **Half-declarative is worse than fully-hand-written or fully-interpreted.** In Slice C.1
  iteration 1 (2026-07-01), the coder built `EmissionRow` / `Template` / `SlotKind` /
  `EMISSION_TABLE` as declarative data with no interpreter — the actual emission was still
  hand-written `render_*` helpers, and the table was dead code. The reviewer flagged this as
  Critical (C1). Iteration 2 closed it by **deleting the scaffolding** rather than adding an
  interpreter. Rule: don't ship declarative data whose only reader is a `#[test]`. Either
  hand-write until you have real clients, or write the interpreter in the same pass. Slice
  C.2 lands the declarative table when it has per-function rows that need it.

- **The up-front audit is worth the cost.** Slice D Phase 1's architect §0 (2026-07-01)
  audited the actual `emission.rs` state before proposing edits and found that `md5`,
  `sha1`/`sha2`, and the full stddev/variance family were already wired in Slice C.2 — the
  readiness map and initial prompt listed them as "wiring to add." The audit collapsed the
  planned edit surface from ~14 arms to 8 and saved multiple Pass-1 iterations that would
  have discovered the overlap during coder work (each `render_function_call` arm collision
  is a fresh compile error + re-review cycle). Rule: for any slice that touches a
  large-surface file already grown by a previous slice, spend a plan §0 pass diff'ing the
  actual substrate against the prompt's assumptions before enumerating deliverables.

- **Approach A is the honest choice when the interpreter is trivial.** Slice C.2's architect
  (2026-07-01) chose Approach A (hand-written per-variant / per-function `match` arms) over
  Approach B (declarative row substrate + interpreter) precisely because the ~50 non-trivial
  function shapes are 3-to-5-line `format!` strings — not enough interpreter substrate to
  justify a table. This is the *inverse* of the Pass-1 dead-data lesson at a different scale:
  once the substrate is real (Slice D adds `spark_*` extension rows, Slice F adds ~30
  complex-type functions), the interpreter becomes non-trivial and the declarative form pays
  off. Rule: pick the shape that matches the interpreter's own weight — trivial interpreter
  → hand-written `match`; non-trivial interpreter → declarative rows.

## ADR-015 discipline (differential oracle > plan document)

- **The legacy `TypeInferenceEngine` is the oracle for Spark-parity coercion, not the plan.**
  In Slice B, `smoke_type_019` (`type-019`: `Decimal(5,0)` unionByName `Decimal(10,2)`) had a
  plan-document expected value of `Decimal(11,2)`. The coder ran the legacy
  `unify_decimal(5,0,10,2)` and got `Decimal(10,2)` — precision =
  `min(max(5-0, 10-2) + max(0,2), 38) = 10`. Fixture updated to match the oracle, not the plan.
  This is the ADR-015 pattern working as designed: LLM-extracted rules stay untrusted until
  the oracle validates. Reuse verbatim; do not re-derive.

## Design tactics learned in Slice C

- **Legacy-verbatim shape hard-copying is the honest cost of a contamination barrier.**
  Slice C.2 (2026-07-01) needed the shape of ~50 Spark scalar functions inside `emission.rs`
  but INV3 forbids importing `crate::functions::FunctionRegistry`. The architect considered a
  narrow accessor (Approach C in ADR-009's refinement) and rejected it: duplication of a
  bounded, stable surface (~50 function shapes) is honest cost, whereas an accessor becomes a
  permanent second-place-to-update. Rule: when a load-bearing contamination barrier (INV3-class)
  forbids importing a substrate, prefer copying the substrate's shape over building a narrower
  accessor — unless the copied surface would grow unbounded or the accessor's maintenance
  surface would demonstrably be smaller.

- **The `spark_return_cast` / `spark_aggregate_return_cast` separation prevents double-cast.**
  In Slice C.2, projection-level and aggregate-level return-type parity live in different
  renderers (`render_projection_slot` vs. `render_aggregate`). Sharing a single
  `spark_return_cast` helper across both would double-cast aggregate output. Rule: the CAST
  that pins Spark's return type belongs at exactly one call site per emission decision. If a
  helper needs to cover multiple emission contexts, split it by context; do not chain.

- **Silently absorbed performance wins are a real refactoring pattern.** Slice C.2's
  `SqlGenerator::gen_expr` seam drain (an architectural change) eliminated Slice C.1's
  per-expression `SqlGenerator::new()` allocation (Pass-1 perf finding OPT-M2 and OPT-L1)
  without a labeled perf change. When a refactor subsumes a perf concern, the perf backlog is
  drained *by* the refactor — no separate perf commit needed. But it should be *named* in the
  Pass N summary so the perf-agent's audit trail stays coherent. Rule: if you're about to
  restructure code the perf backlog references, close the backlog items in your summary as
  "subsumed by <refactor>" rather than letting them go silent.

- **One allowed cross-module edit per slice, flagged and documented.** Slice C.1 needed
  `pub fn with_schema_for_v2` in the legacy `generator/mod.rs` to make the analyzer-side schema
  threadable into the emission-time renderer. The plan's §Scope allowed exactly one such
  cross-module edit; the coder's implementation log flagged it as a deviation; the reviewer
  verified it was minimal and non-behavioral. Rule: cross-module edits outside the slice's
  designated files are permitted only if (a) the plan sanctions them explicitly, (b) the coder
  discloses them as a named deviation, and (c) the reviewer verifies both scope and
  minimality. Silent cross-module edits are a Critical review finding.

## Progress-signal calibration

- **Per-slice progress-signal estimates are lagging indicators; recalibrate after each slice
  lands.** Slice B predicted `[+5, +15]` on `v2_progress.md`, actual delta was 0 (the analyzer
  alone can't move differential counts without dispatch). Slice C predicted `12 → 180-200`,
  actual `12 → 134` (the 46-case gap is the honest DEFER carryover to Slices D/E/F/G). The
  estimates aren't wrong in principle; they're wrong because the readiness-map case-ID target
  lists overcounted what the slice alone could unblock without extension functions (Slice D),
  the full join cluster (Slice E), complex types (Slice F), or verticals (Slice G).
  Corollary: `tests/scripts/v2-progress.sh` is a measurement, not a completion gate. Use it to
  recalibrate the readiness map's per-slice deltas *after* the slice lands, not to score the
  slice's completion during termination.
