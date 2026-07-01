# Slice C.3 — Initial `/new-feature` prompt (pass 1)

Use this file verbatim as the `/new-feature` prompt for pass 1 of Slice C.3 under
the iteration methodology in `tasks/v2-slice-iteration-methodology.md`.

Slice C.3 was created in the Slice D Phase 1 halt-and-flag (2026-07-01,
`tasks/v2-slice-d-iteration-log.md` §"Phase 1 termination"). It closes the
Slice C.2 latent bugs that the Slice D up-front audit misclassified as
"already-passing native." Landing Slice C.3 unblocks the Slice D Phase 1
target case IDs and enables Slice D Phase 1's formal termination.

---

Close the pre-existing Slice C.2 latent bugs blocking Slice D Phase 1's
target case IDs. **Pragmatism path**: no new C++ extension work; every fix
lands on the Rust side (Slice B analyzer + Slice C.2 emission substrate).
Expected progress signal delta: **134 → ~140-142 core_v2 passing**.

## Design mandate (authoritative)

- `docs/thunderduck-rearchitect-ADRs.md`:
  - **ADR-002** (emit-level delegation + additive-landing fallback semantics
    refinement). Corrections that emit wrong SQL are NOT covered by the
    typed-fallback pattern — they hit DuckDB binder errors that don't
    trigger v2 fallback. Slice C.3 closes those directly.
  - **ADR-005 + ADR-006** — analyzer scope. Slice C.3 makes targeted
    corrections inside the Slice-B analyzer (nullability rules for hash
    functions, aggregate-context type inference for count_if predicate).
    Do NOT expand the analyzer's operator surface — just fix the specific
    inference bugs.
  - **ADR-009 + Approach A/B/C refinement** — Slice C.3's emission-side
    fixes stay hand-written match arms; no declarative substrate.
  - **ADR-014** — deliberate-seam pattern still forbidden; INV3 stays load-
    bearing.
  - **§CV.5 INV6** — already activated by Slice D Phase 1 over the ext4
    subset; do not regress.
- `tasks/v2-adr-readiness-map.md` §Slice C.3 — the new formal slice entry.
- `tasks/duckdb-extension-specs/README.md` — reactive protocol for missing
  functions. Slice C.3 does NOT need to invoke this; the pragmatism path
  explicitly avoids new extension specs.

## Inputs to read

- `.agent-output/archive-*/003-review-findings.md` and Slice D Phase 1
  iteration log for context on what surfaced during halt-and-flag.
- `crates/core/src/transpiler_v2/emission.rs::render_function_call` around
  line 1277 (the `sha`/`sha1`/`sha2` arm).
- `crates/core/src/transpiler_v2/emission.rs::render_binary` and the new
  `render_spark_decimal_div` helper (Slice D Phase 1 addition) — verify the
  Div guard fires when expected.
- `crates/core/src/transpiler_v2/analyzer.rs` — the function-return-type
  inference for hash-family functions. Slice B substrate.
- `crates/core/src/functions/mod.rs` — legacy `FunctionRegistry::translate`
  and `translate_typed`. **Read-only reference** (INV3 forbids importing).
  In particular the arg-stripping logic for sha1/sha2 lives here; Slice C.3
  duplicates its shape into `emission.rs`.
- `tests/integration/differential/dataframe_corpus.py` — the 9 target case
  IDs (see Acceptance below).
- `crates/core/src/generator/mod.rs::gen_expr` — legacy oracle for how
  sha1/sha2 emit today. **Read-only reference.**

## Scope

Six correctness fixes on the Rust side. Every fix must have a regression
test that would have caught the bug before Slice C.3.

### C.3-1: `sha`/`sha1`/`sha2` arg-stripping

- **Location:** `crates/core/src/transpiler_v2/emission.rs:1277`.
- **Current shape:** `"sha" | "sha1" | "sha2" => format!("SHA256({})", joined())`.
- **Bug:** For `sha2("name", 256)` — a Spark call with two args — `joined()`
  produces `"name", 256`, causing v2 to emit `SHA256("name", 256)`. DuckDB
  `SHA256` is single-arg, resulting in `Binder Error: No function matches
  the given name and argument types 'sha256(VARCHAR, INTEGER_LITERAL)'`.
- **Fix:** Change the arm to emit `SHA256(<first arg only>)`, discarding any
  additional args. Mirror the legacy behavior at `functions/mod.rs`'s
  `translate_typed` (which discards args beyond arg 0 for these three
  Spark functions). Sketch:
  ```rust
  "sha" | "sha1" | "sha2" => format!(
      "SHA256({})",
      arg_refs.first().copied().unwrap_or("")
  ),
  ```
