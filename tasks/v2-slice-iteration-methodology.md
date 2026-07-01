# v2 Slice Iteration Methodology

**Purpose.** Turn a single-pass `/new-feature` into a bounded iterative loop
that closes every within-slice deferment before the slice is declared done.
The output is a slice that lands with **zero within-slice carryover** — every
review/perf finding is either fixed or explicitly assigned to a later slice
with an ADR justification.

## Objective

A slice `X` is **complete** iff, on the last pass, ALL of the following hold:

1. The reviewer returned `APPROVED` (0 Critical + 0 High).
2. Every review Medium and Low finding is either (a) fixed in this pass or
   (b) explicitly on the DEFER list with a Slice-D..H ADR pointer.
3. Every perf HIGH is fixed; every perf MEDIUM is fixed unless flagged
   NON-APPLICABLE with justification.
4. The architect did NOT propose a further within-slice sub-split in their
   plan. If they did, that sub-slice runs as a follow-up pass under the
   same iteration loop (not deferred to a later slice).
5. `git grep 'TODO INV<N>'` for every invariant activated by this slice
   returns zero hits.
6. The Quality Gate steps from `CLAUDE.md` §Quality Gate all pass.

## The loop

Set `pass = 1`, `carryover_close_now = []`, `defer_later = []`. Cap
`pass ≤ 5`; hitting 5 without termination is an escalation, not a
continuation.

### Per-pass steps

1. **Compose the prompt for pass N:**
   - Pass 1: use `tasks/v2-slice-<X>-initial-prompt.md` verbatim.
   - Pass N > 1: use the pass-1 prompt PLUS a **"Carryover — MUST close
     before APPROVED"** section listing each `carryover_close_now` item
     with its exact file location, the reviewer/perf agent's proposed
     fix, and the invariant that the pipeline "cannot return APPROVED
     with any of these still open." Also include a **"DEFER — do NOT
     reintroduce"** section listing every `defer_later` item so the
     coder does not accidentally re-add deferred work.

2. **Invoke `/new-feature`** with that prompt. Let it run its full
   architect → coder → reviewer → perf → docs pipeline.

3. **Read the outputs:**
   - `.agent-output/001-architecture-plan.md`
   - `.agent-output/002-implementation-log.md`
   - `.agent-output/003-review-findings.md`
   - `.agent-output/004-perf-findings.md`
   - `.agent-output/005-summary.md`

4. **Detect an architect-proposed further split.** In
   `.agent-output/001-architecture-plan.md`, look for language like
   "this slice is genuinely too large" / "propose splitting into <X.1,
   X.2>" (Slice-B's plan §0 had this pattern). If present, treat the
   split's *first* sub-slice as what the architect actually attempted
   this pass; queue the remaining sub-slice(s) as future passes with
   their own carryover, and note this in the iteration log.

5. **Classify every review + perf finding** per §"Classification" below
   into three buckets:
   - `CLOSE_NOW` — must be fixed before this slice can be declared done.
   - `CLOSE_NOW_HYGIENE` — small enough to fix in the next pass; roll
     into carryover unless the reviewer marked APPROVED already and no
     other CLOSE_NOW items exist (in which case defer to `next_slice_
     housekeeping.md` and terminate).
   - `DEFER_LATER_SLICE` — belongs to a later slice per the ADR
     readiness map. Cite the specific slice (D/E/F/G/H) and the ADR
     that owns it.

6. **Termination check:**
   - If `pass ≥ 5`: STOP. Report the current state and ask the human to
     re-scope. This is a slice-boundary problem, not an iteration
     problem.
   - Else, if `CLOSE_NOW = []` AND `CLOSE_NOW_HYGIENE = []` AND no
     architect-proposed further split remains: **terminate**. Go to §
     Termination below.
   - Else: append `CLOSE_NOW ∪ CLOSE_NOW_HYGIENE` to
     `carryover_close_now`, append `DEFER_LATER_SLICE` to
     `defer_later`, increment `pass`, loop.

### Termination

On successful termination:

1. **Commit each pass separately** if the pipeline is left with
   uncommitted work at the end of a pass. Preserve the pass boundary
   in git history.
2. **Update `tasks/v2-adr-readiness-map.md`** §Slice X — replace the
   pre-slice status line with a "Landed YYYY-MM-DD across N passes"
   paragraph and cite each pass's `.agent-output/005-summary.md` path.
3. **Append to `tasks/v2-slice-<X>-iteration-log.md`** the final
   termination row. Also record the cumulative `defer_later` list so
   the next slice's `/goal` invocation can consume it.
