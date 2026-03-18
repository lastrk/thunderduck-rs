# Phase 1 — Detailed Breakdown: Core Types + SQL Generation

## Objective

Build a fully unit-tested `crates/core` crate that translates any `LogicalPlan` tree to correct DuckDB SQL. Zero runtime dependencies (no DuckDB, no gRPC, no Arrow, no network).

At the end of Phase 1, `cargo test -p thunderduck-core` passes everything. The output of the core crate is SQL strings; correctness is validated by comparing generated SQL to expected strings in unit tests.

---

## Module Breakdown

### 1.1 `types` module — `DataType`, `StructType`, `TypeMapper`

**Files**: `crates/core/src/types/mod.rs`, `data_type.rs`, `struct_type.rs`, `type_mapper.rs`, `type_inference.rs`

**Tasks**:

- [ ] Define `DataType` enum with all variants:
  - Scalar: `Boolean`, `Byte`, `Short`, `Integer`, `Long`, `Float`, `Double`, `String`, `Binary`, `Date`, `Timestamp`, `TimestampNtz`, `Null`, `Unresolved`
  - Parameterised: `Decimal { precision: u8, scale: u8 }`
  - Compound: `Array(Box<DataType>)`, `Map { key: Box<DataType>, value: Box<DataType>, value_nullable: bool }`, `Struct(StructType)`
  - Interval: `YearMonthInterval`, `DayTimeInterval`
- [ ] Implement `PartialEq`, `Clone`, `Debug` for `DataType`
- [ ] Define `StructType { fields: Vec<StructField> }` and `StructField { name: String, data_type: DataType, nullable: bool }`
- [ ] `StructType::field_by_name(&str) -> Option<&StructField>`
- [ ] `StructType::field_index(&str) -> Option<usize>`
- [ ] Define `TypeMapper`: maps `DataType` → DuckDB SQL type string (e.g., `Long` → `"BIGINT"`, `Decimal{p,s}` → `"DECIMAL(p,s)"`)
- [ ] `TypeInferenceEngine` — centralised type resolution:
  - `infer_column_type(name: &str, schema: &StructType) -> DataType`
  - `infer_binary_type(op: BinaryOp, left: &DataType, right: &DataType) -> DataType` — Spark's promotion rules
  - `infer_function_type(name: &str, args: &[DataType]) -> DataType` — handles all 500+ functions
  - `infer_aggregate_type(name: &str, arg: &DataType) -> DataType` — `COUNT` → `Long`, `SUM(Integer)` → `Long`, `AVG(Decimal)` → `Decimal`
  - `infer_window_type(name: &str, arg: Option<&DataType>) -> DataType` — `ROW_NUMBER`/`RANK` → `Integer`
  - `is_nullable_aggregate(name: &str) -> bool` — `COUNT` → `false`, others → depends on arg

**Key Spark type promotion rules to encode**:
- Integer arithmetic: `Integer op Integer → Integer`, `Integer op Long → Long`, `Integer op Double → Double`, `Long op Double → Double`
- `SUM(Byte/Short/Integer)` → `Long` (not `HugeInt` — DuckDB default)
- `SUM(Float)` → `Double`
- `AVG(Integer/Long)` → `Double`
- `AVG(Decimal(p,s))` → `Decimal(p+4, s+4)`
- `COUNT(*)` → `Long`, always non-nullable
- String concat (`||`) → `String`

**Unit tests** (`types/tests.rs`):
- Every `DataType` variant round-trips through `TypeMapper`
- All binary operator type promotions
- Aggregate return types for every aggregate function name
- `StructType::field_by_name` finds and misses correctly

---

### 1.2 `expression` module — `Expression` enum + all variants

**Files**: `crates/core/src/expression/mod.rs`, one file per major expression group

**Tasks**:

