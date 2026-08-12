# Spark Parity Report

_Read-only audit against Apache Spark 4.1.1. Structured Streaming is out of scope._

## Verdict

At `f34024b3`, Thunderduck supports a broad, credible Spark 4.1.1 batch-query subset—not the general non-streaming Spark API.

The strongest area is relational DataFrame and Spark SQL query execution. The weakest areas are session/catalog state, extensibility, plan introspection, caching, ML/Pandas APIs, and the wider command protocol.

The honest-boundary architecture works well in the Connect converter and analyzer. Its main weakness is SQL lowering and several compatibility stubs, where accepted input can currently be silently weakened instead of implemented or rejected.

## Surface scorecard

These counts measure recognized protocol shapes, not equally weighted features or compatibility percentages.

| Surface | Recognized | Assessment |
|---|---:|---|
| Connect relations | 36 of 57 non-streaming envelopes | Broad relational core; 21 immediate boundaries |
| Connect expressions | 14 of 22 variants | Strong ordinary expression support |
| Commands | 3 of 16 non-streaming variants | Narrow |
| Catalog operations | 8 of 26 | Mostly read-only and partly synthetic |
| Analyze requests | 1 of 13 non-streaming variants | Schema only |
| Function registry | 352 public spellings | Broad but overload/type support is bounded |

The relation count consists of 34 direct converter arms plus SQL and Catalog prepasses. It overstates semantic support slightly: Catalog is partial, and hints/repartition operations are intentionally cosmetic.

The primary dispatch points are [relation conversion](crates/connect-server/src/converter/v2_relation_converter.rs), [expression conversion](crates/connect-server/src/converter/v2_relation_converter.rs), and [service command handling](crates/connect-server/src/service.rs).

## DataFrame API supported in practice

The implemented batch core includes:

- `createDataFrame`, `range`, named tables, and `spark.sql`.
- `select`, `selectExpr`, aliases, filters, sorting, limits, and offsets.
- `withColumn(s)`, `drop`, renames, `toDF`, and schema conversion.
- Inner, left, right, full, cross, left-semi, and left-anti joins.
- Union, intersect, except, and bounded `unionByName`.
- `groupBy`, aggregation, rollup, cube, grouping sets, and pivot.
- `distinct` and `dropDuplicates`.
- NA fill, drop, and replace.
- Unpivot.
- `describe`, `summary`, `freqItems`, covariance, correlation, approximate quantiles, and crosstab.
- Sampling without replacement and stratified sampling.
- Arrow result collection and actions expressible through relational plans.

Partially supported or cosmetic:

- Repartitioning, repartition-by-expression, coalesce-like shapes, and hints are accepted without meaningful distributed physical effects.
- Sampling with replacement reaches an honest boundary.
- Natural joins are supported through SQL, not the Connect join envelope.
- By-name intersect/except are bounded.
- Dynamic pivot and crosstab require service-side runtime preparation.

Important unsupported DataFrame families include:

- `show`/ShowString and `tail`.
- `mapPartitions`, Pandas/group-map/co-group-map operations.
- Observations and collection metrics.
- ASOF joins and transpose.
- Cache, persist, checkpoint, and storage-level APIs.
- UDF, UDAF, UDTF, and data-source registration.
- ML operations.
- General table-valued functions.
- Connect relation subquery substitution.
- V2 writes and table-oriented writes.

## Spark SQL support

SQL query support is considerably broader than the command API:

