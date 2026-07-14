# Review 006 — F1-F4 USING/default_projections cluster

VERDICT: APPROVE (Critical + High: 0, Medium/Low: 0)

Mechanism traced by hand for all four scenarios:
- F1: drop merge path filters default_slots() by name (case-insensitive),
  hoisted schema order; `* EXCLUDE` only over wraps (inner renders its own
  defaults) or the degenerate all-dropped case. Drop-over-filter-over-join
  merges with defaults intact; drop-over-sort wraps correctly.
- F2: build_join_side peeks from_ref()/pure_from() before consuming; the
  non-inline branch wraps the ORIGINAL block (defaults intact). Ladder
  behavior-equivalent to the old form for every FromItem kind; the only
  divergence is the intended fix. Err arm provably unreachable, panic-free.
- F3: extend_default_projections appends generated columns in schema order;
  no-op on None (cx-007..009 `SELECT *` shape preserved).
- F4: render_project_merge_slots expands bare Star(None) to the slot list
  only when defaults exist; lone-star identity untouched; wrap path verbatim.

No new same-class holes: all default_projections consumers enumerated
(to_sql, drop, project-merge, lateral + 2 producers); explicit-slot-list
operators are immune to the shadow class; wrap leaves fresh defaults None
while the inner block renders its own.

Style/invariants: no unwrap/expect on new paths (one pre-existing expect
removed); /// docs on all new items; no SQL-string parsing; ADR-022 single
path; INV10 green; #[allow(wrong_self_convention)] on from_ref justified.

Tests: 4 of 6 pins fail-on-old-code with specific assertions; 2 no-op
guards legitimate. dept_name-for-budget test-data substitution justified.

(Report transcribed by the orchestrator from the reviewer's inline return —
the reviewer honored its read-only constraint.)
