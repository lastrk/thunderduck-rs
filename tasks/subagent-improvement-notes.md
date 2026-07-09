
## rust-coder
- 2026-07-09 (corpus pass 3): when a crate has multiple test binaries, the
  final `cargo test` summary contains one `test result:` block per binary —
  the agent quoted the LAST block (an all-ignored differential harness: "0
  passed, 15 ignored") as the gate result instead of the 102-passed unit
  block. Improvement: report gate results as the SUM across result blocks,
  or quote all blocks; never just the last one.
