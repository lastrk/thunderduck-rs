# v2 corpus goal pass log (2026-07-09 —)

Goal: zero UNDOCUMENTED failures in the full differential suite
(`tests/scripts/differential-progress.sh`); "documented" = entry in
`.agent-output/unsolvable.md`. Cap 40 passes. This file is the per-pass log
AND the pass-0 regression oracle: no case listed as green at pass 0 may go
red in any later pass.

**Deferred fixable defects/parity gaps** discovered during these passes are
consolidated in [`tasks/v2-corpus-followups.md`](v2-corpus-followups.md)
(distinct from `unsolvable.md`, which is only for ADR-022 unsolvable cases).
When a pass defers a reviewer finding or STOPs on a sub-case, record it there.

## Pass 0 — 2026-07-09 — baseline

- Recorder row (commit 50ac9c4): **1085 passed / 320 failed / 1405 total**
  — DF corpus 369/384, SQL corpus 308/396, other (legacy files) 408/625.
- Raw log: `.agent-output/pass0-full.log` (gitignored, regenerate with
  `DIFFERENTIAL_PROGRESS_LOG=... tests/scripts/differential-progress.sh`).
- Tooling this pass: recorder gained `DIFFERENTIAL_PROGRESS_LOG` keep-log
  hook; `.agent-output/unsolvable.md` recreated fresh (prior gitignored copy
  lost) and force-tracked.

### Baseline failure list (320 — the regression oracle)

    differential/test_aggregation_functions_differential.py::TestCountDistinct::test_count_distinct_multiple_columns
    differential/test_array_functions_differential.py::TestArrayFunctionsDifferential::test_collect_list_with_groupby
    differential/test_array_functions_differential.py::TestArrayFunctionsDifferential::test_size_in_sql_with_filter
    differential/test_array_functions_differential.py::TestArrayFunctionsDifferential::test_split_with_limit
    differential/test_array_functions_differential.py::TestArrayFunctionsDifferential::test_split_with_limit_dataframe_api
    differential/test_catalog_operations.py::TestCurrentCatalog::test_current_catalog_returns_string
    differential/test_catalog_operations.py::TestCurrentDatabase::test_current_database_returns_string
    differential/test_catalog_operations.py::TestDatabaseExists::test_nonexistent_database
    differential/test_catalog_operations.py::TestDropTempView::test_drop_existing_temp_view
    differential/test_catalog_operations.py::TestDropTempView::test_drop_nonexistent_temp_view
    differential/test_catalog_operations.py::TestFunctionExists::test_function_exists_common_functions
    differential/test_catalog_operations.py::TestFunctionExists::test_function_exists_false
    differential/test_catalog_operations.py::TestFunctionExists::test_function_exists_true
    differential/test_catalog_operations.py::TestGetFunction::test_get_function_exists
    differential/test_catalog_operations.py::TestListFunctions::test_list_functions_includes_common_functions
    differential/test_catalog_operations.py::TestTableExists::test_table_exists_false
    differential/test_catalog_operations.py::TestTempViewDifferential::test_temp_view_appears_in_table_exists
    differential/test_catalog_operations.py::TestTempViewDifferential::test_temp_view_not_found_after_drop
    differential/test_column_operations_differential.py::TestColumnOperationsDifferential::test_drop_with_range
    differential/test_column_operations_differential.py::TestColumnOperationsDifferential::test_with_column_on_range
    differential/test_complex_types_differential.py::TestArrayIndexing_Differential::test_array_first_element
    differential/test_complex_types_differential.py::TestArrayIndexing_Differential::test_array_middle_element
    differential/test_complex_types_differential.py::TestChainedExtraction_Differential::test_array_of_structs
    differential/test_complex_types_differential.py::TestChainedExtraction_Differential::test_struct_with_array
    differential/test_complex_types_differential.py::TestDropFields_Differential::test_drop_single_field
    differential/test_complex_types_differential.py::TestMapKeyAccess_Differential::test_map_missing_key
    differential/test_complex_types_differential.py::TestMapKeyAccess_Differential::test_map_string_key
    differential/test_complex_types_differential.py::TestMultipleRows_Differential::test_array_index_multiple_rows
    differential/test_complex_types_differential.py::TestMultipleRows_Differential::test_struct_access_multiple_rows
    differential/test_complex_types_differential.py::TestStructFieldAccess_Differential::test_nested_struct_access
    differential/test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_bracket_notation
    differential/test_complex_types_differential.py::TestStructFieldAccess_Differential::test_struct_field_dot_notation
    differential/test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_multiple
    differential/test_complex_types_differential.py::TestUpdateFields_Differential::test_with_field_add_new
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_distinct_operations
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_filter_operations
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_groupby_aggregation
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_join_operations
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_select_columns
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_union_operations
    differential/test_dataframe_basic_operations_differential.py::TestDataFrameBasicOperationsDifferential::test_window_functions
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q007]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q009]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q012]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q013]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q017]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q020]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q025]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q026]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q029]
    differential/test_dataframe_corpus_differential.py::test_case[tpcds-q098]
    differential/test_dataframe_corpus_differential.py::test_case[tpch-q01]
    differential/test_dataframe_corpus_differential.py::test_case[tpch-q07]
    differential/test_dataframe_corpus_differential.py::test_case[tpch-q08]
    differential/test_dataframe_corpus_differential.py::test_case[tpch-q11]
    differential/test_dataframe_corpus_differential.py::test_case[tpch-q21]
    differential/test_dataframe_functions.py::TestArrayFunctions::test_array_distinct
    differential/test_dataframe_functions.py::TestArrayFunctions::test_array_except
    differential/test_dataframe_functions.py::TestArrayFunctions::test_array_intersect
    differential/test_dataframe_functions.py::TestArrayFunctions::test_array_union
    differential/test_dataframe_functions.py::TestArrayFunctions::test_reverse_array
    differential/test_dataframe_functions.py::TestMapFunctions::test_explode_map
    differential/test_dataframe_functions.py::TestMapFunctions::test_map_from_arrays
    differential/test_dataframe_functions.py::TestMapFunctions::test_size_map
    differential/test_dataframe_functions.py::TestMathFunctions::test_ceil_floor
    differential/test_dataframe_functions.py::TestMathFunctions::test_exp
    differential/test_dataframe_functions.py::TestMathFunctions::test_log
    differential/test_dataframe_functions.py::TestMathFunctions::test_round
    differential/test_dataframe_ops_differential.py::TestTail_Differential::test_tail
    differential/test_dataframe_ops_differential.py::TestTail_Differential::test_tail_more_than_rows
    differential/test_dataframe_ops_differential.py::TestWriteOperation_Differential::test_write_csv
    differential/test_dataframe_ops_differential.py::TestWriteOperation_Differential::test_write_json
    differential/test_dataframe_ops_differential.py::TestWriteOperation_Differential::test_write_parquet
    differential/test_datetime_functions_differential.py::TestDateArithmetic::test_add_months
    differential/test_datetime_functions_differential.py::TestDateArithmetic::test_date_add
    differential/test_datetime_functions_differential.py::TestDateArithmetic::test_date_sub
    differential/test_datetime_functions_differential.py::TestDateTruncation::test_next_day
    differential/test_ddl_corrected.py::TestInsertDifferentialCorrected::test_insert_using_string_type
    differential/test_ddl_corrected.py::TestInsertDifferentialCorrected::test_insert_with_correct_syntax
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_alter_table_add_column
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_create_if_not_exists
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_create_table
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_drop_if_exists
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_drop_table
    differential/test_ddl_operations_differential.py::TestDDLDifferential::test_truncate_table
    differential/test_ddl_operations_differential.py::TestErrorHandlingDifferential::test_duplicate_table
    differential/test_ddl_operations_differential.py::TestErrorHandlingDifferential::test_type_mismatch
    differential/test_ddl_operations_differential.py::TestInsertDifferential::test_insert_multiple_rows
    differential/test_ddl_operations_differential.py::TestInsertDifferential::test_insert_select
    differential/test_ddl_operations_differential.py::TestInsertDifferential::test_insert_single_row
    differential/test_ddl_operations_differential.py::TestInsertDifferential::test_insert_with_nulls
    differential/test_ddl_operations_differential.py::TestInsertDifferential::test_multiple_inserts_then_aggregate
    differential/test_ddl_operations_differential.py::TestWorkflowDifferential::test_create_insert_select_workflow
    differential/test_ddl_parser_differential.py::TestDDLParserCreateTable::test_create_table_if_not_exists
    differential/test_ddl_parser_differential.py::TestDDLParserCreateTable::test_create_table_with_multiple_types
    differential/test_ddl_parser_differential.py::TestDDLParserCreateTable::test_create_table_with_string_type
    differential/test_ddl_parser_differential.py::TestDDLParserDropTable::test_drop_table
    differential/test_ddl_parser_differential.py::TestDDLParserDropTable::test_drop_table_if_exists_nonexistent
    differential/test_ddl_parser_differential.py::TestDDLParserInsert::test_insert_select
    differential/test_ddl_parser_differential.py::TestDDLParserInsert::test_insert_values
    differential/test_ddl_parser_differential.py::TestDDLParserTruncate::test_truncate_table
    differential/test_ddl_parser_differential.py::TestDDLParserView::test_create_temp_view
    differential/test_ddl_parser_differential.py::TestDDLParserView::test_create_view
    differential/test_ddl_parser_differential.py::TestDDLParserView::test_drop_view
    differential/test_ddl_parser_differential.py::TestDDLParserWorkflow::test_full_workflow
    differential/test_ddl_parser_differential.py::TestDDLParserWorkflow::test_type_mapping_correctness
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_full_outer_join_column_resolution
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_join_complex_condition_with_ambiguous_columns
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_join_with_select_after_ambiguous_join
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_left_join_with_ambiguous_columns
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_right_join_column_resolution
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_self_join_with_alias_dataframe_api
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_self_join_with_filter_using_alias
    differential/test_join_advanced_differential.py::TestJoinAdvancedDifferential::test_three_way_join_with_ambiguous_columns
    differential/test_joins_differential.py::TestJoinConditions::test_join_composite_key
    differential/test_joins_differential.py::TestJoinConditions::test_join_inequality
    differential/test_json_functions_differential.py::TestFromJsonDifferential::test_from_json_array_field
    differential/test_json_functions_differential.py::TestFromJsonDifferential::test_from_json_dataframe_api
    differential/test_json_functions_differential.py::TestFromJsonDifferential::test_from_json_missing_keys
    differential/test_json_functions_differential.py::TestFromJsonDifferential::test_from_json_null_input
    differential/test_json_functions_differential.py::TestFromJsonDifferential::test_from_json_simple_ddl
    differential/test_json_functions_differential.py::TestJsonExtraction_Differential::test_json_tuple
    differential/test_json_functions_differential.py::TestJsonInfo_Differential::test_json_object_keys
    differential/test_lambda_differential.py::TestAggregateFunction_Differential::test_aggregate_product
    differential/test_lambda_differential.py::TestAggregateFunction_Differential::test_aggregate_sum
    differential/test_lambda_differential.py::TestAggregateFunction_Differential::test_aggregate_with_init
    differential/test_lambda_differential.py::TestCombinedOperations_Differential::test_filter_in_where
    differential/test_lambda_differential.py::TestCombinedOperations_Differential::test_transform_multiple_rows
    differential/test_lambda_differential.py::TestExistsFunction_Differential::test_exists_false
    differential/test_lambda_differential.py::TestExistsFunction_Differential::test_exists_true
    differential/test_lambda_differential.py::TestFilterFunction_Differential::test_filter_all_pass
    differential/test_lambda_differential.py::TestFilterFunction_Differential::test_filter_even_numbers
    differential/test_lambda_differential.py::TestFilterFunction_Differential::test_filter_greater_than
    differential/test_lambda_differential.py::TestFilterFunction_Differential::test_filter_none_pass
    differential/test_lambda_differential.py::TestForallFunction_Differential::test_forall_false
    differential/test_lambda_differential.py::TestForallFunction_Differential::test_forall_true
    differential/test_lambda_differential.py::TestNestedLambdas_Differential::test_nested_transform
    differential/test_lambda_differential.py::TestNestedLambdas_Differential::test_transform_then_filter
    differential/test_lambda_differential.py::TestSQLLambda_Differential::test_sql_aggregate
    differential/test_lambda_differential.py::TestSQLLambda_Differential::test_sql_aggregate_product
    differential/test_lambda_differential.py::TestSQLLambda_Differential::test_sql_transform_with_table
    differential/test_lambda_differential.py::TestTransformFunction_Differential::test_transform_add_one
    differential/test_lambda_differential.py::TestTransformFunction_Differential::test_transform_from_subquery
    differential/test_lambda_differential.py::TestTransformFunction_Differential::test_transform_multiply
    differential/test_math_bitwise_date_differential.py::TestBitwiseFunctions_Differential::test_bit_get
    differential/test_math_bitwise_date_differential.py::TestDateFunctions_Differential::test_dayname
    differential/test_math_bitwise_date_differential.py::TestDateFunctions_Differential::test_monthname
    differential/test_math_bitwise_date_differential.py::TestMathFunctions_Differential::test_positive
    differential/test_multidim_aggregations.py::TestAdvancedAggregations::test_cube_vs_rollup_difference
    differential/test_multidim_aggregations.py::TestAdvancedAggregations::test_multiple_aggregations_same_column
    differential/test_multidim_aggregations.py::TestCubeFunctions::test_cube_single_column
    differential/test_multidim_aggregations.py::TestCubeFunctions::test_cube_two_columns
    differential/test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping
    differential/test_multidim_aggregations.py::TestCubeFunctions::test_cube_with_grouping_id
    differential/test_multidim_aggregations.py::TestRollupFunctions::test_rollup_single_column
    differential/test_multidim_aggregations.py::TestRollupFunctions::test_rollup_three_columns
    differential/test_multidim_aggregations.py::TestRollupFunctions::test_rollup_two_columns
    differential/test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_filter
    differential/test_multidim_aggregations.py::TestRollupFunctions::test_rollup_with_grouping
    differential/test_new_aggregates_differential.py::TestNewAggregates_Differential::test_max_by
    differential/test_new_aggregates_differential.py::TestNewAggregates_Differential::test_min_by
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_all_rows
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_basic
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_more_than_rows
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_todf_combined
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_with_filter
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_with_limit
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_offset_zero
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_todf_chained_operations
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_todf_with_multiple_columns
    differential/test_offset_operations_differential.py::TestOffsetOperationsDifferential::test_todf_with_range
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_empty_range
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_large_range
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_join_via_sql
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_negative_values
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_schema
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_union
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_aggregation
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_filter
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_large_step
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_limit
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_negative_start
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_orderby
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_select
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_start_end
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_with_step
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_range_zero_start_explicit
    differential/test_range_operations_differential.py::TestRangeOperationsDifferential::test_simple_range
    differential/test_sql_corpus_differential.py::test_case[agg-024]
    differential/test_sql_corpus_differential.py::test_case[jn-017]
    differential/test_sql_corpus_differential.py::test_case[jn-018]
    differential/test_sql_corpus_differential.py::test_case[sq-023]
    differential/test_sql_corpus_differential.py::test_case[tbl-013]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q001]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q002]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q003]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q004]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q005]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q007]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q008]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q009]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q011]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q012]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q013]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q014a]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q014b]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q015]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q016]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q017]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q018]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q020]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q023a]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q023b]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q025]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q026]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q027]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q028]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q029]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q030]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q031]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q034]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q035]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q038]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q039a]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q039b]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q042]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q044]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q045]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q046]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q047]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q048]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q050]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q052]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q053]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q054]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q057]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q058]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q059]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q061]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q062]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q063]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q064]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q065]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q066]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q067]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q068]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q070]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q071]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q073]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q077]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q078]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q079]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q080]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q081]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q083]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q084]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q085]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q086]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q087]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q088]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q089]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q091]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q092]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q093]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q094]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q095]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q096]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q098]
    differential/test_sql_corpus_differential.py::test_case[tpcds-q099]
    differential/test_sql_corpus_differential.py::test_case[tpch-q01]
    differential/test_sql_corpus_differential.py::test_case[tpch-q07]
    differential/test_sql_corpus_differential.py::test_case[tpch-q08]
    differential/test_sql_corpus_differential.py::test_case[tpch-q11]
    differential/test_sql_corpus_differential.py::test_case[tpch-q13]
    differential/test_sql_corpus_differential.py::test_case[tpch-q18]
    differential/test_sql_corpus_differential.py::test_case[tpch-q21]
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_combined_sql_and_dataframe_api
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_filter_with_sql_expression_string
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_selectExpr_with_sql_expressions
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_selectExpr_with_sql_functions
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_case_expression
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_with_aggregation
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_with_join
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_with_subquery
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_with_temp_view
    differential/test_sql_expressions_differential.py::TestSQLExpressionsDifferential::test_sql_with_where
    differential/test_statistics_differential.py::TestStatApproxQuantile_Differential::test_approx_quantile_median
    differential/test_statistics_differential.py::TestStatApproxQuantile_Differential::test_approx_quantile_multiple
    differential/test_statistics_differential.py::TestStatCorr_Differential::test_corr_perfect_correlation
    differential/test_statistics_differential.py::TestStatCorr_Differential::test_corr_range
    differential/test_statistics_differential.py::TestStatCov_Differential::test_cov_positive_correlation
    differential/test_statistics_differential.py::TestStatCov_Differential::test_cov_symmetric
    differential/test_statistics_differential.py::TestStatisticsWithNulls_Differential::test_cov_with_nulls
    differential/test_string_collection_differential.py::TestStringFunctions_Differential::test_btrim
    differential/test_string_collection_differential.py::TestStringFunctions_Differential::test_encode
    differential/test_string_collection_differential.py::TestStringFunctions_Differential::test_substring_index
    differential/test_string_collection_differential.py::TestStringFunctions_Differential::test_to_char
    differential/test_temp_views.py::TestTPCHWithTempViews::test_tpch_q1_with_temp_view
    differential/test_to_schema_differential.py::TestToSchemaBasic_Differential::test_column_projection
    differential/test_to_schema_differential.py::TestToSchemaBasic_Differential::test_column_reorder
    differential/test_to_schema_differential.py::TestToSchemaBasic_Differential::test_empty_dataframe
    differential/test_to_schema_differential.py::TestToSchemaBasic_Differential::test_identical_schema
    differential/test_to_schema_differential.py::TestToSchemaChaining_Differential::test_filter_then_to_schema
    differential/test_to_schema_differential.py::TestToSchemaChaining_Differential::test_to_schema_then_select
    differential/test_to_schema_differential.py::TestToSchemaMultipleRows_Differential::test_multiple_rows_reorder
    differential/test_to_schema_differential.py::TestToSchemaMultipleRows_Differential::test_multiple_rows_with_nulls
    differential/test_to_schema_differential.py::TestToSchemaTypeCasting_Differential::test_bigint_to_int
    differential/test_to_schema_differential.py::TestToSchemaTypeCasting_Differential::test_double_to_float
    differential/test_to_schema_differential.py::TestToSchemaTypeCasting_Differential::test_float_to_double
    differential/test_to_schema_differential.py::TestToSchemaTypeCasting_Differential::test_int_to_bigint
    differential/test_type_casting_differential.py::TestDateTimeCasts::test_date_to_string
    differential/test_type_literals_differential.py::TestArrayLiterals_Differential::test_array_pyspark_literal
    differential/test_type_literals_differential.py::TestArrayLiterals_Differential::test_empty_array
    differential/test_type_literals_differential.py::TestIntervalLiterals_Differential::test_interval_date_arithmetic
    differential/test_type_literals_differential.py::TestIntervalLiterals_Differential::test_year_month_interval_in_arithmetic
    differential/test_type_literals_differential.py::TestIntervalLiterals_Differential::test_year_month_interval_months_arithmetic
    differential/test_type_literals_differential.py::TestMapLiterals_Differential::test_map_from_arrays
    differential/test_type_literals_differential.py::TestMapLiterals_Differential::test_map_pyspark_create_map
    differential/test_type_literals_differential.py::TestStructLiterals_Differential::test_struct_field_access
    differential/test_type_literals_differential.py::TestStructLiterals_Differential::test_struct_pyspark_literal

