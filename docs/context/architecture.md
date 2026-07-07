# Architecture Reference

> **Scope: τ (the only production path per ADR-022).** This document is the condensed reference for the current transpiler. The authoritative design is [`docs/thunderduck-rearchitect-ADRs.md`](../thunderduck-rearchitect-ADRs.md) (ADR-000 → ADR-022 + Cross-Validation). Retired v1 ADRs live under [`docs/adrs/legacy-transpiler/`](../adrs/legacy-transpiler/) and are marked SUPERSEDED.

## SQL Generation Architecture Principles

These are non-negotiable constraints governing all SQL generation and type handling:

1. **All SQL and expression snippets MUST be built from the typed AST.** No string manipulation on SQL text.
2. **Zero pre/post-processing of SQL strings.** All transformations happen on the AST.
3. **SparkSQL data flow**: Spark SQL string → sqlparser-rs parse tree → `CommonAst` → analyzer → `TypedAst` → emission → DuckDB SQL.
4. **DataFrame data flow**: Spark Connect protobuf → `V2RelationConverter` → `CommonAst` → analyzer → `TypedAst` → emission → DuckDB SQL.
5. **Spark parity is the only emission target.** τ matches Apache Spark exactly via (a) CASTs at top-level SELECT projection or (b) DuckDB extension functions; the `thdck_spark_funcs` extension is mandatory (ADR-020).
6. **Zero result copying**: 100% type matching is achieved at SQL generation time using extension functions + AS aliases. No Arrow vector copying or rewriting.
7. **Emission is dedicated.** Use `render_expr` / `dispatch_op`. `Display` / `Debug` implementations are human-readable debug output only — never used to build SQL sent to DuckDB.

## Crate Structure

```
crates/core/                        # Pure translation engine (no gRPC)
  transpiler_v2/                    # τ: CommonAst, analyzer, emission, INV enforcement
    ast.rs                          # CommonAst + CommonOp (shared IR)
    analyzer.rs                     # Resolve + type + nullability → TypedAst
    emission.rs                     # TypedAst → DuckDB SQL
    expression.rs                   # τ Expression enum
    type_inference.rs               # Spark-parity type inference
    invariants.rs                   # INV1-10 mechanical enforcement
  parser_v2/                        # SparkSQL parser (sqlparser-rs + SparkDialect) → CommonAst
  types/                            # DataType, StructField, StructType
  runtime/                          # DuckDB session, Arrow streaming, extension loading

crates/connect-server/              # gRPC binary (tonic)
  service.rs                        # SparkConnectService; per-batch streaming
                                    # (execute_streaming_query emits a proto
                                    # Schema frame first, then transcoded batches)
  session/                          # SessionManager
  converter/
    v2_relation_converter.rs        # Protobuf Relation → CommonAst
                                    # (LocalRelation Arrow interval-value decoder)
    relation_converter.rs           # parse_json_schema helper
    type_converter.rs               # DataType ↔ proto DataType
  arrow_schema_stamp.rs             # Wire schema = τ's resolved_schema
                                    # (interval-aware arms accept pre- + post-
                                    # transcode Arrow types)
  arrow_interval_transcode.rs       # Per-batch DuckDB Interval(MonthDayNano) →
                                    # Spark per-semantic Arrow encoding
                                    # (DayTimeInterval → Duration(us))
```

## Key Types

| Layer | Type | Responsibility |
|-------|------|----------------|
| **Service** | `SparkConnectService` | tonic gRPC service: receives Spark Connect requests |
| **Session** | `SessionManager` | Manages sessions; each session owns a DuckDB Connection on a dedicated OS thread |
| **Converter** | `V2RelationConverter` | Spark Connect protobuf Relation → `CommonAst` |
| **Converter** | `V2ExpressionConverter` | Spark Connect protobuf Expression → τ `Expression` |
| **Parser** | `SparkSqlParserV2` | sqlparser-rs based Spark SQL parser (raw SQL path) → `CommonAst` |
| **IR** | `CommonAst` / `CommonOp` (enum) | Shared IR — same tree fed by both front-ends |
| **Expression** | τ `Expression` (enum) | τ's Spark-parity expression types with `data_type()` / `nullable()` |
| **Analyzer** | `analyze()` | `CommonAst` + `BaseTypes` → `TypedAst { op, resolved_schema }` |
| **Emission** | `dispatch_op()` / `render_expr()` | Traverses `TypedAst`, produces DuckDB SQL |
| **Runtime** | `DuckDbSession` | Owns `duckdb::Connection` on its dedicated OS thread |
| **Types** | `TypeInferenceEngine` | Resolves expression types following Spark semantics |

## Emission entry points

Use `render_expr` / `dispatch_op` inside `crates/core/src/transpiler_v2/emission.rs` to render expressions and operators. The `Display` / `Debug` traits on τ's types are for debug logging only — do not use them to build SQL sent to DuckDB.

## DuckDB Threading Model