- Projection, aliases, stars, `DISTINCT`, and predicates.
- Inner/outer/semi/anti/cross joins, including `ON`, `USING`, and `NATURAL`.
- Aggregation, `HAVING`, group ordinals, rollup, cube, and grouping sets.
- Ordering, ordinals, limits, and offsets.
- Union/intersect/except/minus with `ALL`/`DISTINCT`.
- CTEs, nested CTEs, and a bounded recursive-CTE form.
- Scalar, `IN`, `EXISTS`, nested, and correlated subqueries.
- Windows, ranking, aggregate windows, named windows, and ROWS/RANGE frames.
- CASE, casts, typed literals, arrays, maps, structs, JSON, CSV, and lambdas.
- Lateral views and the eight structured generators.
- Explicit-value pivot and standard unpivot.
- `VALUES`, `range`, and format-qualified paths.
- The full 22-query TPC-H corpus and broad TPC-DS coverage.

The SQL statement IR itself has only eight implemented statement classes in [statement.rs](crates/core/src/transpiler_v2/statement.rs):

- Create temporary view.
- Create view.
- Simple create table.
- Drop table.
- Drop view.
- Insert values.
- Insert-select.
- Truncate table.

Consequently, SQL `UPDATE`, `DELETE`, `MERGE`, `ALTER`, `SHOW`, `EXPLAIN`, `CACHE`, rich CTAS/table options, and most administrative DDL are boundaries.

Other notable query boundaries include:

- SQL union-by-name.
- GROUPS window frames.
- Dynamic pivot values.
- General table functions and UNNEST.
- Outer lateral joins.
- Time travel.
- HiveQL transforms.
- Pipe syntax.
- Richer recursive-CTE forms.
- DataFrame transformations wrapped around a raw SQL relation—the five `sqlwrap-*` corpus failures.

## Functions

The closed registry contains 352 public spellings:

| Implementation class | Count |
|---|---:|
| Scalar | 159 |
| Aggregate | 60 |
| Generator | 8 |
| Special | 123 |
| Frontend-lowered | 2 |

The generators are `explode`, `explode_outer`, `inline`, `inline_outer`, `json_tuple`, `posexplode`, `posexplode_outer`, and `stack`.

This is a substantial function surface, but “352 names” does not imply every Spark overload, coercion, modifier, or input type is supported. Unknown functions correctly reach a boundary rather than falling through to arbitrary DuckDB behavior.

A lexical witness audit found:

- 253 of 348 identifier-like spellings called in the two differential corpora.
- 285 of 348 called somewhere in the test tree.
- 63 without a literal call-site witness.

Aliases and shared implementation rules mean this is a coverage signal, not proof that 63 functions are untested. The registry is in [function_registry.rs](crates/core/src/transpiler_v2/function_registry.rs).

## Reads, writes, and types

Reads:

- Named tables and local Arrow relations.
- Parquet, CSV/text, JSON, and Delta.
- ORC is recognized but honestly bounded.
- Delta currently supports one directory per scan.
- Data-source options are passed as DuckDB reader options rather than comprehensively translated from Spark semantics.

Writes:

- Delta append to an existing path.
- Parquet overwrite to a path.
- Table save targets, WriteOperationV2, other format/mode combinations, and Delta overwrite are unsupported.
- Partitioning, bucketing, clustering, and write sort options are currently ignored rather than bounded.

Types include ordinary numeric types, decimal, strings/binary, date/timestamps, exact year-month and day-time interval spans, calendar intervals, arrays, maps, and structs. Char/varchar normalize to string. UDT, Variant, geometry/geography, time, and unparsed types remain unresolved and are rejected.

## Explicit, well-designed honest boundaries

Several difficult cases are handled particularly well:

- Unknown relation, expression, and function variants do not fall through.
- DataFrame plan-ID resolution preserves Spark identity, qualifier, and nullability semantics.
- Source metadata-column access is bounded because metadata outputs are not represented.
- Hidden donor columns from `USING`/natural joins are bounded where public output cannot represent them faithfully.
- Nested DataFrame-star and regex expansion is bounded instead of incorrectly applying a one-to-many rewrite in scalar contexts.
- Connect subquery expressions are distinguished from supported SQL subqueries.
- Named-table `IDENTIFIER(...)` syntax has an explicit boundary.
- Unsupported table-function and data-source shapes fail deliberately.

