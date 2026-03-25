# Phase 5 — SparkSQL Parser Implementation Plan

## Goal

Replace the `preprocess_spark_sql` text-substitution pipeline with a proper
`sqlparser-rs`-based parser that produces a typed `LogicalPlan` tree — the same
type used by the DataFrame (protobuf) path.  The result feeds directly into the
existing `SqlGenerator` without modification.

The parser is built **demand-driven**, milestone by milestone.  Each milestone
declares the SQL constructs it supports and has a hard `ThunderduckError::Unsupported`
for everything else — no silent fallback.  The preprocessing pass remains as the
fallback only until a milestone is complete, then the corresponding preprocessing
phases are deleted.

---

## Architecture

```
spark.sql("SELECT ...") SQL string
    │
    ▼  RelationConverter::convert_sql()           ← integration point (modified)
    │
    ├─ SparkSqlParser::parse(sql)                 ← NEW (crates/core/src/parser/mod.rs)
    │       │
    │       ├─ SparkDialect                        ← NEW (crates/core/src/parser/dialect.rs)
    │       │   (sqlparser-rs Dialect impl)
    │       │
    │       └─ SqlConverter::convert_statement()  ← NEW (crates/core/src/parser/sql_converter.rs)
    │               │
    │               ▼
    │           LogicalPlan + Expression           ← existing types, unchanged
    │
    ▼  SqlGenerator::generate()                   ← existing, unchanged
DuckDB SQL string
```

This mirrors the DataFrame path exactly:

```
PySpark DataFrame API
    ▼  RelationConverter::convert()
LogicalPlan + Expression
    ▼  SqlGenerator::generate()
DuckDB SQL
```

The key invariant from ADR-21: unrecognised SQL constructs return a hard
`ThunderduckError::Unsupported` — they are NOT silently passed through the old
preprocessing path.  "Supported" means covered by a complete milestone with
passing differential tests.

References: ADR-10 (current preprocessing pass), ADR-21 (parser strategy),
ADR-06 (LogicalPlan enum), ADR-07 (Expression enum), ADR-11 (RelationConverter pattern).

---

## New Files

```
crates/core/src/parser/
    mod.rs             # Public API: SparkSqlParser struct
    dialect.rs         # SparkDialect: sqlparser::dialect::Dialect impl
    sql_converter.rs   # SqlConverter: Statement → LogicalPlan + Expr → Expression
```

`crates/core/src/lib.rs` gains `pub mod parser;`.

---

## Cargo Changes

### `/workspace/Cargo.toml` (workspace deps)

```toml
sqlparser = { version = "0.61", features = [] }
```

### `/workspace/crates/core/Cargo.toml`

```toml
sqlparser.workspace = true
```

No other crates need the dependency directly; `connect-server` calls
`SparkSqlParser` through the `thunderduck-core` public API.

---

## Integration Point

**File**: `crates/connect-server/src/converter/relation_converter.rs`

**Current** `convert_sql` (line 509):

```rust
fn convert_sql(&self, s: &proto::Sql) -> Result<LogicalPlan> {
    Ok(LogicalPlan::SqlRelation(SqlRelation { sql: s.query.clone(), schema: StructType::empty() }))
}
```

**New** `convert_sql` (after M1 scaffold):

```rust
fn convert_sql(&self, s: &proto::Sql) -> Result<LogicalPlan> {
    use thunderduck_core::parser::SparkSqlParser;
    SparkSqlParser::parse(&s.query)
        .map_err(|e| ConnectError::Unsupported(e.to_string()))
}
```

Once `SparkSqlParser::parse` returns `ThunderduckError::Unsupported` for any
construct it does not yet handle, the gRPC service propagates that as
`Status::unimplemented` to the client — the same error semantics already used
for unrecognised protobuf relation types.

The `SqlRelation` plan node and `gen_sql_relation` / `preprocess_spark_sql` in
the generator **are NOT deleted** until a milestone's differential tests all
pass — they serve as a reference during development.  Milestone completion
checklists call out when each set of preprocessing phases can be removed.

---

## Error Handling Convention

`SqlConverter` uses `ThunderduckError` (the existing core error type) directly:

- `ThunderduckError::Parse(msg)` — sqlparser-rs parser errors (invalid SQL)
- `ThunderduckError::Unsupported(msg)` — recognised but unimplemented construct
- `ThunderduckError::SqlGeneration(msg)` — internal converter logic errors

The `Result<T>` alias from `crates/core/src/error.rs` is reused throughout.

---

## Step-by-Step Implementation Milestones

---

### M1 — Scaffold

**Goal**: Wire up the plumbing.  Nothing is converted yet; all SQL is rejected
with `Unsupported`.  This proves the dependency and integration point compile.

**Tasks**:

1. Add `sqlparser = "0.61"` to `[workspace.dependencies]` in `/workspace/Cargo.toml`.
2. Add `sqlparser.workspace = true` to `crates/core/Cargo.toml`.
3. Add `pub mod parser;` to `crates/core/src/lib.rs`.
4. Create `crates/core/src/parser/mod.rs`:

