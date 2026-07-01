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

### C.3-1: `sha`/`sha1`/`sha2` arg-stripping — **LANDED DORMANT 2026-07-01**

**Status:** v2 fix + regression test committed; `hash-002` does **NOT** turn
green because the runtime path routes through the legacy `SqlRelation`
fallback (the `emp` DataFrame built via `spark.createDataFrame(...)` triggers
`AnalyzerError::PuntedOperator` for `SqlRelation`, so v2 punts to legacy),
and legacy's `FunctionRegistry` name-only mapping `sha2 → SHA256` forwards
all args and has the same bug. HALT-AND-FLAG per the coder's
diagnostic-first invariant: the plan's "matches legacy behavior" claim was
factually incorrect (legacy does NOT strip the bit-length), and the fix
belongs on the v2 substrate only. Non-goals forbid touching legacy
`FunctionRegistry`, so `hash-002` stays RED until Slice D/E wires
`SqlRelation` on the v2 common-AST surface. At that point the v2 fix
lights up and the corpus case flips green immediately.

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

### C.3-2: `hash`/`xxhash64` return nullability — **LANDED 2026-07-01**

**Status:** LANDED cleanly; delivered **+1** core_v2 delta (151 → 152)
closing `hash-003`. Fix landed in `crates/core/src/expression/mod.rs`
(single-file edit): extended `FunctionCall::nullable`'s non-nullable
literal list from `"count" | "count_distinct" | "count_if" | "grouping" |
"grouping_id"` to include `"hash" | "murmur3" | "xxhash64"`. `murmur3`
was added alongside `hash` (Spark synonym, same algorithm and
non-nullability; already grouped with `hash` at `type_inference.rs:733`)
to pre-empt a latent bug. `Expression::nullable` is a shared code path
consulted by both v2 and legacy, so the fix closes `hash-003` regardless
of which emission path handles the plan (the corpus case also routes
through the legacy `SqlRelation` fallback per the C.3-1 diagnostic). One
regression test in `expression/mod.rs::tests`:
`hash_and_xxhash64_are_non_nullable_regardless_of_args` — asserts
non-nullability for `hash`, `xxhash64`, and `murmur3` on both single-arg
and multi-arg calls, with a sanity anchor confirming the source args ARE
nullable so the default fall-through arm would have returned true
pre-fix.

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

### C.3-3: `count_if` aggregate-context Decimal-to-Boolean inference — **CLOSED 2026-07-01**

**Status:** LANDED via `/fix-bug` pipeline; delivered **+2** core_v2 delta
(149 → 151) closing `agg-020` and `agg2-006`. Diagnostic report at
`.agent-output/001-diagnostic-report.md`. Two iterations; APPROVED after
iter 2 with 0 Critical + 0 High.

**Scope refinement (retain for future reference).** The initial prompt
speculated on the `salary > 90000` predicate being routed as Decimal.
Corpus-first reading (verified verbatim by the coder) proved otherwise:
`agg-020` at `dataframe_corpus.py:359` uses `F.count_if(F.col("active"))`
— argument is a Boolean *column* — and `agg2-006` at `:655` uses
`F.count_if(F.col("salary") > 90000)` where the argument type is Boolean
(the comparison result), not Decimal. The pyarrow Decimal-vs-Boolean
symptom came from DuckDB returning HUGEINT (Arrow `Decimal128(38,0)`)
while the client schema advertised `Boolean` (courtesy of the buggy
inference helper).

**Actual root cause — symmetric-omission pattern.** Two files listed the
count family (`count`, `count_distinct`, `grouping`, `grouping_id`) and
both omitted `count_if`:

- `TypeInferenceEngine::aggregate_return_type` at
  `crates/core/src/types/type_inference.rs:326` — the fall-through arm
  `_ => arg_type.clone()` returned the argument type (Boolean) instead
  of `Long`.
- `Expression::FunctionCall::nullable` at
  `crates/core/src/expression/mod.rs:1051` — the default `_` arm marked
  the result nullable instead of non-nullable.

The scalar-context helper at `type_inference.rs:797` already handled
`count_if => Long` correctly, so the omission was site-local, not
design-wide. The Java reference at
`.reference/core/src/main/java/com/thunderduck/types/TypeInferenceEngine.java:1755-1757`
has the same latent gap.

**Fix.** Two-file diff on the Rust core:

- `types/type_inference.rs` — added `count_if` to the `count |
  count_distinct => Long` alternation (line 326) and to sibling
  `aggregate_is_non_nullable` (lines 395-398, to prevent future drift).
- `expression/mod.rs` — added `count_if` to `FunctionCall::nullable`'s
  non-nullable-aggregate literal list (line 1051).

**Iteration structure.** Iter 1 landed only the type-inference fix; the
reviewer flagged H1 (High) for the newly-visible nullability mismatch.
Iter 2 closed H1 with the mirror-pattern one-line addition. 4
regression tests added across the two files.

**Lesson recorded** (`tasks/lessons.md` §"Bug-fix diagnostics"): whenever
a code region enumerates the count family, audit for `count_if`
inclusion; the omission travels together across sibling code paths.
Also: corpus-first reading beats prompt speculation.

