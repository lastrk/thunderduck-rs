# Dev Journal — 2026-03-18 — Phase 2 Complete

## Summary

Phase 2 (`thunderduck-core` runtime layer — DuckDB execution + Arrow streaming) is complete,
reviewed, bug-fixed, and committed. The crate compiles clean; 47 Phase 1 unit tests still pass,
and 4 new integration tests bring the total to 51.

---

## What Was Built

The `runtime` module inside `crates/core` wires the Phase 1 SQL generator to an actual DuckDB
engine and streams Arrow `RecordBatch` results back to async callers.

| Module | File | Lines | Purpose |
|--------|------|------:|---------|
| `session` | `runtime/session.rs` | 215 | `DuckDbSession`: dedicated OS thread, mpsc+oneshot channels, Arrow streaming |
| `session_manager` | `runtime/session_manager.rs` | 53 | `SessionManager`: DashMap session pool |
| `compat_mode` | `runtime/compat_mode.rs` | 63 | `RuntimeCompatMode` enum + `resolve()` |
| `extension_loader` | `runtime/extension_loader.rs` | 32 | Bundled extension loader (stub until Phase 6) |
| `config` | `runtime/config.rs` | 76 | `StreamingConfig`, `HardwareProfile` |
| `error` (extended) | `src/error.rs` | 29 | Added `DuckDb` variant + `From<duckdb::Error>` |
| **Integration tests** | `tests/runtime_integration.rs` | 168 | 4 end-to-end tests |
| **Total new** | | **636** | |

The `crates/core` crate now has three non-test dependencies: `thiserror`, `duckdb` (bundled,
1.10500.0 = DuckDB 1.5.0), `dashmap`, and `tokio`.

---

## Key Architecture Decisions

### `duckdb::Connection` is `!Send` — dedicated OS thread per session

This is the central constraint driving the whole runtime design. DuckDB's connection is not
thread-safe and cannot be sent across thread boundaries or held across `.await` points. The
solution is one `std::thread` per session. The tokio async runtime never touches the `Connection`
directly; it communicates through channel pairs:

```
tokio task
  → mpsc::Sender<SessionCommand>
    → session thread (owns Connection, runs blocking_recv loop)
      → oneshot::Sender<SessionResult>
        → tokio task
```

`SessionCommand` and `SessionResult` are `pub(crate)` enums — the public surface is just
`DuckDbSession::execute()` and `DuckDbSession::create_temp_view()`, both `async`.

The thread is named (`duckdb-session-{id}`) for debuggability. On `Drop`, `DuckDbSession`
sends `SessionCommand::Shutdown` best-effort so the thread exits cleanly.

### Ready-channel handshake for synchronous startup

`DuckDbSession::spawn()` needs to propagate startup errors (extension load failure, init SQL
failure) to the caller. A `std_mpsc::sync_channel::<Result<()>>(1)` handles the handshake: the
session thread sends `Ok(())` when it is ready (or an `Err` if anything went wrong), and
`spawn()` blocks on `recv()` before returning. This keeps the API simple — callers get a
`Result<DuckDbSession>` and don't need to deal with a handle that might be broken.

### `DashMap::entry` for race-free session creation

`SessionManager::get_or_create` initially had the obvious implementation: look up the session
in the map, and if missing, spawn a new one and insert it. Under concurrent load this is a
TOCTOU race: multiple callers can all miss the lookup simultaneously, each spawn an independent
DuckDB thread, and the last one wins the map slot while the others are silently discarded.
The fix replaces the two-step get-then-insert with DashMap's `Entry` API:

```rust
match self.sessions.entry(session_id.to_string()) {
    Entry::Occupied(e) => Ok(Arc::clone(e.get())),
    Entry::Vacant(e) => {
        let session = Arc::new(DuckDbSession::spawn(session_id, self.mode, &self.config)?);
        e.insert(Arc::clone(&session));
        Ok(session)
    }
}
```

DashMap's `entry` takes a shard-level lock that covers both the check and the insert atomically.
Eight concurrent callers racing on the same session ID all receive `Arc`s pointing to the same
underlying object.

### `RuntimeCompatMode` as a three-value enum

The compat mode is expressed at two levels. `CompatMode` (Phase 1, in `functions/`) is a
two-value enum used by `SqlGenerator` and `FunctionRegistry` — it's either `Relaxed` or
`Strict`, nothing else. `RuntimeCompatMode` adds a third value, `Auto`, which is meaningful
only at session startup: load the extension if available and fall back to `Relaxed` otherwise.
`compat_mode::resolve()` converts `RuntimeCompatMode` → `CompatMode` at session creation time
by attempting the extension load and inspecting the result. After that point, the session's
compat mode is fixed.

