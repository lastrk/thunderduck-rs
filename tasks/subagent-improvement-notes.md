
## rust-coder
- 2026-07-09 (corpus pass 3): when a crate has multiple test binaries, the
  final `cargo test` summary contains one `test result:` block per binary —
  the agent quoted the LAST block (an all-ignored differential harness: "0
  passed, 15 ignored") as the gate result instead of the 102-passed unit
  block. Improvement: report gate results as the SUM across result blocks,
  or quote all blocks; never just the last one.
- 2026-07-09 (corpus pass 9): agent reported "364 passed, 20 failed — all
  TPC, expected fitness signal" when the baseline was 369/15 — five
  previously-green TPC cases had regressed. "Red TPC is expected" (ADR-022)
  applies to cases that were ALREADY red, never to newly-red ones.
  Improvement: corpus verification must DIFF against the recorded baseline
  (count comparison at minimum), not classify failures by family.
