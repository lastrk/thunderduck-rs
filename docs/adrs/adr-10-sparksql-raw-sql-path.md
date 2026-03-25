# ADR-10: SparkSQL Raw SQL Path

> **⚠️ Superseded by [ADR-21: SparkSQL Parser Strategy](adr-21-sparksql-parser-strategy.md)**
> This ADR describes the interim `preprocess_spark_sql` text-rewrite approach. ADR-21 replaces it with a proper `sqlparser-rs`-based parser that produces a `LogicalPlan` directly, once the parser is built out.

**Decision: Spark→DuckDB SQL preprocessing pass; full parser deferred until differential tests require it**

The DataFrame API path (protobuf → LogicalPlan) does **not** use a SQL parser. Raw SQL strings
passed via `spark.sql("...")` reach the server as a `SQL` relation proto, which
`RelationConverter::convert_sql()` wraps in a `SqlRelation` node containing the original SQL
string verbatim.

`SqlGenerator::gen_sql_relation()` then passes the string through `preprocess_spark_sql()` —
a pure text transformation pipeline that rewrites Spark SQL dialect differences to DuckDB SQL
before the string is executed. This handles the large majority of real-world `spark.sql()` calls
without building a full parser.

**`preprocess_spark_sql` phases** (in order):
1. Backtick identifier → double-quote identifier (`` `col` `` → `"col"`)
2. `ARRAY(...)` → `LIST_VALUE(...)`
3. `NAMED_STRUCT(...)` → struct literal (looped until stable for nested structs)
4. `MAP(k, v, ...)` → `MAP([k, ...], [v, ...])`
5. 1:1 function renames (`SIZE` → `LEN`, `TRANSFORM` → `LIST_TRANSFORM`, etc.)
6. `percentile(col, pct)` → `PERCENTILE_CONT(pct) WITHIN GROUP (ORDER BY col)`
7. `overlay(str PLACING repl FROM pos)` → `LEFT/SUBSTRING` concat
8. Spark angle-bracket type syntax (`ARRAY<T>` → `T[]`)
9. `split(str, pat, n)` three-arg form
10. `DATE 'lit' + INTERVAL 'n' YEAR/MONTH` date arithmetic
11. Higher-order function rewrites (`exists`, `forall`, `aggregate`, `filter`, `zip_with`)
12. `json_tuple(col, 'k1', ...) AS (a1, ...)` multi-column expansion
13. `from_json(col, 'Spark DDL schema')` → `json_transform(col, JSON schema)`

A full `sqlparser-rs`-based parser (originally planned in Phase 5) remains an option if a
differential test gap surfaces that cannot be addressed by the preprocessing pass.

---

← [Back to Architecture Overview](../architecture.md)