### Arrow streaming via `query_arrow`

`duckdb`'s Arrow feature exposes `Statement::query_arrow()` which returns a stream of
`RecordBatch` without materialising a row-by-row result set. For SELECT-like statements
(`SELECT`, `WITH`, `VALUES`, `FROM`) the session thread uses `query_arrow` and collects
the batches. For DDL and DML (`CREATE`, `INSERT`, etc.) it uses `execute_batch`, which
returns nothing. The routing is a simple `starts_with` check on the uppercased SQL prefix —
crude but sufficient for the SQL shapes `SqlGenerator` produces.

### Extension loader stub

The `thdck_spark_funcs` DuckDB extension binaries are not bundled until Phase 6.
`extension_loader.rs` holds a single `const EXTENSION_BYTES: &[u8] = &[]` for all platforms.
When the slice is empty, `load()` returns `Ok(false)` immediately, which means every session
starts in Relaxed mode regardless of the requested compat mode (unless `Strict` is requested,
which returns an `Unsupported` error). The Phase 6 implementation will replace the constant
with `include_bytes!` pointing at platform-specific extension files in `extensions/`.

---

## Technical Problems Encountered

### Finding the right `duckdb` crate version for DuckDB 1.5.0

The `duckdb` crate uses an unconventional version scheme: `1.10500.0` maps to DuckDB 1.5.0
(the second component encodes the DuckDB minor and patch as a four-digit number: 1.05.00 →
10500). This isn't documented prominently in the crate README. The extension binary pinning
requirement in CLAUDE.md says the extension DuckDB version must exactly match the crate
version, so getting this right matters. The resolution was to read the crate's `Cargo.toml`
directly from crates.io to confirm the encoding scheme before writing the workspace dependency.

### `arrow` feature not needed in `crates/core`

The workspace `Cargo.toml` has `arrow = "57"` as a dependency and the `duckdb` crate is
declared with `features = ["bundled"]`. The `duckdb` crate's Arrow integration is exposed
through its own re-export (`duckdb::arrow`) when the `arrow` feature is enabled on `duckdb`
itself. Adding a separate top-level `arrow` dependency in `crates/core/Cargo.toml` would have
pulled in a second copy of the Arrow crate at a potentially mismatched version. The fix was to
not declare `arrow` in `crates/core/Cargo.toml` at all and use only `duckdb::arrow::...` import
paths in `session.rs`. Arrow types appear in the public API (`Vec<RecordBatch>`) but because
`RecordBatch` is re-exported from `duckdb::arrow`, callers that also depend on `duckdb` see the
same type — no duplication.

### DuckDB renamed `range_value` to `"range"` in v1.1.5

The Phase 1 generator emitted:

```sql
(SELECT range_value AS id FROM range(1, 4, 1))
```

Running `generator_to_duckdb` against an actual DuckDB 1.5.0 connection produced an error:
column `range_value` does not exist. DuckDB renamed the column produced by the `range()` table
function from `range_value` to `range` in version 1.1.5. The new column name is a reserved
keyword, so it also needs quoting:

```sql
(SELECT "range" AS id FROM range(1, 4, 1))
```

This was caught immediately by the `generator_to_duckdb` integration test, which validates the
full `LogicalPlan → SQL → DuckDB → Arrow` pipeline. Without the runtime layer there was no way
to catch this kind of drift between what the generator produces and what DuckDB actually accepts.

---

## Code Review Pass

After the initial implementation, a senior Rust code review identified **7 issues** across the
new runtime modules plus one cross-cutting issue in `error.rs`. All were fixed before the
commit was finalised.

| # | Location | Issue | Fix |
|---|----------|-------|-----|
| 1 | `extension_loader.rs` | Five identical `#[cfg(...)]` blocks — one per platform — each containing a `const EXTENSION_BYTES: &[u8] = &[]` | Collapsed to a single unconditional `const EXTENSION_BYTES: &[u8] = &[]`. Platform specificity matters only when real extension binaries exist (Phase 6). |
| 2 | `session.rs` | `_batch_size: usize` field dead-stored from `StreamingConfig::batch_size` but never used | Removed. The `_config` parameter is kept for future wiring but is not accessed yet. |
| 3 | `session.rs` | `SessionCommand` and `SessionResult` were `pub` — exported from the crate | Changed to `pub(crate)`. These are internal channel types; only `DuckDbSession`'s async methods are public. |
| 4 | `session.rs` | `create_temp_view` matched `SessionResult::Batches(_)` with a silent `Ok(())` | Changed to `unreachable!("CreateView never returns batches")`. The `CreateView` command path never produces `Batches`; silently succeeding would mask a logic error. |
| 6 | `session_manager.rs` | TOCTOU race in `get_or_create` (see above) | Replaced with DashMap `Entry` API. Had a concrete failing test (see below). |
| 7 | `compat_mode.rs` | `from_env()` called `std::env::var(...).unwrap_or_default().to_lowercase()` and immediately used the temporary `String` as `&str` in a match — borrow of temporary that wouldn't live long enough in some Rust editions | Extracted into an owned binding: `let val = ...; match val.as_str() { ... }`. |
| 9 | `error.rs` | `ThunderduckError::Arrow` variant and `From<ArrowError>` impl were dead code — Arrow errors from `query_arrow` surface as `duckdb::Error`, not `ArrowError` | Removed variant and impl. Arrow errors route through the existing `DuckDb` variant. |