```rust
//! SparkSQL parser: SQL string → LogicalPlan.
mod dialect;
mod sql_converter;

use crate::error::{Result, ThunderduckError};
use crate::logical::LogicalPlan;

pub struct SparkSqlParser;

impl SparkSqlParser {
    /// Parse a Spark SQL string and return a typed LogicalPlan.
    ///
    /// Returns `ThunderduckError::Unsupported` for any construct not yet
    /// implemented in SqlConverter.
    pub fn parse(sql: &str) -> Result<LogicalPlan> {
        use sqlparser::parser::Parser;
        use sqlparser::dialect::GenericDialect;  // temporary; replaced by SparkDialect in M2
        let dialect = GenericDialect {};
        let mut stmts = Parser::parse_sql(&dialect, sql)
            .map_err(|e| ThunderduckError::Parse(e.to_string()))?;
        if stmts.len() != 1 {
            return Err(ThunderduckError::Unsupported(
                format!("expected exactly one SQL statement, got {}", stmts.len())
            ));
        }
        sql_converter::SqlConverter::new().convert_statement(stmts.remove(0))
    }
}
```

5. Create `crates/core/src/parser/dialect.rs`:

```rust
use sqlparser::dialect::Dialect;

/// Spark SQL dialect for sqlparser-rs.
/// Extended incrementally as milestones add construct support.
#[derive(Debug, Default)]
pub struct SparkDialect;

impl Dialect for SparkDialect {
    fn is_identifier_start(&self, ch: char) -> bool {
        ch.is_alphabetic() || ch == '_'
    }

    fn is_identifier_part(&self, ch: char) -> bool {
        ch.is_alphanumeric() || ch == '_'
    }

    // Backtick identifiers added in M9.
}
```

6. Create `crates/core/src/parser/sql_converter.rs`:

```rust
use sqlparser::ast::Statement;
use crate::error::{Result, ThunderduckError};
use crate::logical::LogicalPlan;

pub struct SqlConverter;

impl SqlConverter {
    pub fn new() -> Self { Self }

    pub fn convert_statement(&self, stmt: Statement) -> Result<LogicalPlan> {
        Err(ThunderduckError::Unsupported(
            format!("SparkSQL parser: statement type not yet implemented: {:?}", stmt)
        ))
    }
}
```

7. Update `crates/connect-server/src/converter/relation_converter.rs`
   `convert_sql` to call `SparkSqlParser::parse` as shown in the Integration
   Point section above.

**Verification**: `cargo build -p thunderduck-core` compiles.
`cargo build -p thunderduck-connect-server` compiles.

**Preprocessing pass fate**: None removed yet.

**Differential tests**: None expected to pass through the new path yet.

---

### M2 — Basic SELECT

**Goal**: Handle `SELECT expr [AS alias], ... FROM table [WHERE cond] [ORDER BY col [ASC|DESC]] [LIMIT n] [OFFSET n]`.

**sqlparser-rs AST nodes to handle**:

| sqlparser `Statement` / `Expr` | Maps to |
|-------------------------------|---------|
| `Statement::Query(Box<Query>)` | entry point for all SELECT |
| `Query { body: SetExpr::Select(...), order_by, limit, offset, .. }` | `LogicalPlan::Sort`, `Limit` wrappers |
| `Select { projection, from, selection, .. }` | `LogicalPlan::Project` + `Filter` |
| `SelectItem::UnnamedExpr(expr)` | `Expression` (no alias) |
| `SelectItem::ExprWithAlias { expr, alias }` | `Expression::Alias` |
| `SelectItem::Wildcard(_)` | `Expression::Star` |
| `TableWithJoins { relation: TableFactor::Table { name, alias, .. }, joins: [] }` | `LogicalPlan::TableScan` |
| `Expr::Identifier(ident)` | `Expression::UnresolvedColumn` |
| `Expr::CompoundIdentifier(parts)` | `Expression::UnresolvedColumn` with qualifier |
| `Expr::Value(Value::Number)` | `Expression::Literal` (Long or Double) |
| `Expr::Value(Value::SingleQuotedString)` | `Expression::Literal(String)` |
| `Expr::Value(Value::Boolean)` | `Expression::Literal(Boolean)` |
| `Expr::Value(Value::Null)` | `Expression::Literal(Null)` |
| `Expr::BinaryOp { left, op, right }` | `Expression::Binary` |
| `Expr::UnaryOp { op, expr }` | `Expression::Unary` |
| `Expr::Nested(expr)` | recurse |
| `Expr::Cast { expr, data_type }` | `Expression::Cast` |
| `Expr::Function(func)` | `Expression::FunctionCall` |
| `OrderByExpr { expr, asc, nulls_first }` | `SortOrder` |
| `Expr::Between { expr, low, high, negated }` | `Expression::Between` |
| `Expr::IsNull(expr)` / `Expr::IsNotNull(expr)` | `Expression::Unary(IsNull/IsNotNull)` |
| `Expr::Like { expr, pattern, negated }` | `Expression::FunctionCall("like"/"not_like")` → `gen_function_call` handles it |
| `Expr::InList { expr, list, negated }` | `Expression::FunctionCall("in"/"not_in")` |

**`SqlConverter` methods to add**:

```rust
fn convert_statement(&self, stmt: Statement) -> Result<LogicalPlan>
fn convert_query(&self, query: Query) -> Result<LogicalPlan>
fn convert_select_body(&self, select: Select, order_by: Vec<OrderByExpr>, limit: Option<Expr>, offset: Option<Offset>) -> Result<LogicalPlan>
fn convert_table_factor(&self, factor: TableFactor) -> Result<LogicalPlan>
fn convert_select_item(&self, item: SelectItem) -> Result<Expression>
fn convert_expr(&self, expr: Expr) -> Result<Expression>
fn convert_data_type(&self, dt: sqlparser::ast::DataType) -> Result<crate::types::DataType>
fn convert_order_by(&self, items: Vec<OrderByExpr>) -> Result<Vec<SortOrder>>
fn convert_binary_op(&self, op: BinaryOperator) -> Result<BinaryOp>
```

