# Slice E Scope — Set-op emission + Join emission + Streaming-query execution

## §Targets

Cumulative per-sub-slice corpus targets (methodology §CV.7 / Open Decision 10).

**E.0 termination (~30-100 cases empirically):** streaming-query execution wired. `crates/connect-server/src/service.rs:407` currently returns `Status::unimplemented("Slice E: streaming query execution over CommonAst")`. E.0 wires the execute path so C.1's already-landed emission arms actually run SQL against DuckDB. **This is a corpus-signal prerequisite for the entire slice** — without it, E.1 and E.2 arms land in dark code and cannot be verified against the corpus.

Cases that turn green once E.0 unblocks C.1's arms (empirical — Pass 1 architect should re-estimate via a preflight run once E.0 lands): `proj-001..015`, `filt-001..015`, `misc-003..005`, `misc-008..010`, `struc-001`/`002`/`005`/`006`, schema-only/nondeterministic cluster.

**E.1 termination (E.0 + ~5-10 cases):** set-op emission with per-column CAST wrapper.
- `set-*` widening cases (`set-001..N` per corpus — architect enumerates from `tests/integration/differential/dataframe_corpus.py`).
- `type-011`, `type-019`, `type-020` (three type-widening cases the morph track pinned to set-op widening).

**E.2 termination (E.1 + 18 cases):** join emission.
- `join-001..014` (14).
- `chain-001`, `chain-003`, `chain-005`, `chain-006` (4).

## §ADRs

