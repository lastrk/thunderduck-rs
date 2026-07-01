# Lessons

Update after any user correction, review finding, or workflow-shape observation worth
generalizing. Terse; one bullet per lesson; cite the concrete instance.

---

## Workflow shape

- **Substrate-only slices are the right shape when the runway to the next unlock is long.**
  Slice B (v2 analyzer, 2026-07-01) landed as substrate with an honest zero-delta on the
  differential progress signal (`v2_progress.md` stayed 12/324). Fighting for a fake `+N`
  before Slice C's dispatch was wired would have forced adapter shortcuts. The `/new-feature`
  pipeline handled this cleanly — the pipeline's summary just reported "0 corpus cases;
  substrate for +5..+15 once Slice C lands." Do not force differential movement out of a
  slice whose ADRs (here ADR-005, ADR-006) explicitly own only typing, not emission.

## ADR-015 discipline (differential oracle > plan document)

- **The legacy `TypeInferenceEngine` is the oracle for Spark-parity coercion, not the plan.**
  In Slice B, `smoke_type_019` (`type-019`: `Decimal(5,0)` unionByName `Decimal(10,2)`) had a
  plan-document expected value of `Decimal(11,2)`. The coder ran the legacy
  `unify_decimal(5,0,10,2)` and got `Decimal(10,2)` — precision =
  `min(max(5-0, 10-2) + max(0,2), 38) = 10`. Fixture updated to match the oracle, not the plan.
  This is the ADR-015 pattern working as designed: LLM-extracted rules stay untrusted until
  the oracle validates. Reuse verbatim; do not re-derive.
