# Differential Test Failure Tracker

**Date**: 2026-04-03
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 723 passed, 112 failed, 1 skipped (836 total)

---

## Relaxed Mode Failures (8)

### ~~Map functions — data value mismatch (6 tests)~~ CLOSED

Fixed: `duckdb_ready` flag on SqlRelation prevents double-processing of DuckDB-native MAP syntax.

- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_keys`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_values`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_entries`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_size_map`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_element_at_map`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_explode_map`

### TPC-DS decimal value truncation (2 tests)

CASE WHEN with decimal/integer branches loses decimal precision in relaxed mode. `Decimal` values returned as `int` (truncated).

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]` — Q40: `sales_before`/`sales_after` Decimal→int
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[66]` — Q66: monthly net values Decimal→int

---

## Strict Mode Failures (115)

### Complex types — struct field nullable (8 tests)

Struct fields from DuckDB fallback all marked `nullable=true`. Spark preserves NOT NULL from source columns accessed via struct field access/update.

- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_dot_notation`
- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_bracket_notation`
- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_nested_struct_access`
- [ ] `test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_new`
- [ ] `test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_multiple`
- [ ] `test_complex_types_differential.py::TestDropFields_Differential::test_drop_single_field`
- [ ] `test_complex_types_differential.py::TestMultipleRows_Differential::test_struct_access_multiple_rows`
- [ ] `test_complex_types_differential.py::TestMultipleRows_Differential::test_array_index_multiple_rows`

### Array functions — type mismatch (2 tests)

- [ ] `test_dataframe_functions.py::TestArrayFunctions::test_flatten` — `ArrayType(IntegerType(), True)` vs `ArrayType(ArrayType(IntegerType(), True), True)` (extra nesting)
- [ ] `test_dataframe_functions.py::TestArrayFunctions::test_reverse_array` — `ArrayType(IntegerType(), True)` vs `StringType()` (wrong return type)

### ~~Map functions — type mismatch + data (7 tests)~~ PARTIALLY CLOSED

6 of 7 fixed by `duckdb_ready` flag. `map_from_arrays` still fails in strict (different root cause: map type construction in strict mode).

- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_keys`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_values`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_map_entries`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_size_map`
- [x] `test_dataframe_functions.py::TestMapFunctions::test_element_at_map`
- [ ] `test_dataframe_functions.py::TestMapFunctions::test_map_from_arrays` — strict only: map type construction
- [x] `test_dataframe_functions.py::TestMapFunctions::test_explode_map`

### Null functions — nullable mismatch (3 tests)

`isnull`/`isnotnull` result should be non-nullable (always returns true/false). `nvl2` result should be non-nullable when both branches are non-nullable.

- [ ] `test_dataframe_functions.py::TestNullFunctions::test_isnull` — `nullable=False` expected, got `True`
- [ ] `test_dataframe_functions.py::TestNullFunctions::test_isnotnull` — `nullable=False` expected, got `True`
- [ ] `test_dataframe_functions.py::TestNullFunctions::test_nvl2` — `nullable=False` expected, got `True`

### String functions — nullable mismatch (1 test)

- [ ] `test_dataframe_functions.py::TestStringFunctions::test_concat_ws` — `nullable=False` expected, got `True`

### Math functions — nullable mismatch (5 tests)

Spark marks math functions as `nullable=True` (edge cases like LN(0)→null). Thunderduck marks them non-nullable.

- [ ] `test_dataframe_functions.py::TestMathFunctions::test_ceil_floor` — `ceiling`/`floored` should be `nullable=True`
- [ ] `test_dataframe_functions.py::TestMathFunctions::test_round` — `round2`/`round0` should be `nullable=True`
- [ ] `test_dataframe_functions.py::TestMathFunctions::test_greatest_least` — `max_val`/`min_val` should be `nullable=False` (opposite direction)
- [ ] `test_dataframe_functions.py::TestMathFunctions::test_log` — `ln`/`log10`/`log2` should be `nullable=True`
- [ ] `test_dataframe_functions.py::TestMathFunctions::test_exp` — `exp_val` should be `nullable=True`

### TPC-H — decimal precision + type (3 tests)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — `Decimal(38,2)` vs `Decimal(32,2)` (SUM precision)
- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[14]` — `Decimal(38,6)` vs `DoubleType()` (division returning Double)
- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[17]` — `Decimal(30,6)` vs `DoubleType()` (AVG returning Double)

### JSON functions (1 test)

- [ ] `test_json_functions_differential.py::TestJsonConversion_Differential::test_schema_of_json` — schema_of_json return type/nullable

### Lambda/HOF — containsNull + nullable (16 tests)

Array containsNull and result nullable from HOF operations. Transform/filter/exists/forall results marked nullable=True when should be False.

- [ ] `test_lambda_differential.py::TestTransformFunction_Differential::test_transform_add_one`
- [ ] `test_lambda_differential.py::TestTransformFunction_Differential::test_transform_multiply`
- [ ] `test_lambda_differential.py::TestTransformFunction_Differential::test_transform_from_subquery`
- [ ] `test_lambda_differential.py::TestFilterFunction_Differential::test_filter_greater_than`
- [ ] `test_lambda_differential.py::TestFilterFunction_Differential::test_filter_even_numbers`
- [ ] `test_lambda_differential.py::TestFilterFunction_Differential::test_filter_all_pass`
- [ ] `test_lambda_differential.py::TestFilterFunction_Differential::test_filter_none_pass`
- [ ] `test_lambda_differential.py::TestExistsFunction_Differential::test_exists_true`
- [ ] `test_lambda_differential.py::TestExistsFunction_Differential::test_exists_false`
- [ ] `test_lambda_differential.py::TestForallFunction_Differential::test_forall_true`
- [ ] `test_lambda_differential.py::TestForallFunction_Differential::test_forall_false`
- [ ] `test_lambda_differential.py::TestNestedLambdas_Differential::test_nested_transform`
- [ ] `test_lambda_differential.py::TestNestedLambdas_Differential::test_transform_then_filter`
- [ ] `test_lambda_differential.py::TestCombinedOperations_Differential::test_transform_multiple_rows`
- [ ] `test_lambda_differential.py::TestCombinedOperations_Differential::test_filter_in_where`
- [ ] `test_lambda_differential.py::TestSQLLambda_Differential::test_sql_transform_with_table`

### Math/bitwise — type mismatch (1 test)

- [ ] `test_math_bitwise_date_differential.py::TestMathFunctions_Differential::test_bin` — `StringType()` vs `IntegerType()` (BIN return type)

### Pivot — nullable + schema (7 tests)

Pivot grouping columns lose non-nullable from DuckDB fallback. Pivot schema is empty → full DuckDB fallback.

- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_simple`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_with_values`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_agg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_avg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_max_min`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_groupby`
- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Cube/Rollup — nullable mismatch (3 tests)

GROUPING/GROUPING_ID nullable semantics differ.

- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping`
- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id`
- [ ] `test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping`

### Statistical aggregates — nullable mismatch (3 tests)

kurtosis/skewness nullable handling.

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — decimal precision/scale + nullable (30 tests)

Decimal precision cascades through CTE-heavy queries. Mix of SUM precision (+10), division scale, ROUND precision, and nullable mismatches.

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[4]` — Q4
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[5]` — Q5: `channel` nullable, `value` Decimal(38,2) vs Decimal(32,2)
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]` — Q9: scalar subquery decimal→Double
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[12]` — Q12
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14a]` — Q14a
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]` — Q14b
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[20]` — Q20
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23a]` — Q23a: gRPC error
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23b]` — Q23b: gRPC error
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[27]` — Q27
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]` — Q40: decimal→int
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[47]` — Q47: AVG precision
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[53]` — Q53: ROUND precision
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[57]` — Q57
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[58]` — Q58
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[61]` — Q61
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[63]` — Q63: ROUND precision
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[66]` — Q66: decimal→int
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[67]` — Q67
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[70]` — Q70
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[77]` — Q77
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]` — Q80
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[83]` — Q83
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[86]` — Q86
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[89]` — Q89
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[93]` — Q93
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[98]` — Q98