**`convert_expr` structure** — mirrors `ExpressionConverter::convert`:

```rust
fn convert_expr(&self, expr: Expr) -> Result<Expression> {
    match expr {
        Expr::Identifier(ident) => Ok(Expression::UnresolvedColumn(...)),
        Expr::CompoundIdentifier(parts) => Ok(Expression::UnresolvedColumn(...)),
        Expr::Value(v) => self.convert_value(v),
        Expr::BinaryOp { left, op, right } => Ok(Expression::Binary(...)),
        Expr::UnaryOp { op, expr } => Ok(Expression::Unary(...)),
        Expr::Cast { expr, data_type, .. } => Ok(Expression::Cast(...)),
        Expr::Function(f) => self.convert_function(f),
        Expr::Nested(e) => self.convert_expr(*e),
        Expr::Between { expr, low, high, negated } => Ok(Expression::Between(...)),
        Expr::IsNull(e) => Ok(Expression::Unary(UnaryExpression { op: UnaryOp::IsNull, operand: Box::new(self.convert_expr(*e)?) })),
        Expr::IsNotNull(e) => Ok(Expression::Unary(UnaryExpression { op: UnaryOp::IsNotNull, operand: Box::new(self.convert_expr(*e)?) })),
        Expr::Like { expr, pattern, negated, .. } => { /* FunctionCall "like" / "not like" */ },
        Expr::InList { expr, list, negated } => { /* FunctionCall "in" */ },
        _ => Err(ThunderduckError::Unsupported(format!("expression not yet supported: {:?}", expr))),
    }
}
```

**`SparkDialect` changes**: switch `SparkSqlParser::parse` from `GenericDialect`
to `SparkDialect`.  No new dialect methods needed for M2 — identifiers and basic
SQL syntax are covered by the defaults.

**`convert_query` wrapping logic**:

```
Query.body = SetExpr::Select(select)
  → convert_select_body(select, order_by, limit, offset)
      → base: convert_from(select.from)    → LogicalPlan (TableScan / Join / Subquery)
      → filter: if select.selection.is_some() → wrap in LogicalPlan::Filter
      → project: LogicalPlan::Project { exprs: select.projection.map(convert_select_item) }
      → order_by: if !query.order_by.is_empty() → wrap in LogicalPlan::Sort
      → limit: if query.limit.is_some() → wrap in LogicalPlan::Limit
      → offset: if query.offset.is_some() → apply offset as Limit(offset=n) or inner Sort trick
```

**Preprocessing phases deprecated by M2**: Phases 0 (backtick), 4 (name renames
for simple identifiers) are partially superseded — but do NOT delete them until
M9 (backtick) and the full function rename path (covered by `FunctionRegistry`)
are verified.  For now, leave the preprocessing pass intact.

**Unit tests** (`crates/core/src/parser/sql_converter.rs` or `mod.rs` `#[cfg(test)]`):

```rust
#[test]
fn test_parse_simple_select() {
    let plan = SparkSqlParser::parse("SELECT a, b FROM t WHERE a > 1 ORDER BY b LIMIT 10").unwrap();
    // assert plan shape: Sort(Limit(Project(Filter(TableScan("t")))))
}

#[test]
fn test_parse_star() {
    let plan = SparkSqlParser::parse("SELECT * FROM t").unwrap();
    // assert Project with Star expression
}

#[test]
fn test_parse_expressions() {
    // literals, binary ops, CAST, IS NULL
}
```

**Differential test acceptance criteria**:
- `test_simple_sql.py` — basic SELECT tests must pass through the new parser path
- `test_column_operations_differential.py` — column selection, rename, ordering

---

### M3 — Aggregation

**Goal**: `GROUP BY`, `HAVING`, `COUNT/SUM/AVG/MIN/MAX/COUNT(DISTINCT ...)`,
`ROLLUP`, `CUBE`, `GROUPING SETS`.

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `Select { group_by: GroupByExpr::Expressions(exprs), having, .. }` | `LogicalPlan::Aggregate` |
| `GroupByExpr::Rollup(exprs)` | `GroupingSets::Rollup` |
| `GroupByExpr::Cube(exprs)` | `GroupingSets::Cube` |
| `GroupByExpr::GroupingSets(sets)` | `GroupingSets::GroupingSets` |
| `Expr::Function(f)` where `f.name` is an aggregate | `AggregateExpr` in `Aggregate.agg_exprs` |
| `f.distinct` | `AggregateExpr::is_distinct` |
| `f.filter` (`FILTER (WHERE cond)`) | `AggregateExpr::filter` |
| `Select { having: Some(expr) }` | `Filter` wrapping the `Aggregate` |

**`SqlConverter` methods to add**:

```rust
fn is_aggregate_function(name: &str) -> bool
fn convert_aggregate(&self, select: &Select, base: LogicalPlan) -> Result<LogicalPlan>
fn convert_group_by(&self, gb: &GroupByExpr) -> Result<(Vec<Expression>, Option<GroupingSets>)>
```

**`LogicalPlan::Aggregate` construction** — mirrors `RelationConverter::convert_aggregate`.
The key fields:

```rust
Aggregate {
    input: Box<LogicalPlan>,
    group_exprs: Vec<Expression>,
    grouping_sets: Option<GroupingSets>,
    agg_exprs: Vec<AggregateExpr>,
    output_exprs: Vec<SelectEntry>,  // derived from select.projection after separating aggregates
}
```

