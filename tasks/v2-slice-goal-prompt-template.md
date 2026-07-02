# Slice X `/goal` Prompt Template

**Purpose.** Reusable prompt for `/goal` to drive one v2 restart slice through iterated `/new-feature` (or `/fix-bug` if diagnostic-first) pipelines. The terminal fitness function is **the target corpus case IDs turning green on the end-to-end differential harness** — unit tests and Quality Gate are per-pass hygiene checks, not the goal.

The template stays lean by keeping slice-specific detail in a companion **scope file** at `tasks/v2-slice-<SLICE_ID>-scope.md`. The template references it; the scope file is where per-slice targets, ADRs, and inheritance-checklist items live.

**Status:** first iteration (2026-07-02). We'll refine this template as we run Slice A and observe what the /goal driver actually needs.

---

## Usage

1. **Draft the scope file** at `tasks/v2-slice-<SLICE_ID>-scope.md` — see §"Scope file structure" below for the required sections.
2. **Substitute the four placeholders** in the /goal prompt block:
   - `<SLICE_ID>` — e.g. `A`, `B`, `C.1`, `D` (matches `tasks/v2-adr-readiness-map.md` §1).
   - `<SLICE_NAME>` — one-line label from the readiness map (e.g. `v2 substrate independence` for Slice A).
   - `<BASELINE_SHA>` — current git HEAD when the /goal fires.
   - `<BASELINE_CORE_V2>` — current `core_v2` count from the latest row of `tests/integration/v2_progress.md`.
3. **Invoke `/goal`** with the substituted text (3852 chars including boilerplate + a 4 × placeholder-expansion overhead; the 4000-char /goal budget is safe with `<SLICE_ID>` up to ~3 chars).

The /goal driver reads the scope file at Pass 1 and uses it as the architect's context. If the scope file is missing or malformed at Pass 1, the architect flags it and halts.

---

## Scope file structure (`tasks/v2-slice-<SLICE_ID>-scope.md`)

Required sections:

- **§Targets** — Comma-separated corpus case IDs that must be green on `core_v2` at termination. Example: `type-011, type-019, type-020, join-001, join-002, ..., join-014, chain-001, chain-003, chain-005, chain-006` for Slice E. Must match the pytest `-k` filter format so it can be dropped into the termination-verification command verbatim.

- **§ADRs** — ADR IDs the slice implements. Example for Slice A: `ADR-003 (common AST), ADR-004 (protobuf-boundary dispatch), ADR-021 (substrate independence)`. Cross-references `docs/thunderduck-rearchitect-ADRs.md`.

- **§Inheritance-checklist sections** — Which sections of `tasks/v2-restart-inheritance-checklist.md` the slice's arms/types must present on day 1. Example for Slice A: `§1.1-1.3 (analyzer symmetric-omissions), §2.1-2.3 (V2RelationConverter discipline), §5.5-5.6 (design patterns: plan_has_empty_scan short-circuit, quote_ident fast path)`.

- **§Sub-slice sketch** — Suggested within-slice decomposition per methodology §CV.7. Pass 1 architect MAY propose a different split; this section is guidance, not a mandate. Example for Slice A: `A.1: v2 Expression + v2 TypeInferenceEngine (types substrate). A.2: v2 CommonAst + V2RelationConverter (plan substrate). A.3: dispatch relocation + wiring in service.rs.`

- **§Non-goals (slice-specific)** — Slice-scope exclusions beyond the universal non-goals in the template. Example for Slice B (analyzer): `No emission-side work — Slice C handles dispatch_op / render_*. Slice B produces a TypedAst; downstream consumption is Slice C's problem.`

- **§Success criteria beyond §Targets** — Optional. Anything additional the reviewer must verify (e.g., "INV10 grep returns zero"). Most slices will just rely on the §Targets clause + the template's checklist-verification loop.

---

## Template (substitute `<SLICE_ID>`, `<SLICE_NAME>`, `<BASELINE_SHA>`, `<BASELINE_CORE_V2>`)

