# Slice C — Pass 2 `/new-feature` prompt (C.2: scalar-expression rows)

Composed per iteration methodology §Loop step 5 from Pass 1's carryover.
Pipeline start SHA context: `208e9b1` (Slice C.1 substrate landed on top of `f5a54c3`).

---

## Section A — Initial slice prompt (north star)

Use the content of `tasks/v2-slice-c-initial-prompt.md` verbatim (past the `---` divider) as the initial slice prompt. It defines the ADR mandate, the ~250 Case-ID target, the Quality Gate, and the out-of-scope items for Slice C.

**Amendment for Pass 2:** the architect for Pass 1 sub-split Slice C into C.1 + C.2 and Pass 1 landed C.1. Pass 2 tackles **C.2 (scalar-expression declarative emission rows)** — the second sub-slice per Pass-1 §0. C.2's scope is fixed by Pass 1's architect: ~50 declarative emission rows for scalar functions requiring Spark-parity top-level projection CASTs, and the drain of the `SqlGenerator::gen_expr` seam in `render_expr`.

C.2's explicit case-ID targets from the initial prompt's Acceptance section:

    cast-001..011 (11 — try_cast is Slice D)
    cond-001, cond-002, cond-006, cond-007, cond-008, cond-012..016 (10)
    str-001..019 (19 — str-020 is spark4/Slice D)
    math-001..014 (14 — math-015 nondet Slice B, math-016 extension Slice D)
    dt-002..017 (16 — dt-001 nondet Slice B)
    ord-001..005, ord-007, ord-010..012 (9)
    agg-001..006, agg-010, agg-014..017 return-type CASTs (10 where needed)
    misc-003..005, misc-008..010 (6)
    struc-001, struc-002, struc-005, struc-006 (4)
    proj-001..015, filt-001..015 (30) — most already green from C.1's operator rows

Plus a subset of C.1's already-passing case IDs that now also require scalar-parity to survive Spark's type-diff.

C.2 progress signal (per Pass-1 §Layer 3 §0): **80-110 → 180-200** on final Slice C termination.

---

## Section B — Carryover — MUST close before APPROVED

Per iteration methodology §Classification, the following are CLOSE_NOW in Pass 2. The pipeline cannot return APPROVED with any of these still open.

### From Pass 1 review (DEFER_LATER_SLICE to C.2)

1. **M5 — Global `EMIT_TAP` not test-isolated.**
   - **Location:** `crates/core/src/transpiler_v2/mod.rs:122` (global static), `crates/core/src/transpiler_v2/invariants.rs:151-169` (INV2 companion test).
   - **Fix (from Pass-1 review):** Serialize the tests that touch the tap using `serial_test` (already common in the workspace) or a module-level `Mutex`. Option (a) is the honest fix; will become load-bearing as Pass 2 adds more tap-touching tests for scalar-emission-row unit tests that fire `dispatch`.

2. **M6 — `render_tail` embeds `child_sql` twice.**
   - **Location:** `crates/core/src/transpiler_v2/emission.rs:821-833`.
   - **Fix:** Use a CTE: `WITH __td_child AS ({child_sql}) SELECT * EXCLUDE (__td_row_num__) FROM (SELECT *, ROW_NUMBER() ... FROM __td_child) WHERE __td_row_num__ > (SELECT COUNT(*) FROM __td_child) - {n_sql}`. Two-line refactor.

3. **L1 — `render_expr` allocates fresh `SqlGenerator` per expression call.**
   - **Location:** `crates/core/src/transpiler_v2/emission.rs:1054`.
   - **Fix:** Dies with C.2's seam drain (below). Once `render_expr` no longer delegates to `SqlGenerator::gen_expr`, the allocation is gone.

### From Pass 1 review markers (TODO Slice C.2:)

4. **`UpdateFields` walking in `ensure_no_ambiguous_columns`.**
   - **Location:** `crates/core/src/transpiler_v2/analyzer.rs:1877-1882` (`TODO Slice C.2:` marker).
   - **Fix:** Add explicit arm recursing into `UpdateFields`' inner expressions.

5. **Union per-column CAST wrapper for widened schema (M3 emission-side).**
   - **Location:** `crates/core/src/transpiler_v2/emission.rs:477` (`TODO Slice C.2:` marker above `render_union`).
   - **Fix:** After the union's SQL is built, wrap each column reference in a CAST to the widened type when the child's declared type diverges. This closes the M3 policy at the emitter side — the analyzer already documents the widened-wins invariant; C.2 materializes it in SQL.

6. **`SqlGenerator::gen_expr` seam drain — the core C.2 architectural change.**
   - **Location:** `crates/core/src/transpiler_v2/emission.rs:598` (`TODO Slice C.2:` marker at top of `render_expr`).
   - **Fix:** Replace the delegation to `SqlGenerator::gen_expr` with declarative per-function rows. This is the load-bearing C.2 deliverable. Approach:
     - Introduce an `EmissionRow` (either as data structures with a real interpreter, OR as declarative match arms extending `render_expr`).
     - Populate rows for `str-*`, `math-*`, `dt-*`, `cast-*`, `cond-*`, and primitive-agg return-type CASTs from the case-ID list.
     - Remove `use crate::generator::SqlGenerator` from `emission.rs`.
     - Tighten INV3's grep to reject `use crate::generator::SqlGenerator` entirely (currently allowed as the C.1 seam).
     - This closes Pass-1 review's H2 (letter-vs-spirit) permanently.