## Pass 1 — 2026-07-09 — spark.range() (DataFrame front-end)

- **Hypothesis:** all 17 test_range_operations failures share one root cause —
  `RelType::Range` unhandled in V2RelationConverter (boundary error).
- **Fix (root-cause general):** new `convert_range` arm maps `proto::Range`
  onto the EXISTING `CommonOp::TableFunction { name: "range" }` path the SQL
  front-end already uses (no new AST node; analyzer + emission untouched).
  `num_partitions` ignored per the Repartition/Hint cosmetic carve-out.
- **Review:** rust-reviewer APPROVE, 0 Critical/High (cross-front-end arg
  parity verified: both front-ends normalize to `range(start,end,step)`).
- **Gate:** 1085→1116 passed (Δ +31, zero regressions). Collateral greens:
  offset_operations +10, type_literals +3, column_operations +2 (range-based
  fixtures). test_range_join_via_sql stays red on an UNRELATED pre-existing
  self-join `id` ambiguity — future pass candidate, not documented-unsolvable.
- **Reflect:** subagents clean this pass; no attributable skill gaps. Lesson
  reinforced: check for an existing CommonOp before assuming a new node
  (TableFunction already carried `range` for SQL).

## Pass 2 — 2026-07-09 — SQL `CREATE [OR REPLACE] TEMP VIEW ... AS SELECT`

- **Hypothesis:** the `sql::create_view` boundary error is one root cause
  spanning lambda (21), complex_types (14), sql_expressions, ddl files —
  DDL fixtures issued via `spark.sql(...)`.
- **Architecture (rust-architect):** statement-level `SqlStatement { Query,
  Ddl }` enum BESIDE CommonAst (DDL is not a relation — no new CommonOp);
  parser_v2 `parse_statement` lowers `Statement::CreateView{temporary}`;
  service.rs SqlCommand arm branches and reuses the EXISTING
  createOrReplaceTempView machinery. INV10 intact (side effects only in
  connect-server). Staged plan: stage 2 (CREATE TABLE/DROP/INSERT) = pass 3.
- **Review:** APPROVE, 0 Critical/High. Medium fixed same pass: Spark
  rejects `IF NOT EXISTS` on temp views at parse time (verified empirically
  against Spark 4.1.1) — field made unrepresentable in the enum, two
  Spark-emulated parse errors added with Spark's exact wording.
- **Gate:** 1116→1147 (Δ +31, zero regressions): lambda +16, complex_types
  +14, ddl_parser +1. Remaining lambda 5 = HOF aggregate() nullable
  mismatch (separate root cause); sql_expressions 10 = `sql::drop` (pass 3).
- **Reflect (orchestrator, not subagent):** the review Medium originated in
  MY coder brief — I embellished the architect's skeleton with a speculative
  `if_not_exists` field without checking Spark's rule. Lesson: pass architect
  skeletons through verbatim; any added surface needs a Spark-source check
  first. Coder did well: verified Spark's actual rule empirically and made
  the invalid state unrepresentable.

## Pass 3 — 2026-07-09 — SQL DDL stage 2 (CREATE TABLE / DROP / INSERT / TRUNCATE / persistent CREATE VIEW)

- **Hypothesis:** remaining ddl_* + sql_expressions reds share the
  sql::create_table / sql::drop / sql::insert boundary root cause.
- **Fix:** DdlStatement grew CreateTable/DropTable/DropView/InsertValues/
  InsertSelect/TruncateTable/CreateView; `render_ddl` builds DuckDB SQL from
  typed parts (quote_ident + render_expr + render_data_type — zero input-SQL
  string manipulation); `SessionCommand::ExecuteDdl` + `SchemaCacheEffect`
  applies cache effects atomically on the session thread; `map_ddl_error`
  re-clothes DuckDB catalog errors as TABLE_OR_VIEW_ALREADY_EXISTS /
  TABLE_OR_VIEW_NOT_FOUND scoped by statement kind. Spark-parity verified
  empirically: CREATE TEMPORARY TABLE rejected (parse), INSERT column-list
  bails loudly (unexercised by corpus).
- **Review:** APPROVE, 0 Critical/High. Medium fixed in-pass:
  `CacheIfAbsent` variant so CREATE TABLE IF NOT EXISTS on an existing table
  cannot overwrite the cached live schema with the redeclaration.
- **Gate:** 1147→1189 (Δ +42, zero regressions): ddl_operations +13,
  ddl_parser +12, sql_expressions +8, dataframe_basic_operations +7 (DDL
  fixtures), ddl_corrected +2. Deferred: ALTER TABLE ADD COLUMN (stage 3),
  from_json, RelType::Sql-in-root, join alias scoping (separate causes).
- **Reflect:** coder reporting slip — quoted the all-ignored differential
  result block ("0 passed") as the connect-server gate result; orchestrator
  re-verified (102 passed). Noted in subagent-improvement-notes. Orchestrator
  lesson: my `-k "ddl or sql_expressions"` focused-verify filter matched test
  NAMES, not file paths — narrower than intended; use file paths as pytest
  args for file-scoped verification instead of -k.