```
/goal Slice <SLICE_ID> (<SLICE_NAME>) — drive target corpus case IDs green via iterated /new-feature (or /fix-bug if diagnostic-first) passes

**Baseline:** commit `<BASELINE_SHA>` (<BASELINE_CORE_V2>/324 core_v2).
**Slice scope:** `tasks/v2-slice-<SLICE_ID>-scope.md` — target case IDs, applicable ADRs, inheritance-checklist sections, sub-slice sketch.
**Methodology:** `tasks/v2-slice-iteration-methodology.md`.
**Readiness map:** `tasks/v2-adr-readiness-map.md` §Slice <SLICE_ID>.
**Design authority:** `docs/thunderduck-rearchitect-ADRs.md` (ADR-000 → ADR-021).
**Inheritance discipline:** `tasks/v2-restart-inheritance-checklist.md` — Pass 1's architect plan MUST cite the applicable sections; reviewer verifies presence.
**Iteration log:** `tasks/v2-slice-<SLICE_ID>-iteration-log.md` (create at Pass 1; append each pass's verdict + delta + lessons).

**Preflight** (halt any fails):
1. `cargo check -p thunderduck-core -p thunderduck-connect-server` clean.
2. Legacy TPC-H differential green (`./tests/scripts/run-differential-tests.sh tpch` = 51/51).
3. `tests/scripts/v2-progress.sh` reports the expected `<BASELINE_CORE_V2>` count.

**Loop** (per methodology §Loop):
- Each pass = one architect-approved `/new-feature` invocation, OR `/fix-bug` if a target's investigation proves the scope file's hypothesis wrong (diagnostic-first).
- Pass 1 architect MAY sub-split the slice (§CV.7). Each sub-slice runs as its own pass; the 5-pass hard cap counts sub-slice passes.
- Architect plan MUST cite `tasks/v2-restart-inheritance-checklist.md` sections and enumerate which items this pass covers.
- Coder implements per plan + runs Quality Gate (CLAUDE.md §Quality Gate) before completing.
- Reviewer approves only if applicable checklist items are present in the diff; else NEEDS_CHANGES with missing items as Critical.
- Classify findings CLOSE_NOW-in-this-pass vs DEFER_LATER_SLICE. **Every DEFER MUST name an owning slice** and append to the readiness map.
- Progress signal measured **at termination only**. Between passes: focused corpus preflight (`pytest -k "<target-ids>"`) for scope calibration.

**Terminate when:**
- Every target corpus case ID in `tasks/v2-slice-<SLICE_ID>-scope.md` §Targets is GREEN on `core_v2` (verify via `cd tests/integration && python3 -m pytest differential/test_dataframe_corpus_differential.py -k "<target-ids>" -v`).
- Legacy TPC-H 51/51 unregressed.
- `./tests/scripts/run-differential-tests.sh all` — no regressions vs baseline.
- Quality Gate green each pass.
- Every applicable inheritance-checklist item verified present.
- Every new DEFER item names an owning slice.

**Hard cap:** 5 passes. Passes 6+ escalate to human — the slice boundary is wrong, not the iteration.

**On termination:**
1. Finalize `tasks/v2-slice-<SLICE_ID>-iteration-log.md`.
2. Update `tasks/v2-adr-readiness-map.md` §Slice <SLICE_ID> with final delta + per-case outcomes.
3. Update `tests/integration/v2_progress.md`.
4. Add `docs/dev_journal/YYYY-MM-DD-v2-slice-<SLICE_ID>.md`; extend `docs/dev-journal-toc.md`.
5. Report closure. **Do NOT commit without user approval** (CLAUDE.md).

**Non-goals:**
- No legacy `SqlGenerator` / `FunctionRegistry` / `RelationConverter` mods (INV3 + INV10; legacy is separate per ADR-021).
- No commits without user approval.
- No skipping Quality Gate.
- No full-differential runs between passes; termination only.
- No scope expansion beyond `tasks/v2-slice-<SLICE_ID>-scope.md` §Targets without user approval.

**HALT-AND-FLAG:** if a target proves diagnostic-first at Pass 1 (architect discovers scope file's hypothesis is wrong), halt for `/fix-bug` rather than iterating with a stale hypothesis. If a fix lands correctly but the corpus stays red because of upstream substrate not yet in place, document + reassign to the appropriate future slice per readiness map.
```

---

## Design notes (why this shape)

**Fitness function = end-to-end differential corpus, not unit tests.** Prior sessions (Slice C.3 remaining, Slice D Phase 2) hit "unit tests green + corpus red" states because v2 arms were correct but routed through legacy fallback. The restart's substrate-independence (ADR-021) eliminates that class, but the discipline still holds: terminate when the corpus is green, not when the arm exists.

**Slice-scope file is the parameterization surface.** Everything else in the template is universal (methodology, non-goals, halt-and-flag triggers, on-termination steps). Per-slice variation lives in one file, not scattered across the /goal invocation.

**Architect can sub-split (§CV.7).** Slice A alone is ~3000 LOC (V2RelationConverter + v2 Expression + v2 TypeInferenceEngine + dispatch relocation). The template expects the architect to propose a sub-split at Pass 1 for large slices, per the methodology's within-slice sub-split provision. The 5-pass hard cap counts sub-slice passes.

**Progress signal at termination only.** Running the full differential between passes wastes ~3 minutes per pass and produces noise (partial arm landings appear regressive). Focused corpus preflight (`pytest -k`) between passes is fine — the full-differential run is a termination-gate check.

**HALT-AND-FLAG discipline.** Two triggers: (a) *diagnostic-first* — the scope file's root-cause hypothesis is wrong, so `/fix-bug` is the right pipeline, not `/new-feature`; (b) *upstream substrate missing* — the v2 arm is correct, but a case fails because a future slice hasn't landed the enabling substrate yet. Both are legitimate outcomes; neither is a slice failure.

**No commits without user approval.** From CLAUDE.md. Non-negotiable.

---

## Iteration notes

- **First slice to try this template on:** Slice A (v2 substrate independence).
- **Expected iterations of the template itself:** as we run each slice, we'll observe what the driver actually needs and adjust. Likely revisions:
  - Whether the scope file needs a §"Known unknowns" section for cases the architect hasn't scoped yet.
  - Whether the "verify each applicable inheritance-checklist item" reviewer gate needs more explicit language for the reviewer subagent.
  - Whether the sub-split machinery needs a "sub-slice retermination criteria" clause distinct from the parent slice's.
- **Template file lives at:** `tasks/v2-slice-goal-prompt-template.md` (this file). Slice-specific scope files live at `tasks/v2-slice-<SLICE_ID>-scope.md`; slice-specific iteration logs at `tasks/v2-slice-<SLICE_ID>-iteration-log.md`.