- [ ] Define `Expression` enum (all variants — see architecture.md ADR-07)
- [ ] For each variant, define the inner struct with all necessary fields:
  - `Literal { value: LiteralValue, data_type: DataType }` — `LiteralValue` enum for all scalar types + `Null`
  - `ColumnReference { name: String, qualifier: Option<String>, data_type: DataType, nullable: bool }`
  - `UnresolvedColumn { name: String, qualifier: Option<String> }`
  - `BinaryExpression { op: BinaryOp, left: Box<Expression>, right: Box<Expression> }`
  - `UnaryExpression { op: UnaryOp, operand: Box<Expression> }`
  - `FunctionCall { name: String, args: Vec<Expression>, distinct: bool }`
  - `CastExpression { expr: Box<Expression>, to_type: DataType }`
  - `CaseWhenExpression { branches: Vec<(Expression, Expression)>, else_expr: Option<Box<Expression>> }`
  - `WindowFunction { func: Box<Expression>, partition_by: Vec<Expression>, order_by: Vec<SortOrder>, frame: Option<WindowFrame> }`
  - `AliasExpression { expr: Box<Expression>, alias: String }`
  - `InSubquery { expr: Box<Expression>, subquery: Box<LogicalPlan>, negated: bool }`
  - `ExistsSubquery { subquery: Box<LogicalPlan>, negated: bool }`
  - `ScalarSubquery { subquery: Box<LogicalPlan> }`
  - `LambdaExpression { params: Vec<String>, body: Box<Expression> }`
  - `LambdaVariableExpression { name: String }`
  - `RawSqlExpression { sql: String }`
  - `ArrayLiteralExpression { elements: Vec<Expression>, element_type: DataType }`
  - `MapLiteralExpression { keys: Vec<Expression>, values: Vec<Expression> }`
  - `StructLiteralExpression { fields: Vec<(String, Expression)> }`
  - `BetweenExpression { expr: Box<Expression>, low: Box<Expression>, high: Box<Expression>, negated: bool }`
- [ ] Define `BinaryOp` enum: `Add`, `Sub`, `Mul`, `Div`, `Mod`, `Eq`, `NotEq`, `Lt`, `LtEq`, `Gt`, `GtEq`, `And`, `Or`, `Concat` (string `||`)
- [ ] Define `UnaryOp` enum: `Not`, `Negate`, `IsNull`, `IsNotNull`
- [ ] Define `SortOrder { expr: Expression, direction: SortDirection, null_ordering: NullOrdering }`
- [ ] Define `WindowFrame { unit: FrameUnit, start: FrameBoundary, end: FrameBoundary }`
- [ ] Implement `Expression::to_sql(&self, generator: &SqlGenerator) -> String` — exhaustive `match`, **never** `Display`
- [ ] Implement `Expression::data_type(&self, schema: &StructType) -> DataType` — delegates to `TypeInferenceEngine`
- [ ] Implement `Expression::nullable(&self) -> bool`

**Critical rules**:
- `to_sql()` must call `to_sql()` recursively on child expressions — never `format!("{}", child)` or `child.to_string()`
- `FunctionCall::to_sql()` calls `FunctionRegistry::translate()` — never hardcodes DuckDB function names
- `BinaryOp::Div` for Decimal types routes through extension function in strict mode

**Unit tests** (`expression/tests.rs`):
- Every expression variant: `to_sql()` produces expected SQL string
- `Literal` all types: `42i32` → `"42"`, `"hello"` → `"'hello'"`, `null` → `"NULL"`, date → `"DATE '2024-01-01'"`
- `ColumnReference` quoting: handles reserved words, spaces, dots
- `BinaryExpression` all operators: precedence-correct parenthesisation
- `FunctionCall`: routed through registry, args recursively rendered
- `CastExpression`: `CAST(expr AS BIGINT)`
- `CaseWhenExpression`: full CASE/WHEN/THEN/ELSE/END
- `WindowFunction`: partition, order, frame all render correctly
- Nested expressions (function inside binary inside case)

---

### 1.3 `logical` module — `LogicalPlan` enum + all node structs

**Files**: `crates/core/src/logical/mod.rs`, one file per plan node group

**Tasks**:

