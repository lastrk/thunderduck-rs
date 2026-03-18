# Gap Analysis — Implementation Tasks

From `/workspace/docs/reference-gap-analysis.md`. Ordered by severity.

## Critical

- [x] **session-init** — Fix 5 session init gaps + initcap macro
  - Timezone: replace hardcoded `'UTC'` with system timezone
  - Add: `SET enable_progress_bar=false`
  - Add: `SET preserve_insertion_order=true`
  - Add: `SET allocator_background_threads=true` (Linux, 8+ cores only)
  - Add: `initcap` Spark-compatible macro
  - File: `crates/core/src/runtime/session.rs`

- [x] **expressions-and-plans** — Add missing Expression variants + SingleRowRelation
  - `Like { value, pattern, negated, case_insensitive }` → `(val [NOT] [I]LIKE pat)`
  - `Interval { months, days, microseconds }` → composite `INTERVAL '...' DAY + ...`
  - `IsDistinctFrom { left, right, negated }` → `left IS [NOT] DISTINCT FROM right`
  - `ExtractValue { child, extraction }` → `child[extraction]` / `child['field']`
  - `RowConstructor { fields }` → `(field1, field2, ...)`
  - `SingleRowRelation` LogicalPlan → no FROM clause in generator
  - Generator dispatch + unit tests for each
  - Files: `expression/mod.rs`, `logical/mod.rs`, `generator/mod.rs`

## Important (follow-up)

- [ ] **polymorphic-functions** — Schema-aware function resolution in generator
  - `reverse(array_col)` → `list_reverse(...)` not `reverse(...)`
  - `size(array)` → `len(...)`, `size(string)` → `length(...)`
  - Requires child schema context in SqlGenerator
  - Files: `generator/mod.rs`, `functions/mod.rs`

- [ ] **arrow-schema-fixup** — Phase 3 (needs gRPC layer first)
- [ ] **spark-connect-converters** — Phase 3 (DROP_NA, FILL_NA, UNPIVOT, etc.)
