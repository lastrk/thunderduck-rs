# v2 Corpus-Driven Iteration Methodology

**Purpose.** Detailed per-pass discipline for the corpus-driven `/goal`
template at `tasks/v2-corpus-driven-goal-prompt-template.md`. This file
is the externalized "how" so the `/goal` prompt itself stays lean
(≤4000 chars).

Analogous in shape to `tasks/v2-slice-iteration-methodology.md` (the
slice-era methodology, retired 2026-07-02) but scoped to corpus cases
rather than sub-slice deliverables.

## Objective

A pass `P` is **complete** iff, on the last iteration, ALL hold:

1. The target corpus case is GREEN on `core_v2`
   (`tests/scripts/v2-progress.sh` records the case as passed).
2. No prior-green case regressed
   (`v2-progress.sh` count monotone: `after ≥ before`).
3. The reviewer returned `APPROVED` (0 Critical + 0 High).
4. Every review Medium/Low finding is **fixed in this pass**
   (zero DEFER — see §DEFER discipline below).
5. Every perf HIGH/MEDIUM finding is **fixed in this pass**.
6. The Quality Gate steps from `CLAUDE.md` §Quality Gate all pass.
7. No **new** compiler warnings on files this pass modified
   (see §Compiler warnings).
8. `git grep 'TODO INV<N>'` returns zero hits crate-wide (invariant
   markers must not accumulate).

## Per-pass steps

Set `fix_iteration = 1`. Cap `fix_iteration ≤ 5`; hitting 5 without
target green is a HALT-AND-FLAG-1 — re-diagnose (§HALT-AND-FLAG).

### 1. Pick

Choose ONE corpus case (or a tight cluster of ≤ 3 same-shape
variants). Prefer the highest-cascade cluster from the post-run
`v2-progress.sh` failure clustering:

```
tests/scripts/run-differential-tests.sh core_v2 2>&1 \
    | grep -oE "τ: [^\"]+|DuckDB error: [^\"]+|assert_dataframes_equal" \
    | sort | uniq -c | sort -rn | head -12
```

Ties broken by simplicity (fewer chained ops, scalar-only fixture).

### 2. Diagnose (mandatory)

Run the target through the differential harness. Use the
`rust-diagnostician` agent for multi-hypothesis investigation:

```
Agent(subagent_type: "rust-diagnostician", prompt: "target: <case-id>; ...")
```

Capture layer-boundary snapshots:
- Request path (PySpark → gRPC → `execute_plan` / `analyze_plan`).
- Proto → CommonAst (V2RelationConverter / V2ExpressionConverter).
- CommonAst → TypedAst (analyzer).
- TypedAst → SQL (dispatch_op).
- SQL → DuckDB result (or DuckDB error).
- Diff outcome (schema / data / null / order).

If layer-boundary tracing is missing at the failure site, **adding
`tracing::debug!` instrumentation is IN SCOPE for the pass**.

Write the diagnostic to `.agent-output/diagnostic-{case-id}.md`.
Category per ADR-022:
- Spark-emulated (analyzer emits) — Spark itself would reject.
- Thunderduck-boundary (unimplemented arm).
- Runtime correctness bug.

### 3. Architect (via `/new-feature`)

Dispatch:

```
Skill(skill: "new-feature", args: "<case-id> per diagnostic at
.agent-output/diagnostic-{case-id}.md; cite ADRs + checklist sections;
smallest correct fix.")
```

Requirements for the architect:
- Cite applicable ADR(s) from `docs/thunderduck-rearchitect-ADRs.md`
  by identifier.
- Cite applicable inheritance-checklist sections from
  `tasks/v2-restart-inheritance-checklist.md`.
- Consult `tasks/v2-restart-open-decisions.md` — if the required
  behavior contradicts a resolved decision, HALT-AND-FLAG-3.
- Identify the smallest correct fix. Name the OWNING LAYER: converter
  (protobuf → CommonAst), analyzer (schema + nullability + types),
  type inference, emission (SQL rendering), runtime (execution).
- Enumerate every fix with file:line.
- Reject partial-arm shortcuts that would land dark (an arm without
  a corpus witness).

### 4. Implement

Coder runs the Quality Gate per `CLAUDE.md` §Quality Gate. Re-runs
the target case AND full `v2-progress.sh`. Requirements:
- Target case: GREEN.
- Full corpus: no regression (count monotone).
- **No new compiler warnings** on files this pass modified — see
  §Compiler warnings.

### 5. Review + Perf

Both invocations MUST complete in-pass:

- **Reviewer** verifies:
  (a) applicable checklist items present;
  (b) ADR citations honest;
  (c) no INV1..INV10 regressions;
  (d) no partial-arm / dead-code shortcuts;
  (e) instrumentation added during diagnosis is either kept (if
    load-bearing for future diagnostics) or removed cleanly;
  (f) no stray `println!` / `dbg!` / `eprintln!`.