## Pass 4 — 2026-07-09 — df.to(schema) (RelType::ToSchema)

- **Hypothesis:** all 12 test_to_schema failures = the single ToSchema
  boundary error.
- **Architecture (rust-architect, evidence-driven):** read all 12 tests +
  Spark's Project.matchSchema (basicLogicalOperators.scala): no error-path or
  nullability-direction cases exercised, and Spark's output nullability is
  source-derived for accepted inputs — so Option A (converter desugars to
  `Project [Alias(Cast(UnresolvedColumn(f.name)) AS f.name)]` in target
  order, no new CommonOp) is faithful for the exercised surface. Option B
  (analyzer-desugared CommonOp::ToSchema, crosstab precedent) documented as
  the upgrade path if error-class/null-fill cases ever land; deviations
  recorded in the method doc comment.
- **Review:** APPROVE, 0 Critical/High. (Reviewer conjectured Spark widens
  non-nullable→nullable-target; architect's source reading says
  source-derived — the 12 green differential cases arbitrate the exercised
  surface; unexercised directions are documented deviations either way.)
- **Gate:** 1189→1201 (Δ +12, zero regressions) — exactly the 12 predicted.
- **Reflect:** clean pass; the architect's "read the tests first, pick the
  minimal faithful shape" pattern avoided a new AST node.

## Pass 5 — 2026-07-09 — catalog operations (RelType::Catalog, 8 ops)

- **Hypothesis:** all 13 test_catalog_operations reds share the
  RelType::Catalog boundary root cause.
- **Architecture (rust-architect):** connect-server pre-pass
  (`catalog_ops::resolve_catalog_relation`, resolve_implicit_pivots
  precedent) rewrites root Catalog relations to CommonOp::Values so the
  unchanged finalize/streaming path (schema frame incl.) serves ExecutePlan
  AND AnalyzePlan. Constants (currentCatalog/currentDatabase/databaseExists),
  a new pure 249-entry `function_catalog` roster in core (INV10-safe) for
  functionExists/getFunction/listFunctions, session-backed
  tableExists/dropTempView (duckdb_tables()/duckdb_views() probes +
  execute_ddl with SchemaCacheEffect::Evict). Other 18 cat_type variants =
  named Status::unimplemented; exhaustiveness compile-enforced.
- **Spark probe:** getFunction metadata mirrored where cheap (name,
  isTemporary, currentCatalog "spark_catalog", currentDatabase "default");
  description/className = typed NULLs (untested by corpus, 249-entry
  boilerplate refused).
- **Review:** APPROVE, 0 Critical/High. Injection surface verified safe
  (quote-doubling escaping; hostile names stay inside literals). Lows:
  escaping helpers duplicated from core emission (re-export later);
  roster contains special-syntax pseudo-functions (cast/if/not/extract).
- **Gate:** 1201→1216 (Δ +15, zero regressions): catalog +13,
  array_functions +2 (collateral). Coder recovered cleanly from a mid-task
  API disconnect (resumed via transcript).
