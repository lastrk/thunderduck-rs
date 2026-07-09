# Known Gotchas

> **Scope: τ (the only production path per ADR-022).** Recurring bug classes and
> non-obvious constraints discovered the hard way. Pull this in before touching
> emission, the analyzer, the LocalRelation converter, extension loading, the
> session thread, or the Arrow wire boundary — each entry marks a trap that has
> already cost a debugging session.

1. **`to_sql()` vs `Display`**: SQL rendering MUST use the dedicated emission functions (`render_expr` / `dispatch_op`). `Display` / `Debug` are debug-only — never used to build SQL sent to DuckDB.

2. **`duckdb::Connection` is `!Send`**: Never move a Connection across thread boundaries or hold it across `.await` points. Use the session thread model.

3. **Composite aggregate expressions**: When adding expression types that can appear inside aggregates, ensure `V2RelationConverter::convert_aggregate()` handles them. A default `_` arm silently drops unknown cases.

4. **Semi/Anti join in flat chains**: the flat-chain rendering branch must break at semi/anti boundaries — folding the chain across one reorders filtering semantics. See `architecture.md` → Joins.

5. **DuckDB SEMI JOIN syntax**: DuckDB uses `SEMI JOIN` and `ANTI JOIN` (without `LEFT` prefix). `LEFT SEMI JOIN` is a parser error.

6. **Extension version pinning**: the `.duckdb_extension` binary's DuckDB version must exactly match the `duckdb` crate version, or `LOAD` fails. The full pin matrix (`ext6` → `v1.5.4` binaries → `duckdb` crate `1.10504.0`) lives in `dependencies.md` → Version Pinning.

7. **HUGEINT overflow**: DuckDB `SUM()` of integer columns returns `HUGEINT` (i128). Spark returns `BIGINT` (i64). SQL generation must emit explicit `CAST(... AS BIGINT)` for integer SUM.

8. **Schema inference vs DESCRIBE**: Prefer `plan.infer_schema()` for schema analysis. Only fall back to issuing `DESCRIBE` queries to DuckDB when plan-level inference is impossible.

9. **Loud-fail on unhandled Arrow types in `local_relation_to_values_sql`**: τ's LocalRelation converter has a loud-fail rule: no catch-all `Ok` fallbacks on typed dispatch. Silent `_ => Ok("NULL")` catch-alls used to map every unhandled Arrow type (including `Decimal128`) to SQL literal `NULL`, corrupting `createDataFrame` payloads. Adding a new Arrow-type payload requires a real match arm, not a silent NULL.

10. **Extension materialization must use a unique per-session temp path**: `extension_loader::load()` writes the embedded `thdck_spark_funcs.duckdb_extension` bytes to a temp path, `LOAD`s it, then removes it. Because `SessionManager` creates sessions concurrently, a *shared fixed* path (e.g. `/tmp/thdck_spark_funcs.duckdb_extension`) lets one session's `remove_file` delete the bytes out from under another session's `LOAD` — a spurious "not a DuckDB extension / at least 512 bytes" error. Each `load()` uses a unique per-process/per-call *directory* (`thdck-ext-<pid>-<seq>/`) but keeps the canonical filename, because DuckDB derives the `_duckdb_cpp_init` entrypoint symbol from the file stem — renaming the file breaks entrypoint resolution.

11. **Data-dependent output schemas need a session-injected discovery pre-pass, not an inline analyzer query**: values-less `.pivot(col)` has an output schema (one column per distinct pivot value) that is unknowable from types alone — it depends on the live data. The τ analyzer is a pure, synchronous stage barred by INV10 from importing `crate::runtime`, so it cannot run the eager `SELECT DISTINCT`; it correctly punts (`PuntedOperator("Pivot[implicit-values]")`) on an empty `pivot_values`. Discovery lives in the connect-server async dispatch layer (`resolve_implicit_pivots`/`discover_pivot_values` in `service.rs`), which holds the `DuckDbSession`: it runs the DISTINCT via the async session channel, rewrites the `CommonOp::Pivot` node to the explicit-values shape *before* `finalize`/`analyze`, then flows through the unchanged explicit-values path. `crosstab(col1, col2)` now rides the same pass (its mirror image): `resolve_implicit_pivots` discovers `col2`'s distinct buckets from the live session, then `analyzer::crosstab_to_aggregate` desugars the `CommonOp::Crosstab` into a conditional-count `Aggregate` *before* `finalize`/`analyze`. Any future op with a data-dependent schema must route through this same eager pre-pass rather than inventing a second mechanism or querying from inside the analyzer.

12. **`.na.fill(value)` must apply Spark's type-compatibility filter server-side**: When the PySpark Connect client sends `NAFill { cols: [] }` (the "single value, no subset" form), Spark's `DataFrameNaFunctions.fillValue` silently skips columns whose static type does not match the fill value's type — only numeric↔numeric, string↔string, and boolean↔boolean pairs fill; every other combination passes through untouched (no COALESCE, nullability preserved). Both `analyzer.rs::analyze_na_fill` (nullability inference) and `emission.rs::render_na_fill` (SQL emission) MUST gate on the shared `na_fill_compatible` predicate (`pub(super) fn` in `analyzer.rs`) — otherwise τ COALESCEs mismatched types and DuckDB throws `Cannot mix values of type VARCHAR and BIGINT in COALESCE` while Spark silently passes the column through. The single-value subset branch (`values.len() == 1`) applies the same predicate; the multi-value dict form pairs by position without filtering (matches Spark's dict form).

13. **Arrow interval wire-encoding is transcoded at the connect-server Arrow boundary — never in τ.** DuckDB emits every SQL `INTERVAL` column as Arrow `Interval(MonthDayNano)`; Spark 4.1 uses a different Arrow type per interval kind (`DayTimeInterval` → `Duration(us)`, `CalendarInterval` → `Interval(MonthDayNano)`, `YearMonthInterval` → `Interval(YearMonth)`). INV10 forbids τ (`transpiler_v2/`) from touching wire encoding, so the fix is a **per-column data rewrite at the Arrow boundary** (`arrow_interval_transcode.rs`, on the tonic task after the mpsc hop — preserving `!Send` `Connection` isolation) — do NOT try to "fix" it in the analyzer or emission. Mechanism: `architecture.md` → Streaming and Arrow interval transcoding.

14. **A dedicated `ExecutePlanResponse.schema` frame is mandatory for any interval-column result.** PySpark's Arrow-schema fallback (`from_arrow_type`) has no `is_interval` arm and raises `UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION` on `Interval(*)`; emitting a schema frame first (from τ's `resolved_schema`) routes the client through `proto_schema_to_pyspark_data_type`, which handles all three kinds. Any op with a client-unfriendly Arrow schema must ride this same schema-frame pattern rather than relying on the fallback. Note: `CalendarInterval`/`YearMonthInterval` still fail on the client's *row* decoder regardless (corpus tracks them with `expected_error=UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION`; both engines fail identically → tri-state PASS). Mechanism: `architecture.md` → Streaming and Arrow interval transcoding.
