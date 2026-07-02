# v2 Corpus-Driven `/goal` Prompt Template

**Purpose.** Reusable `/goal` prompt for driving Thunderduck's v2 restart to
100% corpus green using **corpus-test-driven, diagnostic-first** iteration.

Supersedes `v2-slice-goal-prompt-template.md`. The slice-based top-down
approach was retired 2026-07-02 after 4 slices (A/B/C.1/E.0) produced 0
corpus signal on their own scope-file estimates; a subsequent single
diagnostic pass drove corpus 0 → 25/324 with 4 targeted fixes. The lesson:
**scope files are architectural guesses; corpus traces are ground truth**.

## Primary goal

`tests/scripts/v2-progress.sh` reports **324/324 core_v2 corpus cases passed**
— every case runs end-to-end through Spark Connect → τ → DuckDB → Arrow IPC
→ PySpark client and matches Spark's result byte-identically per the ADR-015
differential oracle (row count, column names, column types, values within
float ε, null handling, sort order).

## Secondary goal (non-negotiable)

Zero outstanding architectural findings, zero performance findings, zero
tech debt on termination. Every review/perf finding surfaced during a pass
is addressed **in that pass**. No `DEFER_LATER_SLICE`, no `next_slice_
housekeeping.md`, no `TODO(<future>)` comments in code. This is the
break from the slice-driven methodology.

## Design authority

- `docs/thunderduck-rearchitect-ADRs.md` — ADR-000 to ADR-022. Every design
  decision cites the applicable ADR. ADR-022 is load-bearing: τ is the only
  path; two error categories (Spark-emulated vs Thunderduck-boundary).
- `tasks/v2-restart-open-decisions.md` — 13 decisions RESOLVED. New
  architectural gaps → Decision 14+ (HALT-AND-FLAG-3, see below).
- `tasks/v2-restart-inheritance-checklist.md` — the morph-track bug catalog.
  Every item MUST be present in the running system by termination.
- `tasks/lessons.md` — corrections observed during prior sessions.

## Methodology: corpus-test-driven, diagnostic-first

Each pass = one corpus case (or a tight cluster of same-shape variants)
driven to green end-to-end. Slice boundaries are irrelevant to the loop;
the fix touches whichever layer(s) the diagnostic points at.

### Per-pass steps

1. **Pick a target case.** Prefer cases with the largest cascade multiplier
   from prior failure clustering — after each pass, re-cluster remaining
   `v2-progress.sh` failures by error signature and pick from the
   highest-count cluster. When the corpus is red, sort by
   `pytest -k` → error-message frequency. Ties broken by simplicity
   (fewer chained ops, scalar-only fixture over complex-type).

2. **Diagnose.** Run the target through the differential harness with
   tracing instrumentation ACTIVE. Capture, for the failing case:
   - The full request path: PySpark → gRPC → `execute_plan`/`analyze_plan`.
   - Layer-boundary snapshots: proto → CommonAst → BaseTypes → TypedAst
     (analyzer output) → dispatched SQL → DuckDB result / DuckDB error.
   - Where the failure surfaces: analyzer error, converter unimplemented
     shape, emission unsupported expression, DuckDB parse/bind/execute
     error, harness diff (row/schema/value mismatch).
   - Category per ADR-022: Spark-emulated (analyzer) vs Thunderduck-boundary
     (unimplemented arm) vs runtime correctness bug.

   If layer-boundary tracing is not present at the site of the failure,
   **adding `tracing::debug!` instrumentation is in scope for the pass**.
   Log the trace to `.agent-output/diagnostic-{case-id}.md`.

   Use the `rust-diagnostician` agent for multi-hypothesis investigation
   when the failure category is not obvious from a single trace.

3. **Architect.** `/new-feature` architect reads the diagnostic log, then:
   - Cites the applicable ADR(s) and checklist section(s) by identifier.
   - Considers `v2-restart-open-decisions.md` for any relevant resolved
     decision; if the required behavior contradicts a resolved decision,
     HALT-AND-FLAG-3.
   - Identifies the smallest correct fix — which layer owns the semantic:
     converter (protobuf → CommonAst), analyzer (schema + nullability +
     types), type inference, emission (SQL rendering), runtime (execution
     wiring). Multiple layers may need the same pass.
   - Enumerates every fix required for the case to pass, with file:line
     citations.
   - Rejects partial-arm shortcuts that would land dark (arm without a test
     that proves it runs against the corpus).