- **ADR-006** — widened set-op CAST wrapper (per-column CAST from parent's widened schema). Applies to UNION, INTERSECT, EXCEPT per Open Decision 5b.
- **ADR-009** — declarative emission table. Approach A hand-written match arms permanent per Open Decision 7. `render_union`/`render_intersect`/`render_except`/`render_join` add new arms to `dispatch_op`'s `TypedOp` match.
- **ADR-021** — τ owns substrate.
- **ADR-022** — τ is the only path; two error categories (Spark-emulated vs Thunderduck-boundary).

## §Inheritance-checklist sections

- **§2.3** — Plan-ID qualifier encoding as first-class field on `TypedOp::Join` (`left_plan_ids: Vec<i64>`, `right_plan_ids: Vec<i64>` — verify against Slice B's `TypedOp::Join` shape) and on v2 `Expression::UnresolvedColumn` (`plan_id: Option<i64>`). Emission uses the structured plan_ids to decide subquery aliasing. Eliminates the `__plan_id_*` string-parsing legacy has to do.
- **§5.1** — `spark_return_cast` and `spark_aggregate_return_cast` remain SEPARATE. Already active from C.1; Slice E does not touch them.

Additional join-related items from the checklist (Pass 1 architect enumerates as they audit the corpus join cases): whichever `§3.x` or `§5.x` items map to join/set-op behaviors (e.g., natural-flat-join folding must break at SEMI/ANTI boundaries per Slice-C.1's plan §CLAUDE.md "Known Gotcha #4").

## §Sub-slice sketch

Sequential; each sub-slice's cumulative target includes prior sub-slices' targets.

**E.0 — Streaming-query execution wiring.** Deliverables:
- `crates/connect-server/src/service.rs::execute_streaming_query` — replace the unconditional `Status::unimplemented("Slice E: streaming query execution over CommonAst")` with real wiring: (a) call `transpile_relation(...)` (already present) to get SQL, (b) submit to the session's DuckDB thread via the existing `SessionCommand` channel, (c) stream Arrow record batches back via the response stream. The session-command / mpsc pattern already exists in legacy code — verify Slice A.3's `SessionCommand` transport is intact and route through it (INV3/INV10 allow: session/runtime imports are value-level, not SQL-generator imports).
- `render_null_literal_row` / `render_zero_row_relation` — if `dispatch_op` for `LocalRelation` with zero rows produces SQL that DuckDB rejects, fix here (Pass 1 architect verifies via preflight).
- Any C.1 arm that surfaces as red once execution runs → HALT-AND-FLAG-1 (diagnostic-first, `/fix-bug`) with the specific case ID.

**E.1 — Set-op emission with per-column CAST wrapper.** Depends on E.0. Deliverables:
- `render_union`, `render_intersect`, `render_except` in `emission.rs` — hand-written match arms in `dispatch_op` for `TypedOp::SetOp { kind, all, by_name, children }` (Slice B landed the variant). Apply per-column CAST from the parent's widened schema (analyzer already computed the widening per ADR-006 refinement + Open Decision 5b).
- UNION BY NAME → `EmissionError::UnsupportedOp` (Slice G owns per Open Decision 5b — do NOT wire in E.1).

**E.2 — Join emission.** Depends on E.1. Deliverables:
- `render_join` in `emission.rs` — hand-written match arm in `dispatch_op` for `TypedOp::Join { left, right, join_type, condition, left_plan_ids, right_plan_ids }` (verify Slice B's Join shape carries plan_ids per §2.3; if not, that's a substrate-extension sub-task — HALT-AND-FLAG-2 or in-slice fix).
- Natural-flat-join branch: fold `Join(Join(...), ...)` chains into a flat `FROM a JOIN b JOIN c ON ...` shape.
- **SEMI/ANTI break**: the flat-chain fold MUST break at SEMI/ANTI boundaries — folding across would change filtering semantics (CLAUDE.md Known Gotcha #4). Emit `SEMI JOIN` / `ANTI JOIN` (DuckDB syntax; no `LEFT` prefix, which is a parser error per Gotcha #5).
- `ensure_no_ambiguous_columns` — walk subquery bodies to surface any duplicate unqualified name at emission time as `EmissionError::UnsupportedExpression` (Thunderduck-boundary per ADR-022 — analyzer's central-ambiguity check from Slice B fix pass H1 should catch these upstream; ensure_no_ambiguous_columns is a defensive last-mile check).

## §Non-goals (slice-specific)

- **Scalar function arms** (Slice C.2) — `render_function_call` remains a stub returning `UnsupportedFunction`.
- **Aggregate arms** (Slice C.3) — `render_aggregate` remains a stub returning `UnsupportedOp`.
- **Extension dispatch** (Slice D) — `extension_targets()` remains empty; no ext6 arms.
- **Complex-type emission** (Slice F) — array/map/struct/HOF/inline remain `UnsupportedExpression`.
- **UNION BY NAME** — Slice G (Open Decision 5b).
- **`rewrites.rs` population** — Slice G.
- **Legacy modifications** — none. Do not touch `crates/core/src/{generator,functions,logical,expression,parser}/` or legacy converter modules.
- **Corpus regressions**: any case green at post-C.1 baseline (currently 0/324, so vacuously satisfied) must stay green.
- **Vertical extensions** — Slice G.

## §Success criteria beyond §Targets

1. `execute_streaming_query` no longer returns `Status::unimplemented`; a smoke case (SingleRow → `SELECT`) round-trips through the harness and returns a valid Arrow response.
2. `render_union`/`render_intersect`/`render_except` each land as new `dispatch_op` arms with per-column CAST wrapper honoring the analyzer's widened parent schema.
3. `render_join` lands with (a) all six `JoinType` variants (INNER/LEFT/RIGHT/FULL/SEMI/ANTI/CROSS) exhaustively matched, (b) natural-flat-join folding with SEMI/ANTI break, (c) DuckDB `SEMI JOIN` / `ANTI JOIN` syntax (no `LEFT` prefix).
4. `ensure_no_ambiguous_columns` walker present and tested.
5. First-class `plan_ids` on `TypedOp::Join` used at emission (§2.3) — no `__plan_id_*` string parsing.
6. INV2 companion, INV3 grep barrier, INV4/INV5/INV10 remain active (C.1's activations preserved).
7. `EmissionError` variants stay Thunderduck-boundary only; no Spark-emulated variants leak into emission.
8. Quality Gate green each pass.
9. `v2-progress.sh` records the cumulative target at each sub-slice's termination (E.0 ≈ 30-100 empirical; E.1 adds ~5-10; E.2 adds 18). Deltas measured against C.1 baseline of 0/324.
10. No corpus regressions on any case green at prior sub-slice termination.
