# Plan 013 — F15 (derive SelectBlock scope from FromItem) — EXECUTABLE

**Tree:** `feat/v2-transpiler` in `/workspace`. Edit
`crates/core/src/transpiler_v2/sql_block.rs` only. Pure simplification — NO
behavior change. Removes the hand-maintained `SelectBlock.scope` copy so
merge-visibility can never desync from the actual FROM item.

## Change
`SelectBlock.scope: Vec<String>` (sql_block.rs ~215) duplicates
`self.from.exposed()`. It is set in `from_item` (= `from.exposed()`) and
mutated in `extend_from`; the only reader is `exposes()`. After `extend_from`,
`self.from` is a `FromItem::Raw { exposed }` whose `exposed` already equals the
updated `scope`, so `self.from.exposed()` == `self.scope` at all times. Remove
the field:

1. Delete the `scope: Vec<String>` field (~215) and its doc line (~214), and
   fix the module doc reference at ~14 (`[SelectBlock::scope]`) to point at
   `exposes()`/`FromItem::exposed()` instead.
2. `from_item` (~220): drop `let scope = from.exposed();` and the `scope,`
   initializer.
3. `extend_from` (~269): replace `let mut exposed = self.scope.clone();` with
   `let mut exposed = self.from.exposed();`, keep `exposed.extend(extra_aliases…)`
   and `self.from = FromItem::Raw { sql, exposed };`, and DELETE the trailing
   `self.scope.extend(extra_aliases);`. (`extra_aliases` is now consumed once
   into `exposed` — adjust the clone/move accordingly.)
4. `exposes` (~334): `self.from.exposed().iter().any(|a| a.eq_ignore_ascii_case(qualifier))`.

Let the compiler surface any other `self.scope` reader (grep says there is
none beyond the above). Update any unit test that referenced the field
(e.g. `wrap_uses_uniform_alias_and_scope` ~546 — keep its behavioral assertion,
just express it via `exposes(...)` instead of the raw field if it peeked it).

## Verification (coder — NO corpora, NO commits)
- `cargo check -p thunderduck-core`
- `cargo test -p thunderduck-core --lib` ALL green (979)
- `rustfmt --check` clean on sql_block.rs
- `cargo clippy -p thunderduck-core --lib` — no new warnings on touched lines
- Log to `.agent-output/013-implementation-f15.md`

## Acceptance (orchestrator)
Full `witness-progress.sh`: 0 regressions, 10/10 witnesses stay green (pure
refactor — nothing should move).