- [ ] Define `LogicalPlan` enum (all 29 variants — see architecture.md ADR-06):

  | Variant | Fields |
  |---------|--------|
  | `Project` | `input: Box<LogicalPlan>, projections: Vec<Expression>` |
  | `Filter` | `input: Box<LogicalPlan>, condition: Expression` |
  | `Aggregate` | `input: Box<LogicalPlan>, grouping: Vec<Expression>, aggregates: Vec<AggregateExpr>, having: Option<Expression>, grouping_sets: Option<GroupingSets>, select_order: Vec<SelectEntry>` |
  | `Join` | `left: Box<LogicalPlan>, right: Box<LogicalPlan>, join_type: JoinType, condition: Option<Expression>, using_columns: Vec<String>` |
  | `Sort` | `input: Box<LogicalPlan>, order: Vec<SortOrder>, limit: Option<Expression>, offset: Option<Expression>` |
  | `Limit` | `input: Box<LogicalPlan>, limit: Expression` |
  | `Tail` | `input: Box<LogicalPlan>, limit: Expression` |
  | `Union` | `left: Box<LogicalPlan>, right: Box<LogicalPlan>, all: bool` |
  | `Except` | `left: Box<LogicalPlan>, right: Box<LogicalPlan>, all: bool` |
  | `Intersect` | `left: Box<LogicalPlan>, right: Box<LogicalPlan>, all: bool` |
  | `Distinct` | `input: Box<LogicalPlan>` |
  | `Sample` | `input: Box<LogicalPlan>, fraction: f64, seed: Option<i64>, with_replacement: bool` |
  | `TableScan` | `table: String, alias: Option<String>` |
  | `SqlRelation` | `sql: String` |
  | `LocalRelation` | `schema: StructType` |
  | `LocalDataRelation` | `schema: StructType, data: Vec<arrow::record_batch::RecordBatch>` (gated behind feature flag for Phase 1, schema only) |
  | `RangeRelation` | `start: i64, end: i64, step: i64` |
  | `InMemoryRelation` | `view_name: String, schema: StructType` |
  | `WithCte` | `ctes: Vec<(String, Box<LogicalPlan>)>, input: Box<LogicalPlan>` |
  | `WithColumns` | `input: Box<LogicalPlan>, columns: Vec<(String, Expression)>` |
  | `AliasedRelation` | `input: Box<LogicalPlan>, alias: String, column_aliases: Vec<String>` |
  | `RawDdlStatement` | `sql: String` |
  | `ToDataFrame` | `input: Box<LogicalPlan>, column_names: Vec<String>` |

- [ ] Define `JoinType` enum: `Inner`, `Left`, `Right`, `Full`, `Cross`, `LeftSemi`, `LeftAnti`
- [ ] Define `AggregateExpr { func: Expression, is_distinct: bool, filter: Option<Expression> }`
- [ ] Define `GroupingSets` enum: `Rollup(Vec<Vec<Expression>>)`, `Cube(Vec<Vec<Expression>>)`, `GroupingSets(Vec<Vec<Expression>>)`
- [ ] Define `SelectEntry` enum: `Column(Expression)`, `Aggregate(usize)` — for interleaving in Aggregate output
- [ ] Implement `LogicalPlan::infer_schema(&self) -> StructType` — each variant computes its output schema
- [ ] Note: `infer_schema` on `TableScan` returns `StructType::unknown()` in Phase 1 (resolved at runtime in Phase 2+)

**Unit tests** (`logical/tests.rs`):
- Construct each plan variant; verify `infer_schema()` returns correct field names and types where inferrable
- Verify plan trees can be nested (Filter wrapping Project wrapping TableScan)

---

### 1.4 `functions` module — `FunctionRegistry`

**Files**: `crates/core/src/functions/mod.rs`, `registry.rs`, `mappings/*.rs`

**Tasks**:

- [ ] Define `FunctionRegistry` with:
  - `direct: HashMap<&'static str, &'static str>` — majority of mappings
  - `custom: HashMap<&'static str, fn(args: &[&str], mode: CompatMode) -> String>` — complex cases
- [ ] Static initialisation via `std::sync::LazyLock` (Rust 1.80+)
- [ ] Port all 500+ function mappings from the Java `FunctionRegistry`, organised in sub-modules:
  - `string_functions.rs` — `upper`, `lower`, `trim`, `substring`, `concat`, `split`, `initcap`, etc.
  - `math_functions.rs` — `abs`, `ceil`, `floor`, `round`, `sqrt`, `pow`, `log`, trig functions, etc.
  - `date_functions.rs` — `year`, `month`, `day`, `date_add`, `datediff`, `to_timestamp`, etc.
  - `aggregate_functions.rs` — `sum`, `avg`, `min`, `max`, `count`, `stddev`, `variance`, `percentile`, etc.
  - `window_functions.rs` — `row_number`, `rank`, `dense_rank`, `lag`, `lead`, `first_value`, `last_value`, etc.
  - `array_functions.rs` — `array_contains`, `array_concat`, `explode`, `flatten`, `transform`, `filter`, etc.
  - `conditional_functions.rs` — `if`, `coalesce`, `nullif`, `nvl`, etc.
  - `json_functions.rs` — `get_json_object`, `json_tuple`, `from_json`, `to_json`, etc.
  - `cast_functions.rs` — `to_date`, `to_timestamp`, `to_number`, `try_cast`, etc.