- **Reflect:** the runner treats a bare `.py` first arg as "run ALL with it
  as a pytest arg" (silently ran the full suite when given a file path) —
  runner ergonomics fix queued as tech debt. My pass-4 lesson ("pass file
  paths instead of -k") was WRONG for this runner — corrected in lessons.md.

## Pass 6 — 2026-07-09 — ROLLUP/CUBE grouping-column nullability

- **Hypothesis:** the 11 multidim AssertionErrors share one schema root cause.
- **Confirmed:** Spark's Expand node marks EVERY grouping column of a
  ROLLUP/CUBE/GROUPING SETS aggregate nullable (super-aggregate rows hold
  NULL); τ preserved source nullability. grouping()/grouping_id() types were
  ALREADY correct (Byte/Long) — the nullable mismatch masked everything.
- **Fix:** 24 lines in analyzer.rs Aggregate arm — force nullable=true on
  output fields matching grouping-column names under Rollup/Cube/
  GroupingSets; plain GroupBy untouched (flip_all_nullable precedent).
- **Review:** APPROVE, 0 Critical/High. Known Medium (follow-up, not
  blocking): matching is name-based, so an aggregate ALIASED to a grouping
  column's name would be wrongly forced nullable (Spark matches by ExprId);
  bounded to alias collisions, errs toward over-nullable, no corpus witness.
- **Gate:** 1216→1230 (Δ +14, zero regressions): multidim +11 (21/21 file
  green), SQL corpus +3 collateral (311/396).
- **Reflect:** clean; single-root-cause hypothesis validated by an 11-case
  cluster falling to one rule. The "nullable mismatch masks deeper diffs"
  pattern recurs (pass 18, this) — diagnose schema mismatches BEFORE value
  mismatches.

## Pass 7 — 2026-07-09 — df.stat cov/corr/approxQuantile

- **Root cause:** RelType::Cov/Corr/ApproxQuantile converter gaps (boundary).
- **Fix:** converter desugar to global CommonOp::Aggregate (no new node, no
  core changes). Spark-parity findings: cov applies na.fill(0) before
  covar_samp (verified from Spark bytecode + Java reference) → COALESCE(col,
  0); corr does NOT fill; approxQuantile emitted as nested ArrayLiteral of
  percentile_approx → DuckDB quantile_disc (GK-on-small-data returns actual
  elements; t-digest approx_quantile would diverge); relativeError ignored
  (exact computation subsumes it, documented).
- **Review:** APPROVE, 0 Critical/High; nested-aggregate-in-array emission
  risk discharged by the 16/16 statistics run.
- **Gate:** 1230→1237 (Δ +7, zero regressions); statistics file 16/16.

## Pass 8 — 2026-07-09 — aggregate() HOF nullability

- **Root cause (source-verified from spark-catalyst 4.1.1 bytecode):**
  ArrayAggregate.nullable = argument.nullable || finish.nullable, and
  bindInternal hardcodes the accumulator LambdaVariable nullable=true — so
  the rule is effectively ALWAYS true. τ's fallback (any-arg-nullable) gave
  false for non-null arrays + literal seeds.
- **Fix:** `aggregate | reduce | list_reduce` added to the always-nullable
  arm of function_call_nullable (expression.rs) — covers both front-ends via
  the shared Expression type. Two unit tests lock the rule.
- **Review:** SKIPPED deliberately (stated per verification policy): 3-name
  match-arm addition with the Spark rule cited from bytecode, unit-locked,
  full gate arbitrates.
- **Gate:** 1237→1242 (Δ +5, zero regressions); lambda file 27/27.

## Pass 9 — 2026-07-09 — plan_id-scoped join column disambiguation

- **Diagnosis (rust-diagnostician):** all 9 join-ambiguity reds (8
  join_advanced + range_join_via_sql) share ONE cause — Spark Connect
  disambiguates DataFrame refs solely by plan_id; τ honored plan_id only in
  join CONDITIONS (qualify_plan_id_refs), never in parent-operator
  expressions, so resolve_column's ambiguity scan threw AmbiguousColumn.
- **Fix (Option A):** QualifierScopes carries plan_id→(range, side-qualifier)
  bindings registered at Join arms (outermost-first, stops at
  schema-reshaping ops — same depth rule as name qualifiers);
  resolve_column consults plan_id before the ambiguity scan.
- **In-pass regression + fix:** first version stamped __td_jl/__td_jr
  UNCONDITIONALLY → 5 TPC-DS DataFrame cases (q015/q032/q037/q048/q092)
  regressed: those aliases are only in scope under alias-transparent
  rendering; Filter-over-Join wraps as __td_filter → binder error. Fixed by
  stamping the qualifier ONLY when the name is genuinely ambiguous across
  the join schema. All 5 back green; 27/27 held.
- **Review:** APPROVE, 0 Critical/High. Known Medium (documented): nested
  non-self joins with a column name duplicated WITHIN one outer side can
  mis-resolve (inner aliases not in parent scope); self-join shapes immune;
  no corpus witness.
- **Gate:** 1242→1253 (Δ +11, zero regressions vs oracle): join_advanced +8,
  joins +2, range +1.
- **Reflect:** coder misread its own corpus check — called 364/20 "all TPC,
  expected" when baseline was 369/15; ADR-022's "red TPC is expected
  fitness signal" does NOT mean TPC regressions are exempt from the oracle.
  Orchestrator caught it by comparing counts. Noted in
  subagent-improvement-notes.

## Pass 10 — 2026-07-11 — grouping-fold false-positive AMBIGUOUS_REFERENCE (Family A)

- **Baseline (fresh, HEAD c913c33 after 37 commits since pass 9):** 1324 passed
  / 118 failed / 5 skipped / 1447 total (DF 393/402, SQL 346/404, other 585/641).
- **Cluster pick:** the AMBIGUOUS_REFERENCE family (highest-cascade coherent
  τ-error signature). `rust-diagnostician` CORRECTED the noisy stdout-banner
  attribution and split it into two independent root causes: **Family A** (6
  cases, τ analyzer FALSE-POSITIVE `[AMBIGUOUS_REFERENCE]`: q046, q053, q063,
  q068, q079, q098) and **Family B** (3 cases, real DuckDB self-join ambiguity
  from a bare ORDER BY: q039a/b, q064). One root cause per pass → Family A here,
  Family B deferred to Pass 11.
- **Hypothesis (Family A):** a SQL aggregate whose `GROUP BY` contains a key not
  in the SELECT list makes `grouping_already_folded` (analyzer.rs) return false
  (it was ALL-or-nothing), so the Aggregate arm prepends the WHOLE grouping list,
  duplicating already-selected keys → the enclosing ORDER BY/`SELECT *` trips
  `resolve_column`'s ambiguity scan → false-positive `[AMBIGUOUS_REFERENCE]`.
  Spark accepts all six → ADR-022 Thunderduck-boundary correctness bug.
- **Fix (root-cause, single line + doc):** flip `grouping_already_folded` from
  `grouping.iter().all(..)` to `.any(..)`. The predicate is the shared source of
  truth for both the analyzer's resolved schema AND emission's SELECT slots, so
  they flip together (no column-count desync); the GROUP BY body is rendered
  from the full grouping list independent of the fold verdict. `any` correctly
  discriminates the SQL path (≥1 selected grouping key present → folded) from the
  DataFrame path (aggregates = agg exprs only, no grouping key → still prepend).
  3 unit tests pin both branches + the single-key case.
- **Review (`rust-reviewer`):** APPROVE-WITH-NITS, 0 Critical/High. Two Mediums,
  both witness-free and gated by the differential: (1) the DataFrame edge
  `.groupBy(k1,k2).agg(k1,agg)` folds where Spark prepends — inside an
  already-approximate heuristic, robust fix = a front-end `folded` flag on
  `CommonOp::Aggregate` (ast.rs:107 TODO), deferred; (2) a narrower
  reprojection-asymmetry desync newly reachable under `any` only when an
  aggregate sits directly over a duplicate-name input — unconstructed, would be
  a hard error caught by the gate. Doc comment updated to disclose both honestly.
- **Gate:** 1324→1326 (Δ +2, **zero regressions** vs the c913c33 oracle — full
  per-case set diff, no green→red). τ-emitted `[AMBIGUOUS_REFERENCE]` token
  12→0 (the defect is eliminated). Newly green: **tpcds-q046, tpcds-q068** (SQL
  corpus). The other four Family A cases (q053, q063, q079, q098) stopped
  emitting the false positive and PROGRESSED to distinct downstream blockers
  (q053/q063/q098 are window-over-agg; now data/other errors) — separate root
  causes for future passes, not this one. `cargo test -p thunderduck-core --lib`
  1039 green.
- **Reflect:** (1) stdout-banner correlation for case→signature attribution is
  unreliable (captured-stdout ordering) — the diagnostician re-derived membership
  from the summary block and corrected a 6-case mis-attribution; trust the
  diagnostician's confirmed membership over my log forensics. (2) A cluster that
  looks like one signature (AMBIGUOUS_REFERENCE) can be two root causes at
  different layers (analyzer fabrication vs emission under-qualification) — split
  by mechanism, one per pass. (3) "6 cases share a signature" ≠ "6 cases flip";
  fixing the root cause flipped 2 and advanced 4 to their next blocker, which is
  honest forward progress. Family B is Pass 11.

## Pass 11 — 2026-07-11 — bare ORDER BY key over a self-join → DuckDB ambiguous (Family B)

- **Baseline (post-Pass-10, commit 2b19b57):** 1326 passed / 116 failed / 5
  skipped / 1447 total.
- **Cluster:** Family B from the Pass-10 diagnosis (`.agent-output/diagnostic-ambiguous-ref.md`
  §1B/§3B): tpcds-q039a, q039b, q064 — self-joins of a CTE where the SELECT
  projects the SAME output name from both sides (two `w_warehouse_sk` / two
  `cnt`). The analyzer's tier-(f) source_quals arm drops the qualifier on the
  projected-through ORDER BY key (→ `qualifier:None, ordinal:Some(k)`), then
  `build_sort` merged it BARE into the duplicate-name SELECT list → DuckDB
  `Binder Error: Ambiguous reference`. Spark runs these → ADR-022
  Thunderduck-boundary correctness bug; all three pass-0 reds.
- **Fix (root-cause, one conjunct + tests):** in `build_sort`'s occupied-SELECT
  merge predicate `keys_bind` (emission.rs ~1501), require additionally that
  `bare_dup_ordinal(c, &input.resolved_schema).is_none()`. A bare key whose
  output name is duplicated in the input schema now fails `keys_bind` and falls
  through to the PRE-EXISTING wrap+uniquify branch (`output_uniquified` →
  `reproject_qualifiers`'s bare-ordinal arm rewrites the key to the uniquified
  name, `wrap_reprojected` re-exposes the child as `__td_sub(name, name_1)`), so
  the key binds unambiguously. Pure reuse — no analyzer change, no new operator.
  The guard is strictly stricter, so it can only remove ambiguous merges (a green
  case that merged a dup-name-ordinal key would already be a hard DuckDB error).
- **Review (`rust-reviewer`):** APPROVE, 0 Critical/High. Verified the
  fall-through emits unambiguous SQL (incl. the k==0 sub-case), mixed/qualified
  key sets, and LIMIT/OFFSET+DISTINCT placement unchanged; confirmed the
  discrimination test fails on revert. Informational (pre-existing, not
  worsened): a bare dup-name key with `ordinal==None` would still merge —
  unreachable in practice (analyzer stamps the ordinal when it drops the
  qualifier). PROCESS NOTE: the reviewer reverted emission.rs via Bash+Python to
  test discrimination WHILE the gate ran on the same tree; validated harmless
  here because cargo compiles a coherent snapshot and the gate flipped exactly
  the 3 targets (a reverted build would be +0), but future passes must not run a
  mutating reviewer in parallel with the gate.
- **Gate:** 1326→1329 (Δ +3, **zero regressions** vs the 2b19b57 oracle — full
  per-case set diff). DuckDB "Ambiguous reference to column name" occurrences
  6→0. Newly green: **tpcds-q039a, tpcds-q039b, tpcds-q064** (SQL corpus).
  `cargo test -p thunderduck-core --lib` 1037 green.
- **Reflect:** the Pass-10 diagnostic's Family-B split paid off directly — a
  clean, pre-scoped one-conjunct fix landed all 3 predicted cases with zero
  surprises. The AMBIGUOUS_REFERENCE cluster (both families) is now fully
  resolved: 9 cases diagnosed, 5 turned green (q046/q068 Pass 10; q039a/b/q064
  Pass 11), 4 (q053/q063/q079/q098) advanced to distinct downstream blockers for
  future passes. Lesson banked: never run a reviewer that can mutate the tree in
  parallel with a differential gate on the same worktree — serialize, or isolate.

## Pass 12 — 2026-07-11 — spark_decimal_div DOUBLE-operand crash eliminated (decimal-div type-safety)

- **Baseline (post-Pass-11, commit 3c999fd):** 1329 passed / 113 failed / 5
  skipped / 1447 total.
- **Cluster:** the `spark_decimal_div requires DECIMAL … got DOUBLE and DOUBLE`
  crash (5 SQL corpus cases: tpcds-q047, q053, q057, q063, q089; q053/q063 are
  two Family-A cases that advanced in Pass 10). Diagnosis:
  `.agent-output/diagnostic-decimal-div.md`.
- **Root cause:** `render_binary` (emission.rs ~5662) routes `Div` →
  `spark_decimal_div` when BOTH operands' ANALYZER types are Decimal, but τ
  emits DuckDB-native `avg` (DOUBLE even over DECIMAL) — a type-lie (the
  reconciling `spark_aggregate_return_cast` is dead/unwired; the ADR-020
  `spark_avg`/`spark_sum` routing was half-reverted). The extension then rejected
  the DOUBLE operands. ADR-022 Thunderduck-boundary correctness bug.
- **Fix (minimal, local):** at the `spark_decimal_div` site, CAST each operand to
  its analyzer-declared `DECIMAL(p,s)`. No-op re-cast for a genuine decimal;
  coerces a native-double operand — so the extension always gets real decimals
  and Spark DECIMAL/DECIMAL division semantics are preserved. 2 unit tests.
- **Review (`rust-reviewer`, no-tree-mutation brief):** APPROVE-WITH-NITS, 0
  Critical/High. Verified the CAST-wrapped render, that the one green case taking
  this path (`type-005`) is a no-op re-cast (unaffected), and that the new
  overflow surface applies ONLY to the previously-crashing double path (cannot
  regress green). Nits (Low, deferred): mixed decimal/int not asserted; overflow
  parity (Spark ansi=false → NULL vs DuckDB throw) — relevant only to the Option-B
  avg pass.
- **Gate:** 1329→1329 (**Δ +0 green, zero regressions**). The
  `spark_decimal_div … DOUBLE` error occurrences dropped **10→0** — the crash
  CLASS is eliminated. But all 5 target cases ADVANCED from the crash to a
  downstream `AssertionError`: the projected windowed-`avg` column emits as
  DuckDB DOUBLE while Spark yields DECIMAL, so the value/type now mismatches.
  This is the avg type-lie itself, a SEPARATE (bigger) root cause. `cargo test
  -p thunderduck-core --lib` 1039 green.
- **Reflect:** honest +0-green pass — a correct, zero-regression crash-class
  elimination and a necessary robustness layer (τ must never feed DOUBLE to
  `spark_decimal_div`), but NOT sufficient to green these 5. The empirical result
  pinpoints the next target: **decimal-`avg` type coherence** (make emission
  produce the Decimal the analyzer declares). Two paths — Path A: rewrite
  `avg(x)`→`CAST(spark_decimal_div(sum(x) OVER w, count(x) OVER w) AS DECIMAL(...))`
  with a `count=0→NULL` guard, reusing existing primitives (no new extension);
  Path B: implement/wire the ADR-020 `spark_avg`/`spark_sum` extension aggregates.
  Next pass starts with Path A (reuse-first); escalate to Path B only if Path A
  can't match Spark precision. Expected to green these 5 PLUS the decimal-scale
  cluster (q058/q067/q083).

## Pass 13 — 2026-07-11 — decimal-avg type coherence: re-wire shipped spark_avg (B-lite)

- **Baseline (post-Pass-12, commit 6d67c5f):** 1329 passed / 113 failed / 5
  skipped / 1447 total.
- **Cluster / root cause:** the decimal-`avg` type-lie behind Pass 12's residual
  AssertionErrors. Analyzer types `avg(Decimal(p,s))` as Spark's DECIMAL(p+4,s+4)
  (type_inference.rs AvgLike), but emission passed `avg` through as DuckDB-native
  `avg`, which returns DOUBLE over DECIMAL — so the projected/derived avg value
  and type diverged from Spark. Full design: `.agent-output/013-design-avg-coherence.md`.
- **KEY DISCOVERY (durable):** `spark_avg`/`spark_sum` with DECIMAL overloads
  ALREADY ship in the ext6 binary (`extensions/ext6/thdck_spark_funcs-v1.5.4-*`,
  verified via `strings`; contract in `docs/context/dependencies.md:30-32`), and
  `spark_try_avg`/`spark_try_sum` from the same family are wired today
  (emission.rs:5451-5452). The τ-side `avg`/`sum` routing was deleted as
  COLLATERAL in the v2-restart commit `d846663`, not a deliberate revert (it was
  differential-validated pre-restart, commit 610618b). So this is restoring lost
  wiring, not new extension work — re-honoring ADR-020.
- **Decision (user-approved):** Path B-lite (re-wire shipped `spark_avg`) over
  Path A (compose sum/count/spark_decimal_div). Simpler, native NULL-on-empty,
  no rounding seam.
- **Probe P1 (kept test, extension_loader.rs):** loads real ext6, confirms
  `spark_avg(DECIMAL(9,2))` → native `DECIMAL(13,6)` (== Spark's AvgLike type)
  both grouped and windowed. So the emission-side outer CAST is a no-op on the
  canonical shape (no rounding seam). Probe asserts the EXACT (13,6) to guard
  that invariant against future extension rebuilds.
- **Fix:** decimal-only `avg`/`mean` (1 arg, `DataType::Decimal`) →
  `CAST(spark_avg({DISTINCT }x) [OVER (...)] AS DECIMAL(pa,sa))`, `(pa,sa)` from
  the analyzer's `aggregate_return_type` (so wire schema == emitted type). Hooks:
  extracted `render_over_clause`, new `render_decimal_avg` + `is_decimal_avg`,
  guard arm in `render_aggregate`, interception in `render_window` (CAST wraps the
  whole `spark_avg(...) OVER (...)`). `sum`/`try_sum`/`try_avg`/integer+float
  `avg` untouched. INV6 `extension_targets()` is an unlanded `todo!()` stub
  (existing extension arms aren't registered either) — correctly left alone.
- **Review (`rust-reviewer`, no-tree-mutation brief):** APPROVE-WITH-NITS, 0
  Critical/High. Verified all 7 focus points (decimal-only predicate; windowed
  CAST-outside-OVER; type-source coherence analyzer↔emission; DISTINCT; canary
  safety; INV6; tests). 2 Low nits — BOTH APPLIED: probe tightened to exact
  `DECIMAL(13,6)`; added `avg_of_integer_stays_native` negative test.
- **Gate:** 1329→1348 (**Δ +19, zero regressions** — including zero among the 16
  currently-green cases whose emission changed; canaries tpch-q17/q22,
  tpcds-q024a/b, q032, q092 all held). SQL corpus 351→364 (+13), DF 393→398 (+5).
  Newly green (19): tpcds-q047/q053/q057/q063/q089 (the target cluster), agg-024
  (witness), tpch-q01 (SQL+DF+temp-view), and the co-failure cases
  tpcds-q007/q009/q013/q018/q026/q027/q028 (SQL and/or DF) that were gated on
  decimal-avg. `cargo test -p thunderduck-core --lib` 1051 green.
- **Reflect:** biggest pass of the session — exact-decimal emission matched Spark
  better than the old double-truncation, and the type-lie was gating far more
  than the design's conservative +7-8 estimate (many "co-failure" TPC cases were
  primarily blocked by it). Deferred, distinct root causes for future passes:
  (1) `sum(Decimal)` → `spark_sum` for strict ADR-020 fidelity (no red cases
  attributable; left native); (2) the analyzer declared-schema typing cluster
  (q058/q067/q083/q093/tpch-q11/q061 — τ's AnalyzePlan decimal types differ from
  Spark's over set-op/rollup-derived inputs; emission changes cannot fix these).

## Pass 14 — 2026-07-11 — dot-in-bracket-chain field access (parser_v2 boundary expansion)

- **Baseline (post-Pass-13, commit f8de854):** 1348 passed / 94 failed / 5 skipped.
- **Cluster / root cause:** `parser_v2/v2_lowering.rs` `Expr::CompoundFieldAccess`
  arm bailed on `AccessExpr::Dot(_)` (`sql::field_access::dot` boundary). 5 cases:
  4× `from_json(...).field` (test_json_functions FromJsonDifferential) + 1×
  `named_struct('x',100,...).x` (test_type_literals StructLiterals). All are a
  plain single-identifier dot-access on a function-call root. ADR-022
  Thunderduck-boundary (Spark supports it); currently red.
- **Fix (boundary expansion, one arm):** lower `AccessExpr::Dot(Expr::Identifier(id))`
  to a string-key `ExtractValue` via the existing `str_lit(id.value)` helper —
  `.field` ≡ `['field']`, byte-identical to the bracket string-key path, so it
  inherits the already-green analyzer (`extract_value_data_type` dispatches on the
  child type → Struct field-by-name) + emission (`extract_struct_field` →
  `(child).field`) pipeline. Non-identifier `Dot` (`.true`, `.5`,
  `CompoundIdentifier`) still bails (honest boundary). sqlparser flattens chained
  dots (`a['k'].b.c`) into successive `Dot(Identifier)` elements, so multi-segment
  chains fold for free. 4 unit tests (3 positive + 1 negative).
- **Review (`rust-reviewer`, no-tree-mutation brief):** APPROVE, 0 findings.
  Traced lowering→analyzer→emission; confirmed node identity with the bracket
  path, precise match placement, chained-dot folding, and quote_ident'd field
  names. Informational (pre-existing, not introduced): array-of-struct field
  projection via string key mis-emits — same latent gap as the `['field']`
  spelling, outside these 5 cases.
- **Gate:** 1348→1353 (**Δ +5, zero regressions**). `field_access::dot`
  boundary occurrences 10→0. Newly green: the 5 target cases exactly.
  `cargo test -p thunderduck-core --lib` 1050 green.
- **Reflect:** clean boundary-expansion pass — pinpointed site + mirror-an-existing-
  path fix + structurally-can't-regress (erroring→supported). The compressed
  investigate-then-implement coder brief (skipping a separate diagnostician for a
  pinpointed small gap) worked well; one API-error retry cost nothing (no partial
  edits). Note for future: sqlparser 0.61 `CompoundFieldAccess` flattening
  behavior is a useful parser fact.

## Pass 15 — 2026-07-11 — Spark DecimalPrecision for decimal ⊗ integral arithmetic

- **Baseline (post-Pass-14, commit 6978f5c):** 1353 passed / 89 failed / 5 skipped.
- **Cluster / root cause (ONE):** `binary_data_type` (expression.rs:876) applied
  Spark's decimal arithmetic formulas ONLY when BOTH operands were `Decimal`.
  When exactly one was Decimal and the other integral (column/expr or integer
  literal), it fell through to `promote_numeric`→`unify_decimal` (Spark UNION
  widening), not Spark's `DecimalPrecision` (cast integral→decimal, then apply
  the arithmetic formula) → wrong declared precision/scale → differential SCHEMA
  mismatch (values were correct). Diagnosis: `.agent-output/015-diagnostic-decimal-schema.md`.
- **Fix:** in `binary_data_type`, for Add/Sub/Mul/Div/Mod, when exactly one
  operand is Decimal and the other integral, coerce the integral side —
  integer LITERAL → Spark `fromLiteral` minimal precision `(digits,0)` (exact
  digit count); else `decimal_form` (Int(10,0)/Long(20,0)/…) — then apply the
  same `decimal_{add,mul,div,mod}_type`. Float/Double excluded (stay
  promote_numeric → Double). Both-Decimal & int⊗int paths byte-identical.
  `decimal_form` bumped to `pub(crate)`. 7 unit tests.
- **Review (`rust-reviewer`, no-tree-mutation brief):** APPROVE-WITH-NITS.
  Digit-count/accessor/symmetry/guards verified sound. Caught ONE genuine
  latent regression (Medium) + 2 Low nits:
  - **Medium (FIXED post-review):** `decimal // integral` (Spark `div`/
    IntegralDivide) newly entered the decimal block → `_ => promote_numeric` →
    Decimal, but Spark's IntegralDivide is Long; before this pass it was Long.
    Zero corpus exposure (grep confirms NO `div`/IntDiv on decimals in either
    corpus; only `type-007` uses `div`, on integrals — unaffected), so the gate
    couldn't catch it. Fixed by hoisting the `IntDiv → Long` guard ABOVE the
    decimal block (also corrects a pre-existing `decimal // decimal` defect).
    +2 regression-guard tests (`decimal_intdiv_stays_long`,
    `int_literal_div_decimal_is_symmetric`). The fix touches only a
    zero-corpus-exposure path and moves toward Spark, so the gate result below
    is unchanged by it (verified: full lib suite green + corpus grep empty).
  - **Low (DEFERRED, documented):** a BARE Byte integer literal gets `(digits,0)`
    where Spark `fromLiteral` uses `forType(3,0)` — witness-free (SQL has no bare
    TINYINT literals; casts return None here and correctly fall to `(3,0)`), so
    not worth variant-matching complexity. Recorded parity gap.
- **Gate:** 1353→1366 (**Δ +13, zero regressions**). DataFrame corpus **402/402
  — FULLY GREEN**; SQL corpus 364→373. Newly green (13): tpch-q11, tpcds-q067,
  q093, q083, q058 (the 5 diagnosed targets) + q012, q014b, q020, q023b, q098
  (shared the decimal⊗integral typing bug). tpcds-q061's TYPE is now correct but
  it stays red on a SEPARATE column-name defect (documented in the diagnosis).
  `cargo test -p thunderduck-core --lib` 1059 green.
- **Reflect:** highest-cascade analyzer fix — one missing coercion rule produced
  13 flips. Milestone: **the DataFrame corpus is now 100% green (402/402).** The
  reviewer catching a zero-corpus-exposure regression validated the "review even
  when the gate is green" discipline (the gate alone would have shipped the
  IntDiv defect). Remaining: SQL corpus 31 red + the feature-family "other"
  bucket (unresolved-type scalar functions, Binder-Error mix, RelType::Tail,
  singletons) + q061's name defect.

## Pass 16 — 2026-07-11 — scalar-function batch 1: dayname / monthname / btrim

- **Baseline (post-Pass-15, commit 341fe09):** 1366 passed / 76 failed.
- **Root cause:** 3 feature-family scalar functions failed `τ boundary:
  unresolved type` — the analyzer's `function_return_type` had no arm for them.
  From the batched survey `.agent-output/016-survey-scalar-funcs.md` (batch 1).
- **Fix:** type_inference.rs `function_return_type` — added `btrim` to the string
  `String` arm, `dayname`/`monthname` to the date-family `String` arm. emission.rs
  — one `btrim → trim` rename (Spark btrim(str[,trimStr]) = DuckDB trim(str[,chars]),
  same order); `dayname`/`monthname` are DuckDB-native and pass through the
  scalar dispatch's `_ => name` fallback once typed. 7 unit tests. No extension.
- **Review:** documented-skip (trivial cited + unit-locked boundary expansion:
  add names to a type table + one rename; structurally cannot regress a green
  case — these were erroring boundaries). Gate arbitrated.
- **Gate:** 1366→1369 (**Δ +3, zero regressions**). Newly green: test_dayname,
  test_monthname, test_btrim. `cargo test -p thunderduck-core --lib` 1066 green.
- **Reflect:** first batched-tail pass; the survey's mechanism grouping (shared
  `function_return_type` arm + shared rename locus) held exactly. Next: batch 2
  (emission renames reverse→list_reverse, size(map)→cardinality,
  array_except→list_filter lambda).