- **Regression test:** add a unit test in `emission::tests` that constructs
  a `FunctionCall { name: "sha2", args: vec![col("name"), lit(256)], ... }`,
  dispatches, and asserts the emitted SQL contains `SHA256(` but does NOT
  contain `256`. Would have failed before Slice C.3.
- **Target case ID unblocked:** `hash-002`.

### C.3-2: `hash`/`xxhash64` return nullability

- **Location:** analyzer's function-return-type inference — likely
  `crates/core/src/transpiler_v2/analyzer.rs::assign_types_function_call`
  or wherever the analyzer looks up per-function return types. Grep for
  where the analyzer sets `nullable` on `Expression::FunctionCall` results.
- **Bug:** Spark's `hash(...)` and `xxhash64(...)` return **non-nullable**
  INT / BIGINT (the hash of a NULL row is a defined seed-hashed value, not
  NULL). The v2 analyzer marks them nullable, causing `hash-003`'s schema
  to fail with:
  ```
  Column 'h': nullable mismatch - Reference=False, Test=True
  Column 'xx': nullable mismatch - Reference=False, Test=True
  ```
- **Fix:** Add explicit non-nullable return rules for these two function
  names in the analyzer's return-type inference. Legacy's
  `crate::types::TypeInferenceEngine` may already have this rule — verify;
  if it does, the v2 analyzer just needs to consult it correctly.
- **Regression test:** add a unit test in `analyzer::tests` that constructs
  a `Project([hash("name", "dept_id")])`, analyzes, and asserts the
  projection slot's `TypedAttr.nullable == false`.
- **Target case ID unblocked:** `hash-003`.

### C.3-3: `count_if` aggregate-context Decimal-to-Boolean inference

- **Location:** analyzer's aggregate-return-type or predicate-type
  inference. Grep for how the analyzer handles the boolean predicate
  inside `count_if(cond)`.
- **Bug:** `agg-020` and `agg2-006` fail with:
  ```
  pyarrow.lib.ArrowInvalid: Could not convert Decimal('2')
    with type decimal.Decimal: tried to convert to boolean
  ```
  This suggests the emitted SQL runs correctly on DuckDB, but the fetched
  result column has a Decimal type where a Boolean is expected — a schema
  or return-type-inference bug at the count_if aggregate level.
- **Fix:** Investigate at implementation time. Two plausible root causes:
  (a) the analyzer infers `count_if(col > 90000)` as returning Decimal
  (from the `> 90000` predicate) instead of Long; (b) an emission-side
  cast wrapper misfires. Diagnostic: build a mini fixture, run through
  v2, inspect the emitted SQL AND the resulting Arrow schema. Fix at the
  layer where the divergence surfaces.
- **Regression test:** unit test asserting `count_if(F.col("salary") > 90000)`
  has `TypedAttr.data_type == Long, nullable == false`.
- **Target case IDs unblocked:** `agg-020`, `agg2-006`.

### C.3-4: `Binary(Div, Decimal, Decimal)` routing verification

- **Location:** `emission.rs::render_binary` (Slice D Phase 1 addition;
  the `render_spark_decimal_div` guard).
- **Bug hypothesis:** `type-005` and Div-in-`chain-*` fail on `core_v2`
  despite Slice D Phase 1 having added the `render_spark_decimal_div`
  branch. Possible causes: (a) guard condition too narrow (matches only
  literal `Decimal(_,_)` but analyzer produces `Decimal { precision, scale }`
  with specific values); (b) type inference upstream not classifying
  operands as Decimal; (c) `TypeInferenceEngine::decimal_div_type` returns
  something the emission's outer `if let DataType::Decimal { .. } = ...`
  drops as `Ok(None)` (see reviewer M4).
- **Fix:** Trace one failing case (`type-005`) end-to-end: what does the
  analyzer say the operand types are? What does `render_binary` receive?
  Does `render_spark_decimal_div` return `Ok(Some(...))` or `Ok(None)`?
  Root-cause and fix at the correct layer. This is a diagnosis task first,
  fix second.
- **Regression test:** unit test in `emission::tests` that constructs a
  `Binary(Div, ColumnReference(a: Decimal(5,2)), ColumnReference(b: Decimal(3,1)))`,
  dispatches, and asserts the emitted SQL contains `spark_decimal_div(`.
