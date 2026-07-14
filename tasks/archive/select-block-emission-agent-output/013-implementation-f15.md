# 013 — Implementation log: F15 (derive SelectBlock scope from FromItem)

**File touched:** `crates/core/src/transpiler_v2/sql_block.rs` only (as scoped).
No other file was edited.

## Changes (matches plan's 4 edits exactly)

1. **Removed the `scope: Vec<String>` field** and its doc comment from
   `SelectBlock` (was ~line 215). Fixed the module-doc reference at the top
   of the file (was `[SelectBlock::scope]`) to instead point at
   `[SelectBlock::exposes]` (backed by `[FromItem::exposed]`).
2. **`from_item`**: dropped `let scope = from.exposed();` and the `scope,`
   struct-literal field.
3. **`extend_from`**: replaced `let mut exposed = self.scope.clone();` with
   `let mut exposed = self.from.exposed();`; kept
   `exposed.extend(extra_aliases)` (simplified — no longer needs
   `.iter().cloned()` since `extra_aliases` is now consumed exactly once,
   directly into `exposed`, instead of being cloned into `exposed` and then
   separately extended into `self.scope`); kept
   `self.from = FromItem::Raw { sql, exposed };`; deleted the trailing
   `self.scope.extend(extra_aliases);` (no longer applicable — there is no
   `self.scope` to update).
4. **`exposes`**: now reads
   `self.from.exposed().iter().any(|a| a.eq_ignore_ascii_case(qualifier))`
   instead of iterating the cached `self.scope` Vec.

No other `self.scope` reader existed in the file — the compiler surfaced
none, matching the plan's expectation.

## Test changes

None needed. The one test the plan flagged as a possible peek —
`wrap_uses_uniform_alias_and_scope` (~line 546) — already asserted purely
via `b.exposes(...)`, not the raw field, so no edit was required. Grepped
the whole file for `.scope` / `scope:` post-edit; the only remaining hit is
an unrelated doc comment ("A join living flat in ONE FROM scope:") that
predates this change and refers to the general SQL concept, not the field.

## Verification (all green)

- `cargo check -p thunderduck-core` — clean, no warnings.
- `cargo test -p thunderduck-core --lib` — **979 passed; 0 failed; 5
  ignored** (unchanged from baseline — a pure refactor moves no test, no
  new/removed test observed).
- `rustfmt --check` on `sql_block.rs` — clean (exit 0).
- `cargo clippy -p thunderduck-core --lib -- -D warnings` — 2 pre-existing
  errors surfaced, both unrelated to this change and outside the touched
  file: `too_many_arguments` in `parser_v2/v2_lowering.rs::reject_unsupported_view_clauses`
  and `map_entry` in `runtime/session.rs`. No clippy output at all
  referencing `sql_block.rs` (confirmed by grepping clippy's output for
  `sql_block` — zero hits), and a plain `cargo clippy -p thunderduck-core
  --lib` (no `-D warnings`) likewise shows nothing for `sql_block.rs`.

## Deviations from plan

None. All 4 edits applied exactly as specified; no additional readers of
`self.scope` were found; no test needed updating.

## Performance note (per task instructions — not acted on)

`exposes()` now calls `self.from.exposed()` on every invocation instead of
reading a pre-cached `Vec<String>`. `FromItem::exposed()` allocates a fresh
`Vec` (cloning leaf alias lists, or recursing/concatenating for join/derived
shapes) on each call, so a merge-heavy emission path that calls `exposes()`
repeatedly against the same block now redoes that work each time rather than
reading a cached copy. This is unlikely to matter in practice (`exposed()`
walks a shallow `FromItem` tree — no more than a few joins deep per query —
and the field itself was already `O(n)` to build once), but it is a real
trade of a cached read for a derived one. Per the task, no caching was
added; flagging only for awareness.