The tricky part: sqlparser puts aggregate functions in `projection` alongside
non-aggregate expressions.  `convert_aggregate` must:
1. Scan `projection` for aggregate `Expr::Function` calls → populate `agg_exprs`.
2. The remaining `projection` entries become `group_exprs` or pass-through columns.
3. Wrap in `Filter` if `having` is present.

**`SparkDialect` changes**: none for M3.

**Unit tests**:
```rust
#[test]
fn test_parse_count() { /* SELECT COUNT(*), SUM(a) FROM t GROUP BY b */ }
#[test]
fn test_parse_count_distinct() { /* SELECT COUNT(DISTINCT a) FROM t */ }
#[test]
fn test_parse_rollup() { /* SELECT a, SUM(b) FROM t GROUP BY ROLLUP(a) */ }
#[test]
fn test_parse_having() { /* SELECT a, COUNT(*) FROM t GROUP BY a HAVING COUNT(*) > 1 */ }
```

**Differential test acceptance criteria**:
- `test_aggregation_functions_differential.py`
- `test_multidim_aggregations.py`
- `test_new_aggregates_differential.py`
- TPC-H queries Q1, Q3, Q5, Q6, Q7, Q8, Q9 (aggregate-heavy)

**Preprocessing phases deprecated**: Phase 4 renames for aggregate function names
(`SIZE` → `LEN`, etc.) are superseded because `FunctionRegistry` handles the
mapping from `Expression::FunctionCall` name → DuckDB name.

---

### M4 — Joins

**Goal**: `[INNER] JOIN`, `LEFT [OUTER] JOIN`, `RIGHT [OUTER] JOIN`, `FULL OUTER JOIN`,
`CROSS JOIN`, `LEFT SEMI JOIN`, `LEFT ANTI JOIN`, `ON expr`, `USING (cols)`.

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `TableWithJoins { joins: Vec<Join>, .. }` | `LogicalPlan::Join` (recursive) |
| `Join { relation, join_operator }` | `LogicalPlan::Join { join_type, condition/using_columns }` |
| `JoinOperator::Inner(constraint)` | `JoinType::Inner` |
| `JoinOperator::LeftOuter(constraint)` | `JoinType::Left` |
| `JoinOperator::RightOuter(constraint)` | `JoinType::Right` |
| `JoinOperator::FullOuter(constraint)` | `JoinType::Full` |
| `JoinOperator::CrossJoin` | `JoinType::Cross` |
| `JoinOperator::LeftSemi(constraint)` | `JoinType::LeftSemi` |
| `JoinOperator::LeftAnti(constraint)` | `JoinType::LeftAnti` |
| `JoinConstraint::On(expr)` | `Join.condition = Some(convert_expr(expr))` |
| `JoinConstraint::Using(cols)` | `Join.using_columns = cols.map(|c| c.to_string())` |

**`SqlConverter` methods to add**:

```rust
fn convert_table_with_joins(&self, twj: TableWithJoins) -> Result<LogicalPlan>
fn convert_join_operator(&self, op: JoinOperator) -> Result<(JoinType, Option<Expression>, Vec<String>)>
```

**Multi-join chain**: `TableWithJoins.joins` is a flat `Vec<Join>`.  The converter
must fold left:

```rust
let mut plan = self.convert_table_factor(twj.relation)?;
for join in twj.joins {
    let (join_type, condition, using_columns) = self.convert_join_operator(join.join_operator)?;
    let right = self.convert_table_factor(join.relation)?;
    plan = LogicalPlan::Join(Join {
        left: Box::new(plan),
        right: Box::new(right),
        join_type,
        condition,
        using_columns,
        left_plan_id: None,
        right_plan_id: None,
    });
}
```

**Note on `plan_id`**: The `left_plan_id` / `right_plan_id` fields are used by
the `SEMI/ANTI` qualifier-stripping logic in `gen_join`.  For the SQL path these
can be `None` — the converter should produce column references without
`plan_id` qualifiers (i.e., `UnresolvedColumn { qualifier: None }`).  The
generator's `strip_qualifiers_in_expr` only acts when qualifiers are present.

**`SparkDialect` changes**: none for M4.  Standard SQL join syntax is covered by
`GenericDialect` defaults.

**Unit tests**:
```rust
#[test]
fn test_parse_inner_join() { /* SELECT * FROM a JOIN b ON a.id = b.id */ }
#[test]
fn test_parse_left_join() { /* SELECT * FROM a LEFT JOIN b ON a.id = b.id */ }
#[test]
fn test_parse_using() { /* SELECT * FROM a JOIN b USING (id) */ }
#[test]
fn test_parse_multi_join() { /* three-table join */ }
```

**Differential test acceptance criteria**:
- `test_joins_differential.py`
- `test_join_advanced_differential.py`
- TPC-H queries Q2, Q4, Q5, Q7, Q8, Q9, Q10, Q11, Q12, Q13, Q14, Q17, Q18, Q19, Q20, Q21, Q22

---

### M5 — Subqueries

**Goal**: Scalar subqueries in expressions, `IN (SELECT ...)`, `EXISTS (SELECT ...)`,
correlated and uncorrelated, subqueries in FROM (derived tables).

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `Expr::Subquery(query)` | `Expression::ScalarSubquery` |
| `Expr::InSubquery { expr, subquery, negated }` | `Expression::InSubquery` |
| `Expr::Exists { subquery, negated }` | `Expression::ExistsSubquery` |
| `TableFactor::Derived { subquery, alias, .. }` | `LogicalPlan::AliasedRelation(Box<convert_query(subquery)>, alias)` |