### TPC-DS DataFrame — decimal precision (10 tests)

DataFrame-path equivalents of SQL queries above. Same decimal precision root causes.

- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q12_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q20_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q62_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q84_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q98_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q99_dataframe`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[12]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[20]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[40]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[62]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[84]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[98]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[99]`

### TPC-H DataFrame — decimal precision (1 test)

- [ ] `test_tpch_differential.py::TestTPCHDifferential::test_q11_dataframe` — Decimal(38,2) vs Decimal(32,2)

### Type literals — interval arithmetic + map/struct types (14 tests)

Interval arithmetic type handling. Map/struct literal type construction.

- [ ] `test_type_literals_differential.py::TestTimestampNTZLiterals_Differential::test_timestamp_ntz_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_year_month_interval_in_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_year_month_interval_months_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_day_time_interval_days_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_day_time_interval_hours_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_day_time_interval_compound_arithmetic`
- [ ] `test_type_literals_differential.py::TestIntervalLiterals_Differential::test_interval_date_arithmetic`
- [ ] `test_type_literals_differential.py::TestArrayLiterals_Differential::test_array_with_null`
- [ ] `test_type_literals_differential.py::TestMapLiterals_Differential::test_map_literal_via_sql`
- [ ] `test_type_literals_differential.py::TestMapLiterals_Differential::test_map_from_arrays`
- [ ] `test_type_literals_differential.py::TestMapLiterals_Differential::test_map_pyspark_create_map`
- [ ] `test_type_literals_differential.py::TestStructLiterals_Differential::test_struct_field_access`
- [ ] `test_type_literals_differential.py::TestComplexNestedTypes_Differential::test_map_with_array_values`
- [ ] `test_type_literals_differential.py::TestEdgeCases_Differential::test_zero_interval`

---

## Summary by Root Cause Category

| Category | Relaxed | Strict | Total Unique |
|----------|---------|--------|-------------|
| ~~Map type construction (extra array nesting)~~ | ~~6~~ 0 | ~~7~~ 1 | ~~7~~ 1 | CLOSED (duckdb_ready flag) |
| Decimal precision/scale (SUM/AVG/DIV/ROUND cascades) | 2 | ~40 | ~40 |
| Nullable: struct fields from DuckDB fallback | 0 | 8 | 8 |
| Nullable: math functions should be always-nullable | 0 | 5 | 5 |
| Nullable: null-checking functions (isnull/isnotnull) | 0 | 3 | 3 |
| Nullable: HOF/lambda containsNull + result | 0 | 16 | 16 |
| Nullable: pivot grouping columns | 0 | 7 | 7 |
| Nullable: cube/rollup grouping | 0 | 3 | 3 |
| Nullable: statistical aggregates | 0 | 3 | 3 |
| Type: interval arithmetic | 0 | 7 | 7 |
| Type: array functions (flatten, reverse) | 0 | 2 | 2 |
| Type: BIN return type | 0 | 1 | 1 |
| Type: JSON schema_of_json | 0 | 1 | 1 |
| Type: string concat_ws nullable | 0 | 1 | 1 |
| gRPC errors (server crash) | 0 | ~3 | ~3 |