- **Target case IDs unblocked:** `type-005`, `math-011` (indirect — same
  root cause if the int/int → DOUBLE cast isn't firing).

### C.3-5: `sum(decimal)` routing verification

- **Location:** `emission.rs::render_aggregate` + `spark_aggregate_rewrite`
  (Slice D Phase 1 addition).
- **Bug hypothesis:** `agg-007` fails despite Phase 1 having wired
  `spark_aggregate_rewrite` for DECIMAL SUM/AVG. Possible causes similar
  to C.3-4: guard too narrow, analyzer's arg-type inference wrong, or
  return-cast formula miscomputed.
- **Fix:** Same diagnostic + fix pattern as C.3-4. Trace `agg-007` through.
- **Regression test:** unit test asserting `SUM(Decimal(9,2))` emits
  `spark_sum(...)` wrapped in the correct outer CAST.
- **Target case ID unblocked:** `agg-007`.

### C.3-6: `percentile_approx` / `median` shape verification

- **Location:** `emission.rs::render_function_call` (Slice D Phase 1
  additions at ~lines 1300-ish).
- **Bug hypothesis:** `agg-013` fails despite Phase 1 wiring
  `approx_quantile` (per legacy oracle) and `MEDIAN`. Possible: arg-order
  bug in `percentile_approx` (Spark: `percentile_approx(col, percentage)`;
  DuckDB `approx_quantile`: `approx_quantile(x, pos)` — check the arg
  order matches).
- **Fix:** Verify against legacy `functions/mod.rs:460-465`. If arg order
  or arity differs, correct.
- **Regression test:** unit test asserting the emitted SQL matches legacy's
  output byte-for-byte on a fixed input.
- **Target case ID unblocked:** `agg-013`.

## Acceptance

- Case IDs green on `core_v2` (via `tests/scripts/v2-progress.sh`):
  `hash-002`, `hash-003`, `agg-020`, `agg2-006`, `type-005`, `math-011`,
  `agg-007`, `agg-013` (8 direct unblocks). `hash-001` MAY unblock if
  the sha1→SHA256 approximation is within differential tolerance;
  otherwise deferred to a hypothetical future `Slice K — Hash-family
  Spark parity` (spec files not drafted this pass; pragmatism path).
- **Progress signal target: 134 → ~140-142** on `core_v2` after Slice C.3
  lands. Matches the original Slice D Phase 1 estimate — delivered by
  C.3 rather than D.
- Quality gate (per `CLAUDE.md` §Quality Gate) green on all passes.
- Legacy TPC-H differential 51/51 remains unregressed.
- INV6 remains activated (Slice D Phase 1's 6-entry `extension_targets()`
  allow-list unchanged).
- INV3 remains load-bearing (no new legacy imports in `emission.rs`).
- **After Slice C.3 lands, update `tasks/v2-adr-readiness-map.md` §Slice D
  Phase 1** to mark "landed" (target case IDs now pass). Slice D as a
  whole still awaits Phase 2.

## Out of scope

- New C++ extension work. `spark_sha1` / `spark_sha2` (proper Spark
  parity for the hash-001 approximation gap) are explicitly deferred to
  a hypothetical `Slice K` if that path is ever chosen. Pragmatism path
  keeps the sha1→SHA256 legacy-parity approximation.
- Any Slice D Phase 2 work (ext5-blocked).
- Analyzer expansion beyond the specific inference bugs C.3-2 and C.3-3
  target.
- New operator surface in `CommonAst`.
- Extension-target additions to `extension_targets()`.

## Non-goals — do NOT do any of these

- Do NOT introduce new `spark_*` extension specs.
- Do NOT reintroduce `use crate::generator::SqlGenerator` or
  `use crate::functions::FunctionRegistry` in `emission.rs`. INV3 stays
  load-bearing.
- Do NOT modify legacy `SqlGenerator`, `FunctionRegistry`, or
  `TypeInferenceEngine` bodies (the last one is the analyzer oracle; the
  fixes for C.3-2 and C.3-3 belong on the v2 analyzer's side, not in the
  legacy engine).
- Do NOT expand `CommonAst` operators or `Expression` variants.
- Do NOT regress any Slice C.2 / Slice D Phase 1 test that currently
  passes.
- Do NOT run the differential suite between passes; only at Phase-1-of-
  Slice-C.3 termination.
- Do NOT commit if any Quality Gate step fails.

## Note to the architect

Slice C.3 is small in volume but investigative in nature: at least three
of the six fixes (C.3-3, C.3-4, C.3-5) need Pass-1 diagnostics before the
fix can be scoped. Budget the architect stage for diagnosis; the coder
stage should be short once the root causes are named. Every fix should
be paired with a unit-test regression case that would have caught the
bug empirically, so the halt-and-flag-audit-miss pattern (wired but
broken) can't recur.

If Pass 1 reveals that any of the six fixes requires **more than a
localized change** (e.g., C.3-3 needs a new analyzer pass), consider
proposing a sub-split per §CV.7. But the honest expectation is that
all six fits one pass.