Bug numbers are not contiguous because the review used a global numbering scheme across Phase 1
and Phase 2 items; Phase 1 consumed bugs 0–7 and Phase 2 started from the same pool.

### The TOCTOU test

The TOCTOU bug (issue 6) had a concrete reproduction test written before the fix was applied:

```rust
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn get_or_create_is_race_free() {
    const CONCURRENCY: usize = 8;
    let mgr = Arc::new(SessionManager::new(...));
    let barrier = Arc::new(tokio::sync::Barrier::new(CONCURRENCY));

    // All 8 tasks hit get_or_create at the same instant via barrier.
    let sessions: Vec<Arc<DuckDbSession>> = ...;

    let first = &sessions[0];
    for (i, other) in sessions[1..].iter().enumerate() {
        assert!(Arc::ptr_eq(first, other), "caller 0 and caller {} got different instances", i + 1);
    }
}
```

The test uses `Arc::ptr_eq` to verify pointer identity, not just value equality. With the
old get-then-insert implementation, concurrent callers all miss the lookup and each receive
a freshly-spawned session; they pass the map entry around but end up pointing at different
objects. With `Entry`, the first caller to acquire the shard lock inserts and all others get
a clone of its `Arc`. The test went from failing with pointer-inequality assertions to passing
in one commit.

---

## Test Suite

| Suite | Count | Notes |
|-------|------:|-------|
| Unit tests (`cargo test`) | 47 | All Phase 1 tests, unchanged |
| Integration tests (`--include-ignored`) | 4 | New in Phase 2; marked `#[ignore]` so they don't run in offline CI |
| **Total** | **51** | |

The four integration tests are:

- `session_round_trip` — creates a temp view from a `range()` query, selects from it, verifies
  column names and values across all returned batches.
- `session_manager_isolation` — two sessions with different IDs cannot see each other's tables
  (each session has its own in-memory DuckDB database).
- `generator_to_duckdb` — assembles a `LogicalPlan` tree by hand (`Project` over `RangeRelation`),
  runs `SqlGenerator::generate()`, executes the resulting SQL in a real DuckDB session, and
  confirms the row count. This is the full end-to-end pipeline proof.
- `get_or_create_is_race_free` — 8-way concurrent TOCTOU regression test (described above).

The integration tests are `#[ignore]` because they spin up DuckDB connections and OS threads,
which is acceptable in a development environment but too heavy for a fast unit-test cycle. The
intent is to gate them in CI as a separate `cargo test -- --include-ignored` step once the CI
pipeline is set up in Phase 3.

---

## Architecture Validated

1. **`!Send` constraint fully contained** — `duckdb::Connection` never crosses a thread boundary
   or appears in an `async` function. All session work is `blocking_recv` on the session thread.
   The compiler enforces this: the `SessionCommand` enum does not contain a `Connection`, and
   `DuckDbSession` is `Send` because it only holds a `mpsc::Sender`.

2. **`to_sql()` invariant still holds** — The runtime layer calls `SqlGenerator::generate()` and
   passes the resulting `String` to DuckDB. No `Display` or `Debug` usage crept into the SQL
   execution path.

3. **Phase 1 unit tests unaffected** — Adding `duckdb`, `dashmap`, and `tokio` to the crate
   did not change any Phase 1 behaviour. All 47 unit tests pass without modification.

4. **`ThunderduckError` is `Clone + PartialEq`** — The error type must cross the `oneshot`
   channel from the session thread to the async caller. Removing the `Arrow` variant kept
   the derive macros working (the removed `ArrowError` type is not `Clone` or `PartialEq`).

---

## Commits

| Hash | Message |
|------|---------|
| `35153f0` | docs: Phase 1 dev journal entry |
| `6bff5c2` | feat: Phase 2 complete — DuckDB runtime + Arrow streaming (51 tests pass) |

---

## Post-Commit Additions — Reference Gap Closure

