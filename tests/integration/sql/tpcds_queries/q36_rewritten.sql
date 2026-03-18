-- Q36 Rewritten for DuckDB Compatibility
-- Original uses GROUPING() in PARTITION BY which DuckDB doesn't support
-- This version uses UNION ALL approach (similar to DuckDB's official implementation)

WITH store_sales_summary AS (
  -- Detail level: GROUP BY i_category, i_class
  SELECT
    i_category,
    i_class,
    0 AS grouping_id,
    SUM(ss_ext_sales_price) AS total_sales
  FROM store_sales
  JOIN item ON ss_item_sk = i_item_sk
  JOIN date_dim ON ss_sold_date_sk = d_date_sk
  JOIN store ON ss_store_sk = s_store_sk
  WHERE d_year = 2001
    AND s_state = 'TN'
  GROUP BY i_category, i_class

  UNION ALL

  -- Category level: GROUP BY i_category only
  SELECT
    i_category,
    NULL AS i_class,
    1 AS grouping_id,
    SUM(ss_ext_sales_price) AS total_sales
  FROM store_sales
  JOIN item ON ss_item_sk = i_item_sk
  JOIN date_dim ON ss_sold_date_sk = d_date_sk
  JOIN store ON ss_store_sk = s_store_sk
  WHERE d_year = 2001
    AND s_state = 'TN'
  GROUP BY i_category

  UNION ALL

  -- Grand total level: No GROUP BY
  SELECT
    NULL AS i_category,
    NULL AS i_class,
    2 AS grouping_id,
    SUM(ss_ext_sales_price) AS total_sales
  FROM store_sales
  JOIN item ON ss_item_sk = i_item_sk
  JOIN date_dim ON ss_sold_date_sk = d_date_sk
  JOIN store ON ss_store_sk = s_store_sk
  WHERE d_year = 2001
    AND s_state = 'TN'
),
ranked_sales AS (
  SELECT
    total_sales / 1000000 AS gross_margin,
    i_category,
    i_class,
    grouping_id,
    CASE
      WHEN grouping_id = 0 THEN i_category
      ELSE NULL
    END AS lochierarchy_cat,
    RANK() OVER (
      PARTITION BY grouping_id, lochierarchy_cat
      ORDER BY total_sales DESC
    ) AS rank_within_parent
  FROM store_sales_summary
)
SELECT
  gross_margin,
  i_category,
  i_class,
  grouping_id AS lochierarchy,
  rank_within_parent
FROM ranked_sales
ORDER BY
  grouping_id DESC,
  CASE WHEN grouping_id = 0 THEN i_category END,
  rank_within_parent
LIMIT 100;