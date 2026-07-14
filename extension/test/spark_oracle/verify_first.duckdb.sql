-- Verify-first triage: DuckDB native candidates.
-- Fixtures are inlined per case so each row is independent.
-- Output: one (case, value) pair per row; NULL prints as empty.
-- Pair fixture P(x,y): 8 clean rows + one (NULL,y) + one (x,NULL) to test null-skip.
.mode list
.headers off
SELECT 'corr_main'        AS c, CAST(round(corr(x,y),10) AS VARCHAR)        FROM (VALUES (1.0,2.0),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(NULL,100),(9,NULL)) p(x,y)
UNION ALL SELECT 'covar_samp_main', CAST(round(covar_samp(x,y),10) AS VARCHAR) FROM (VALUES (1.0,2.0),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(NULL,100),(9,NULL)) p(x,y)
UNION ALL SELECT 'covar_pop_main',  CAST(round(covar_pop(x,y),10) AS VARCHAR)  FROM (VALUES (1.0,2.0),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(NULL,100),(9,NULL)) p(x,y)
UNION ALL SELECT 'regr_slope_main', CAST(round(regr_slope(y,x),10) AS VARCHAR) FROM (VALUES (1.0,2.0),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(NULL,100),(9,NULL)) p(x,y)
UNION ALL SELECT 'regr_r2_main',    CAST(round(regr_r2(y,x),10) AS VARCHAR)    FROM (VALUES (1.0,2.0),(2,4),(3,5),(4,4),(5,8),(6,7),(7,9),(8,8),(NULL,100),(9,NULL)) p(x,y)
UNION ALL SELECT 'kurtosis_pop_main', CAST(round(kurtosis_pop(v),10) AS VARCHAR) FROM (VALUES (1.0),(2),(3),(4),(5),(6),(7),(8),(9),(10),(NULL)) s(v)
UNION ALL SELECT 'kurtosis_samp_main', CAST(round(kurtosis(v),10) AS VARCHAR)  FROM (VALUES (1.0),(2),(3),(4),(5),(6),(7),(8),(9),(10),(NULL)) s(v)
UNION ALL SELECT 'skewness_main',   CAST(round(skewness(v),10) AS VARCHAR)     FROM (VALUES (1.0),(2),(3),(4),(5),(6),(7),(8),(9),(10),(NULL)) s(v)
UNION ALL SELECT 'count_if_main',   CAST(count_if(b) AS VARCHAR)               FROM (VALUES (true),(true),(false),(CAST(NULL AS BOOLEAN)),(true)) bb(b)
UNION ALL SELECT 'corr_const_y',    CAST(round(corr(x,y),10) AS VARCHAR)       FROM (VALUES (1.0,5.0),(2,5),(3,5)) p(x,y)
UNION ALL SELECT 'covar_samp_1row', CAST(round(covar_samp(x,y),10) AS VARCHAR) FROM (VALUES (1.0,2.0)) p(x,y)
UNION ALL SELECT 'corr_empty',      CAST(round(corr(x,y),10) AS VARCHAR)       FROM (VALUES (1.0,2.0)) p(x,y) WHERE x > 100
UNION ALL SELECT 'regr_slope_constx', CAST(round(regr_slope(y,x),10) AS VARCHAR) FROM (VALUES (5.0,1.0),(5,1),(5,1)) p(y,x)
UNION ALL SELECT 'tc_abc',   CAST(try_cast('abc' AS INTEGER) AS VARCHAR)
UNION ALL SELECT 'tc_123',   CAST(try_cast('123' AS INTEGER) AS VARCHAR)
UNION ALL SELECT 'tc_null',  CAST(try_cast(CAST(NULL AS VARCHAR) AS INTEGER) AS VARCHAR)
UNION ALL SELECT 'tc_overflow', CAST(try_cast(10000000000 AS INTEGER) AS VARCHAR)
UNION ALL SELECT 'tc_date',  CAST(try_cast('not-a-date' AS DATE) AS VARCHAR)
UNION ALL SELECT 'tc_1e100', CAST(try_cast('1e100' AS INTEGER) AS VARCHAR);