4. **Implement.** Coder applies the plan. Runs the Quality Gate per
   CLAUDE.md §Quality Gate. Re-runs the target case AND the full
   `v2-progress.sh` — the target must be green AND no regression on prior
   green cases.

5. **Review.** Reviewer verifies: (a) applicable checklist items present,
   (b) ADR citations honest, (c) no INV1..INV10 regressions, (d) no
   partial-arm / dead-code shortcuts, (e) instrumentation added during
   diagnosis is either kept (if load-bearing for future diagnostics) or
   removed cleanly (no stray `println!` / `dbg!`).

6. **Perf.** Perf agent runs. Any HIGH/MEDIUM finding CLOSED IN THIS PASS.

7. **Close findings — zero DEFER.** Every review + perf finding is
   CLOSE_NOW_IN_THIS_PASS. If a finding proposes deferring to a "future
   slice" or "housekeeping" — reject the classification: this is
   corpus-driven, not slice-driven. Findings the reviewer marks as truly
   out of scope for the target case (touching unrelated code) must be
   converted into their own dedicated pass that immediately follows;
   the pass isn't done until the closure runs.

### Iteration budget per pass

No hard cap on total passes. But per pass: if the coder exceeds 5 fix
iterations trying to close review findings, HALT-AND-FLAG-1 — the
diagnostic hypothesis is wrong; re-run step 2 with fresh instrumentation.

## Terminate when

- `tests/scripts/v2-progress.sh` reports **324/324** passing.
- No regressions: prior-green cases stay green (`v2_progress.md` monotone).
- All INV1..INV10 active tests green.
- Quality Gate green.
- Every applicable inheritance-checklist item verified present.
- Zero `DEFER` items outstanding (open-decisions.md and code both clean).
- (TPC-H rejoins mandatory gates once 324/324 is stable; not a per-pass
  check during the climb.)

## On termination

1. Update `tests/integration/v2_progress.md`.
2. Add a final dev journal entry summarizing the climb.
3. Extend `docs/dev-journal-toc.md`.
4. **Do NOT commit without user approval** (CLAUDE.md).

## Non-goals

- Legacy modifications — reference-only per ADR-022.
- Runtime fallback / dual-path plumbing.
- Slice-based scope files — obsolete; corpus is the fitness function.
- Architectural rework outside the ADR set — halt-and-flag as Decision 14+
  rather than resolving in a plan document.
- Commits without user approval.
- Skipping Quality Gate.
- Landing arms without corpus witnesses (dead code).

## HALT-AND-FLAG (three triggers, all legitimate)

1. **Diagnostic hypothesis wrong.** Coder hit 5 fix iterations trying to
   close review findings without the target case going green. Re-diagnose
   from step 2.

2. **Upstream substrate missing.** Fix lands correctly but corpus stays
   red because a *prior* corpus-driven pass should have supplied the
   enabling substrate. Reopen the prior pass or extend this pass to cover
   the substrate too; do NOT punt to a hypothetical future slice.

3. **New architectural decision surfaced.** A gap not covered by any ADR
   or by the resolved decisions in `v2-restart-open-decisions.md` shows up.
   Append as Decision 14+ in that file; halt for user resolution before
   the coder proceeds.

---

## Template (paste into `/goal` — 3900 chars including boilerplate)