4. **Report back** to the human with: pass count, cumulative test
   delta on `tests/scripts/v2-progress.sh`, deferred items with their
   assigned slice, and any invariants activated.

## Classification

**Review findings** (from `.agent-output/003-review-findings.md`):

| Severity | Default | Exceptions |
|---|---|---|
| Critical | CLOSE_NOW | never DEFER — Critical never belongs to a later slice |
| High | CLOSE_NOW | never DEFER |
| Medium | Depends — see below | |
| Low | CLOSE_NOW_HYGIENE if trivial (≤ 5 line touch), else DEFER | |

**Medium classification decision tree:**
- Would this finding materialize as a bug once the slice's dispatch is
  actually wired end-to-end (not just under the slice's tests)? → CLOSE_NOW.
  (Example: Slice B's M1 "walker false-positive on `Star`" — this bug
  only appears once the emitter starts sending `SELECT *`, which is
  Slice C's job, so it's Slice C CLOSE_NOW, not Slice B's problem.)
- Is it a pure hygiene / style / naming concern with no correctness
  implication? → CLOSE_NOW_HYGIENE.
- Does it touch code owned by a later slice per the readiness map
  §Slice-<Y> entry? → DEFER_LATER_SLICE, citing Y.
- Is it a new "TODO Slice Y:" comment being added by the reviewer to
  document a known gap? → DEFER_LATER_SLICE.

**Perf findings** (from `.agent-output/004-perf-findings.md`):

| Severity | Default | Exceptions |
|---|---|---|
| HIGH | CLOSE_NOW | never DEFER |
| MEDIUM | CLOSE_NOW | DEFER if: (a) correctness risk with no
                     compensating gain at slice scale, (b) requires
                     changes outside slice scope, or (c) self-flagged
                     "optional" / "walker is fine as-is" by the perf
                     agent |
| LOW | DEFER | CLOSE_NOW if truly one-line (`Vec::with_capacity`, etc.) |

**Architect proposals in the plan document:**
- Any explicit "recommend splitting into <X.1, X.2>" → treat as
  further-split proposal (see §Loop step 4). NOT a DEFER unless the
  architect themselves classifies the second sub-slice as later-slice
  work.
- Any "open questions" (plan's `§Open questions` section) that name a
  choice this slice must make → the coder should have resolved them; if
  the reviewer flagged them as unresolved, that's CLOSE_NOW.
- Any "flag: <finding>" from the coder's log about a scope escalation
  they hit → treat as reviewer High-equivalent; CLOSE_NOW.

## Anti-patterns to avoid

- **Do not** re-run the differential suite between passes as part of
  this loop. CLAUDE.md's Quality Gate excludes it. Run it ONCE at final
  termination as the empirical progress-signal read.
- **Do not** carry a "TODO" comment forward as a substitute for closing
  the finding. If the reviewer said fix it, fix it — don't just note it.
- **Do not** let the pass count grow unbounded. Cap at 5. If pass 5
  still has CLOSE_NOW items, the slice boundary is wrong; escalate.
- **Do not** merge DEFER items into a "known gaps" list in the code.
  Deferred items live in `tasks/v2-adr-readiness-map.md` and the
  iteration log, not scattered as `// TODO` in source.
- **Do not** discard the architect's proposed sub-split by folding the
  second sub-slice into "just one more pass." Honor the split: each
  sub-slice is its own pass with its own carryover close.

## Iteration log format

`tasks/v2-slice-<X>-iteration-log.md`:

```markdown
# Slice <X> Iteration Log

## Pass 1 — YYYY-MM-DDTHH:MMZ
- Prompt: tasks/v2-slice-<X>-initial-prompt.md (verbatim)
- Verdict: APPROVED | NEEDS_CHANGES
- Architect proposed further split: no | yes (details)
- CLOSE_NOW carried forward: <count> (list)
- CLOSE_NOW_HYGIENE carried forward: <count>
- DEFER_LATER_SLICE: <count> (list with slice IDs)
- Progress signal: <baseline before → baseline after this pass>
- Commit SHA(s): <sha>...

## Pass 2 — YYYY-MM-DDTHH:MMZ
...

## Final termination — YYYY-MM-DDTHH:MMZ, pass N
- All within-slice items closed: yes
- Cumulative DEFER list handed off to readiness map: <count>
- Final progress signal: <count>/324
- Readiness map §Slice <X> updated: yes
```