**`SqlConverter` methods to add**:

```rust
fn convert_subquery_expr(&self, query: Box<Query>) -> Result<LogicalPlan>
```

This calls `convert_query` recursively — the same entry point used for top-level
queries.  The `SqlConverter` is stateless, so recursion is trivial.

**`SparkDialect` changes**: none for M5.

**Unit tests**:
```rust
#[test]
fn test_scalar_subquery() { /* SELECT (SELECT MAX(b) FROM t2) FROM t1 */ }
#[test]
fn test_in_subquery() { /* SELECT a FROM t WHERE a IN (SELECT x FROM t2) */ }
#[test]
fn test_exists_subquery() { /* SELECT a FROM t WHERE EXISTS (SELECT 1 FROM t2 WHERE ...) */ }
#[test]
fn test_derived_table() { /* SELECT * FROM (SELECT a, b FROM t) sub */ }
```

**Differential test acceptance criteria**:
- TPC-H queries Q4, Q17, Q20, Q21, Q22 (subquery-heavy)
- `test_differential_v2.py` subquery tests

---

### M6 — Set Operations

**Goal**: `UNION ALL`, `UNION DISTINCT`, `INTERSECT [ALL]`, `EXCEPT [ALL]`.

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `SetExpr::SetOperation { op: Union, all, left, right }` | `LogicalPlan::Union` |
| `SetExpr::SetOperation { op: Intersect, all, left, right }` | `LogicalPlan::Intersect` |
| `SetExpr::SetOperation { op: Except, all, left, right }` | `LogicalPlan::Except` |

**`convert_query`** becomes a recursive function:

```rust
fn convert_query(&self, query: Query) -> Result<LogicalPlan> {
    match *query.body {
        SetExpr::Select(select) => self.convert_select_body(*select, query.order_by, query.limit, query.offset),
        SetExpr::SetOperation { op, all, left, right } => {
            let left_plan = self.convert_query(Query { body: left, .. Default::default() })?;
            let right_plan = self.convert_query(Query { body: right, .. Default::default() })?;
            match op {
                SetOperator::Union => Ok(LogicalPlan::Union(Union { left: Box::new(left_plan), right: Box::new(right_plan), by_name: false, allow_missing: false, is_all: all })),
                SetOperator::Intersect => Ok(LogicalPlan::Intersect(Intersect { left: Box::new(left_plan), right: Box::new(right_plan), is_all: all })),
                SetOperator::Except => Ok(LogicalPlan::Except(Except { left: Box::new(left_plan), right: Box::new(right_plan), is_all: all })),
                _ => Err(ThunderduckError::Unsupported(...)),
            }
        }
        SetExpr::Query(q) => self.convert_query(*q),
        _ => Err(ThunderduckError::Unsupported(...)),
    }
}
```

**Note**: `Query` structs in set operation branches contain only the body — no
ORDER BY or LIMIT at the branch level.  Only the outermost `Query` carries them.
Construct a minimal `Query` for recursive calls.

**`SparkDialect` changes**: none for M6.

**Unit tests**:
```rust
#[test]
fn test_union_all() { /* SELECT a FROM t1 UNION ALL SELECT a FROM t2 */ }
#[test]
fn test_except() { /* SELECT a FROM t1 EXCEPT SELECT a FROM t2 */ }
#[test]
fn test_intersect() { /* SELECT a FROM t1 INTERSECT SELECT a FROM t2 */ }
```

**Differential test acceptance criteria**:
- `test_set_operations_differential.py`

---

### M7 — Window Functions

**Goal**: `func() OVER (PARTITION BY ... ORDER BY ... [ROWS|RANGE BETWEEN ... AND ...])`.

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `Expr::Function(f)` where `f.over = Some(WindowType::WindowSpec(spec))` | `Expression::Window` |
| `WindowSpec { partition_by, order_by, window_frame }` | `WindowFunction { partition_by, order_by, frame }` |
| `WindowFrame { units, start_bound, end_bound }` | `WindowFrame { unit, start, end }` |
| `WindowFrameBound::CurrentRow` | `FrameBoundary::CurrentRow` |
| `WindowFrameBound::Preceding(None)` | `FrameBoundary::UnboundedPreceding` |
| `WindowFrameBound::Following(None)` | `FrameBoundary::UnboundedFollowing` |
| `WindowFrameBound::Preceding(Some(Expr))` | `FrameBoundary::Preceding(convert_expr)` |
| `WindowFrameBound::Following(Some(Expr))` | `FrameBoundary::Following(convert_expr)` |
| `WindowFrameUnits::Rows` | `FrameUnit::Rows` |
| `WindowFrameUnits::Range` | `FrameUnit::Range` |

**`SqlConverter` methods to add**:

```rust
fn convert_window_spec(&self, spec: WindowSpec) -> Result<WindowFunction>
fn convert_frame_bound(&self, bound: WindowFrameBound) -> Result<FrameBoundary>
```

`Expression::Window` wraps a `WindowFunction` which carries the base function
call plus the window spec.  The base function comes from `f.name` and
`f.args` — use `convert_function` to build the inner `FunctionCall`, then wrap.

**`SparkDialect` changes**: none for M7; window syntax is standard SQL.