After the initial Phase 2 commit, a gap analysis against the Java reference
(`docs/reference-gap-analysis.md`) identified critical omissions. All critical and important
items from Phase 1/2 scope were addressed in a follow-up pass.

### Session init gaps (`runtime/session.rs`)

Five settings present in `DuckDBRuntime.configureConnection()` were missing from the Rust port:

| Setting | Fix |
|---------|-----|
| `SET TimeZone` hardcoded `'UTC'` | Added `detect_timezone()`: TZ env → `/etc/timezone` → `"UTC"` |
| `SET enable_progress_bar = false` | Added to init SQL |
| `SET preserve_insertion_order = true` | Added to init SQL |
| `SET allocator_background_threads = true` | Added, gated on `#[cfg(target_os = "linux")]` and `hw.cpu_threads >= 8` |
| `initcap` macro | Added `CREATE OR REPLACE MACRO initcap(s) AS regexp_replace(...)` — whitespace-only word boundaries matching Spark semantics |

The timezone bug was a correctness issue: `hour()`, `date_trunc()`, and similar timestamp
functions returned wrong results in any non-UTC environment.

### Missing Expression variants and LogicalPlan

Six constructs from `expression/` and `logical/` in the reference had no Rust equivalent:

| Type | SQL produced | Notes |
|------|-------------|-------|
| `LikeExpression` | `(val [NOT] [I]LIKE pattern)` | Critical — ubiquitous in filters |
| `IntervalExpression` | Composite `INTERVAL 'n' DAY + ...` | Three sub-types: year-month, day-time, calendar |
| `IsDistinctFromExpression` | `left IS [NOT] DISTINCT FROM right` | Null-safe equality; always non-nullable |
| `ExtractValueExpression` | `child['key']` / `child[idx]` | String keys use single-quoted bracket form |
| `RowConstructorExpression` | `(a, b, c)` | Tuple comparisons: `WHERE (x, y) IN (...)` |
| `SingleRowRelation` | *(no FROM clause)* | `SELECT 1`, `SELECT now()`, scalar UDFs |

`gen_project()` detects `SingleRow` input and omits the `FROM` clause entirely.

`gen_interval()` ports the Java three-path decomposition faithfully:
- Year-month only → `INTERVAL 'n' MONTH`
- Day-time (microseconds only) → decompose into DAY/HOUR/MINUTE/SECOND parts, sign on first non-zero
- Calendar (mixed) → join month + day + second components with ` + `

### Code review findings (post-agent)

Three issues found and fixed during the senior review pass:

| # | Location | Issue | Fix |
|---|----------|-------|-----|
| 1 | `expression/mod.rs` | `let func_type = match {...}; func_type` in `Window` arm of `data_type()` | Collapsed to direct match arm — intermediate binding added nothing |
| 2 | `generator/mod.rs` | `crate::expression::Expression::Literal` / `crate::expression::LiteralValue::String` in `gen_extract_value()` | Shortened to `Expression::Literal` / `LiteralValue::String` — both already in scope |
| 3 | `generator/mod.rs` | Dead `if parts.is_empty()` guard in `gen_interval()` calendar branch | Removed — unreachable because the all-zero case is caught by the earlier `!has_months && !has_days` early-return |

### Test count

| Milestone | Tests |
|-----------|------:|
| Phase 1 complete | 47 |
| Phase 2 complete | 51 |
| Gap closure (this pass) | **62** |

16 new unit tests added: 3 for new expression types in `expression/mod.rs`, 1 for
`SingleRow` schema in `logical/mod.rs`, 12 generator tests covering all new
expression variants and the no-FROM `SingleRow` path.

---

## Commits

| Hash | Message |
|------|---------|
| `35153f0` | docs: Phase 1 dev journal entry |
| `6bff5c2` | feat: Phase 2 complete — DuckDB runtime + Arrow streaming (51 tests pass) |
| *(pending)* | feat: close Phase 1/2 gaps — session init, 5 expression variants, SingleRowRelation (62 tests) |

---

## Phase 3 Preview

Phase 3 wires in the gRPC layer (`crates/connect-server`): a `tonic`-based Spark Connect
service that receives protobuf `Relation` messages, converts them to `LogicalPlan` trees via
`RelationConverter` and `ExpressionConverter`, runs them through `SqlGenerator`, and streams
Arrow batches back over gRPC. The session threading model from Phase 2 is the foundation;
Phase 3 adds the protobuf converter layer on top of it.

The six deferred `LogicalPlan` variants (`DropColumns`, `Unpivot`, `Pivot`, `FillNa`, `DropNa`,
`Replace`) can be added as the converter is built, since by then there is a session available
to query DuckDB for schema information — the one blocker that made them impractical in Phase 1.
