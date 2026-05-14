# Bug Report: spark_sum crashes DuckDB optimizer in UNION ALL CTEs

**Target repo**: `nubank/thunderduck-duckdb-extension`
**Title**: `spark_sum crashes DuckDB optimizer in UNION ALL CTEs`

---

## Description

The `spark_sum()` extension aggregate function crashes DuckDB's optimizer when used inside UNION ALL CTEs. The crash occurs in `CompressedMaterialization::CompressAggregate` which fails to handle extension-provided aggregate functions.

## Reproduction

```sql
WITH cte AS (
    SELECT spark_sum(decimal_col) AS total FROM table1
    UNION ALL
    SELECT spark_sum(decimal_col) AS total FROM table2
)
SELECT * FROM cte;
```

This triggers a crash/assertion failure in DuckDB's `CompressedMaterialization` optimizer pass.

## Expected Behavior

`spark_sum()` should work identically to native `SUM()` in all query contexts including UNION ALL CTEs.

## Workaround

The Java reference implementation works around this by using native `SUM()` wrapped with an explicit CAST to the Spark-compatible return type:

```sql
-- Instead of:
spark_sum(decimal_col)

-- Use:
CAST(SUM(decimal_col) AS DECIMAL(min(p+10, 38), s))
```

This produces correct results and avoids the optimizer crash.

## Impact

This affects strict-mode compatibility in Thunderduck when queries combine decimal aggregation with UNION ALL (common in TPC-DS queries like Q5, Q14, Q23, Q24).

## Environment

- DuckDB version: 1.5.1
- Extension release: `duckdb1.5.1-ext1`
- Platform: linux_arm64, linux_amd64

## Recommendation

Investigate whether the crash is in the extension's aggregate function registration or in DuckDB's `CompressedMaterialization` pass. If the latter, this may need to be reported upstream to DuckDB as well.