- [ ] `FunctionRegistry::translate(name: &str, args: &[&str], mode: CompatMode) -> String`
- [ ] Mode-aware routing: in strict mode, `round()` and `avg()` on Decimal → extension functions
- [ ] DuckDB macro registration list: functions that need `CREATE MACRO` at session startup (e.g., `initcap`)

**Unit tests** (`functions/tests.rs`):
- Every direct mapping translates correctly
- Every custom translator produces expected SQL for representative inputs
- Mode-aware functions route correctly in both strict and relaxed mode
- Unknown function name falls through to passthrough (name unchanged)

---

### 1.5 `generator` module — `SqlGenerator`

**Files**: `crates/core/src/generator/mod.rs`, `sql_generator.rs`, `quoting.rs`

**Tasks**:

- [ ] Define `SqlGenerator { alias_counter: u32, subquery_depth: u32, compat_mode: CompatMode }`
- [ ] `SqlGenerator::generate(&mut self, plan: &LogicalPlan) -> String` — exhaustive `match` on all 29 variants
- [ ] Implement `visit_*` methods for each plan node. Key generation patterns:

  **Project**:
  ```sql
  SELECT <projections> FROM (<child_sql>) AS <alias>
  ```

  **Filter**:
  ```sql
  SELECT * FROM (<child_sql>) AS <alias> WHERE <condition>
  ```
  (or merged into parent SELECT WHERE when nesting is unnecessary)

  **Aggregate**:
  ```sql
  SELECT <select_list> FROM (<child_sql>) AS <alias>
  [GROUP BY <grouping>]
  [HAVING <condition>]
  ```
  - Handles ROLLUP: `GROUP BY ROLLUP(<cols>)`
  - Handles CUBE: `GROUP BY CUBE(<cols>)`
  - Handles GROUPING SETS: `GROUP BY GROUPING SETS((<a>),(<b>),...)`
  - Single canonical path — no dual-path issue

  **Join** (DUAL PATH — both must be maintained):
  - `visit_join()`: converts LeftSemi → `WHERE EXISTS (...)`, LeftAnti → `WHERE NOT EXISTS (...)`; uses `SEMI JOIN` / `ANTI JOIN` syntax for DuckDB (no `LEFT` prefix)
  - `generate_flat_join_chain()`: flattens chain of INNER joins into single `FROM a JOIN b ON ... JOIN c ON ...`; breaks chain at LeftSemi/LeftAnti

  **Sort**:
  ```sql
  SELECT * FROM (<child_sql>) AS <alias>
  ORDER BY <exprs>
  [LIMIT <n>] [OFFSET <m>]
  ```

  **Union/Except/Intersect**:
  ```sql
  (<left_sql>) UNION [ALL] (<right_sql>)
  ```

  **TableScan**:
  ```sql
  <table_name> [AS <alias>]
  ```

  **WithCte**:
  ```sql
  WITH <name1> AS (<sql1>), <name2> AS (<sql2>) <input_sql>
  ```

  **Sample**:
  ```sql
  SELECT * FROM (<child_sql>) AS <alias> USING SAMPLE <pct>%
  ```

  **Range**:
  ```sql
  SELECT (range + <start>) AS id FROM range(<count>)
  ```

  **RawDdlStatement**: emit SQL string directly

- [ ] `SqlQuoting::quote_identifier(name: &str) -> String` — double-quote identifiers that are DuckDB reserved words or contain special characters; detect reserved word list

- [ ] Auto-alias generation: `__td_<counter>` (e.g., `__td_0`, `__td_1`) for subquery aliases; counter increments per `generate()` call

- [ ] Recursive subquery generation: `generate()` called on child plans; subquery depth tracked for proper aliasing

**Unit tests** (`generator/tests.rs`):
- Every plan node type: given a hand-constructed `LogicalPlan` tree, assert `generate()` produces exactly the expected SQL string
- Key cases:
  - Nested Project → Filter → TableScan → correct single SQL with WHERE
  - Join with USING columns vs ON condition
  - LeftSemi join → EXISTS subquery
  - Aggregate with ROLLUP
  - CTE with multiple WITH clauses
  - LIMIT + OFFSET on Sort
  - Sample with seed
  - Identifier quoting: table named `order` → `"order"`
  - All 12 binary operators render with correct precedence
  - Window function with PARTITION BY + ORDER BY + ROWS BETWEEN frame

---

### 1.6 `error` module

**Files**: `crates/core/src/error.rs`

**Tasks**:

- [ ] Define `ThunderduckError` enum with `thiserror`:
  ```rust
  #[derive(thiserror::Error, Debug)]
  pub enum ThunderduckError {
      #[error("SQL generation failed: {0}")]
      SqlGeneration(String),
      #[error("Type inference error: {0}")]
      TypeInference(String),
      #[error("Unsupported operation: {0}")]
      Unsupported(String),
      #[error("Parse error: {0}")]
      Parse(String),
      #[error("Schema error: {0}")]
      Schema(String),
  }
  ```
- [ ] `pub type Result<T> = std::result::Result<T, ThunderduckError>;`

---

### 1.7 `Cargo.toml` and workspace setup

**Files**: `Cargo.toml` (workspace), `crates/core/Cargo.toml`

**Tasks**:

- [ ] Create workspace `Cargo.toml`:
  ```toml
  [workspace]
  members = ["crates/core", "crates/connect-server"]
  resolver = "2"

  [workspace.dependencies]
  thiserror = "2"
  # Add more shared deps as needed
  ```

- [ ] `crates/core/Cargo.toml` — Phase 1 dependencies only:
  ```toml
  [package]
  name = "thunderduck-core"
  version = "0.1.0"
  edition = "2021"

  [dependencies]
  thiserror = { workspace = true }
  once_cell = "1"   # or use std::sync::LazyLock (Rust 1.80+)

  [dev-dependencies]
  # No extra test deps needed for Phase 1
  ```

- [ ] Note: `duckdb`, `arrow`, `tonic`, `prost` are **NOT** in `core/Cargo.toml`. Added in Phase 2+.

---

## Phase 1 Test Plan

### Unit Test Coverage Requirements

Every module must have a `#[cfg(test)] mod tests` block. Minimum coverage:

| Module | Tests |
|--------|-------|
| `types` | 20+ tests |
| `expression` | 40+ tests (at least 2 per variant) |
| `logical` | 30+ tests |
| `functions` | 50+ tests (representative sample + edge cases) |
| `generator` | 60+ tests (every plan node, every expression variant in context) |

### Key Test Scenarios for `SqlGenerator`

These must all produce byte-for-byte correct SQL:

1. **Simple Project + TableScan**: `SELECT a, b FROM my_table`
2. **Filter**: `SELECT * FROM (SELECT * FROM t) AS __td_0 WHERE x > 10`
3. **Aggregate**: `SELECT dept, SUM(salary) AS total FROM emp GROUP BY dept`
4. **Aggregate with HAVING**: adds `HAVING SUM(salary) > 50000`
5. **ROLLUP**: `GROUP BY ROLLUP(a, b)`
6. **Inner Join ON**: `t1 JOIN t2 ON t1.id = t2.id`
7. **Inner Join USING**: `t1 JOIN t2 USING (id)`
8. **Left Semi Join**: `WHERE EXISTS (SELECT 1 FROM t2 WHERE t1.id = t2.id)`
9. **Left Anti Join**: `WHERE NOT EXISTS (...)`
10. **Sort + Limit + Offset**: `ORDER BY col DESC LIMIT 10 OFFSET 20`
11. **Union ALL / UNION**: `(sql1) UNION ALL (sql2)`
12. **CTE**: `WITH cte AS (SELECT ...) SELECT * FROM cte`
13. **Window function**: `ROW_NUMBER() OVER (PARTITION BY a ORDER BY b ROWS BETWEEN ...)`
14. **Subquery expression**: scalar subquery in SELECT
15. **Reserved word quoting**: `"order"`, `"group"`, `"select"` as identifiers
16. **String literal escaping**: single quotes in strings
17. **Decimal literal**: `DECIMAL '123.45'`
18. **Cast**: `CAST(x AS BIGINT)`
19. **CASE WHEN**: full form with multiple branches and ELSE
20. **SUM(integer)**: generates `CAST(SUM(x) AS BIGINT)` to avoid HUGEINT

---

## Definition of Done for Phase 1

- [ ] `cargo check -p thunderduck-core` — zero warnings (use `#[allow]` only where justified)
- [ ] `cargo test -p thunderduck-core` — all tests pass
- [ ] `cargo clippy -p thunderduck-core` — zero lint errors
- [ ] All 29 `LogicalPlan` variants handled in `SqlGenerator::generate()` — no `todo!()` or `unimplemented!()`
- [ ] All 21+ `Expression` variants handled in `Expression::to_sql()` — no `todo!()` or `unimplemented!()`
- [ ] `FunctionRegistry` contains all 500+ mappings from the Java reference
- [ ] No `unwrap()` or `expect()` in non-test code (use `?` and `ThunderduckError`)