## Pass 17 — 2026-07-11 — scalar-function batch 2: reverse / size(map) / array_except

- **Baseline (post-Pass-16, commit 2d48baa):** 1369 passed / 73 failed.
- **Root cause:** 3 DuckDB `Binder Error: No function matches` boundaries (survey
  batch 2, emission-only — analyzer already types them). τ emitted `reverse(array)`
  (DuckDB reverse is VARCHAR-only), `len(MAP)` (unsupported), and `list_filter`
  with two arrays for `array_except`.
- **Fix (emission.rs, type-dependent dispatch in render_function_call):**
  - `reverse`: Array arg → `list_reverse`; String stays native `reverse`.
  - `size`/`cardinality`: Map arg → `CAST(cardinality(map) AS BIGINT)` (native
    cardinality returns UBIGINT which Arrow rejects); Array/other → `len`.
  - `array_except(a,b)` → `list_filter(a, (x,i) -> list_position(a,x)=i AND NOT
    list_contains(b,x))` wrapped in NULL-propagation CASE — order-preserving
    distinct (the survey's `list_distinct(...)` form was verified WRONG: it
    reorders by hash; corrected against live PySpark).
  - **Root-cause dig (session.rs):** removed a global session-init macro
    `CREATE OR REPLACE MACRO cardinality(x) AS len(x)` that shadowed DuckDB's
    native MAP-aware `cardinality` (user macros outrank builtins) — the actual
    reason size(map) still failed after the emission arm alone.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Confirmed the macro
  removal is SAFE (only emitter of `cardinality(` is the new map arm; array
  `cardinality`/`size` route to `len`, identical to the macro; map cardinality
  was already broken, now fixed). Type dispatch correct; 5 tests pin each arm.
  Latent NULL-parity gap in `array_except` (drops a NULL element Spark would
  keep) — NOT a regression (byte-identical to the old macro's behavior), no
  corpus witness → recorded as a follow-up, not blocking. Also noted (out of
  scope): several now-shadowed dead macros in session.rs + a pre-existing
  `array_distinct → list_distinct` reordering smell — future cleanup tickets.
- **Gate:** 1369→1372 (**Δ +3, zero regressions**). Newly green: test_array_except,
  test_reverse_array, test_size_map. `cargo test -p thunderduck-core --lib` 1071.
- **Reflect:** the survey's "emission-only rename" batch was 2/3 renames but
  array_except needed real semantic derivation + a live-Spark check, and size(map)
  needed the macro-shadowing root-cause dig — good example of not trusting a
  "trivial" label. Follow-ups banked: array_except NULL parity; dead-macro sweep;
  array_distinct order.

## Pass 18 — 2026-07-11 — scalar-function batch 3: max_by / min_by / json_object_keys

- **Baseline (post-Pass-17, commit 265fd40):** 1372 passed / 70 failed.
- **Root cause:** these 3 names had NO entry in type_inference.rs → analyzer
  `unresolved type` boundary (not the Binder error the survey guessed). Survey
  batch 3.
- **Fix:** type_inference.rs — AGG_SPECS rows for max_by/min_by
  (AggRet::ArgType = type of first arg, AlwaysNullable), function_return_type arm
  json_object_keys → Array(String, nullable). emission.rs — aggregate renames
  max_by→arg_max, min_by→arg_min (Spark max_by(x,y)=arg_max(x,y), same order,
  verified native); scalar rename json_object_keys→json_keys (returns VARCHAR[]
  natively, no CAST). 8 unit tests.
- **Review:** documented-skip (additive boundary expansion, unit-locked, DuckDB
  shapes verified empirically against a live duckdb binary). Gate arbitrated.
- **Gate:** 1372→1375 (**Δ +3, zero regressions**). Newly green: test_max_by,
  test_min_by, test_json_object_keys. `cargo test -p thunderduck-core --lib` 1078.
- **Known latent gap (documented, no witness):** json_keys returns [] on a
  non-object/non-null JSON where Spark returns NULL (corpus exercises object
  inputs only). Follow-up alongside the array_except NULL gap from Pass 17.
- **Reflect:** batches 1-3 done (9 scalar functions, +9). Remaining survey
  batches: 4 (positive, bit_get), 5 (substring_index, count(DISTINCT a,b)),
  6 (to_char date-form). Then the hard from_json DataFrame-API case + TPC
  AssertionErrors + singletons (Tail, json_tuple, alter_table, Array type).

## Pass 19 — 2026-07-11 — scalar-function batches 4+6: positive / bit_get / to_char

- **Baseline (post-Pass-18, commit 2ff1510):** 1375 passed / 67 failed.
- **Root cause:** 3 feature-family functions with no type_inference entry
  (analyzer `unresolved type` boundary). Survey batches 4 (positive, bit_get) +
  6 (to_char) — combined as one easy pass.
- **Fix:** type_inference.rs — positive → first-arg type (mirrors negative),
  bit_get/getbit → Byte, to_char folded into the date_format String arm.
  emission.rs — positive → `(x)`; bit_get → `CAST(((x >> pos) & 1) AS TINYINT)`;
  to_char → strftime via the shared `spark_fmt_to_duckdb` helper (mirrors
  date_format, date-form only — corpus has no number-format arg). 6 unit tests.
- **Review:** documented-skip (additive boundary expansion, unit-locked, DuckDB
  shapes verified against the live duckdb binary; all 3 confirmed end-to-end).
- **Gate:** 1375→1378 (**Δ +3, zero regressions**). Newly green: test_positive,
  test_bit_get, test_to_char. `cargo test -p thunderduck-core --lib` 1085.
- **Follow-up noted (out of scope):** `negative`/`negate` has a type arm but NO
  emission arm → would emit invalid `negative(x)`; no current witness. Batch with
  a future scalar pass.
- **Reflect:** scalar-function tail nearly done — batches 1/2/3/4/6 landed (12
  functions, +12 total). Remaining: batch 5 (substring_index emulation +
  count(DISTINCT a,b) null-guarded ROW — the two MODERATE ones), then the hard
  from_json DataFrame-API case, ~32 TPC AssertionErrors, and singletons
  (RelType::Tail, json_tuple, alter_table, Array-type, RelType::Sql, Parser).

## Pass 20 — 2026-07-11 — scalar-function batch 5: substring_index / count(DISTINCT a,b)

- **Baseline (post-Pass-19, commit 94d72eb):** 1378 passed / 64 failed.
- **Root cause:** substring_index — no type arm (analyzer `unresolved type`);
  count(DISTINCT a,b) multi-arg — τ emitted illegal DuckDB `count(DISTINCT a, b)`
  (Binder error). Survey batch 5 (the two moderate ones).
- **Fix (both native, no extension):**
  - substring_index: String return arm; emission CASE on count sign —
    `string_split` + `list_slice` (count>0 → slice(1,count); count<0 →
    slice(count,-1); count=0 → empty) + `array_to_string`. list_slice clamps
    out-of-range and propagates NULL, matching Spark for ± / 0 / overflow /
    delim-absent / NULL.
  - count(DISTINCT a,b): guard in render_aggregate (`f.distinct && duck_name==
    "count" && args.len()>1`) emits `count(DISTINCT CASE WHEN a IS NULL OR b IS
    NULL THEN NULL ELSE (a,b) END)` — Spark drops any row with a NULL distinct
    arg; DuckDB's bare ROW(a,b) is non-NULL even all-NULL, so the naive form
    over-counts. Single-arg count(DISTINCT x)/count(x)/count(*) UNCHANGED.
  - 6 unit tests (incl. `count_distinct_single_arg_unaffected_by_tuple_guard`).
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Confirmed the guard
  fires ONLY for multi-arg count-DISTINCT (single-arg byte-identical); NULL
  semantics + 3-arg generalization + substring_index sign/clamp/NULL all
  re-verified live. Non-blocking Lows (follow-ups): non-DISTINCT multi-arg
  `count(a,b)` still emits invalid SQL (Spark ACCEPTS it — pre-existing gap, task
  premise was wrong that Spark rejects it); substring_index repeats sub-exprs
  (cosmetic, CASE short-circuits).
- **Gate:** 1378→1380 (**Δ +2, zero regressions** — single-arg count-distinct
  intact across all green cases). Newly green: test_count_distinct_multiple_columns,
  test_substring_index. `cargo test -p thunderduck-core --lib` 1091.
- **Reflect:** **scalar-function arc COMPLETE** — survey batches 1-6 all landed
  across Passes 16-20 (14 functions, +14: dayname, monthname, btrim, reverse,
  size(map), array_except, max_by, min_by, json_object_keys, positive, bit_get,
  to_char, substring_index, count-distinct-multi). Remaining 62: the HARD tail —
  ~32 TPC AssertionErrors (data diffs, each needs individual diagnosis),
  from_json DataFrame-API (JSON-schema parsing, flagged hard), and singletons
  (RelType::Tail×2, json_tuple, alter_table, Array-type, RelType::Sql, 2 Parser
  errors, negative-emission, jn-018, tbl-013). Follow-up tickets banked:
  array_except NULL, json_keys non-object, negative emission, non-distinct
  multi-count.

## Pass 21 — 2026-07-11 — Spark toPrettySQL naming for unaliased FunctionCall columns

- **Baseline (post-Pass-20, commit e2f30af):** 1380 passed / 62 failed.
- **Cluster (survey `.agent-output/021-diagnostic-tpc-assertion.md`):** 11 of 12
  TPC AssertionErrors were SCHEMA-NAME diffs — τ named an unaliased
  aggregate/function column by its BARE name (`sum`) where Spark toPrettySQL
  uses `fn(args)` (`sum(ss_net_profit)`, `round((s1 / s2), 2)`). q061 also names
  Cast operands. (q066 is a separate decimal-typing case, out of scope.)
- **Fix:** analyzer.rs — `expression_output_name`'s FunctionCall arm →
  `pretty_name(expr)` (Spark toPrettySQL); added a `Cast` arm to `pretty_name`
  with a new `spark_type_sql` helper (uppercase Catalyst spelling
  `DECIMAL(15,4)`, distinct from emission's DuckDB `render_data_type`). Coder
  verified against LIVE Spark that unaliased multi-agg PIVOT is also `{pv}_fn(args)`
  (τ's old bare form was an untested parity bug, not a regression risk).
- **Fix-loop (regression caught at gate, then fixed):** the first gate flipped
  the 11 targets +tpch-q13 (+12) but REGRESSED **win2-002** green→red: Spark's
  time-`window` function names its struct column BARE `window`, not
  `window(last_login, 1 day)` — the one case where the OLD bare name MATCHED
  Spark. Added a special-case to `expression_output_name` (window/session_window
  → bare name) + a regression-guard unit test. Re-gated clean.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE-WITH-NITS. Confirmed
  the strict-name harness means no OTHER green case had an unaliased-function
  column (bare name never matched Spark EXCEPT window/session_window, now
  handled). Residual stay-red gaps banked as follow-ups (NOT regressions): M1
  `pretty_name` ignores `f.distinct` (unaliased count(DISTINCT x)→count(x)); M2
  DataFrame `count(*)`→`count(*)` vs Spark `count(1)`; M3 uppercase SQL func
  names not lowercased; N1 stale v2_lowering comment; N2 interval/struct type
  spellings.
- **Gate (re-gate, window fix):** 1380→1392 (**Δ +12, zero regressions**;
  win2-002 recovered). DataFrame corpus back to 402/402; SQL corpus 373→385.
  Newly green: tpcds-q002/q008/q013/q014a/q015/q023a/q035/q045/q048/q061 +
  tpch-q13/q18. `cargo test -p thunderduck-core --lib` 1102.
- **Reflect:** biggest remaining cluster cleared; the fix-loop validated the
  gate as the safety net (the reviewer's "bare never matches Spark" premise
  had exactly one exception — window — that only the corpus gate surfaced).
  Remaining ~50: q066 (decimal typing), from_json DF-API, singletons
  (RelType::Tail, json_tuple, alter_table, Array-type, RelType::Sql, Parser),
  and the residual-naming M1/M2/M3 gaps.

## Pass 22 — 2026-07-11 — ORDER BY-over-aggregate resolution, increment 1 (Cluster A)

- **Baseline (post-Pass-21, commit 0312239):** 1392 passed / 50 failed.
- **Cluster (diagnosis `.agent-output/022`, design `.agent-output/023`):** 15 SQL
  corpus reds where ORDER BY re-states an aggregate/expression (not the SELECT
  alias) — τ resolved sort keys against the Sort INPUT (= Project/Aggregate
  OUTPUT), so base-column leaves didn't resolve (`UNRESOLVED_COLUMN`); q096
  resolved then died in DuckDB (`count(1) must appear in GROUP BY`). τ lacked
  Spark's `ResolveAggregateFunctions`/`ResolveReferencesInSort`.
- **Fix (analyzer Sort arm, increment 1 of 3):** new `analyze_sort` — step 1 is
  today's `resolve_and_stamp` (byte-identical); FALLBACK only on
  `UnknownColumn` OR an aggregate-call key over an Aggregate child (q096) →
  re-resolve against the child INPUT, `semantic_eq`-match the WHOLE key against
  the child's aggregates/projections (alias/qualifier-stripped, nondeterministic-
  excluded), rewrite to a bare ColumnReference on that output column, alias-pin
  the matched entry if unaliased. Spark 4.1.1 semantics confirmed against the
  apache/spark v4.1.1 sources (curl). Increment 2 (subtree match + hidden
  outputs + trim Project) deferred → will flip q098 + the 4 substr cases.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE-WITH-NITS. Confirmed
  the green-case byte-identical claim (fallback fires only on red-today paths;
  the +9/zero-regression gate proves it). Caught a HIGH latent silent-wrong-data
  risk: `canonicalize_for_semantic_eq` stripped qualifier AND ordinal, so
  same-base-named columns (`t1.x`/`t2.x`) collapsed and matched the first →
  wrong ORDER BY column. FIXED before commit: added `ordinals_compatible` run
  after the structural `==` (ColumnReference::PartialEq excludes ordinal by
  design, so the reviewer's retain-ordinal one-liner was a no-op) — rejects a
  match when the same name binds to different ordinals, at every nesting depth.
  +collision regression test. Lows: extended nondeterministic roster
  (input_file_name/spark_partition_id/random); windowed-agg trigger note.
- **Gate (re-gate after HIGH fix):** 1392→1401 (**Δ +9, zero regressions**; the
  hardening held all 9 flips, un-flipped none). SQL corpus 385→394. Newly green:
  tpcds-q016/q042/q071/q084/q091/q092/q094/q095/q096. `cargo test -p
  thunderduck-core --lib` 1111.
- **Reflect:** biggest analyzer feature of the session; the fix-loop caught a
  genuine silent-wrong-data risk the gate alone would only surface if a target
  hit the collision (none did) — review earned its keep again. Remaining ~41:
  increment 2 (q098 + substr cases q062/q079/q085/q099), the 4 Cluster-A lone
  cases (jn-018 reserved-word quoting, tbl-013 qualified-arg naming, q044
  subquery-arity, q066 decimal-union), feature-family clusters (array-ordering,
  date-arith, math, interval), and singletons (RelType::Tail, json_tuple, etc.).

## Pass 23 — 2026-07-11 — order-preserving array set-ops + array_intersect typing

- **Baseline (post-Pass-22, commit f665796):** 1401 passed / 41 failed.
- **Root cause:** τ emitted Spark array set-ops via DuckDB ops that reorder by
  hash and/or mishandle NULL: `array_distinct→list_distinct`,
  `array_intersect→list_intersect` (both hash-reorder), `array_union` never
  deduped `a`, and the Pass-20 `array_except` idiom `NOT list_contains(b,x)` was
  null-unsafe (dropped a shared NULL Spark keeps). Plus `array_intersect` typed
  its element `containsNull=false` unconditionally.
- **Fix (emission.rs + type_inference.rs), all verified vs live DuckDB + Spark:**
  order-preserving/null-safe idioms — `array_distinct(a)`→`list_filter(a,(x,i)->
  list_position(a,x)=i)`; `array_union(a,b)`→`array_distinct(list_concat(a,b))`;
  `array_intersect(a,b)`→`list_filter(a,(x,i)->list_position(a,x)=i AND
  list_position(b,x) IS NOT NULL)`; `array_except` membership switched to the
  null-safe `list_position(b,x) IS NULL` (non-null result byte-identical to
  before; shared-NULL now Spark-correct — fixes a latent unwitnessed bug). All
  wrapped in the existing NULL-propagation CASE. Typing: `array_intersect`
  element containsNull = `leftContainsNull AND rightContainsNull` (Catalyst;
  arr2-005 unaffected — right arg non-nullable).
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE-WITH-NITS. Verified
  every NULL/order claim directly against the DuckDB binary; confirmed
  array_except (previously green) is non-null-byte-identical and its shared-NULL
  change is a strict correctness improvement; 1-based list_position/list_filter
  indexing correct; typing sound. Lows (non-blocking): repeated-subexpr fan-out
  (volatile-arg, same tradeoff as existing array ops); pre-existing `->` lambda
  deprecation warning.
- **Gate:** 1401→1404 (**Δ +3, zero regressions**; array_except stayed green).
  Newly green: test_array_distinct, test_array_intersect, test_array_union.
  `cargo test -p thunderduck-core --lib` 1114.
- **Reflect:** the Pass-17 array_except lambda pattern generalized cleanly to the
  three set-ops; the coder's live-DuckDB+Spark verification caught a real
  null-safety bug in the pattern it was told to mirror. Remaining ~38: ORDER BY
  increment 2 (q098 + substr q062/q079/q085/q099), Cluster-A lone (jn-018,
  tbl-013, q044, q066), feature-family (date-arith, math, interval), singletons.

## Pass 24 — 2026-07-11 — DATE-arithmetic result coercion (Date ± INTERVAL → DATE)

- **Baseline (post-Pass-23, commit dc3810e):** 1404 passed / 38 failed.
- **Root cause (ONE):** DuckDB promotes `DATE ± INTERVAL` (and the next_day
  macro, and bare `DATE '...'` literals emitted as `+ INTERVAL n DAY`) to
  TIMESTAMP. τ's analyzer correctly declares DATE, but emission yielded a
  TIMESTAMP runtime value → collected as datetime not date (schemas matched,
  values diverged). Diagnosis `.agent-output/026-diagnostic-date-interval.md`.
- **Fix (emission.rs + session.rs — coerce to DATE where inferred type is Date),
  4 sites, all verified vs live DuckDB:**
  1. `add_months`/`date_add`/`date_sub` scalar arms → `CAST(... AS DATE)`
     (add_months end-of-month clamp preserved under the cast).
  2. `render_binary`: CAST-to-DATE guard when `op ∈ {Add,Sub}` & one operand
     Date & other `is_interval()` — the exact structural twin of
     `binary_data_type`'s Date±interval arm, so Timestamp±interval (disjoint
     enum arm) is NEVER wrapped (no regression to day-time-interval cases).
  3. `next_day` macro → integer-day add (dropped `* INTERVAL 1 DAY`).
  4. **`LiteralValue::Date` renderer** (coder's 4th find): bare `DATE '...'` was
     `DATE '1970-01-01' + INTERVAL n DAY` → TIMESTAMP; changed to integer-day
     `+ (n)` → DATE (broad: every date literal; a fix, re-aligning emission with
     the already-declared Date type).
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Confirmed the
  render_binary guard mirrors `binary_data_type` exactly (timestamp-safe by
  disjoint enum arms), the DATE-literal change is a strict fix (no green case
  relied on the timestamp promotion — that would itself be red), next_day/
  add_months semantics preserved. Lows (non-blocking): O(n²) type re-derivation
  on deep Add/Sub chains (negligible); reversed-Sub guard fires on invalid
  `interval - date` (harmless, DuckDB rejects). Correctly predicted intv-004
  (Date+DayTimeInterval) flips green — Spark casts that to DATE too.
- **Gate:** 1404→1412 (**Δ +8, zero regressions** — the broad DATE-literal
  change touched every date literal with zero green→red). Newly green: the 7
  date/interval targets + bonus test_date_to_string. `cargo test -p
  thunderduck-core --lib` 1123.
- **Reflect:** one root cause (DuckDB DATE→TIMESTAMP promotion), 4 emission
  sites, +8. The coder's 4th-site discovery (DATE literal renderer) via dumping
  emitted SQL is the pattern that keeps finding latent siblings. Remaining 30:
  ORDER BY increment 2 (q098 + substr q062/q079/q085/q099), Cluster-A lone
  (jn-018, tbl-013, q044, q066), math (ceil_floor/exp/log/round), map, and
  singletons (Tail, json_tuple, alter_table, write, split_with_limit, etc.).

## Pass 25 — 2026-07-11 — math-function nullability (ceil/floor/round/exp/log family)

- **Baseline (post-Pass-24, commit 81e3841):** 1412 passed / 30 failed.
- **Root cause (ONE):** `function_call_nullable` (expression.rs ~1274) had no arm
  for ceil/ceiling/floor/round/bround/exp/ln/log/log10/log2, so they fell to the
  default any-arg-nullable → declared non-nullable over a non-nullable input.
  Spark declares these UNCONDITIONALLY nullable (verified live vs Spark 4.1.1;
  `abs`/`pow(x,lit)` do NOT get the override — not a blanket rule). Values were
  already byte-identical; only the wire nullability flag diverged.
- **Fix:** add those 10 names to the always-nullable arm. 1 unit test.
- **Review:** documented-skip (additive nullability-table entry, unit-locked,
  live-Spark-verified; a green case can't rely on non-nullable where Spark says
  nullable — that would be red). Gate arbitrated.
- **Gate:** 1412→1416 (**Δ +4, zero regressions**). Newly green: test_ceil_floor,
  test_exp, test_log, test_round. `cargo test -p thunderduck-core --lib` 1124.
- **Follow-up noted:** sqrt/cbrt/sin/cos and the rest of Spark's UnaryMathExpression
  family show the same always-nullable override empirically but have no failing
  witness — deferred.
- **Reflect:** the "nullable mismatch masks nothing here, it IS the defect"
  variant. Remaining 26: ORDER BY inc-2 (q098 + substr q062/q079/q085/q099),
  Cluster-A lone (jn-018/tbl-013/q044/q066), map (explode_map/map_from_arrays),
  singletons (Tail, write, split_with_limit, json_tuple, alter_table, encode,
  from_json-DF, empty_array, dl-write-append-002).

## Pass 26 — 2026-07-11 — map_from_arrays type derivation (explode_map deferred)

- **Baseline (post-Pass-25, commit 1e284ca):** 1416 passed / 26 failed.
- **Root cause:** `map_from_arrays` was bundled into the hardcoded
  `Map<String,String,value_nullable=true>` fallback (type_inference.rs:710),
  regardless of arg types (map/create_map had a dedicated fast path;
  map_from_arrays didn't). Wrong declared key/value types → schema mismatch.
- **Fix:** `function_call_data_type` (expression.rs) arm deriving key type from
  the keys-array element type and value type + valueContainsNull from the
  values-array element (matches Spark MapFromArrays.dataType, verified live).
  2 unit tests.
- **Review:** documented-skip (additive type-derivation, unit-locked, live-Spark-
  verified). Gate arbitrated.
- **Gate:** 1416→1418 (**Δ +2, zero regressions**). Newly green:
  test_map_from_arrays (DataFrame + type_literals). `cargo test` 1126.
- **STOPPED / deferred: `test_explode_map`** — unaliased `explode(map)` should
  emit two default-named columns `key`,`value`, but τ's multi-column map-explode
  expansion (emission.rs:3850, type_inference.rs:829) only fires on an EXPLICIT
  alias (SQL `AS (k,v)` / DataFrame `.alias(k,v)`). Bare explode(map) falls to
  the single-col scalar path → `UNRESOLVED_COLUMN cannot resolve key`. Needs a
  schema-aware Project pre-pass generator expansion (mirror
  expand_json_tuple/stack_projections, dispatch Array=1col vs Map=2cols) — real
  generator-machinery work, no corpus witness. Follow-up ticket.
- **Reflect:** remaining 24: ORDER BY inc-2 (q062/q078/q079/q085/q098/q099),
  Cluster-A lone (jn-018/tbl-013/q044/q066), and singletons (Tail, write,
  split_with_limit, json_tuple, alter_table, encode, from_json-DF, empty_array,
  explode_map, combined_sql_df, dl-write-append-002).

## Pass 27 — 2026-07-11 — ORDER BY-over-aggregate resolution, increment 2 (Cluster A)

- **Baseline (post-Pass-26, commit 164bed3):** 1418 passed / 24 failed.
- **Re-diagnosis of the 6 candidates:** only q078 (Project missing-ref) + q098
  (grouping col not in SELECT) are genuine increment-2 shapes. q062/q079/q099 =
  a SEPARATE `substr`→`substring` display-name bug (ORDER BY resolution already
  correct). q085 = a pre-existing `grouping_already_folded` false-negative when
  the grouping key appears only NESTED in a SELECT expr (independent of Sort).
  Both flagged as follow-up tickets.
- **Fix (analyzer.rs increment 2, extends Pass-22):** when increment-1's
  whole-key match fails, walk the input-resolved key top-down; promote a matching
  subtree to the child output column, or append a hidden aggregate/grouping/
  passthrough output (Alias at END, dedup+uniquify) and synthesize a trailing
  trim Project restoring the original output columns. `opaque_to_subtree_promotion`
  guards Window etc. from decomposition; a leftover non-grouped/non-aggregated
  bare column re-raises UnknownColumn. Also fixed a pre-existing offset/length
  guard that had unconditionally bailed the fallback on non-folded aggregates.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE-WITH-NITS. Confirmed
  green byte-identity (fallback only on red-today keys; folded whole-key cases
  offset==0 byte-identical; no schema growth → Sort returned directly). Medium
  (DEFERRED — witness-free, NOT a regression, hard-error not wrong-data):
  a non-folded DataFrame aggregate with a COMPUTED grouping key
  (`groupBy(a+b).agg(sum).orderBy(a+b)`) would append instead of binding to the
  grouping-prefix column → column-count desync → downstream error; the shape was
  already red pre-inc-2 and no corpus case hits it; clean fix = extend the
  whole-key match to the grouping-prefix output columns. Lows: source_quals lost
  on re-stamp of an alias-pinned grouping col (inert for emission here).
- **Gate:** 1418→1420 (**Δ +2, zero regressions**; the coder's own full-corpus
  run independently showed base 24 → 22, failed-set = base minus q078/q098).
  SQL corpus 394→396. Newly green: tpcds-q078, tpcds-q098. `cargo test -p
  thunderduck-core --lib` 1130.
- **Reflect:** ORDER BY-over-aggregate resolution (Cluster A) now COMPLETE for
  the corpus (increments 1+2, 11 cases across Passes 22+27); increment 3
  (ordinals) is error-parity-only, no witness. Remaining 22: substr-naming
  (q062/q079/q099), grouping-fold-nested (q085), Cluster-A lone (jn-018, tbl-013,
  q044, q066), and singletons (Tail, write, split_with_limit, json_tuple,
  alter_table, encode, from_json-DF, empty_array, explode_map, combined_sql_df,
  dl-write-append-002). Deferred inc-2 Medium + inc-1 follow-ups tracked here.

## Pass 28 — 2026-07-11 — substr shorthand output name (clears F-substr-name)

- **Baseline (post-Pass-27, commit e7e3ab1):** 1420 passed / 22 failed.
- **Root cause:** `parser_v2/v2_lowering.rs` `Expr::Substring` arm discarded
  sqlparser's `shorthand: bool` and always named the lowered FunctionCall
  `"substring"`. τ's naming pass (pretty_name/toPrettySQL) names an unaliased
  column verbatim by FunctionCall.name → every unaliased `substr(...)` SQL
  column came out `substring(...)`, mismatching Spark (which names by the typed
  keyword). This also left emission's `"substr"=>"substring"` rename arm dead.
- **Fix:** destructure `shorthand`; name the FunctionCall `"substr"` when true,
  else `"substring"` — emission's existing rename normalizes the DuckDB call.
  Live Spark confirmed: `substr(...)`→`substr(...)`, `substring(...)`→
  `substring(...)`, both comma-arg form. One-file change; DataFrame path
  (verbatim wire name) unaffected. 1 test updated.
- **Review:** documented-skip (one-line SQL-lowering naming fix, unit-locked,
  live-Spark-verified; a green case can't rely on the wrong name — that'd be
  red). Gate arbitrated.
- **Gate:** 1420→1423 (**Δ +3, zero regressions**). SQL corpus 396→399. Newly
  green: tpcds-q062, q079, q099. `cargo test -p thunderduck-core --lib` 1130.
- **Reflect:** clears follow-up `F-substr-name`. Remaining 19: F-groupfold-nested
  (q085), Cluster-A lone (jn-018, tbl-013, q044, q066), and singletons (Tail,
  write, split_with_limit, json_tuple, alter_table, encode, from_json-DF,
  empty_array, explode_map, combined_sql_df, dl-write-append-002).

## Pass 29 — 2026-07-11 — scalar/IN subquery arity false-positive (tpcds-q044)

- **Baseline (post-Pass-28, commit 125ce20):** 1423 passed / 19 failed.
- **Root cause:** SAME `grouping_already_folded` false-negative as q085
  (F-groupfold-nested): a subquery body `select avg(x) rank_col ... group by
  store_sk` (GROUP BY key NOT in SELECT) makes the heuristic guess DataFrame-shape
  and prepend the grouping key → 2-field schema → `analyze_single_column_subquery`
  correctly rejects the (wrongly-2-col) subquery as "must return exactly one column".
- **Fix (scoped, analyzer.rs):** `unfold_ungrouped_aggregate_subquery` — since
  `ScalarSubquery`/`InSubquery` are built ONLY by the SQL front-end (verified
  exhaustively; DataFrame subqueries hit a boundary error at conversion), any
  inner Aggregate is SQL-origin, so a prepended grouping key is always a
  schema-construction artifact. When the body is an Aggregate with non-empty
  grouping and !grouping_already_folded, wrap it in a Project selecting the
  trailing aggregates.len() fields (drop the spurious prefix). No-op otherwise;
  does NOT touch the global heuristic or its other call sites. 2 tests.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Confirmed the
  SQL-only linchpin (ScalarSubquery/InSubquery constructed only in
  parser_v2:3196/3203; DataFrame path bails at v2_relation_converter:1128),
  no-op-except-mis-detected-shape, wrap correctness (drops exactly grouping.len()
  prefix), genuine-2-col rejection intact (ExistsSubquery doesn't route here),
  no double-application. Low nit: the 2-col test doesn't isolate the no-op path.
- **Gate:** 1423→1424 (**Δ +1, zero regressions**; coder's own full run showed
  18 reds, q044 gone). SQL corpus 399→400. `cargo test -p thunderduck-core --lib`
  1132.
- **Reflect:** q044 and q085 (F-groupfold-nested) share the SAME root cause
  (grouping_already_folded false-negative for a not-restated GROUP BY key). This
  scoped fix handles the SUBQUERY-arity manifestation; q085's TOP-LEVEL SELECT
  manifestation (grouping key nested in a projection expr) is still open. The
  deferred `F-agg-folded-flag` (front-end folded flag on CommonOp::Aggregate)
  would subsume both cleanly. Remaining 18: q085, Cluster-A lone (jn-018,
  tbl-013, q066), + singletons.

## Pass 30 — 2026-07-11 — reserved-keyword `at` quoting (jn-018)

- **Baseline (post-Pass-29, commit aa9109e):** 1424 passed / 18 failed.
- **Root cause:** a derived-table alias `at` (a DuckDB `TYPE_FUNC_NAME` reserved
  keyword) was emitted unquoted → DuckDB Parser Error. The `quote_ident` /
  `is_safe_identifier` mechanism was already correct (char-shape + reserved-word
  binary search); the curated `DUCKDB_RESERVED` const was just MISSING `"at"`
  (its category peers cross/full/inner/join/map/struct were already present) —
  an isolated omission, not a systemic gap.
- **Fix:** add `"at"` to `DUCKDB_RESERVED` (sorted position); 1 regression test.
- **Review:** documented-skip (one-line curated-const addition; quoting a reserved
  word is always valid — a green case with an unquoted reserved word would be
  red). Coder verified full sql_v2 (402/405) + DataFrame (402/402) corpora.
- **Gate:** 1424→1425 (**Δ +1, zero regressions**). SQL corpus 400→401. Newly
  green: jn-018. `cargo test -p thunderduck-core --lib` 1133.
- **Reflect:** SQL corpus now 401/404 — 3 lone cases from 100% (tbl-013
  qualified-arg naming, tpcds-q066 decimal-UNION, tpcds-q085 grouping-fold-nested
  = F-groupfold-nested). Remaining 17 total (those 3 + ~14 feature-family/
  singletons: Tail, write, split_with_limit, json_tuple, alter_table, encode,
  from_json-DF, empty_array, explode_map, combined_sql_df, dl-write-append-002).

## Pass 31 — 2026-07-11 — WithColumnsRenamed positional emission (tbl-013)

- **Baseline (post-Pass-30, commit 203ba54):** 1425 passed / 17 failed.
- **Root cause:** `build_with_columns_renamed` emitted a BY-NAME rename
  (`SELECT "<old tracked name>" AS new ...`). For an unaliased `count(d.dept_id)`
  the inner Aggregate's tracked name is `count(dept_id)` (qualifier-stripped,
  correct Spark parity), but DuckDB's actual default column name KEEPS the
  qualifier (`count(d.dept_id)`), so the by-name reference pointed at a
  nonexistent column → binder error (tbl-013).
- **Fix:** emit DuckDB's POSITIONAL derived-table column-alias-list
  `SELECT * FROM (child) AS __td_wcr(new1, …, newN)` (same blessed pattern as
  `wrap_reprojected`). Sidesteps the whole name-mismatch class for both the SQL
  `ToDf` (`AS t(a,b)`) and DataFrame `withColumnsRenamed`/`toDF` paths (shared fn).
  1 regression test.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Verified the key risk
  (subset `withColumnsRenamed` → wrong arity) is NOT a bug: the fn expands the
  rename map against the child's FULL schema (unmatched fields keep their name),
  so `dst_names.len() == fields.len()` always; `SELECT *` order/count holds for
  all block children; `__td_wcr` no collision. Low (pre-existing, NOT introduced,
  → ledger `F-todf-dupname`): `toDF` over a duplicate-column child collapses via
  the last-wins by-name map in `analyze_to_df`.
- **Gate:** 1425→1426 (**Δ +1, zero regressions**; coder verified full sql_v2 +
  DataFrame corpora). SQL corpus 401→402. Newly green: tbl-013. `cargo test -p
  thunderduck-core --lib` 1134.
- **Reflect:** SQL corpus now 402/404 — 2 from 100% (tpcds-q066 decimal-UNION,
  tpcds-q085 grouping-fold-nested/F-groupfold-nested). Remaining 16 total (those
  2 + ~14 feature-family/singletons).

## Pass 32 — 2026-07-11 — decimal/integral division routes to spark_decimal_div (tpcds-q066)

- **Baseline (post-Pass-31, commit f50c195):** 1426 passed / 16 failed.
- **Root cause:** `sum(jan_sales / w_warehouse_sq_ft)` = `DECIMAL(38,2) / BIGINT`.
  The analyzer declares Decimal (Pass-15 widening), but `render_binary`'s Div arm
  routed to `spark_decimal_div` only when BOTH operands were literally Decimal,
  so it emitted plain `/` → DuckDB DOUBLE (verified: `DECIMAL/BIGINT`→DOUBLE,
  while +/-/* stay DECIMAL) → schema said decimal, value came back double →
  PySpark Arrow `isinstance(Decimal)` assertion failed (q066).
- **Fix:** extend the Div→spark_decimal_div routing in `render_binary` to the
  decimal⊗integral case using the SAME `decimalize`/`decimal_form` logic the
  analyzer uses (`emission.rs` mirrors `binary_data_type`): CAST both operands to
  their widened DECIMAL(p,s) and route through the extension. `decimalize` bumped
  to `pub(crate)`. No analyzer change (declared type already correct). 1 test.
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Confirmed the emission
  routing now mirrors `binary_data_type`'s Decimal inference EXACTLY (fires on
  the same both-Decimal ∪ one-Decimal-one-integral set; Float/Double stay plain
  `/`); no green regression (a DOUBLE-emitting decimal/int Div was already
  schema-red since Spark yields decimal); cast targets + overflow correct. Low
  (→ ledger F-decimal-div-dup-logic): the widening logic is now duplicated in
  analyzer + emission and must stay in lockstep — extract a shared helper.
- **Gate:** 1426→1427 (**Δ +1, zero regressions**; coder's full run corroborated).
  SQL corpus 402→403. Newly green: tpcds-q066. `cargo test -p thunderduck-core
  --lib` 1135.
- **Reflect:** SQL corpus now **403/404** — ONE case (tpcds-q085,
  F-groupfold-nested) from 100%. Completes the decimal-division-type-coherence
  arc (Pass 12 cast → Pass 15 typing → Pass 24 date → Pass 32 div-by-integral).
  Remaining 15: q085 + ~14 feature-family/singletons (Tail, write,
  split_with_limit, json_tuple, alter_table, encode, from_json-DF, empty_array,
  explode_map, combined_sql_df, dl-write-append-002).

## Pass 33 — 2026-07-11 — retire grouping_already_folded via explicit AggregateProjection flag (tpcds-q085) — SQL CORPUS 100%

- **Baseline (post-Pass-32, commit 422cc3e):** 1427 passed / 15 failed (corpus).
- **Root cause (recurring — bit Passes 10, 27, 29, and blocked q085):** the
  `grouping_already_folded` heuristic guessed SQL-folded vs DataFrame-grouped
  Aggregate shape by name-matching grouping keys against aggregates. q085's GROUP
  BY key appears only NESTED in a SELECT expr (`substr(r_reason_desc,1,20)`) →
  false-negative → spurious prepend → column-count mismatch. UNFIXABLE by nested
  matching (a DataFrame `.groupBy(a).agg(sum(a+1))` legitimately nests the key yet
  Spark prepends) — the SQL-vs-DataFrame distinction is an ORIGIN property.
- **Fix (design `.agent-output/035`, 6 files):** explicit `AggregateProjection
  {Folded, Grouped}` on CommonOp/TypedOp::Aggregate, set per front-end (SQL
  lower_aggregate_select=Folded — the sole SQL site; DataFrame convert_aggregate/
  cov/corr/approx_quantile/crosstab=Grouped). All 6 `grouping_already_folded`
  consumers (analyzer schema, ADR-023 source_quals ×2, Pass-27 rebind offset,
  emission build_aggregate) read the immutable flag in lockstep (emission's
  recompute-over-reprojected desync hazard gone). Heuristic DELETED (~65 lines).
  Pass-29 `unfold_ungrouped_aggregate_subquery` DELETED (provably dead: SQL
  subqueries now Folded → never prepend → strip condition can't fire). ~25 test
  fixtures migrated zero-churn (flag := old heuristic verdict).
- **Review (`rust-reviewer`, no-tree-mutation):** APPROVE. Verified all 6
  construction-site flags (lower_aggregate_select subsumes GROUP BY ALL/ordinal/
  ROLLUP/CUBE/HAVING/implicit-global; no Default derive → field mandatory →
  clean build proves coverage), faithful fixture migration (self-checking —
  wrong flag breaks byte-exact assertions), lockstep consumer reads, dead Pass-29
  deletion, and both regression directions (SQL false→prepend never load-bearing;
  DataFrame divergence set empty across 54 sites). Also CLOSES the Pass-10
  `.groupBy(k1,k2).agg(k1,…)` edge. Low nits: substring-count test fragility.
- **Gate:** SQL corpus 403→**404/404 (100%!)**, DataFrame 402/402 (100%),
  q085 red→green, **zero corpus regressions**, zero feature-family regressions
  (excluding a CONCURRENT review session's newly-added test files present in the
  shared worktree). Recorder full-suite row 1428 passed; core 1137 + connect-server
  132 unit tests green.
- **Concurrent-worktree note:** a parallel review session (writing
  `tasks/v2-review-findings-2026-07-11.md`) added `test_sorting_differential.py::
  ...test_order_by_grouping_expression_over_multikey_aggregate` — a failing
  witness for F-orderby-computed-groupkey (unfixed missing-match half). It was
  ABSENT from the P32 baseline (not a regression of this change). Committed ONLY
  my files (explicit paths), not the concurrent session's test/doc changes.
- **Reflect:** BOTH corpora now 100% green (SQL 404/404, DataFrame 402/402).
  Retired the fold heuristic + 2 follow-ups (F-groupfold-nested, F-agg-folded-flag)
  + resolved the desync half of F-orderby-computed-groupkey. Remaining 14 are the
  feature-family "other" bucket (Tail, write, split_with_limit, json_tuple,
  alter_table, encode, from_json-DF, empty_array, explode_map, combined_sql_df,
  dl-write-append-002) + the concurrent-added F-orderby test.

---

## Pass 34 — split(str, regex, limit) 3-arg limit semantics

- **Baseline:** 1428 passed / 15 failed @ `55577ef` (both corpora 100%). Targets:
  the two feature-family "other"-bucket cases `test_array_functions_differential.py
  ::TestArrayFunctionsDifferential::{test_split_with_limit,test_split_with_limit_dataframe_api}`.
- **Root cause:** the `"split"` emission arm (emission.rs) dropped the `limit`
  argument unconditionally — every call (2-arg or 3-arg) rendered as plain
  `split({a},{b})`. DuckDB `split`/`string_split_regex` has no limit parameter.
- **Fix (1 file, `emission.rs`):** the split arm now branches on `f.args.len() >= 3`.
  3-arg emits a CASE implementing Java/Spark `String.split(regex, limit)`:
  `limit <= 0` → unlimited (trailing empties preserved, per Spark 4.1.1 verified
  live — the differential test's own "limit=0 trims" docstring is wrong);
  `limit >= len(full)` → unbounded split; else first `limit-1` pieces via
  `list_slice` + the delimiter-**rejoined** remainder (`array_to_string(...,{b})`).
  NULL in any arg → NULL. 2-arg path byte-identical. Core builtins only — no
  extension. Coder verified every in-scope case against live DuckDB
  (`.duckdb-delta/build/release/duckdb`) and live Spark 4.1.1.
- **Review:** documented-skip — bounded additive emission arm, unit-locked
  (4 new tests: 2-arg-unchanged, positive-limit-caps-and-rejoins,
  nonpositive-unlimited, null-propagates) and live-verified against both engines.
- **Gate:** full-suite **1430 passed / 12 failed (Δ +2)**, both split_with_limit
  cases red→green, **zero regressions** vs the P33 baseline (identical concurrent
  test set → clean diff). Core `cargo test -p thunderduck-core --lib` 1141 passed.
  (A third case, the concurrent session's `test_order_by_grouping_expression_over_
  multikey_aggregate`, also flipped green from their own work landing — not mine,
  not a regression.)
- **Out-of-scope carry-over:** splitting `""` gives Spark `[]` vs DuckDB `['']` —
  inherited from the untouched 2-arg path, no red witness.
- **Reflect:** first "other"-bucket case cleared. ~11 feature-family cases remain
  (Tail, write csv/json, json_tuple, alter_table, encode, from_json-DF,
  empty_array, explode_map, combined_sql_df, dl-write-append-002), several
  structural. Goal was cleared by the user after this pass; committing this fix
  and stopping per instruction.
