# Slice B Scope — τ analyzer (typing, nullability, set-op widening, outer-join nullability)

## §Targets

**Corpus case IDs expected to turn green on `core_v2` at Slice B termination** (~15-25 cases, empirically determined):

Analyzer-only-passing (schema_only / nondeterministic cluster) + type-derivation anchors:

- `type-001`..`type-022` (22 cases — schema derivation, type coercion, nullability)
- `cond-003`..`cond-011` (9 cases — CASE WHEN / COALESCE / IF nullability)
- `agg-013` (percentile_approx return-type)
- `agg-018`..`agg-020` (aggregate schema anchors)
- `chain-004` (schema-only chain)
- Plus schema-only / nondeterministic cluster cases (~15 across misc / cast / expr categories)

**Baseline:** commit `d826fe0` (0/324 core_v2 post-A.3). **Target:** ~15-25/324.

Note: corpus case turnover requires τ's `generate()` to produce SQL — but Slice B alone doesn't wire emission. Cases that pass at B are those the differential harness validates via AnalyzePlan schema-diff only (not full result-diff). If empirical N is below 15, that indicates the harness requires full execution and B alone can't move it — that's expected and a HALT-AND-FLAG surface for the /goal driver.

## §ADRs

- **ADR-005** (τ owns Spark type/nullability inference, symmetric-omission discipline)
- **ADR-006** (bounded coordinated passes — resolve → assign_types → derive_nullability, plus set-op widening downward sub-sweep + outer-join nullability derivation)
- **ADR-021** (τ owns substrate — analyzer is τ's, not delegating to legacy `crate::types::TypeInferenceEngine`)
- **ADR-022** (two error categories — `AnalyzerError` splits into Spark-emulated vs Thunderduck-boundary)

## §Inheritance-checklist sections

Pass 1 architect MUST cite and reviewer verifies:

- **§1.4** — SparkSQL parser's `is_aggregate_function` classifier includes `count_if`, `try_sum`, `try_avg`, `try_divide`.
- **§5.2** — `plan_has_empty_scan` short-circuit (used by dispatch; verified in A.2 but analyzer must not re-invoke).

## §Sub-slice sketch

Slice B is likely a single pass; architect MAY sub-split at Pass 1 per §CV.7 if analyzer scope proves too large.

Candidate sub-split (if used):
- **B.1** — Analyzer substrate + `resolve` pass. Covers `TypedAst`, `TypedAttr`, `HasSchema` sealed trait, `AnalyzerError` (both categories), five input-relation fixtures, `resolve()` bottom-up structural pass, INV5 walker (`has_resolved_schema`).
- **B.2** — `assign_types` + `derive_nullability` + set-op widening sub-sweep + outer-join nullability. `inference_smoke()` INV4 activation. Wired end-to-end with `resolve`.

Non-sub-split path: one pass covering everything.

## §Non-goals (slice-specific)

- **No emission arms.** `dispatch_op`, `render_project`, `render_filter`, etc. — Slice C.
- **No extension dispatch.** ext6 arms, `extension_targets()`, INV6 — Slice D.
- **No join emitter / set-op CAST wrapper.** Slice E (Slice B computes widened schema; Slice E applies per-column CAST at emission time per ADR-006 refinement).
- **No complex-type analysis edge cases.** Slice F.
- **No SQL desugarings** (GROUPING SETS, PIVOT, LATERAL VIEW) — Slice G populates `rewrites.rs` (Slice C.1 creates empty).
- **No write-side analyzer** (store-assignment casts) — Slice H.
- **No modifications outside `crates/core/src/transpiler_v2/analyzer.rs` + `analyzer_fixtures.rs`** (plus `mod.rs` re-export of the analyzer entry point). Legacy modules stay untouched (Slice K owns deletion).

## §Success criteria beyond §Targets

Reviewer verifies at Slice B termination:

1. **INV4 grep passes** — `inference_smoke()` iterates `analyzer_fixtures.rs` fixtures and asserts per-field schema equivalence against expected typed forms.
2. **INV5 grep passes** — `has_resolved_schema(&TypedAst)` walks the TypedAst tree and asserts no `DataType::Unresolved` remains after `assign_types`.
3. **INV10 still passes** — analyzer imports only from τ (no `crate::types::TypeInferenceEngine`, no `crate::logical`, no `crate::expression`).
4. **`AnalyzerError` variants split correctly** per ADR-022:
   - Spark-emulated: `UnknownTable`, `UnknownColumn`, `AmbiguousColumn`, `TypeMismatch`, `Other` — client sees the same errors as reference Spark.
   - Thunderduck-boundary: `PuntedOperator`, `UnsupportedRule` — client sees "not implemented in Thunderduck."
5. **Set-op widening covers UNION, INTERSECT, EXCEPT** per Open Decision 5 (UNION BY NAME deferred to Slice G).
6. **Outer-join nullability derivation** — LEFT OUTER makes right-side columns nullable in output; RIGHT OUTER mirror; FULL OUTER both sides nullable.
7. **SparkSQL `is_aggregate_function`** includes `count_if`, `try_sum`, `try_avg`, `try_divide` (checklist §1.4).
8. **Symmetric-omission discipline** preserved — analyzer's function-name enumerations stay in sync with `TypeInferenceEngine::AGGREGATE_NAMES` (promoted to `pub(crate)` at A.2).
9. **Analyzer is τ-native.** No import of `crate::types::TypeInferenceEngine`; use `crate::transpiler_v2::type_inference::TypeInferenceEngine`.
10. **`generate()` signature refined** to invoke the analyzer before emission stub. `generate()` still returns `Err(EmissionError::UnsupportedOp)` at Slice B — analyzer produces `TypedAst` but emission (Slice C) hasn't landed. But if `analyze()` fails with a Spark-emulated error, that surfaces first (before `UnsupportedOp`).
11. **Quality Gate green** each pass.
12. **`tests/scripts/v2-progress.sh` reports N/324** where 0 ≤ N ≤ ~25 (empirical; recorded in iteration log).