**Unit tests**:
```rust
#[test]
fn test_window_row_number() { /* ROW_NUMBER() OVER (PARTITION BY a ORDER BY b) */ }
#[test]
fn test_window_rank() { /* RANK() OVER (ORDER BY b) */ }
#[test]
fn test_window_lag() { /* LAG(a, 1) OVER (ORDER BY b) */ }
#[test]
fn test_window_frame_rows() { /* SUM(a) OVER (ORDER BY b ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) */ }
```

**Differential test acceptance criteria**:
- TPC-H queries Q1 (window in subquery), Q10
- `test_differential_v2.py` window tests

---

### M8 — CTEs (Common Table Expressions)

**Goal**: `WITH name AS (SELECT ...) SELECT ...`; recursive CTEs deferred.

**sqlparser-rs AST nodes to handle**:

| sqlparser node | Maps to |
|----------------|---------|
| `Query { with: Some(With { recursive, cte_tables }), .. }` | `LogicalPlan::WithCte` |
| `Cte { alias, query, .. }` | `(name: String, plan: LogicalPlan)` in `WithCte.ctes` |

**`convert_query`** extended:

```rust
fn convert_query(&self, query: Query) -> Result<LogicalPlan> {
    let base = self.convert_query_body(...)?;
    if let Some(with) = query.with {
        if with.recursive {
            return Err(ThunderduckError::Unsupported("recursive CTEs not yet supported".into()));
        }
        let ctes = with.cte_tables.into_iter()
            .map(|cte| {
                let plan = self.convert_query(*cte.query)?;
                Ok((cte.alias.name.value, plan))
            })
            .collect::<Result<Vec<_>>>()?;
        Ok(LogicalPlan::WithCte(WithCte { ctes, body: Box::new(base) }))
    } else {
        Ok(base)
    }
}
```

**`SparkDialect` changes**: none for M8.

**Unit tests**:
```rust
#[test]
fn test_simple_cte() { /* WITH t AS (SELECT 1 AS a) SELECT a FROM t */ }
#[test]
fn test_multi_cte() { /* WITH t1 AS (...), t2 AS (...) SELECT ... */ }
#[test]
fn test_recursive_cte_unsupported() { /* WITH RECURSIVE t AS (...) -- expect Unsupported */ }
```

**Differential test acceptance criteria**:
- TPC-H queries Q15 (CTE), Q18 (CTE in subquery)
- `test_differential_v2.py` CTE tests

**Preprocessing pass retirement**: after M8, the parser covers all standard SQL
constructs handled by the preprocessing pass (except Spark-specific constructs in
M9).  At this point, integrate the new path end-to-end and run the full
differential suite.  If all 670 tests pass, delete preprocessing phases 1–9
(`ARRAY(`, `NAMED_STRUCT`, `MAP`, function renames, `percentile`, `overlay`,
type syntax, `split`, date interval).  Phases 10–11 (HOF rewrites, `json_tuple`,
`from_json`) are handled in M9 or remain as targeted preprocessing.

---

### M9 — Spark-Specific Constructs

**Goal**: Backtick identifiers, `LATERAL VIEW [OUTER] EXPLODE/POSEXPLODE`,
`DISTRIBUTE BY`, `SORT BY`, higher-order functions with lambda syntax.

#### 9a — Backtick Identifiers

**`SparkDialect` change**:

```rust
impl Dialect for SparkDialect {
    fn is_delimited_identifier_start(&self, ch: char) -> bool {
        ch == '"' || ch == '`'
    }

    fn is_proper_identifier_inside_quotes(&self, _ch: char) -> bool {
        true
    }
}
```

sqlparser-rs normalises backtick-quoted identifiers to plain `Ident` nodes with
the `quote_style` field set to `Some('`')`.  `convert_expr(Expr::Identifier)`
and `convert_table_factor` should map these to the same unquoted string they
would otherwise produce — no special handling needed in `SqlConverter` beyond
stripping the surrounding backticks which sqlparser-rs does automatically.

**Preprocessing phase retired**: Phase 0 (`rewrite_backtick_identifiers`).

#### 9b — Lambda Expressions (HOF)

sqlparser-rs `DatabricksDialect` enables lambda parsing via `supports_lambda_functions`.
`SparkDialect` adds the same:

```rust
fn supports_lambda_functions(&self) -> bool { true }
```

**sqlparser-rs AST node**: `Expr::Lambda(LambdaFunction { params, body })`.

**Maps to**: `Expression::Lambda(LambdaExpression { params: Vec<String>, body: Box<Expression> })`.

Spark HOFs (`transform`, `filter`, `exists`, `forall`, `aggregate`, `zip_with`)
then use standard `convert_function` — the lambda arg is just an `Expression`.

**Preprocessing phases retired**: Phase 10 (`rewrite_hof_func` for all HOF
rewrites) — HOF calls are now converted to typed `FunctionCall` + `Lambda`
expressions that `gen_function_call` already handles.

#### 9c — `LATERAL VIEW EXPLODE`

sqlparser-rs does not have a first-class `LATERAL VIEW` AST node.  Use the
`parse_statement` override in `SparkDialect` to detect `LATERAL VIEW` keywords
and emit a `Statement::Extension` carrying a custom `LateralViewClause` payload.

```rust
// In dialect.rs
use sqlparser::ast::{Statement, ObjectName};

