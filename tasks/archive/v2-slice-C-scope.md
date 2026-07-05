# Slice C Scope — Core emission substrate + operator arms + scalar/aggregate arms

## §Targets

Cumulative per-sub-slice corpus targets (methodology §CV.7 / Open Decision 10):

**C.1 termination (~30-50 cases):** operator arms wire; analyzer-only-passing cluster turns green.
- `proj-001..015` (15)
- `filt-001..015` (15)
- `misc-003..005` (3), `misc-008..010` (3)
- `struc-001`, `struc-002`, `struc-005`, `struc-006` (4)
- Plus schema-only / nondeterministic cluster that C.1 unblocks (~10).

**C.2 termination (~120-150 cases, cumulative):** C.1 + scalar function arms.
- `cast-001..011` (11)
- `cond-*` (~12)
- `str-001..020` except `str-020` (spark4 → Slice G)
- `math-001..014` (14)
- `dt-002..017` except `dt-001` (⌂ schema-only)
- `ord-*` (12) except schema-only variants

**C.3 termination (~180-200 cases, cumulative):** C.2 + primitive aggregate arms.
- `agg-*` primitives (count/sum/avg/min/max/stddev/variance families; `agg-013` percentile_approx via §3.2)

## §ADRs

- **ADR-009** — declarative emission table (Approach A hand-written match arms permanent per Open Decision 7).
- **ADR-021** — τ owns substrate.
- **ADR-022** — τ is the only path; two error categories (Spark-emulated vs Thunderduck-boundary).

## §Inheritance-checklist sections

- **§3.1** — `sha` / `sha1` / `sha2` → `SHA256(arg_0)` (single-arg only). C.2 arm.
- **§3.2** — `percentile_approx` FLOAT CAST for quantile arg. C.3 arm.
- **§4.2** (first item) — `Cast::try_cast` branch emitting `TRY_CAST(expr AS ty)`. C.1 substrate.
- **§5.1** — `spark_return_cast` (projection-slot level) vs `spark_aggregate_return_cast` (aggregate-return level) kept SEPARATE. C.1 substrate.
- **§5.3** — `EMIT_TAP` counter + `EMIT_TAP_MUTEX` for INV2 activation. C.1 substrate.
- **§5.4** — `render_tail` CTE rewrite (not double-embedded `child_sql`). C.1 substrate.
- **§5.6** — `quote_ident` no-quote fast path for unquoted-safe identifiers. C.1 substrate.
- **§5.7** — `spark_aggregate_rewrite` helper for DECIMAL widening on `sum`/`avg`/`sum_distinct`. C.3 arm.

## §Sub-slice sketch

Sequential post-C.1 (C.2 and C.3 may proceed in parallel per readiness map §7).

**C.1 — Emission substrate + operator arms.** Deliverables:
- `crates/core/src/transpiler_v2/emission.rs` — new file. `EmittedSql` newtype, `EMIT_TAP` + `EMIT_TAP_MUTEX`, `dispatch_op(&TypedOp, &Schema)` exhaustive match, `render_expr` exhaustive match, `render_cast` with `Cast::try_cast` branch, per-operator renderers (`render_project`, `render_filter`, `render_sort`, `render_limit`, `render_tail` [CTE per §5.4], `render_distinct`, `render_with_columns`, `render_drop_columns`, `render_aliased_relation`, `render_table_scan`, `render_local_relation`, `render_range_relation`, `render_values`). `spark_return_cast` + `spark_aggregate_return_cast` separate helpers (§5.1). `quote_ident` fast path (§5.6). INV3 grep barrier tests.
- `crates/core/src/transpiler_v2/rewrites.rs` — created EMPTY per Open Decision 6 (Slice G populates).
- `extension_targets()` stub — empty at C.1, populated at Slice D.
- `mod.rs`: `generate()` invokes `analyze()` (Slice B) then `dispatch_op()` (Slice C.1) — real emission replaces the `<slice-b-analyzer-ok>` marker. Un-covered variants surface `EmissionError::UnsupportedOp` per ADR-022.

**C.2 — Scalar function arms.** Depends on C.1.
- `render_function_call` — ~130 arms across `str-*`/`math-*`/`dt-*`/`cast-*`/`cond-*` clusters. Approach A hand-written match. `sha`/`sha1`/`sha2` single-arg only (§3.1). Un-handled function names → `EmissionError::UnsupportedFunction`.

**C.3 — Primitive aggregate arms + DECIMAL widening.** Depends on C.1. Parallel with C.2.
- Primitive-aggregate arms in `render_aggregate` (count/sum/avg/min/max/stddev/variance families).
- `spark_aggregate_rewrite` helper (§5.7).
- `percentile_approx` FLOAT CAST for quantile arg (§3.2).

## §Non-goals (slice-specific)

- **Extension dispatch** (`extension_targets()` allow-list, ext6 arms, `spark_hash`/`spark_xxhash64`/`spark_skewness`/etc.) — Slice D.
- **Join emission** (`render_join`, SEMI/ANTI break, ambiguous-column resolution) — Slice E.
- **Set-op emission** (`render_union`/`render_intersect`/`render_except` with per-column CAST wrapper) — Slice E.
- **Complex-type emission** (array/map/struct/HOF/inline) — Slice F.
- **Vertical extensions** (temporal, grouping/pivot, windows, JSON, parsing) — Slice G.
- **SQL desugarings** (GROUPING SETS, PIVOT, LATERAL VIEW) — Slice G populates `rewrites.rs`.
- **Legacy modifications.**

## §Success criteria beyond §Targets

1. **INV2 companion active** — `EMIT_TAP` counter + `EMIT_TAP_MUTEX` present; `inv2_dispatch_is_only_sql_writer` test passes (dispatch table is the only writer to `EMIT_TAP`).
2. **INV3 grep barrier active** — `git grep -E 'use crate::(generator|functions)::' crates/core/src/transpiler_v2/emission.rs` returns zero. Coverage anchor: set of `render_*` helper names is greppable from source.
3. **`render_tail` uses CTE** (checklist §5.4) — no double-embedded `child_sql`.
4. **`spark_return_cast` and `spark_aggregate_return_cast` are separate helpers** (checklist §5.1) — projection-slot casts and aggregate-return casts do NOT share code path.
5. **`quote_ident` fast-path** (checklist §5.6) — unquoted-safe identifiers bypass the double-quote machinery.
6. **Approach A dispatch** (Open Decision 7) — hand-written `match` arms, no data-driven interpreter. No half-declarative row substrate.
7. **Empty `rewrites.rs` module** created at C.1 (Open Decision 6) — B-layer home for Slice G's SQL desugarings.
8. **`extension_targets()` stub** returns empty at C.1 (Slice D populates with 9-entry ext6 allow-list).
9. **`EmissionError::UnsupportedFunction`** returned for scalar function names τ doesn't yet handle (Thunderduck-boundary per ADR-022, not silent partial SQL).
10. **`Cast::try_cast` branch** emits `TRY_CAST(expr AS ty)` (checklist §4.2 first item).
11. **INV10 grep zero** — emission substrate imports only value-level types from outside τ.
12. **Quality Gate green** each pass.
13. **`v2-progress.sh` records the cumulative target** at each sub-slice's termination (C.1 ≈ 30-50, C.2 ≈ 120-150, C.3 ≈ 180-200 — empirically ±10%).