`duckdb::Connection` is `!Send + !Sync`. Each session runs on a dedicated `std::thread`. The gRPC async handler communicates via `tokio::sync::mpsc` channels:

```
tokio task → mpsc::Sender<SessionCommand> → session thread (owns Connection)
session thread → oneshot::Sender<SessionResult> → tokio task → gRPC stream
```

Never attempt to move a `Connection` across thread boundaries or hold it across `.await` points.

## Streaming and Arrow interval transcoding

`execute_streaming_query` (`crates/connect-server/src/service.rs`) is a true per-batch stream driven by `futures::stream::unfold`. For every ExecutePlan request it:

1. Yields one `ExecutePlanResponse.schema` frame built from τ's `resolved_schema` (via `build_schema_response`). This bypasses PySpark's `from_arrow_schema` fallback, which lacks `is_interval` arms and would raise `UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION` on any interval column.
2. Receives Arrow batches from the session thread via a bounded `tokio::sync::mpsc` (buffer 4). The session thread owns `duckdb::Connection` (`!Send`) and never sees the transcoder.
3. Applies `arrow_interval_transcode::apply(&batch, &plan)` on the tonic async task: DuckDB's uniform `Interval(MonthDayNano)` is rewritten per column to Spark's per-semantic Arrow encoding (currently `DayTimeInterval` → `Duration(Microsecond)`; `CalendarInterval` and `YearMonthInterval` pass through). The wire `RecordBatch` is constructed exactly once per batch from the transcoded columns and a stamped `Arc<Schema>` cached on the first batch (via `arrow_schema_stamp::build_stamped_schema`).
4. Yields the Arrow batch frame, then loops. On session error the message is reconstructed as `ThunderduckError::DuckDb(msg)`, run through `reclassified_spark_runtime()`, and bridged to `tonic::Status` — the same ANSI error-class parity the synchronous path applies.

INV10 forbids the transcoder (or any wire-shape concern) from living inside `crates/core/transpiler_v2/`. Any future op with a data-driven or otherwise client-unfriendly Arrow schema must route through this same schema-frame + per-batch transcode pattern rather than expecting the client's Arrow-schema fallback to save it.

## Joins

τ emits native DuckDB `SEMI JOIN` / `ANTI JOIN` directly in `emission.rs::render_join` (no EXISTS-subquery conversion). The flat-chain rendering branch must break at semi/anti boundaries — folding the chain across a semi/anti would change the tree shape and reorder filtering semantics. DuckDB uses `SEMI JOIN` / `ANTI JOIN` without `LEFT` prefix — `LEFT SEMI JOIN` is a parser error.

Aggregate SQL generation uses a single canonical path through `render_aggregate`.

## Expression Hierarchy (key variants)

```
Expression (enum)
  Literal               # constant values
  ColumnReference       # resolved column ref with type info and qualifier
  UnresolvedColumn      # unresolved (pre-resolution)
  Binary(BinaryExpression)   # left OP right
  Unary(UnaryExpression)     # OP operand
  FunctionCall          # func(args) — τ owns its Spark-parity translation table
  Cast(CastExpression)  # CAST(expr AS type)
  CaseWhen              # CASE WHEN ... THEN ... END
  Window(WindowFunction)     # ROW_NUMBER, RANK, LAG, LEAD, etc.
  Alias(AliasExpression)     # expr AS alias
  Star                  # *
  InSubquery / ExistsSubquery / ScalarSubquery
  Lambda / LambdaVariable    # HOF lambdas (transform, filter, etc.)
  RawSql                # raw SQL passthrough from spark.expr()
  ArrayLiteral / MapLiteral / StructLiteral
```

## Raw SQL vs DataFrame Code Paths

Both paths converge on the shared `CommonAst` before emission:

- **Raw SQL**: Spark SQL string → `SparkSqlParserV2` (sqlparser-rs + SparkDialect) → `CommonAst` → analyzer → emission → DuckDB SQL.
- **DataFrame API**: Spark Connect protobuf → `V2RelationConverter` → `CommonAst` → analyzer → emission → DuckDB SQL.

Both paths flow through τ's analyzer for full type awareness before emission.

**Implication**: type-inference and emission fixes affect both raw SQL and DataFrame queries.

## Spark Parity Requirements

**Critical Rule**: Thunderduck must match Spark EXACTLY, not just produce equivalent results.

- **Return types**: If Spark returns DOUBLE, Thunderduck must return DOUBLE (not BIGINT).
- **Rounding conventions**: Must match Spark's rounding behavior (`ROUND_HALF_UP`).
- **Type coercion**: Implicit casts must follow Spark's rules.
- **NULL handling**: Must match Spark's null propagation.
- **COUNT**: Always non-nullable `Long`.
- **SUM/AVG**: Nullability depends on argument nullability.

Differential tests validate: same row count, same column names, **same column types**, same values (with epsilon for floats), same null handling, same sort order.

**Goal**: Drop-in replacement for Spark, not "Spark-like" behavior.