### From Pass 1 perf (deferred to C.2)

7. **OPT-M2 — `render_expr`'s per-expression `SqlGenerator::new().with_schema_for_v2(schema.clone())`.**
   - **Location:** `crates/core/src/transpiler_v2/emission.rs:1054`.
   - **Fix:** Subsumed by the seam drain (#6). Once C.2 replaces `render_expr`'s body, the `SqlGenerator` allocation is gone.

8. **OPT-M3 — `build_base_types_from_plan` unconditional schema clones.**
   - **Location:** `crates/connect-server/src/service.rs:108-179`.
   - **Fix:** Requires a semantics decision on the `BaseTypes` overlay contract per Pass-1 perf finding: whether `resolve_table_scan` reads `BaseTypes` unconditionally or only as fallback for `TypedOp::TableScan` whose `schema` is unresolved. This is a C.2 architectural decision, not just an allocation cleanup.

### INV state target for Pass 2

- **INV3** — tighten the grep to reject `use crate::generator::SqlGenerator` and `use crate::generator::*` — currently permitted as the C.1 seam. Update the docstring: "with C.2, the seam is drained; no legacy generator dependency remains."
- **INV1** — stays stubbed with honest TODO (differential-harness slice owns it). Do NOT force activation.
- **INV2 escape-hatch dimension** — stays stubbed (ADR-007 slice owns it). Slice C.2 does NOT need to enumerate additional escape hatches; if it introduces one, ADD it to `C_ESCAPE_HATCHES` with a rationale.

### Reviewer contract for Pass 2

The reviewer cannot return APPROVED if any of the 8 CLOSE_NOW items above is still open. Explicitly verify each:

- Test isolation for `EMIT_TAP`: `serial_test` (or equivalent) applied to both INV1 alias-check test AND INV2 companion test.
- `render_tail` uses a CTE.
- `SqlGenerator` import gone from `emission.rs`.
- `render_union` wraps per-column CAST for widened types.
- `UpdateFields` walking in `ensure_no_ambiguous_columns` is present.
- INV3 grep rejects the legacy generator imports.
- Every C.2 target case-ID's SQL emission produces the correct Spark return type (differential test suite would confirm — but that runs only at final termination).
- No Punt for any operator that Pass 1 supported (Pass 2 is additive only).

---

## Section C — DEFER — do NOT reintroduce

The following are DEFER_LATER_SLICE per Pass 1 and MUST NOT be reintroduced in Pass 2. If the architect proposes any of these as in-scope, flag as scope creep.

- **Extension functions** (`spark_sum`, `spark_avg`, `spark_hash`, `spark_skewness`, `spark_decimal_div`, `spark_xxhash64`, `try_cast`, `try_divide`, `spark4` syntax). → Slice D.
- **Full `join-*` cluster** beyond what M2 fixed structurally. → Slice E.
- **Complex-type emission** (arrays, maps, structs, HOFs, explode/inline). → Slice F.
- **Verticals** (interval math, pivot/unpivot, window frames, JSON, parsing). → Slice G.
- **Command arm / lakehouse writes.** → Slice H.
- **Subquery-body walking in `ensure_no_ambiguous_columns`.** → Slice G (marked `TODO Slice G:` at `analyzer.rs:1883-1891`).
- **INV1 full activation.** → **differential-harness slice** (new; add to readiness map after Pass 2 lands).
- **INV2 escape-hatch full activation.** → **ADR-007 slice** (existing readiness map slice).
- **Reintroducing `EmissionRow`/`Template`/`SlotKind` as dead scaffolding.** If C.2 goes with a data-structures-with-real-interpreter approach, the interpreter MUST be live in the same commit. Half-declarative is a Pass-1 iteration-1 anti-pattern that must not repeat.
- **Corpus / harness edits.** → Never in this slice.
- **Legacy `SqlGenerator` behavior changes.** → Never — legacy remains untouched.

---

## Section D — Reminders

- The architect for Pass 1 explicitly said: "Slice C.2 promotes the arms to declarative per-function rows once row count justifies the substrate; INV3's `use crate::generator::SqlGenerator` allowance is a *deliberate seam* that C.2 will drain." Pass 2 delivers that promise.
- The methodology's Pass 2 termination check runs `git grep 'TODO INV3'` (empty), plus verifies APPROVED with zero CLOSE_NOW items. INV1/INV2-escape-hatch stay stubbed with honest DEFER markers pointing at the correct future slices.
- Pass 2's expected empirical delta lands at final Slice C termination — per methodology, `v2-progress.sh` runs ONCE at final termination, not between passes. The progress-signal claim (12 → 180-200) is validated then.
- If the architect proposes a further sub-split within C.2 (e.g., "C.2.a = scalar rows, C.2.b = seam drain"), that is legitimate under methodology §Loop step 4 and Pass 2 tackles the sub-split's first sub-slice. Each becomes its own pass with its own carryover.

---

**Pipeline start SHA for Pass 2:** `208e9b1` (post-C.1 commit).
**Expected pass count for Slice C completion:** 2 (this one).
**Contingencies:** if Pass 2 reviewer returns NEEDS_CHANGES, inner-loop fix; if it returns APPROVED with new CLOSE_NOW Mediums, Pass 3.