### C.3-4: `Binary(Div, Decimal, Decimal)` routing verification — **CLOSED 2026-07-01**

**Status:** LANDED via `/fix-bug` pipeline; delivered **+15** core_v2 delta
(134 → 149). Diagnostic report at `.agent-output/001-diagnostic-report.md`.

**Scope overturn (retain for future reference):** the diagnostician
falsified the prompt's scope. `emission.rs::render_binary` /
`render_spark_decimal_div` was byte-correct against the analyzer's typed
shape — verified by constructing the exact typed AST for
`nums.select((d1 / d2).alias("r"))` and observing the emitted SQL was
`CAST(spark_decimal_div(CAST("d1" AS DECIMAL(10,2)), CAST("d2" AS DECIMAL(6,3))) AS DECIMAL(20,9)) AS "r"`.
Confirming evidence: `type-005` also failed under
`THUNDERDUCK_TRANSPILER=legacy` with identical `None`-row symptoms,
proving the bug lived upstream of transpiler selection.

**Actual root cause:** silent-NULL catch-all in
`crates/connect-server/src/converter/relation_converter.rs:2513`
(`local_relation_to_values_sql::val()`). Every Arrow type not enumerated
by the match — including `Decimal128` — was silently mapped to SQL
literal `"NULL"`, corrupting every DECIMAL cell in `createDataFrame`
payloads while the schema stayed correct.

**Fix landed in the connect-server converter, not the v2 substrate:**
added a `Decimal128(p, s)` match arm with a new `format_decimal128`
helper (renders the scaled literal DuckDB requires — DuckDB rejects raw
unscaled `i128` in `CAST(...)`), and replaced the silent-NULL catch-all
with a loud `Err(ConnectError::PlanConversion("unsupported arrow type ..."))`.
The regression unit test the initial prompt asked for landed in
`emission.rs::tests` as `decimal_div_decimal_routes_through_spark_decimal_div`,
locking in the (already-correct) Div-routing invariant with no
production-code change on the v2 substrate.

**Target case IDs unblocked:** `type-005`, `type-003`, `type-004`
(reproducer trio) plus ~12 indirect unlocks touching decimal-payload
columns. `math-011` was **not** the same bug — it's a reference-side
Spark 4.x ANSI `DIVIDE_BY_ZERO` on the corpus row `(10, 0, ...)` and is
tracked separately.

**Lesson**: recorded in `tasks/lessons.md` under "Bug-fix diagnostics" —
diagnostician-overturned scopes are a strong signal; trust the
falsification. Silent-NULL catch-alls in typed dispatch are
data-corruption anti-patterns.

### C.3-5: `sum(decimal)` routing verification — **CLOSED 2026-07-01 (verify-only)**

**Status:** LANDED via `/fix-bug` pipeline as a **verify-only** pass;
delivered **+0** core_v2 delta (151 → 151). The diagnostician's
"rerun first" preflight fired: `agg-007` was already GREEN on v2 as of
C.3-4 + Slice D Phase 1's composition (Decimal128 `LocalRelation`
marshalling + `spark_aggregate_rewrite` routing for DECIMAL SUM/AVG).
No production code change was needed.

**Landing shape.** Two regression unit tests were added to
`crates/core/src/transpiler_v2/emission.rs::tests` adjacent to
`decimal_div_decimal_routes_through_spark_decimal_div` (the C.3-4
sibling anchor), locking in the routing invariant:

- `sum_of_decimal_routes_through_spark_sum` — asserts
  `SUM(Decimal(9,2))` emits `spark_sum(` with outer `CAST(... AS DECIMAL(19,2))`.
- `avg_of_decimal_routes_through_spark_avg` — asserts `AVG(Decimal(9,2))`
  emits `spark_avg(` with outer `CAST(... AS DECIMAL(13,6))`.

Both tests would have failed against pre-Slice-D-Phase-1 emission
(before `spark_aggregate_rewrite` existed) and pass today.

**Target case ID:** `agg-007` — already inside the 151 baseline; no
counter movement expected or observed.

### C.3-6: `percentile_approx` / `median` shape verification — **HALT-AND-FLAG 2026-07-01**

**Status:** HALT-AND-FLAG. Preflight showed `agg-013` RED (not GREEN as
the plan predicted). Root cause: DuckDB's `approx_quantile` requires
`FLOAT` for the quantile arg but the emission passes `0.5::DOUBLE`
(`Binder Error: approx_quantile(DOUBLE, DOUBLE) — Candidate:
approx_quantile(DOUBLE, FLOAT) -> DOUBLE`). The bug is emission-side
(the literal `0.5` is rendered with a DOUBLE type-suffix rather than
FLOAT) and does not match C.3-6's verify-only shape. Out of C.3-6 scope
per the branch table; needs a follow-up `/fix-bug` for the FLOAT/DOUBLE
quantile-arg emission. No production change or regression tests added
for `agg-013` this pass. Tracked as **C.3-6b (follow-up)**.

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
