# Phase 2 Code Review — Bug Fixes

All bugs from the senior Rust review are resolved.

## Bugs with observable test failures

- [x] **Bug 6 — TOCTOU race in `SessionManager::get_or_create`**
  Fixed: replaced get → spawn → insert with DashMap `entry` API (atomic check-and-insert).
  Test `get_or_create_is_race_free` failed before fix, passes after.

## Code fixes (no meaningful failing test)

- [x] **Bug 1 — extension_loader: 5 identical `cfg` blocks**
  Collapsed to a single `const EXTENSION_BYTES: &[u8] = &[];`.

- [x] **Bug 2 — `_batch_size` dead code in `session.rs`**
  Removed. Parameter renamed to `_config` (kept for future wiring; not yet used).

- [x] **Bug 3 — `SessionCommand`/`SessionResult` over-exposed**
  Changed from `pub` to `pub(crate)`.

- [x] **Bug 4 — impossible `Batches(_)` arm in `create_temp_view`**
  Changed from silent `Ok(())` to `unreachable!("CreateView never returns batches")`.

- [x] **Bug 7 — `from_env` temporary borrow chain**
  Extracted owned `String` before matching; two-line form is clear and safe.

- [x] **Bug 9 — `ThunderduckError::Arrow` variant is dead code**
  Removed `Arrow` variant and `From<ArrowError>` impl. Arrow errors surface via
  duckdb::Error at `query_arrow` time, mapped through the existing `DuckDb` variant.
