# Retired task-tracker docs

Historical planning artifacts from the slice-based reimplementation phase
(retired 2026-07-02) and the v1-transpiler restart phase. Kept for
reference; **do not use as guidance** for new work.

## Slice-era templates & methodology

- `v2-slice-goal-prompt-template.md` — `/goal` template that pre-dated
  the corpus-driven approach.
- `v2-slice-iteration-methodology.md` — slice-per-pass loop rules.

## Per-slice scope + iteration logs

- `v2-slice-{A,B,C,E}-scope.md` — scope docs for individual slices.
- `v2-slice-{A,B,C,E}-iteration-log.md` — pass-by-pass logs from those
  slices' iterations.

## v1-transpiler restart artifacts

- `v2-restart-inheritance-checklist.md` — the concrete bugs the
  morph-track v1 debugging arc surfaced; kept for parity reference.
- `v2-restart-open-decisions.md` — open decisions during the restart.

## Dated snapshots

- `v2-architecture-review-2026-07-03.md` — architecture review notes.
- `v2-corpus-remaining-blockers-2026-07-03.md` — remaining-blocker
  triage.
- `v2-group-a-blockers-deep-dive-2026-07-03.md` — deep dive into a
  specific blocker group.

## v2-transpiler design & review artifacts (retired 2026-07-11)

- `phase2-design.md` — design for the Phase 2+2.1 ordinal requalifier +
  demand-driven join wrap. Shipped at `2b3d06c`; the `__td_jl/__td_jr`
  synthetic-alias machinery it targeted was fully retired in Phase 4
  (`1b7bf0a`, `023fc2e`).
- `phase3a-design.md` — design for Phase 3a (unify join-condition
  plan_id resolution, delete `qualify_plan_id_refs`). Shipped at `b85590e`.
- `phase3b-design.md` — design for Phase 3b (above-join refs drop side
  strings; synthetic tier retires). Shipped at `f431044`.
- `select-block-review-findings.md` — max-effort review of the
  `feat/select-block-emission` branch (15 findings). Closed: 11 FIXED,
  4 DEFERRED, 0 regressions, 10/10 witnesses green.
- `v2-corpus-driven-iteration-methodology.md` — the detailed per-pass
  "how" for the corpus loop; folded into `tasks/goal-corpus-to-100.md`
  (which leans on CLAUDE.md for the gate) and retired 2026-07-11.

## Active docs (kept in `tasks/`)

The current corpus-driven pipeline lives at:

- `tasks/goal-corpus-to-100.md` — active `/goal` (drive corpus to 100%).
- `tasks/v2-corpus-driven-goal-prompt-template.md` — prior `/goal` template.
- `tasks/v2-corpus-driven-pass-log.md` — authoritative pass log.
- `tasks/v3-corpus-driven-goal-prompt-template.md` — v3 pipeline template.
- `tasks/lessons.md` — lessons learned.