This is where the architectural investment in typed IR and closed enums is paying off.

## Where the honest boundary currently leaks

These are the most important findings.

### 1. SQL TABLESAMPLE is silently ignored

The parser retains `TableFactor.sample`, but lowering discards it. Existing corpus cases are nondeterministic/schema-only, so they do not validate sampled rows.

That is a wrong-result risk, not an honest boundary.

### 2. SORT BY, CLUSTER BY, and DISTRIBUTE BY are dropped

The SQL AST stores these clauses, but [v2_lowering.rs](crates/core/src/parser_v2/v2_lowering.rs) does not consume them. Schema-only tests hide the missing ordering/partitioning semantics.

### 3. HAVING without aggregation is discarded

`HAVING` does not itself cause aggregate lowering. A query such as:

```sql
SELECT * FROM VALUES (1), (2) AS t(x) HAVING x > 1
```

is rejected by Spark 4.1.1 with `MISSING_GROUP_BY`, while τ can lower it as a plain projection and lose the clause.

### 4. Function syntax modifiers are dropped

The SQL function lowerer ignores or weakens:

- `IGNORE NULLS` / `RESPECT NULLS`.
- `WITHIN GROUP`.
- Argument-list clauses.
- Named argument labels.

This can change values or turn a Spark error into successful positional execution. For example, Spark accepts `first_value(...) IGNORE NULLS`, but the modifier is lost.

### 5. Some generic sqlparser fields lack rejection guards

`FETCH FIRST` and `QUALIFY` fields are not consumed or explicitly rejected. Spark 4.1.1 rejects the probed forms, so accepting and dropping them would violate error parity.

### 6. Global temporary views are downgraded

`is_global` emits a warning and registers a session-local view. That should either gain real global semantics or become an honest boundary.

### 7. Compatibility endpoints claim success without state

Notable examples:

- Config `Set`/`Unset` are no-ops.
- AddArtifacts reports success without retaining artifacts.
- ArtifactStatus always returns empty.
- ReleaseExecute returns success without execution state.

These may help client bootstrapping, but they must be documented as compatibility stubs, not supported Spark behavior.

### 8. Write-layout semantics are ignored

Partitioning, bucketing, clustering, and sort specifications are accepted but ignored. These should be rejected unless the no-op is proven result-insensitive for the requested operation.

## Catalog, session, and analysis APIs

Catalog support is narrow:

- Current catalog/database return fixed values.
- Function existence/get/list.
- Table existence.
- Drop temporary view.

There is no real mutable catalog/database state, cache management, table creation through Catalog, refresh/recover, or global-temp-view management. Some parameters such as namespace or list pattern are ignored.

Of the non-streaming AnalyzePlan requests, only schema analysis is implemented. Explain, tree strings, input files, semantic hashes, persistence, storage levels, DDL parsing, and similar introspection APIs are unsupported.

The service behavior is visible in [service.rs](crates/connect-server/src/service.rs) and [catalog_ops.rs](crates/connect-server/src/catalog_ops.rs).

## Test evidence

The current manifests contain:

- DataFrame corpus: 429 cases — 422 green and 7 known red.
- SQL corpus: 428 cases — 426 green and 2 skipped.
- Total: 857 cases, 848 green.

The seven DataFrame reds are five nested-SQL-wrapper cases and two pretty-name/error-shape cases. The SQL skips concern malformed-parser classification and time-travel error classification.

Coverage is broad—joins, aggregates, windows, correlated subqueries, TPC-H, and TPC-DS—but corpus green is not synonymous with semantic support. Schema-only, nondeterministic, and cosmetic cases can conceal dropped semantics, as TABLESAMPLE and SORT/CLUSTER demonstrate.

The latest recorded full Rust gates were also green: 165 Connect tests and 1,332 core tests with four ignored.

This was a read-only audit; I did not rerun the entire corpus.