pub struct LateralViewClause {
    pub function: String,         // "EXPLODE" or "POSEXPLODE"
    pub args: Vec<sqlparser::ast::Expr>,
    pub table_alias: String,
    pub column_aliases: Vec<String>,
    pub outer: bool,
}
```

`SqlConverter::convert_statement` matches `Statement::Extension` carrying
`LateralViewClause` and converts it to the Thunderduck `LogicalPlan` equivalent
(typically a `Project` wrapping an `UNNEST` via `FunctionCall`).

This is the first use of `parse_statement` override — follow the pattern from
ADR-21 §SparkDialect scope.

#### 9d — `DISTRIBUTE BY` / `SORT BY`

These are Hive sort directives with no DuckDB equivalent.  The strategy:

- `SORT BY` → treat as `ORDER BY` (within-partition sort approximation; semantics
  differ in distributed mode but Thunderduck is single-node)
- `DISTRIBUTE BY` → no-op (single-node: all data is local)

Add these as `Statement::Extension` via `parse_statement` override in
`SparkDialect`, or map them directly during `convert_select_body` if sqlparser-rs
parses them as `Select` modifiers.

**Differential test acceptance criteria**:
- `test_lambda_differential.py`
- `test_array_functions_differential.py` (HOF subset)

**Preprocessing phases retired after M9**: Phases 0 (backtick), 10 (HOF), and
potentially 11 (`json_tuple`, `from_json`) if those functions are wired through
`FunctionRegistry` properly.

---

### M10 — DML Passthrough

**Goal**: `CREATE TABLE`, `DROP TABLE`, `INSERT INTO`, `CREATE VIEW`, `SET` — pass
these through to DuckDB largely unchanged.

These statements do not produce a `LogicalPlan` in the relational sense.  The
existing `RawDdlStatement` plan node is the correct mapping:

```rust
Statement::CreateTable { .. } | Statement::Drop { .. } | Statement::Insert { .. }
    | Statement::CreateView { .. } | Statement::SetVariable { .. }
    => Ok(LogicalPlan::RawDdlStatement(RawDdlStatement {
        // Reconstruct the SQL from the sqlparser AST using its Display impl.
        // IMPORTANT: this is the one place where Display IS the correct output,
        // because gen_raw_ddl passes it verbatim to DuckDB without further
        // transformation by SqlGenerator.
        sql: stmt.to_string(),
    }))