- **Perf** analyzes for HIGH/MEDIUM findings. Both must close in-pass.

### 6. Close findings — ZERO DEFER

Every review + perf finding is `CLOSE_NOW_IN_THIS_PASS`. If a finding
proposes deferring to a "future slice" or "housekeeping":
- **Reject the classification.** Corpus-driven, not slice-driven.
- If the finding truly touches unrelated code that shouldn't land in
  this pass (e.g., a Slice-B analyzer bug surfacing here), convert it
  to a dedicated follow-up pass queued **immediately after** — not
  deferred indefinitely.

The pass isn't done until findings are 0.

## Compiler warnings

New compiler warnings introduced by this pass MUST be fixed in the
same pass. Discipline:

- Baseline: `cargo check -p <touched-crate>` records the current
  warning set.
- After implementation: warnings emitted from *touched files* must not
  exceed baseline. Warnings from *untouched files* (the pre-existing
  workspace baseline) are outside scope.
- Common issues to address in-pass:
  - `unused_imports` on files this pass touched.
  - `unused_variables` — either use, `_`-prefix, or remove.
  - `dead_code` on new items — remove or wire.
  - `unreachable_pattern` on match arms — reorder / delete.
  - `deprecated` — pick the successor.
- The workspace-baseline warnings (~31 pre-existing on
  `crates/connect-server/src/converter/relation_converter.rs` etc.)
  are NOT scope for this pass unless directly touched.

Verify with:
```
cargo build --release -p thunderduck-connect-server 2>&1 \
    | grep -E "^warning:" | wc -l
```

Must match the pre-pass baseline or be lower.

## HALT-AND-FLAG (three triggers, all legitimate)

1. **Diagnostic hypothesis wrong** — coder hit 5 fix iterations
   without target green. Re-diagnose from §2 with fresh
   instrumentation. If second diagnostic also fails to converge in
   5 iterations, ESCALATE (the case is genuinely at the frontier
   of the substrate; the pass may need a substrate slice first).

2. **Upstream substrate missing** — a fix lands correctly at the
   named layer, but the target case still fails because a **prior
   corpus-driven pass** should have supplied the enabling substrate.
   Reopen the prior pass or extend this pass to cover the substrate;
   do NOT punt to a hypothetical future pass.

3. **New architectural decision surfaced** — a gap not covered by any
   ADR (ADR-000..ADR-022) or by the resolved decisions in
   `tasks/v2-restart-open-decisions.md`. Append as Decision 14+ in
   that file; halt for user resolution before the coder proceeds.

## Pass log

Each pass appends to a rolling pass log
`tasks/v2-corpus-driven-pass-log.md`:

```markdown
## Pass N — YYYY-MM-DDTHH:MMZ
- Case: <case-id> [+ ≤2 cluster variants]
- Diagnostic: .agent-output/diagnostic-{case-id}.md
- Architect verdict: APPROVED | NEEDS_CHANGES (fix iteration K)
- Layer(s) touched: converter | analyzer | type-inference | emission | runtime
- ADR citations: ADR-<n>, ADR-<m>, ...
- Checklist §-anchors: §<x.y>, §<x.z>, ...
- Corpus signal: <before> → <after>
- Findings CLOSE_NOW_IN_THIS_PASS: <count> (list)
- Compiler warning delta: <baseline> → <after> (must be ≤0)
- Commit SHA: <sha>
```

Update this file at the end of every pass (before commit).

## Anti-patterns to avoid

- **Do not** carry "TODO Future Pass" comments forward as a substitute
  for closing the finding. Zero DEFER means zero.
- **Do not** run the differential suite between fix iterations of the
  same pass — use focused `pytest -k "<case-id>"` for calibration; the
  full `v2-progress.sh` runs only once per pass as the termination check.
- **Do not** merge deferred items into a "known gaps" list in the code.
  Anything not fixed in-pass either lands as a follow-up pass or is
  Decision 14+ material (HALT-AND-FLAG-3).
- **Do not** silence compiler warnings with `#[allow(...)]` attributes
  when the correct action is to fix the underlying issue.
- **Do not** land unwired arms (dead code) hoping a future pass will
  connect them. Every arm must have a corpus witness this pass or the
  same session.

## Iteration cap

- Per pass: 5 fix iterations MAX. Exceeding → HALT-AND-FLAG-1.
- Total passes: unbounded. Corpus-driven work has ~324 passes (one
  per case or cluster) as a natural ceiling; there is no per-session
  cap.

## References

- Design authority: `docs/thunderduck-rearchitect-ADRs.md`
  (ADR-000..ADR-022).
- Open decisions: `tasks/v2-restart-open-decisions.md`.
- Inheritance checklist: `tasks/v2-restart-inheritance-checklist.md`.
- Quality Gate: `CLAUDE.md` §Quality Gate.
- Template: `tasks/v2-corpus-driven-goal-prompt-template.md`.
