-- Real Spark 4.1.1 goldens for the 3 definite ext5 functions.
-- try_divide (scalar): value + return type via typeof.
SELECT 'td_int'     AS c, CAST(try_divide(10,2) AS STRING) AS v UNION ALL
SELECT 'td_int_type',     typeof(try_divide(10,2))               UNION ALL
SELECT 'td_zero',         CAST(try_divide(10,0) AS STRING)       UNION ALL
SELECT 'td_zero_dbl',     CAST(try_divide(10.0,0.0) AS STRING)   UNION ALL
SELECT 'td_null_num',     CAST(try_divide(CAST(NULL AS INT),2) AS STRING) UNION ALL
SELECT 'td_null_den',     CAST(try_divide(10,CAST(NULL AS INT)) AS STRING) UNION ALL
SELECT 'td_dec',          CAST(try_divide(CAST(1.5 AS DECIMAL(3,1)),CAST(0.5 AS DECIMAL(3,1))) AS STRING) UNION ALL
SELECT 'td_dec_type',     typeof(try_divide(CAST(1.5 AS DECIMAL(3,1)),CAST(0.5 AS DECIMAL(3,1)))) UNION ALL
SELECT 'td_bigint',       CAST(try_divide(CAST(7 AS BIGINT),CAST(2 AS BIGINT)) AS STRING) UNION ALL
SELECT 'td_bigint_type',  typeof(try_divide(CAST(7 AS BIGINT),CAST(2 AS BIGINT)));
-- try_sum / try_avg (aggregate): normal, type, overflow(NULL), decimal.
SELECT 'ts_normal' AS c, CAST(try_sum(v) AS STRING) AS v FROM VALUES (1L),(2L),(3L) t(v);
SELECT 'ts_type'   AS c, typeof(try_sum(v)) AS v FROM VALUES (1L),(2L),(3L) t(v);
SELECT 'ts_overflow' AS c, CAST(try_sum(v) AS STRING) AS v FROM VALUES (9223372036854775807L),(9223372036854775807L) t(v);
SELECT 'tavg_normal' AS c, CAST(try_avg(v) AS STRING) AS v FROM VALUES (1L),(2L),(3L) t(v);
SELECT 'tavg_type'   AS c, typeof(try_avg(v)) AS v FROM VALUES (1L),(2L),(3L) t(v);
SELECT 'tavg_overflow' AS c, CAST(try_avg(v) AS STRING) AS v FROM VALUES (9223372036854775807L),(9223372036854775807L) t(v);
SELECT 'ts_dec_normal' AS c, CAST(try_sum(v) AS STRING) AS v FROM VALUES (CAST(1.5 AS DECIMAL(3,1))),(CAST(2.5 AS DECIMAL(3,1))) t(v);
SELECT 'ts_dec_type'   AS c, typeof(try_sum(v)) AS v FROM VALUES (CAST(1.5 AS DECIMAL(3,1))),(CAST(2.5 AS DECIMAL(3,1))) t(v);