```

**Important note**: `RawDdlStatement.sql` is passed verbatim to DuckDB by
`gen_raw_ddl`.  This is acceptable for DDL because DuckDB SQL is close enough to
Spark SQL DDL for the constructs used in differential tests.  If Spark DDL
diverges significantly from DuckDB DDL in future, a proper DDL converter will be
needed (tracked as a separate ADR).

**`SparkDialect` changes**: possibly `supports_table_hints`, `parse_insert` for
`INSERT OVERWRITE ... PARTITION (k=v)` handling.

**Differential test acceptance criteria**:
- `test_ddl_operations_differential.py`
- `test_ddl_corrected.py`
- `test_catalog_operations.py`

---

## Preprocessing Pass Retirement Schedule

| Preprocessing Phase | Deprecated by Milestone | When to Delete |
|---------------------|------------------------|----------------|
| Phase 0: backtick rewrite | M9a | After M9 differential tests pass |
| Phase 1: `ARRAY(` → `LIST_VALUE(` | M2 (function registry handles it) | After M2 differential tests pass |
| Phase 2: `NAMED_STRUCT(` rewrite | M2 / M9 (struct literal support) | After struct literal support verified |
| Phase 3: `MAP(...)` rewrite | M2 (via FunctionRegistry) | After M2 differential tests pass |
| Phase 4: 1:1 function renames | M2 (FunctionRegistry routing) | After M2–M3 differential tests pass |
| Phase 5: `percentile` rewrite | M3 (FunctionRegistry aggregate handling) | After M3 |
| Phase 6: `overlay` rewrite | M2 (FunctionRegistry special case) | After M2 |
| Phase 7: angle-bracket types | M2 (`convert_data_type` handles `ARRAY<T>`) | After M2 |
| Phase 8: `split(str, pat, n)` | M2 (FunctionRegistry) | After M2 |
| Phase 9: date interval arithmetic | M2 (binary op on date + interval literal) | After M2 |
| Phase 10: HOF rewrites | M9b (lambda + FunctionRegistry) | After M9 |
| Phase 11a: `json_tuple` | M9 (FunctionRegistry extension) | After M9 |
| Phase 11b: `from_json` | M9 (FunctionRegistry extension) | After M9 |

**Deletion is not automatic** — only remove a preprocessing phase after confirming
the corresponding differential tests pass through the new parser path without it.
Use `TD_DEBUG_SQL=1` to verify the generated SQL matches expectations.

---

## `SparkDialect` Scope Per Milestone

| Milestone | SparkDialect additions |
|-----------|----------------------|
| M1 | None (stub) |
| M2–M8 | Use default `GenericDialect` behaviour; `SparkDialect` scaffolded but empty |
| M9a | `is_delimited_identifier_start` (backtick), `is_proper_identifier_inside_quotes` |
| M9b | `supports_lambda_functions → true` |
| M9c | `parse_statement` override for `LATERAL VIEW` |
| M9d | `parse_statement` override for `DISTRIBUTE BY` / `SORT BY` |
| M10 | `parse_insert` override for `INSERT OVERWRITE ... PARTITION` (if needed) |

---

## Testing Strategy Per Milestone

Each milestone adds:

1. **Unit tests** in `crates/core/src/parser/` (`#[cfg(test)]` blocks in
   `sql_converter.rs` or a sibling `tests.rs`).  These test `SqlConverter` in
   isolation — no DuckDB, no gRPC.  Pattern: call `SparkSqlParser::parse(sql)`,
   assert the returned `LogicalPlan` shape using `matches!` or explicit field
   checks.

2. **Differential tests** — use the existing suite.  No new test files are
   needed; the existing tests already cover all constructs.  Run with:
   ```bash
   cargo build --release
   ./tests/scripts/run-differential-tests.sh all
   ```
   A milestone is complete when the listed differential tests pass without
   regression.

3. **Debug SQL verification** — temporarily set `TD_DEBUG_SQL=1` (add a debug
   `eprintln!` in `SqlGenerator::generate`) to inspect what DuckDB receives.
   Confirm it matches the expected rewrite.

---

## Acceptance Criteria (Full Phase 5)

Phase 5 is complete when:

1. All 670 differential tests pass through the new parser path (no fallback to
   `preprocess_spark_sql`).
2. `preprocess_spark_sql` and all its helper functions are deleted from
   `crates/core/src/generator/mod.rs`.
3. `LogicalPlan::SqlRelation` is removed (or kept only as a dead-code stub pending
   a decision on non-SELECT DDL passthrough).
4. `cargo test -p thunderduck-core` passes all unit tests including new parser
   unit tests.
5. `cargo build --release` produces a clean binary with no warnings in the parser
   module.

---

## Out of Scope (Excluded from Phase 5)

The following constructs are **not** addressed in Phase 5.  They remain as
`ThunderduckError::Unsupported` when encountered:

- **Recursive CTEs** (`WITH RECURSIVE`) — non-trivial semantics; deferred to a
  separate ADR if workloads require it.
- **`TABLESAMPLE (BUCKET n OUT OF m ON col)`** — Spark-specific sampler; requires
  `SparkDialect` `parse_table_factor` override and a new LogicalPlan node.
- **`TRANSFORM (cols) USING script`** — Hive streaming; requires external process
  invocation, not a pure SQL concern.
- **`INSERT OVERWRITE ... PARTITION (k=v)`** — partition-aware DML; requires
  understanding partition metadata.
- **Multi-insert `FROM t INSERT INTO t1 ... INSERT INTO t2`** — no DuckDB
  equivalent; requires multiple SQL statements.
- **Spark `INTERVAL expr TO expr` compound syntax** — rare; preprocessing handles
  the common case.
- **Full schema-aware column resolution** — the converter produces
  `UnresolvedColumn` references; resolution is done by DuckDB at execution time,
  not by Thunderduck at plan time.
- **Strict-mode CAST injection at the parser level** — Strict mode (Phase 6) adds
  CASTs at the `SqlGenerator` level, not the parser level.

---

## Key Design Decisions

### `SqlConverter` is Stateless

Unlike `RelationConverter` (which carries `session` for schema inference and
`ExpressionConverter` for lambda scope), `SqlConverter` is stateless — no
`&mut self`.  SQL string input is self-contained: there are no external schema
references needed for the parse → plan conversion step.  Column resolution
happens in DuckDB.

This keeps the converter simple and avoids threading concerns.

### `to_sql()` vs `Display` Invariant

`SqlConverter` builds `Expression` and `LogicalPlan` nodes — it never calls
`to_sql()` directly.  That invariant is preserved.  The only place `Display` is
used on a sqlparser AST node is in `M10` DDL passthrough where `stmt.to_string()`
reconstructs the SQL for `RawDdlStatement.sql` — this is deliberate and
documented.

### `SparkSqlParser` Lives in `core`, Not `connect-server`

The parser is part of the core translation engine (pure Rust, no gRPC, no
DuckDB).  This mirrors how `SqlGenerator` lives in `core`.  The `connect-server`
calls `SparkSqlParser::parse` but has no knowledge of its internals.

### Fallback Behaviour During Development

While a milestone is in progress, the integration point in `convert_sql` should
distinguish parser errors from "unsupported construct" errors:

```rust
fn convert_sql(&self, s: &proto::Sql) -> Result<LogicalPlan> {
    use thunderduck_core::parser::SparkSqlParser;
    SparkSqlParser::parse(&s.query)
        .map_err(|e| ConnectError::Unsupported(e.to_string()))
}
```

When `SparkSqlParser::parse` returns `ThunderduckError::Unsupported`, the gRPC
handler returns `Status::unimplemented`.  There is NO silent fallback to
`preprocess_spark_sql` — this is a deliberate design decision from ADR-21.
Unimplemented constructs fail loudly so coverage gaps are visible.

---

## File Change Summary

| File | Change |
|------|--------|
| `/workspace/Cargo.toml` | Add `sqlparser = "0.61"` to `[workspace.dependencies]` |
| `crates/core/Cargo.toml` | Add `sqlparser.workspace = true` |
| `crates/core/src/lib.rs` | Add `pub mod parser;` |
| `crates/core/src/parser/mod.rs` | New: `SparkSqlParser` public API |
| `crates/core/src/parser/dialect.rs` | New: `SparkDialect` |
| `crates/core/src/parser/sql_converter.rs` | New: `SqlConverter` |
| `crates/connect-server/src/converter/relation_converter.rs` | Modify `convert_sql` to call `SparkSqlParser::parse` |
| `crates/core/src/generator/mod.rs` | Remove `preprocess_spark_sql` and helpers (milestone by milestone) |
| `crates/core/src/logical/mod.rs` | `SqlRelation` may be removed in final cleanup |
