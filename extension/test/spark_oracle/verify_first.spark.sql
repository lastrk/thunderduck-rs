-- Verify-first triage: real Apache Spark 4.1.1 oracle (mirror of verify_first.duckdb.sql).
-- Spark's kurtosis(v) is EXCESS POPULATION kurtosis -> compare to DuckDB kurtosis_pop.
-- Run: $SPARK_HOME/bin/spark-sql --master 'local[1]' -f this 2>/dev/null
SELECT 'corr_main'          AS c, CAST(round(corr(x,y),10) AS STRING) AS v FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(CAST(NULL AS DOUBLE),100),(9,CAST(NULL AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'covar_samp_main', CAST(round(covar_samp(x,y),10) AS STRING) FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(CAST(NULL AS DOUBLE),100),(9,CAST(NULL AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'covar_pop_main',  CAST(round(covar_pop(x,y),10) AS STRING)  FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(CAST(NULL AS DOUBLE),100),(9,CAST(NULL AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'regr_slope_main', CAST(round(regr_slope(y,x),10) AS STRING) FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(CAST(NULL AS DOUBLE),100),(9,CAST(NULL AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'regr_r2_main',    CAST(round(regr_r2(y,x),10) AS STRING)    FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(CAST(NULL AS DOUBLE),100),(9,CAST(NULL AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'kurtosis_pop_main', CAST(round(kurtosis(v),10) AS STRING)   FROM VALUES (CAST(1 AS DOUBLE)),(2),(3),(4),(5),(6),(7),(8),(9),(10),(CAST(NULL AS DOUBLE)) AS s(v)
UNION ALL SELECT 'skewness_main',   CAST(round(skewness(v),10) AS STRING)     FROM VALUES (CAST(1 AS DOUBLE)),(2),(3),(4),(5),(6),(7),(8),(9),(10),(CAST(NULL AS DOUBLE)) AS s(v)
UNION ALL SELECT 'count_if_main',   CAST(count_if(b) AS STRING)               FROM VALUES (true),(true),(false),(CAST(NULL AS BOOLEAN)),(true) AS bb(b)
UNION ALL SELECT 'corr_const_y',    CAST(round(corr(x,y),10) AS STRING)       FROM VALUES (CAST(1 AS DOUBLE),CAST(5 AS DOUBLE)),(2,5),(3,5) AS p(x,y)
UNION ALL SELECT 'covar_samp_1row', CAST(round(covar_samp(x,y),10) AS STRING) FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)) AS p(x,y)
UNION ALL SELECT 'corr_empty',      CAST(round(corr(x,y),10) AS STRING)       FROM VALUES (CAST(1 AS DOUBLE),CAST(2 AS DOUBLE)) AS p(x,y) WHERE x > 100
UNION ALL SELECT 'regr_slope_constx', CAST(round(regr_slope(y,x),10) AS STRING) FROM VALUES (CAST(5 AS DOUBLE),CAST(1 AS DOUBLE)),(5,1),(5,1) AS p(y,x)
UNION ALL SELECT 'tc_abc',   CAST(try_cast('abc' AS INT) AS STRING)
UNION ALL SELECT 'tc_123',   CAST(try_cast('123' AS INT) AS STRING)
UNION ALL SELECT 'tc_null',  CAST(try_cast(CAST(NULL AS STRING) AS INT) AS STRING)
UNION ALL SELECT 'tc_overflow', CAST(try_cast(10000000000 AS INT) AS STRING)
UNION ALL SELECT 'tc_date',  CAST(try_cast('not-a-date' AS DATE) AS STRING)
UNION ALL SELECT 'tc_1e100', CAST(try_cast('1e100' AS INT) AS STRING);
