# Differential Test Failure Tracker

**Date**: 2026-04-04
**Relaxed mode**: 828 passed, 2 failed, 6 skipped (836 total)
**Strict mode**: 755 passed, 80 failed, 1 skipped (836 total)

---

## Relaxed Mode Failures (2)

### TPC-DS decimal value truncation (2 tests)

CASE WHEN with decimal/integer branches loses decimal precision. Decimal values returned as `int`.

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]` — Q40: sales_before/sales_after Decimal→int
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[66]` — Q66: monthly net values Decimal→int

---

## Strict Mode Failures (80)

### Complex types — struct field nullable from DuckDB fallback (8 tests)

Schema from `createDataFrame` with NOT NULL fields doesn't propagate through struct access.

- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_dot_notation`
- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_bracket_notation`
- [ ] `test_complex_types_differential.py::TestStructFieldAccess_Differential::test_nested_struct_access`
- [ ] `test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_new`
- [ ] `test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_multiple`
- [ ] `test_complex_types_differential.py::TestDropFields_Differential::test_drop_single_field`
- [ ] `test_complex_types_differential.py::TestMultipleRows_Differential::test_struct_access_multiple_rows`
- [ ] `test_complex_types_differential.py::TestMultipleRows_Differential::test_array_index_multiple_rows`

### TPC-H — decimal precision + type (3 tests)

- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[11]` — Decimal(38,2) vs Decimal(32,2)
- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[14]` — Decimal(38,6) vs DoubleType()
- [ ] `test_differential_v2.py::TestTPCH_AllQueries_Differential::test_query_differential[17]` — Decimal(30,6) vs DoubleType()

### JSON functions (1 test)

- [ ] `test_json_functions_differential.py::TestJsonConversion_Differential::test_schema_of_json`

### Lambda/HOF — containsNull + nullable (16 tests)

DataFrame-path HOF operations: containsNull and result nullable not matching Spark.

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

### Pivot — nullable from DuckDB fallback (7 tests)

- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_simple`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_with_values`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_agg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_avg`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_max_min`
- [ ] `test_multidim_aggregations.py::TestPivotFunctions::test_pivot_multiple_groupby`
- [ ] `test_multidim_aggregations.py::TestAdvancedAggregations::test_pivot_then_aggregate`

### Cube/Rollup — nullable mismatch (3 tests)

- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping`
- [ ] `test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id`
- [ ] `test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping`

### Statistical aggregates — nullable mismatch (3 tests)

- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_kurtosis`
- [ ] `test_new_aggregates_differential.py::TestStatisticalAggregates_Differential::test_skewness`
- [ ] `test_new_aggregates_differential.py::TestGroupedNewAggregates_Differential::test_kurtosis_grouped`

### TPC-DS SQL — decimal precision/scale + nullable (25 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[4]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[5]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[9]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[12]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[20]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[27]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[40]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[47]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[53]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[57]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[58]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[61]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[63]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[66]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[67]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[70]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[77]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[80]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[83]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[86]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[89]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[93]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[98]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14a]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[14b]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23a]`
- [ ] `test_tpcds_differential.py::TestTPCDS_Differential::test_query_differential[23b]`

### TPC-DS DataFrame — decimal precision (11 tests)

- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q12_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q20_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q62_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q98_dataframe`
- [ ] `test_tpcds_differential.py::TestTPCDS_DataFrame_Differential::test_q99_dataframe`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[12]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[20]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[40]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[62]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[98]`
- [ ] `test_tpcds_dataframe_differential.py::test_tpcds_dataframe_query[99]`

### TPC-H DataFrame (1 test)

- [ ] `test_tpch_differential.py::TestTPCHDifferential::test_q11_dataframe`

---

## Summary by Root Cause

| Category | Relaxed | Strict | Notes |
|----------|---------|--------|-------|
| TPC-DS/TPC-H decimal precision | 2 | ~36 | CTE schema + expression-level gaps |
| Lambda/HOF containsNull + nullable | 0 | 16 | DataFrame-path schema not propagated |
| Struct field nullable (DuckDB fallback) | 0 | 8 | createDataFrame NOT NULL lost |
| Pivot grouping nullable | 0 | 7 | Pivot schema empty → DuckDB fallback |
| Cube/Rollup grouping nullable | 0 | 3 | GROUPING/GROUPING_ID semantics |
| Statistical aggregate nullable | 0 | 3 | kurtosis/skewness |
| JSON schema_of_json | 0 | 1 | Return type |