```
/goal Drive Thunderduck v2 corpus to 100% e2e green via iterated
/new-feature (diagnostic-first) passes

**Primary goal.** 324/324 core_v2 corpus cases green
(`tests/scripts/v2-progress.sh` reports 100%); every case matches Spark
byte-identically per ADR-015.
**Secondary goal (non-negotiable).** Zero DEFER items — review + perf
findings closed in the pass they surface. No slice-based housekeeping.

**Baseline:** current git HEAD; run `v2-progress.sh` at start; record.
**Design authority:** `docs/thunderduck-rearchitect-ADRs.md`
(ADR-000..ADR-022). Every fix cites applicable ADR.
**Open decisions:** `tasks/v2-restart-open-decisions.md` — 13 RESOLVED.
New gaps → Decision 14+ (HALT-AND-FLAG-3).
**Inheritance checklist:** `tasks/v2-restart-inheritance-checklist.md` —
every applicable item present by termination.
**Methodology:** `tasks/v2-corpus-driven-goal-prompt-template.md`.

**Loop** (per pass):
1. **Pick.** One corpus case (≤ 3 same-shape variants). Prefer highest-
   cascade cluster from post-pass `v2-progress.sh` failure clustering.
2. **Diagnose.** Run target with tracing instrumentation. Capture
   layer-boundary snapshots (proto → CommonAst → TypedAst → SQL →
   DuckDB result). If instrumentation missing at failure site, ADD it —
   in scope for pass. Log `.agent-output/diagnostic-{case-id}.md`. Use
   `rust-diagnostician` for multi-hypothesis investigation.
3. **Architect** (via `/new-feature`). Reads diagnostic; cites ADRs +
   checklist sections; identifies smallest correct fix (analyzer /
   converter / type inference / emission / runtime); enumerates every
   change with file:line.
4. **Implement.** Quality Gate (CLAUDE.md); target case green AND full
   `v2-progress.sh` no-regress.
5. **Review + Perf.** ALL findings CLOSE_NOW_IN_THIS_PASS. Zero DEFER.
   Findings requiring unrelated-code touch become dedicated follow-up
   passes queued immediately after — not deferred indefinitely.

**Per-pass HALT-AND-FLAG:**
- 5 fix iterations without target green → re-diagnose.
- Upstream substrate gap in a prior pass → reopen it.
- New architectural decision → append as Decision 14+.

**Terminate when:** 324/324; no regressions; INV1..INV10 green; Quality
Gate green; every applicable checklist item present; zero DEFER.

**Non-goals:** legacy mods (ADR-022), fallback plumbing, slice-based
scoping, commits without user approval, dead-code arms.

**On termination:** update `v2_progress.md`; dev journal + TOC entry;
NO commits without user approval.
```

## Design notes

**Why corpus-driven, not slice-driven.** The slice-based top-down plan
produced 4 landings with 0 corpus signal because each slice's scope
estimated "cases unlocked" without a corpus trace to validate. Once one
representative case was actually traced end-to-end, the real blockers
surfaced in minutes: SingleRow subquery shape, complex-type literals,
timestamp construction. Fixing 4 small issues cost 80 LOC and unlocked 25
cases. Corpus traces are ground truth; scope-file estimates are not.

**Why zero DEFER.** The slice methodology allowed CLOSE_NOW_HYGIENE and
DEFER_LATER_SLICE. In practice, DEFER items accumulated across slices
because the "future slice" they named often never materialized as
scheduled. Zero DEFER forces every finding to close, which keeps the
codebase honest with the corpus signal.

**Why diagnostic-first.** The morph track's `/fix-bug` pipeline was the
right shape but scoped to individual bugs. Corpus-driven work is *always*
bug-fixing (each red case is a bug). Making diagnostics the entry point
avoids the top-down "which slice owns this?" question — the answer is
"whichever layer the trace points at."

**Why instrumentation is in scope.** Without layer-boundary traces, the
diagnostic step is guessing. Adding `tracing::debug!` at layer boundaries
is a permanent investment; the same instrumentation serves every future
diagnostic. Keep it in the code (feature-gate if noise is a concern).

**Why no hard pass cap.** The slice methodology used a 5-pass cap as an
escalation signal. Corpus-driven work has ~324 passes (one per case, or a
cluster). What we cap instead is *fix iterations within a pass* — 5 fix
iterations without the target green means the diagnostic was wrong.

## Iteration notes

- First test of this template: whichever corpus cluster the user picks
  after 2026-07-02's 25/324 landing.
- Slice-era artifacts (`v2-slice-*.md` files) stay in the repo for
  historical reference; they document what was landed and when.
