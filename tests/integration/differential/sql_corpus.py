"""
thunderduck — Spark SQL transpiler conformance corpus
=====================================================

The SQL-front-end counterpart to `thunderduck_dataframe_corpus.py`. Per ADR-004,
SQL and the DataFrame API both lower to the same common AST; this corpus exercises
the *SQL parser path* (`spark.sql("...")`) and, above all, the constructs that have
**no first-class DataFrame API** and were therefore kept out of the DataFrame
corpus: correlated subqueries (ADR-008), CTEs, GROUPING SETS, LATERAL VIEW, PIVOT/
UNPIVOT, and SQL-only expression syntax.

Relationship to the DataFrame corpus
-------------------------------------
Same inputs (`emp`, `dept`, `emp2`, `nums`, `raw`), registered as temp views so SQL
can reference them by name. This corpus is standalone: it does not cross-reference
or pair with the DataFrame corpus. The DataFrame corpus is biased toward type/
nullability inference (ADR-005's divergent slice); this corpus is biased toward SQL
grammar coverage and SQL-only query structure — plus the numeric-tower section,
which pins per-type built-in-function result types across the numeric tower.

Ordering (most-frequently-used features first)
-----------------------------------------------
Sections are ordered by how often the feature appears in real Spark SQL, densest-
first: projection -> predicates -> joins -> group/aggregate -> order/limit ->
conditional/null -> scalar functions/operators -> subqueries -> CTEs -> window ->
set ops -> grouping extensions -> complex types / lateral view -> table expressions
-> advanced predicates -> pivot/unpivot -> typed literals & intervals -> numeric
tower.

Scope
-----
Query (relation) statements only — every case returns a result set the differential
oracle can diff. DDL/DML **commands** (CREATE/INSERT/MERGE/...) go through the
command arm and the state-diff oracle (ADR-011) and are a *separate* corpus, out of
scope here.

How to use it (differential oracle, ADR-015)
--------------------------------------------
    build_inputs(spark)                 # registers the temp views
    for c in CASES:
        df = spark.sql(c.sql)
        ref_schema = df.schema          # (a) schema diff (also AnalyzePlan)
        ref_rows   = df.collect()       # (b) result diff (after harness canonicalization)
    ...repeat against the thunderduck session and diff.

Flags
-----
  "nondeterministic" : value differs run-to-run (current_date/timestamp, rand, ...).
  "schema_only"      : compare schema only (unordered collect_*, distribution-only
                       ops, and the numeric-tower result-type cases).
  "spark4"           : relies on Spark 3.4/4.x grammar whose availability on the pin
                       (4.1.1) should be smoke-tested first: `::` cast, GROUP BY ALL,
                       ORDER BY ALL, lateral column alias, UNPIVOT, recursive CTE,
                       aggregate FILTER clause, two-arg ceil/floor, try_* functions.

CAUTION
-------
Validated as Python + unique ids only. The SQL itself is NOT parser-checked in this
environment (no live Spark; consistent with the no-JVM premise). `run()` catches
per-case analysis errors, so a first pass against reference Spark 4.1.1 immediately
flags any `spark4`/grammar case that does not parse on the pin — quarantine, don't
assume.
"""

from __future__ import annotations

import datetime
from dataclasses import dataclass
from decimal import Decimal
from typing import Dict, List, Tuple

from pyspark.sql import DataFrame, SparkSession
from pyspark.sql.types import (
    ArrayType, BooleanType, DateType, DecimalType, DoubleType, FloatType,
    IntegerType, LongType, MapType, ShortType, StringType, StructField,
    StructType, TimestampType,
)

NAN = float("nan")


# ---------------------------------------------------------------------------
# Inputs — registered as temp views.
# ---------------------------------------------------------------------------

def build_inputs(spark: SparkSession) -> Dict[str, DataFrame]:
    emp_schema = StructType([
        StructField("id", LongType(), False),
        StructField("name", StringType(), True),
        StructField("dept_id", IntegerType(), True),
        StructField("manager_id", LongType(), True),
        StructField("age", IntegerType(), True),
        StructField("salary", DoubleType(), True),
        StructField("bonus", DecimalType(9, 2), True),
        StructField("hire_date", DateType(), True),
        StructField("last_login", TimestampType(), True),
        StructField("active", BooleanType(), True),
        StructField("score", DoubleType(), True),
        StructField("tags", ArrayType(StringType()), True),
        StructField("attrs", MapType(StringType(), StringType()), True),
        StructField("address", StructType([
            StructField("city", StringType(), True),
            StructField("zip", StringType(), True),
            StructField("geo", StructType([
                StructField("lat", DoubleType(), True),
                StructField("lng", DoubleType(), True),
            ]), True),
        ]), True),
    ])

    def d(s):
        return datetime.date.fromisoformat(s)

    def ts(s):
        return datetime.datetime.fromisoformat(s)

    emp_rows = [
        (1, "Alice",   10,   None, 34, 95000.0, Decimal("1500.50"), d("2018-03-01"), ts("2026-06-01T09:15:00"), True,  0.91, ["python", "rust"],   {"team": "infra", "tz": "UTC"},   ("Vienna",  "1010", (48.20, 16.37))),
        (2, "Bob",     10,   1,    41, 120000.0, Decimal("3000.00"), d("2015-07-12"), ts("2026-06-02T18:40:00"), True,  NAN,  ["scala"],            {"team": "infra"},                ("Berlin",  "10115", (52.52, 13.40))),
        (3, "Carol",   20,   1,    29, 78000.0,  None,               d("2021-01-20"), None,                       False, 0.55, [],                   {},                               ("Vienna",  None,   (48.21, 16.36))),
        (4, "Dan",     20,   3,    52, 64000.0,  Decimal("500.00"),  d("2009-11-05"), ts("2026-05-30T07:00:00"), True,  None, None,                 None,                              (None,      "20095", None)),
        (5, "Eve",     None, 3,    38, 88000.0,  Decimal("750.25"),  d("2020-06-30"), ts("2026-06-03T12:00:00"), True,  0.10, ["go", "c", "rust"],  {"team": "data", "tz": "CET"},    ("Munich",  "80331", (48.14, 11.58))),
        (6, "Frank",   30,   2,    45, 105000.0, Decimal("2200.00"), d("2012-02-14"), ts("2026-06-01T22:05:00"), False, 0.73, ["java"],             {"team": "ml"},                   ("Zurich",  "8001", (47.37, 8.54))),
        (7, "Grace",   30,   6,    27, 72000.0,  Decimal("0.00"),    d("2023-09-01"), ts("2026-06-02T03:30:00"), True,  NAN,  ["python"],           {"team": "ml", "tz": "UTC"},      ("Vienna",  "1020", (48.22, 16.40))),
        (8, "Heidi",   None, None, 60, 150000.0, Decimal("9999.99"), d("2001-04-23"), None,                       True,  1.00, None,                 {"team": None},                   (None,      None,   None)),
    ]
    emp = spark.createDataFrame(emp_rows, emp_schema)

    dept_schema = StructType([
        StructField("dept_id", IntegerType(), False),
        StructField("dept_name", StringType(), True),
        StructField("budget", DecimalType(12, 2), True),
        StructField("location", StringType(), True),
        StructField("country", StringType(), True),
    ])
    dept_rows = [
        (10, "Infrastructure", Decimal("2500000.00"), "Vienna", "AT"),
        (20, "Data",           Decimal("1800000.00"), "Vienna", "AT"),
        (30, "ML",             Decimal("3200000.00"), "Zurich", "CH"),
        (40, "Security",       None,                  "Berlin", "DE"),
    ]
    dept = spark.createDataFrame(dept_rows, dept_schema)

    emp2_schema = StructType([
        StructField("id", LongType(), False),
        StructField("name", StringType(), True),
        StructField("dept_id", IntegerType(), True),
        StructField("age", IntegerType(), True),
        StructField("salary", DoubleType(), True),
        StructField("country", StringType(), True),
    ])
    emp2_rows = [
        (101, "Ivan",  10, 33, 91000.0, "AT"),
        (102, "Judy",  40, 47, 99000.0, "DE"),
        (103, "Mallory", None, 25, 60000.0, None),
    ]
    emp2 = spark.createDataFrame(emp2_rows, emp2_schema)

    nums_schema = StructType([
        StructField("a", IntegerType(), True),
        StructField("b", IntegerType(), True),          # contains 0 and null: divide-by-zero / null
        StructField("x", DoubleType(), True),
        StructField("y", DoubleType(), True),
        StructField("f", FloatType(), True),            # 32-bit float: distinct result-type from double
        StructField("d1", DecimalType(10, 2), True),
        StructField("d2", DecimalType(6, 3), True),
        StructField("d3", DecimalType(38, 6), True),    # high-precision: sum/avg widening + 38-digit boundary
        StructField("lng", LongType(), True),
        StructField("sml", ShortType(), True),          # smallint: promotion + ceil/floor typing
    ])
    nums_rows = [
        (7,   2,    3.5,  2.0,  3.5,   Decimal("100.25"),  Decimal("3.142"),  Decimal("100.250000"),  9000000000, 7),
        (10,  0,   -1.0,  0.0,  -1.0,  Decimal("0.00"),    Decimal("0.001"),  Decimal("0.000000"),    1,          -10),
        (-3,  5,    NAN,  4.0,   NAN,  Decimal("9999.99"), Decimal("12.500"), Decimal("9999.990000"), -42,        32000),
        (0,   None, 2.0,  None,  2.0,  None,               Decimal("1.000"),  None,                   None,       None),
    ]
    nums = spark.createDataFrame(nums_rows, nums_schema)

    raw_schema = StructType([
        StructField("id", LongType(), False),
        StructField("json_str", StringType(), True),
        StructField("csv_str", StringType(), True),
        StructField("url", StringType(), True),
        StructField("num_str", StringType(), True),
    ])
    raw_rows = [
        (1, '{"a": 1, "b": ["x", "y"], "c": {"d": true}, "e": 2.5}', "10,foo,2.5", "https://user:pw@host.example.com:8080/a/b?q=1&r=2#frag", "1,234.56"),
        (2, '{"a": 2, "b": [], "c": null, "e": -7}',                 "20,bar,",    "http://host.example.com/p",                              "(9.99)"),
        (3, None,                                                     None,         None,                                                     None),
    ]
    raw = spark.createDataFrame(raw_rows, raw_schema)

    views = {"emp": emp, "dept": dept, "emp2": emp2, "nums": nums, "raw": raw}
    for name, df in views.items():
        df.createOrReplaceTempView(name)
    return views


# ---------------------------------------------------------------------------
# Case registry
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Case:
    id: str
    category: str
    description: str
    sql: str
    flags: Tuple[str, ...] = ()


CASES: List[Case] = []


def case(id, category, description, sql, flags=()):
    CASES.append(Case(id, category, description, sql.strip(), tuple(flags)))


# ===========================================================================
# Ordered MOST-FREQUENT-FIRST.
# ===========================================================================

# ── 1. SELECT / projection (most fundamental) ───────────────────────────────
case("sel-001", "select", "select column list", "SELECT id, name, age FROM emp")
case("sel-002", "select", "select star", "SELECT * FROM emp")
case("sel-003", "select", "qualified star", "SELECT emp.* FROM emp")
case("sel-004", "select", "expression with alias", "SELECT salary * 1.1 AS raised FROM emp")
case("sel-005", "select", "expression without AS keyword", "SELECT salary * 1.1 raised FROM emp")
case("sel-006", "select", "SELECT DISTINCT", "SELECT DISTINCT dept_id, active FROM emp")
case("sel-007", "select", "constant select (no table)", "SELECT 1 AS one, 'x' AS s, true AS b")
case("sel-008", "select", "arithmetic across columns", "SELECT id, age + 1, salary / 1000 FROM emp")
case("sel-009", "select", "backtick-quoted alias", "SELECT name AS `full name` FROM emp")
case("sel-010", "select", "nested expression", "SELECT (age + 1) * 2 - dept_id AS expr FROM emp")
case("sel-011", "select", "select with column twice", "SELECT id, id AS id2 FROM emp")
case("sel-012", "select", "boolean expression as column", "SELECT name, salary > 100000 AS is_high FROM emp")
case("sel-013", "select", "qualified column references", "SELECT emp.id, emp.name FROM emp")
case("sel-014", "select", "select struct field path", "SELECT id, address.city FROM emp")
case("sel-015", "select", "select with table alias", "SELECT e.id, e.name FROM emp AS e")
case("sel-016", "select", "computed columns with function + alias", "SELECT id, age + 1 AS age1, upper(name) AS up FROM emp")

# ── 2. WHERE / predicates ────────────────────────────────────────────────────
case("whr-001", "predicate", "equality", "SELECT * FROM emp WHERE dept_id = 10")
case("whr-002", "predicate", "inequality <>", "SELECT * FROM emp WHERE dept_id <> 10")
case("whr-003", "predicate", "comparison >", "SELECT * FROM emp WHERE age > 30")
case("whr-004", "predicate", "AND / OR / NOT", "SELECT * FROM emp WHERE (age > 30 AND active) OR NOT active")
case("whr-005", "predicate", "IN list", "SELECT * FROM emp WHERE dept_id IN (10, 20)")
case("whr-006", "predicate", "NOT IN list", "SELECT * FROM emp WHERE dept_id NOT IN (10, 20)")
case("whr-007", "predicate", "BETWEEN", "SELECT * FROM emp WHERE age BETWEEN 30 AND 45")
case("whr-008", "predicate", "IS NULL", "SELECT * FROM emp WHERE dept_id IS NULL")
case("whr-009", "predicate", "IS NOT NULL", "SELECT * FROM emp WHERE bonus IS NOT NULL")
case("whr-010", "predicate", "LIKE", "SELECT * FROM emp WHERE name LIKE 'A%'")
case("whr-011", "predicate", "NOT LIKE", "SELECT * FROM emp WHERE name NOT LIKE 'A%'")
case("whr-012", "predicate", "ILIKE (case-insensitive)", "SELECT * FROM emp WHERE name ILIKE 'a%'")
case("whr-013", "predicate", "RLIKE / REGEXP", "SELECT * FROM emp WHERE name RLIKE '^[A-D]'")
case("whr-014", "predicate", "predicate on expression", "SELECT * FROM emp WHERE age - 18 > 10")
case("whr-015", "predicate", "null-safe equality <=>", "SELECT * FROM emp WHERE dept_id <=> NULL")
case("whr-016", "predicate", "parenthesized AND/OR/NOT predicate", "SELECT * FROM emp WHERE (age > 30 AND active) OR (NOT active)")

# ── 3. Joins ─────────────────────────────────────────────────────────────────
case("jn-001", "join", "INNER JOIN ON", "SELECT e.name, d.dept_name FROM emp e JOIN dept d ON e.dept_id = d.dept_id")
case("jn-002", "join", "implicit join (comma + WHERE)", "SELECT e.name, d.dept_name FROM emp e, dept d WHERE e.dept_id = d.dept_id")
case("jn-003", "join", "LEFT OUTER JOIN", "SELECT e.name, d.dept_name FROM emp e LEFT JOIN dept d ON e.dept_id = d.dept_id")
case("jn-004", "join", "RIGHT OUTER JOIN", "SELECT e.name, d.dept_name FROM emp e RIGHT JOIN dept d ON e.dept_id = d.dept_id")
case("jn-005", "join", "FULL OUTER JOIN", "SELECT e.name, d.dept_name FROM emp e FULL OUTER JOIN dept d ON e.dept_id = d.dept_id")
case("jn-006", "join", "CROSS JOIN", "SELECT e.id, d.dept_id FROM emp e CROSS JOIN dept d")
case("jn-007", "join", "JOIN USING", "SELECT name, dept_name FROM emp JOIN dept USING (dept_id)")
case("jn-008", "join", "NATURAL JOIN", "SELECT * FROM emp NATURAL JOIN dept")
case("jn-009", "join", "LEFT SEMI JOIN", "SELECT e.* FROM emp e LEFT SEMI JOIN dept d ON e.dept_id = d.dept_id")
case("jn-010", "join", "LEFT ANTI JOIN", "SELECT e.* FROM emp e LEFT ANTI JOIN dept d ON e.dept_id = d.dept_id")
case("jn-011", "join", "multi-condition join", "SELECT e.name FROM emp e JOIN dept d ON e.dept_id = d.dept_id AND e.salary > 70000")
case("jn-012", "join", "non-equi join", "SELECT e.name, d.dept_name FROM emp e JOIN dept d ON e.salary > d.budget / 100")
case("jn-013", "join", "three-way join", "SELECT e.name, d.dept_name FROM emp e JOIN dept d ON e.dept_id = d.dept_id JOIN emp2 m ON d.country = m.country")
case("jn-014", "join", "self join on manager", "SELECT e.name AS emp, m.name AS mgr FROM emp e LEFT JOIN emp m ON e.manager_id = m.id")
case("jn-015", "join", "join then aggregate", "SELECT d.dept_name, avg(e.salary) AS avg_sal FROM emp e JOIN dept d ON e.dept_id = d.dept_id GROUP BY d.dept_name")
case("jn-016", "join", "USING with subsequent unqualified col", "SELECT dept_id, count(*) AS n FROM emp JOIN dept USING (dept_id) GROUP BY dept_id")

# ── 4. GROUP BY / aggregates ─────────────────────────────────────────────────
case("agg-001", "aggregate", "COUNT(*)", "SELECT count(*) AS n FROM emp")
case("agg-002", "aggregate", "COUNT(col) ignores nulls", "SELECT count(bonus) AS n FROM emp")
case("agg-003", "aggregate", "COUNT(DISTINCT)", "SELECT count(DISTINCT dept_id) AS nd FROM emp")
case("agg-004", "aggregate", "SUM / AVG / MIN / MAX", "SELECT sum(salary) s, avg(age) a, min(salary) mn, max(salary) mx FROM emp")
case("agg-005", "aggregate", "GROUP BY single key", "SELECT dept_id, avg(salary) AS avg_sal FROM emp GROUP BY dept_id")
case("agg-006", "aggregate", "GROUP BY multiple keys", "SELECT dept_id, active, count(*) AS n FROM emp GROUP BY dept_id, active")
case("agg-007", "aggregate", "GROUP BY expression", "SELECT age >= 40 AS senior, avg(salary) AS s FROM emp GROUP BY age >= 40")
case("agg-008", "aggregate", "GROUP BY ordinal", "SELECT dept_id, count(*) FROM emp GROUP BY 1")
case("agg-009", "aggregate", "GROUP BY ALL (spark4)", "SELECT dept_id, active, count(*) FROM emp GROUP BY ALL", flags=("spark4",))
case("agg-010", "aggregate", "HAVING on aggregate", "SELECT dept_id, avg(salary) AS s FROM emp GROUP BY dept_id HAVING avg(salary) > 80000")
case("agg-011", "aggregate", "HAVING with count", "SELECT dept_id, count(*) AS n FROM emp GROUP BY dept_id HAVING count(*) > 1")
case("agg-012", "aggregate", "multiple aggs + aliases", "SELECT dept_id, count(*) n, avg(age) avg_age, max(salary) max_sal FROM emp GROUP BY dept_id")
case("agg-013", "aggregate", "STDDEV / VARIANCE", "SELECT stddev(salary) sd, variance(salary) v FROM emp")
case("agg-014", "aggregate", "aggregate over arithmetic expr", "SELECT dept_id, sum(salary + coalesce(cast(bonus AS double), 0)) AS total FROM emp GROUP BY dept_id")
case("agg-015", "aggregate", "COUNT_IF / conditional count", "SELECT dept_id, count_if(active) AS n_active FROM emp GROUP BY dept_id")
case("agg-016", "aggregate", "sum(CASE WHEN ...) idiom", "SELECT dept_id, sum(CASE WHEN active THEN 1 ELSE 0 END) AS n_active FROM emp GROUP BY dept_id")
case("agg-017", "aggregate", "aggregate FILTER (WHERE) clause (spark4)", "SELECT dept_id, count(*) FILTER (WHERE salary > 90000) AS n_high FROM emp GROUP BY dept_id", flags=("spark4",))
case("agg-018", "aggregate", "collect_list / collect_set", "SELECT dept_id, collect_list(name) names FROM emp GROUP BY dept_id", flags=("schema_only",))
case("agg-019", "aggregate", "percentile / median", "SELECT percentile(salary, 0.5) AS p50, median(salary) AS med FROM emp")
case("agg-020", "aggregate", "any / every boolean aggregates", "SELECT dept_id, any(active) any_a, every(active) all_a FROM emp GROUP BY dept_id")
# agg-021: pass-3 fix — `expr_has_aggregate` (v2_lowering.rs) used to check only
# whether a projection item's OWN top-level call name was an aggregate, so
# `abs(count(*))` (aggregate nested inside a non-aggregate call's args) was
# misclassified as non-aggregate. That misclassification fed `GROUP BY ALL`'s
# non-aggregate-column inference, which would incorrectly try to group by
# `abs(count(*))` itself. `function_call_has_aggregate` now descends into call
# args (and window PARTITION BY / ORDER BY), so GROUP BY ALL correctly groups
# only by `dept_id`.
case("agg-021", "aggregate", "GROUP BY ALL excludes aggregate nested in fn args (spark4)", "SELECT dept_id, abs(count(*)) n FROM emp GROUP BY ALL", flags=("spark4",))
# agg-022 / agg-023: KNOWN-RED witnesses (NOT a pass-3 fix — out of findings
# 1-6). `function_call_has_aggregate` (v2_lowering.rs, added by the agg-021 fix)
# walks sqlparser's `Expr` tree, but SQL *special-form* syntax parses to
# dedicated `Expr` variants — `EXTRACT(f FROM x)` -> `Expr::Extract`,
# `SUBSTRING(s FROM p FOR n)` -> `Expr::Substring` — for which the aggregate
# walker has no arms. An aggregate nested inside such a special form is missed,
# so under `GROUP BY ALL` it is not excluded from the grouping keys and τ tries
# to GROUP BY an expression that contains an aggregate -> DuckDB error. Spark
# groups only by `dept_id`. Fails until the walker covers the special-form
# `Expr` shapes (same bug class as agg-021, different syntax).
case("agg-022", "aggregate", "aggregate nested in EXTRACT special form under GROUP BY ALL (known gap)", "SELECT dept_id, extract(YEAR FROM max(last_login)) y FROM emp GROUP BY ALL", flags=("spark4",))
case("agg-023", "aggregate", "aggregate nested in SUBSTRING special form under GROUP BY ALL (known gap)", "SELECT dept_id, substring(max(name) FROM 1 FOR 2) s FROM emp GROUP BY ALL", flags=("spark4",))

# ── 5. ORDER BY / LIMIT ──────────────────────────────────────────────────────
case("ord-001", "ordering", "ORDER BY asc (default)", "SELECT * FROM emp ORDER BY salary")
case("ord-002", "ordering", "ORDER BY DESC", "SELECT * FROM emp ORDER BY salary DESC")
case("ord-003", "ordering", "NULLS FIRST / LAST", "SELECT * FROM emp ORDER BY dept_id ASC NULLS FIRST, bonus DESC NULLS LAST")
case("ord-004", "ordering", "multiple sort keys", "SELECT * FROM emp ORDER BY dept_id, salary DESC")
case("ord-005", "ordering", "ORDER BY ordinal", "SELECT name, salary FROM emp ORDER BY 2 DESC")
case("ord-006", "ordering", "ORDER BY expression", "SELECT * FROM emp ORDER BY age * salary DESC")
case("ord-007", "ordering", "ORDER BY ALL (spark4)", "SELECT dept_id, active FROM emp ORDER BY ALL", flags=("spark4",))
case("ord-008", "ordering", "LIMIT", "SELECT * FROM emp ORDER BY id LIMIT 3")
case("ord-009", "ordering", "LIMIT with OFFSET", "SELECT * FROM emp ORDER BY id LIMIT 3 OFFSET 2", flags=("spark4",))
case("ord-010", "ordering", "OFFSET only", "SELECT * FROM emp ORDER BY id OFFSET 2", flags=("spark4",))
case("ord-011", "ordering", "SORT BY (Spark per-partition)", "SELECT * FROM emp SORT BY salary DESC", flags=("schema_only",))
case("ord-012", "ordering", "DISTRIBUTE BY + SORT BY / CLUSTER BY", "SELECT * FROM emp CLUSTER BY dept_id", flags=("schema_only",))

# ── 6. Conditional / null handling ───────────────────────────────────────────
case("cnd-001", "conditional", "searched CASE WHEN", "SELECT name, CASE WHEN age >= 40 THEN 'senior' ELSE 'junior' END AS band FROM emp")
case("cnd-002", "conditional", "simple CASE (expr WHEN val)", "SELECT dept_id, CASE dept_id WHEN 10 THEN 'infra' WHEN 20 THEN 'data' ELSE 'other' END AS nm FROM emp")
case("cnd-003", "conditional", "chained WHEN branches", "SELECT CASE WHEN age < 30 THEN 'a' WHEN age < 45 THEN 'b' ELSE 'c' END AS band FROM emp")
case("cnd-004", "conditional", "CASE without ELSE (-> nullable)", "SELECT CASE WHEN active THEN salary END AS maybe_sal FROM emp")
case("cnd-005", "conditional", "COALESCE", "SELECT coalesce(dept_id, -1) AS dept_or FROM emp")
case("cnd-006", "conditional", "NULLIF", "SELECT nullif(age, 60) AS age_or_null FROM emp")
case("cnd-007", "conditional", "NVL / NVL2", "SELECT nvl(dept_id, -1) AS a, nvl2(bonus, 'has', 'none') AS b FROM emp")
case("cnd-008", "conditional", "IFNULL", "SELECT ifnull(manager_id, 0) AS mgr FROM emp")
case("cnd-009", "conditional", "IF(cond, a, b)", "SELECT if(salary > 100000, 'high', 'low') AS band FROM emp")
case("cnd-010", "conditional", "CASE inside aggregate", "SELECT avg(CASE WHEN active THEN salary END) AS avg_active_sal FROM emp")
case("cnd-011", "conditional", "nested CASE", "SELECT CASE WHEN active THEN CASE WHEN age > 40 THEN 'senior-active' ELSE 'active' END ELSE 'inactive' END AS s FROM emp")
case("cnd-012", "conditional", "COALESCE chain", "SELECT coalesce(bonus, cast(score AS decimal(9,2)), 0) AS c FROM emp")

# ── 7. Scalar functions & SQL-syntax operators ───────────────────────────────
case("fn-001", "scalar_fn", "string concat || operator", "SELECT name || '@' || cast(dept_id AS string) AS handle FROM emp")
case("fn-002", "scalar_fn", "CONCAT / CONCAT_WS", "SELECT concat(name, '-', cast(dept_id AS string)) c1, concat_ws('|', name, name) c2 FROM emp")
case("fn-003", "scalar_fn", "SUBSTRING(s FROM p FOR n) SQL syntax", "SELECT substring(name FROM 1 FOR 2) AS ss FROM emp")
case("fn-004", "scalar_fn", "substr function form", "SELECT substr(name, 2, 3) AS ss FROM emp")
case("fn-005", "scalar_fn", "TRIM(BOTH 'x' FROM s)", "SELECT trim(BOTH 'A' FROM name) AS t FROM emp")
case("fn-006", "scalar_fn", "POSITION(sub IN s)", "SELECT position('a' IN name) AS p FROM emp")
case("fn-007", "scalar_fn", "OVERLAY(... PLACING ... FROM ... FOR ...)", "SELECT overlay(name PLACING 'XX' FROM 1 FOR 2) AS ov FROM emp")
case("fn-008", "scalar_fn", "UPPER / LOWER / LENGTH", "SELECT upper(name) u, lower(name) l, length(name) ln FROM emp")
case("fn-009", "scalar_fn", "regexp_replace / regexp_extract", "SELECT regexp_replace(name, '[aeiou]', '_') r, regexp_extract(name, '^(.)', 1) f FROM emp")
case("fn-010", "scalar_fn", "CAST(... AS ...)", "SELECT cast(salary AS int) si, cast(age AS double) ad FROM emp")
case("fn-011", "scalar_fn", ":: cast operator (spark4)", "SELECT salary::int AS si, age::double AS ad FROM emp", flags=("spark4",))
case("fn-012", "scalar_fn", "TRY_CAST (spark4)", "SELECT try_cast(name AS int) AS tc FROM emp", flags=("spark4",))
case("fn-013", "scalar_fn", "EXTRACT(field FROM source)", "SELECT extract(YEAR FROM hire_date) y, extract(MONTH FROM hire_date) m FROM emp")
case("fn-014", "scalar_fn", "date_add / datediff", "SELECT date_add(hire_date, 30) plus, datediff(DATE '2026-06-30', hire_date) days FROM emp")
case("fn-015", "scalar_fn", "year / month / dayofmonth", "SELECT year(hire_date) y, month(hire_date) m, dayofmonth(hire_date) d FROM emp")
case("fn-016", "scalar_fn", "to_date / date_format", "SELECT to_date('15/01/2026', 'dd/MM/yyyy') td, date_format(hire_date, 'yyyy/MM') df FROM emp")
case("fn-017", "scalar_fn", "round / abs / ceil / floor", "SELECT round(x, 1) r, abs(a) ab, ceil(x) c, floor(x) f FROM nums")
case("fn-018", "scalar_fn", "int/int division -> double", "SELECT a / b AS dv FROM nums")
case("fn-019", "scalar_fn", "current_date / current_timestamp", "SELECT current_date() cd, current_timestamp() ct FROM emp", flags=("nondeterministic",))
case("fn-020", "scalar_fn", "binary literal X'..' + hex", "SELECT hex(X'1F2A') AS h, cast(X'41' AS string) AS s FROM emp")

# ── 8. Subqueries (the headline SQL-only family; ADR-008) ────────────────────
case("sq-001", "subquery", "scalar subquery in SELECT (uncorrelated)", "SELECT name, salary, (SELECT max(salary) FROM emp) AS gmax FROM emp")
case("sq-002", "subquery", "scalar subquery in WHERE (uncorrelated)", "SELECT * FROM emp WHERE salary > (SELECT avg(salary) FROM emp)")
case("sq-003", "subquery", "correlated scalar in SELECT (dept avg)", "SELECT e.name, (SELECT avg(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id) AS dept_avg FROM emp e")
case("sq-004", "subquery", "correlated scalar in WHERE", "SELECT * FROM emp e WHERE e.salary > (SELECT avg(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id)")
case("sq-005", "subquery", "EXISTS (uncorrelated)", "SELECT * FROM emp WHERE EXISTS (SELECT 1 FROM dept)")
case("sq-006", "subquery", "correlated EXISTS", "SELECT * FROM emp e WHERE EXISTS (SELECT 1 FROM dept d WHERE d.dept_id = e.dept_id)")
case("sq-007", "subquery", "correlated NOT EXISTS (orphans)", "SELECT * FROM emp e WHERE NOT EXISTS (SELECT 1 FROM dept d WHERE d.dept_id = e.dept_id)")
case("sq-008", "subquery", "IN (subquery)", "SELECT * FROM emp WHERE dept_id IN (SELECT dept_id FROM dept WHERE country = 'AT')")
case("sq-009", "subquery", "NOT IN with NULL (3VL trap)", "SELECT * FROM emp WHERE dept_id NOT IN (SELECT dept_id FROM dept WHERE budget IS NULL OR dept_id = 40)")
case("sq-010", "subquery", "correlated IN with predicate", "SELECT * FROM emp e WHERE e.dept_id IN (SELECT d.dept_id FROM dept d WHERE d.budget > e.salary)")
case("sq-011", "subquery", "> ALL", "SELECT * FROM emp WHERE salary > ALL (SELECT salary FROM emp WHERE dept_id = 20)")
case("sq-012", "subquery", "> ANY / SOME", "SELECT * FROM emp WHERE salary > ANY (SELECT salary FROM emp WHERE dept_id = 30)")
case("sq-013", "subquery", "= ANY (membership)", "SELECT * FROM emp WHERE dept_id = ANY (SELECT dept_id FROM dept WHERE country = 'CH')")
case("sq-014", "subquery", "derived table (subquery in FROM)", "SELECT dept_id, n FROM (SELECT dept_id, count(*) AS n FROM emp GROUP BY dept_id) t WHERE n > 1")
case("sq-015", "subquery", "correlated subquery in HAVING", "SELECT e.dept_id, count(*) AS n FROM emp e GROUP BY e.dept_id HAVING count(*) > (SELECT avg(cnt) FROM (SELECT count(*) AS cnt FROM emp GROUP BY dept_id))")
case("sq-016", "subquery", "nested correlated (2 levels)", "SELECT e.name FROM emp e WHERE e.salary > (SELECT avg(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id AND e2.age > (SELECT min(e3.age) FROM emp e3 WHERE e3.dept_id <=> e.dept_id))")
case("sq-017", "subquery", "scalar subquery -> NULL when no match", "SELECT d.dept_name, (SELECT max(e.salary) FROM emp e WHERE e.dept_id = d.dept_id) AS top_sal FROM dept d")
case("sq-018", "subquery", "subquery inside COALESCE", "SELECT e.name, coalesce((SELECT d.dept_name FROM dept d WHERE d.dept_id = e.dept_id), 'UNASSIGNED') AS dept FROM emp e")
case("sq-019", "subquery", "multiple correlated subqueries", "SELECT e.name, (SELECT min(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id) AS lo, (SELECT max(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id) AS hi FROM emp e")
case("sq-020", "subquery", "subquery in CASE branch", "SELECT name, CASE WHEN salary > (SELECT avg(salary) FROM emp) THEN 'above' ELSE 'below' END AS rel FROM emp")
case("sq-021", "subquery", "IN (group HAVING) -> semi-join (TPC-H Q18 shape)", "SELECT * FROM emp WHERE dept_id IN (SELECT dept_id FROM emp GROUP BY dept_id HAVING sum(salary) > 200000)")
case("sq-022", "subquery", "de-correlatable avg (TPC-H Q17 shape)", "SELECT sum(e.salary) / 7.0 AS avg_yearly FROM emp e WHERE e.salary < 0.2 * (SELECT avg(e2.salary) FROM emp e2 WHERE e2.dept_id <=> e.dept_id)")

# ── 9. CTEs (WITH) ───────────────────────────────────────────────────────────
case("cte-001", "cte", "single CTE", "WITH ds AS (SELECT dept_id, avg(salary) a FROM emp GROUP BY dept_id) SELECT e.name, s.a FROM emp e JOIN ds s ON e.dept_id = s.dept_id")
case("cte-002", "cte", "multiple CTEs", "WITH a AS (SELECT dept_id, count(*) c FROM emp GROUP BY dept_id), b AS (SELECT dept_id, avg(salary) s FROM emp GROUP BY dept_id) SELECT a.dept_id, a.c, b.s FROM a JOIN b USING (dept_id)")
case("cte-003", "cte", "CTE referenced twice", "WITH e AS (SELECT id, name, manager_id FROM emp) SELECT emp.name AS emp, mgr.name AS mgr FROM e emp LEFT JOIN e mgr ON emp.manager_id = mgr.id")
case("cte-004", "cte", "nested CTE (CTE references CTE)", "WITH a AS (SELECT dept_id, salary FROM emp), b AS (SELECT dept_id, avg(salary) s FROM a GROUP BY dept_id) SELECT * FROM b")
case("cte-005", "cte", "CTE with explicit column list", "WITH t(k, v) AS (SELECT dept_id, count(*) FROM emp GROUP BY dept_id) SELECT k, v FROM t")
case("cte-006", "cte", "CTE feeding a subquery", "SELECT * FROM emp e WHERE e.dept_id IN (WITH big AS (SELECT dept_id FROM dept WHERE budget > 2000000) SELECT dept_id FROM big)")
case("cte-007", "cte", "CTE + window", "WITH ranked AS (SELECT name, dept_id, salary, row_number() OVER (PARTITION BY dept_id ORDER BY salary DESC) rn FROM emp) SELECT * FROM ranked WHERE rn = 1")
case("cte-008", "cte", "two independent CTEs unioned", "WITH a AS (SELECT id FROM emp WHERE active), b AS (SELECT id FROM emp WHERE NOT active) SELECT * FROM a UNION ALL SELECT * FROM b")
case("cte-009", "cte", "recursive CTE (verify on pin)", "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5) SELECT * FROM seq", flags=("spark4",))
case("cte-010", "cte", "recursive CTE org hierarchy (verify)", "WITH RECURSIVE chain(id, name, manager_id, lvl) AS (SELECT id, name, manager_id, 0 FROM emp WHERE manager_id IS NULL UNION ALL SELECT e.id, e.name, e.manager_id, c.lvl + 1 FROM emp e JOIN chain c ON e.manager_id = c.id) SELECT * FROM chain", flags=("spark4",))

# ── 10. Window functions ─────────────────────────────────────────────────────
case("win-001", "window", "ROW_NUMBER", "SELECT name, dept_id, row_number() OVER (PARTITION BY dept_id ORDER BY salary DESC) AS rn FROM emp")
case("win-002", "window", "RANK / DENSE_RANK", "SELECT name, rank() OVER (ORDER BY salary) rk, dense_rank() OVER (ORDER BY salary) drk FROM emp")
case("win-003", "window", "PERCENT_RANK / CUME_DIST", "SELECT name, percent_rank() OVER (ORDER BY salary) pr, cume_dist() OVER (ORDER BY salary) cd FROM emp")
case("win-004", "window", "NTILE", "SELECT name, ntile(4) OVER (ORDER BY salary) AS q FROM emp")
case("win-005", "window", "LAG / LEAD with default", "SELECT name, lag(salary, 1, 0.0) OVER (PARTITION BY dept_id ORDER BY hire_date) prev, lead(salary) OVER (PARTITION BY dept_id ORDER BY hire_date) nxt FROM emp")
case("win-006", "window", "FIRST_VALUE / LAST_VALUE", "SELECT name, first_value(name) OVER (PARTITION BY dept_id ORDER BY salary) fv FROM emp")
case("win-007", "window", "NTH_VALUE", "SELECT name, nth_value(salary, 2) OVER (PARTITION BY dept_id ORDER BY salary) AS second FROM emp")
case("win-008", "window", "running SUM (ROWS UNBOUNDED..CURRENT)", "SELECT name, sum(salary) OVER (PARTITION BY dept_id ORDER BY hire_date ROWS BETWEEN UNBOUNDED PRECEDING AND CURRENT ROW) AS run FROM emp")
case("win-009", "window", "moving AVG (ROWS -1..1)", "SELECT name, avg(salary) OVER (PARTITION BY dept_id ORDER BY hire_date ROWS BETWEEN 1 PRECEDING AND 1 FOLLOWING) AS ma FROM emp")
case("win-010", "window", "RANGE BETWEEN", "SELECT name, sum(salary) OVER (PARTITION BY dept_id ORDER BY age RANGE BETWEEN 5 PRECEDING AND 5 FOLLOWING) AS rb FROM emp")
case("win-011", "window", "aggregate over partition (no order)", "SELECT name, avg(salary) OVER (PARTITION BY dept_id) AS dept_avg FROM emp")
case("win-012", "window", "named WINDOW clause", "SELECT name, rank() OVER w rk, sum(salary) OVER w s FROM emp WINDOW w AS (PARTITION BY dept_id ORDER BY salary)")
case("win-013", "window", "RANGE BETWEEN INTERVAL (time frame)", "SELECT id, last_login, count(*) OVER (ORDER BY last_login RANGE BETWEEN INTERVAL 1 DAY PRECEDING AND CURRENT ROW) AS w FROM emp WHERE last_login IS NOT NULL")
case("win-014", "window", "top-per-group via window + outer filter", "SELECT * FROM (SELECT name, dept_id, salary, row_number() OVER (PARTITION BY dept_id ORDER BY salary DESC) rn FROM emp) WHERE rn = 1")
case("win-015", "window", "multiple windows different partitions", "SELECT name, rank() OVER (PARTITION BY dept_id ORDER BY salary) r1, rank() OVER (PARTITION BY active ORDER BY age) r2 FROM emp")
case("win-016", "window", "window over expression", "SELECT name, sum(salary + coalesce(cast(bonus AS double),0)) OVER (PARTITION BY dept_id) AS comp_sum FROM emp")
# win-017: pass-3 fix — `resolve_named_windows_in_expr` (v2_lowering.rs) only
# descended through Function/Nested/UnaryOp/Cast/BinaryOp, so a named-window
# reference nested inside a CASE branch fell to the `_ => {}` no-op arm and
# stayed an unresolved `WindowType::NamedWindow`, surfacing a spurious "not
# defined in WINDOW clause" boundary error even though `w` IS defined. The
# widened walk now descends into CASE (and BETWEEN, InList, IS NULL, etc.).
case("win-017", "window", "named WINDOW clause referenced inside CASE branch", "SELECT name, CASE WHEN active THEN rank() OVER w ELSE NULL END AS rk FROM emp WINDOW w AS (PARTITION BY dept_id ORDER BY salary)")

# ── 11. Set operations ───────────────────────────────────────────────────────
case("set-001", "setop", "UNION (distinct)", "SELECT id, name FROM emp UNION SELECT id, name FROM emp2")
case("set-002", "setop", "UNION ALL", "SELECT id, name FROM emp UNION ALL SELECT id, name FROM emp2")
case("set-003", "setop", "UNION DISTINCT (explicit)", "SELECT dept_id FROM emp UNION DISTINCT SELECT dept_id FROM emp2")
case("set-004", "setop", "INTERSECT", "SELECT dept_id FROM emp INTERSECT SELECT dept_id FROM dept")
case("set-005", "setop", "INTERSECT ALL", "SELECT dept_id FROM emp INTERSECT ALL SELECT dept_id FROM emp")
case("set-006", "setop", "EXCEPT", "SELECT dept_id FROM dept EXCEPT SELECT dept_id FROM emp")
case("set-007", "setop", "EXCEPT ALL", "SELECT dept_id FROM emp EXCEPT ALL SELECT dept_id FROM emp WHERE active")
case("set-008", "setop", "MINUS (EXCEPT synonym)", "SELECT dept_id FROM dept MINUS SELECT dept_id FROM emp")
case("set-009", "setop", "3-way UNION ALL", "SELECT id FROM emp UNION ALL SELECT id FROM emp2 UNION ALL SELECT dept_id FROM dept")
case("set-010", "setop", "set op then ORDER BY", "SELECT id, name FROM emp UNION SELECT id, name FROM emp2 ORDER BY id")

# ── 12. GROUP BY extensions ──────────────────────────────────────────────────
case("gx-001", "group_ext", "ROLLUP", "SELECT dept_id, active, count(*) n FROM emp GROUP BY ROLLUP (dept_id, active)")
case("gx-002", "group_ext", "CUBE", "SELECT dept_id, active, count(*) n FROM emp GROUP BY CUBE (dept_id, active)")
case("gx-003", "group_ext", "GROUPING SETS (explicit)", "SELECT dept_id, active, count(*) n FROM emp GROUP BY GROUPING SETS ((dept_id, active), (dept_id), ())")
case("gx-004", "group_ext", "GROUPING SETS multiple combos", "SELECT dept_id, active, count(*) n FROM emp GROUP BY GROUPING SETS ((dept_id), (active))")
case("gx-005", "group_ext", "GROUPING() flag", "SELECT dept_id, grouping(dept_id) g, count(*) n FROM emp GROUP BY CUBE (dept_id)")
case("gx-006", "group_ext", "GROUPING_ID()", "SELECT dept_id, active, grouping_id() gid, count(*) n FROM emp GROUP BY ROLLUP (dept_id, active)")
case("gx-007", "group_ext", "ROLLUP with HAVING", "SELECT dept_id, count(*) n FROM emp GROUP BY ROLLUP (dept_id) HAVING count(*) > 1")
case("gx-008", "group_ext", "CUBE 3 columns", "SELECT dept_id, active, year(hire_date) y, count(*) n FROM emp GROUP BY CUBE (dept_id, active, year(hire_date))")
case("gx-009", "group_ext", "ROLLUP + order by grouping_id", "SELECT dept_id, active, grouping_id() gid, count(*) n FROM emp GROUP BY ROLLUP (dept_id, active) ORDER BY gid")
case("gx-010", "group_ext", "GROUP BY ... WITH ROLLUP (Hive syntax)", "SELECT dept_id, active, count(*) n FROM emp GROUP BY dept_id, active WITH ROLLUP")
# gx-011: pass-3 fix — `rewrite_grouping_id` (emission.rs) previously walked
# only a hand-enumerated set of containers (FunctionCall args / Alias / Cast /
# CaseWhen); a no-arg `grouping_id()` nested inside a `Binary` expression (here,
# `+ 1`) fell through untouched and reached DuckDB as a literal zero-arg
# `grouping_id()` -> parser error (DuckDB requires explicit grouping-column
# args). The generic `children_mut` walk now splices the ROLLUP grouping
# columns regardless of the surrounding container shape.
case("gx-011", "group_ext", "grouping_id() nested in arithmetic expr (ROLLUP)", "SELECT dept_id, active, grouping_id() + 1 AS gid1, count(*) n FROM emp GROUP BY ROLLUP (dept_id, active)")
# gx-012: fixed (corpus-driven pass 1). The gx-011 fix widened
# `rewrite_grouping_id` (emission.rs) so a no-arg `grouping_id()` gets the
# grouping columns spliced in anywhere in the aggregate SELECT list — but
# `render_aggregate_op` still emitted the HAVING predicate through plain
# `render_expr`, WITHOUT the rewrite, so a no-arg `grouping_id()` in HAVING
# reached DuckDB as a zero-arg call -> parser error. The HAVING branch of
# `render_aggregate_op` now applies `rewrite_grouping_id` before rendering, so
# grouping functions in HAVING over ROLLUP/CUBE/GROUPING SETS splice the
# grouping columns exactly as the SELECT list does.
case("gx-012", "group_ext", "grouping_id() in HAVING over ROLLUP", "SELECT dept_id, count(*) n FROM emp GROUP BY ROLLUP (dept_id) HAVING grouping_id() = 0")

# ── 13. Complex types & LATERAL VIEW ─────────────────────────────────────────
case("cx-001", "complex_type", "array literal + element access", "SELECT array(1, 2, 3) AS arr, array(1,2,3)[0] AS first")
case("cx-002", "complex_type", "map literal + key access", "SELECT map('a', 1, 'b', 2) AS m, map('a',1)['a'] AS av")
case("cx-003", "complex_type", "named_struct + field access", "SELECT named_struct('n', name, 'a', age) AS info FROM emp")
case("cx-004", "complex_type", "struct field path", "SELECT address.city AS city, address.geo.lat AS lat FROM emp")
case("cx-005", "complex_type", "array column functions", "SELECT name, size(tags) sz, array_contains(tags, 'rust') has_rust FROM emp")
case("cx-006", "complex_type", "map_keys / map_values / element_at", "SELECT map_keys(attrs) ks, element_at(attrs, 'team') team FROM emp")
case("cx-007", "complex_type", "LATERAL VIEW explode", "SELECT e.id, t.tag FROM emp e LATERAL VIEW explode(e.tags) t AS tag")
case("cx-008", "complex_type", "LATERAL VIEW OUTER explode", "SELECT e.id, t.tag FROM emp e LATERAL VIEW OUTER explode(e.tags) t AS tag")
case("cx-009", "complex_type", "LATERAL VIEW posexplode", "SELECT e.id, t.pos, t.tag FROM emp e LATERAL VIEW posexplode(e.tags) t AS pos, tag")
case("cx-010", "complex_type", "explode in SELECT (generator)", "SELECT id, explode(tags) AS tag FROM emp")
case("cx-011", "complex_type", "explode map -> key,value", "SELECT id, explode(attrs) AS (k, v) FROM emp")
case("cx-012", "complex_type", "inline (array<struct> -> cols)", "SELECT inline(array(named_struct('a', 1, 'b', 'x'), named_struct('a', 2, 'b', 'y')))")
case("cx-013", "complex_type", "higher-order transform lambda", "SELECT transform(tags, x -> upper(x)) AS up_tags FROM emp")
case("cx-014", "complex_type", "higher-order filter / exists", "SELECT filter(tags, x -> x LIKE 'r%') r_tags, exists(tags, x -> x = 'rust') has_rust FROM emp")

# ── 14. Table-valued / FROM-clause expressions ───────────────────────────────
case("tbl-001", "table_expr", "inline table VALUES", "SELECT * FROM VALUES (1, 'a'), (2, 'b'), (3, 'c') AS t(n, s)")
case("tbl-002", "table_expr", "VALUES feeding a join", "SELECT e.name, v.label FROM emp e JOIN (VALUES (10, 'infra'), (20, 'data')) AS v(did, label) ON e.dept_id = v.did")
case("tbl-003", "table_expr", "TABLESAMPLE PERCENT", "SELECT * FROM emp TABLESAMPLE (50 PERCENT)", flags=("nondeterministic",))
case("tbl-004", "table_expr", "TABLESAMPLE ROWS", "SELECT * FROM emp TABLESAMPLE (3 ROWS)", flags=("schema_only",))
case("tbl-005", "table_expr", "LATERAL subquery (correlated derived table)", "SELECT e.name, t.dept_avg FROM emp e JOIN LATERAL (SELECT avg(e2.salary) AS dept_avg FROM emp e2 WHERE e2.dept_id <=> e.dept_id) t")
case("tbl-006", "table_expr", "range() table function", "SELECT id FROM range(5)")
case("tbl-007", "table_expr", "explode() as table function", "SELECT * FROM explode(array(1, 2, 3))")
case("tbl-008", "table_expr", "broadcast hint", "SELECT /*+ BROADCAST(d) */ e.name, d.dept_name FROM emp e JOIN dept d ON e.dept_id = d.dept_id", flags=("cosmetic",))
case("tbl-009", "table_expr", "coalesce/repartition hint", "SELECT /*+ COALESCE(1) */ * FROM emp", flags=("cosmetic",))
case("tbl-010", "table_expr", "subquery alias required", "SELECT t.dept_id, t.n FROM (SELECT dept_id, count(*) n FROM emp GROUP BY dept_id) AS t")
# tbl-011: pass-3 fix — `lower_table_factor`'s `TableFactor::Function` arm
# (`LATERAL f(...)`) built the table-function node but silently dropped the
# user alias (`alias: _`), unlike the sibling `Derived` / `Table`-with-args
# branches. Now composed through the shared `apply_table_alias` helper, so
# `AS r(id)` renames the single output column via `ToDf`. Uses `range` rather
# than `explode` as the underlying table function — `explode` hits an
# unrelated, pre-existing τ boundary gap ("table-function analysis" not yet
# implemented for `TableFunction[explode]`, see `tbl-007`), which would mask
# the alias-plumbing fix under test here.
#
# NOTE: the sibling fix in the same finding — `TableFactor::TableFunction`
# (ANSI `TABLE(<expr>) AS alias` syntax) — is unit-test-only. Real Spark's
# grammar treats a bare `TABLE(...)` in FROM position as invoking a
# table-valued function literally NAMED `TABLE` (used for passing a TABLE
# argument to a Python UDTF/PTF), not as "call the function inside the
# parens" — `TABLE(explode(...))` throws Spark's own
# `UNRESOLVABLE_TABLE_VALUED_FUNCTION`/`UNRESOLVED_ROUTINE`, verified against
# the vendored Spark 4.1.1 reference. That branch is therefore unreachable via
# real PySpark SQL traffic and is covered only by
# `table_function_table_syntax_with_alias_columns_renames_via_todf` in
# `crates/core/src/parser_v2/v2_lowering.rs`.
case("tbl-011", "table_expr", "LATERAL table function with column alias list", "SELECT r.id FROM LATERAL range(3) AS r(id)")
# tbl-012: KNOWN-RED witness (NOT a pass-3 fix — out of findings 1-6). The
# tbl-011 fix restored the *alias* on `TableFactor::Function` (`LATERAL
# f(...)`), but the arm still swallows the `lateral: true` flag (`..`), so
# correlation to outer columns is lost; and the underlying generator TVF
# (`explode`) has no τ table-function analysis yet (see tbl-007). A correlated
# lateral generator — `LATERAL explode(e.tags)` referencing the outer row — is
# valid Spark 4.x (SPARK-41961) but fails end-to-end in τ today. Flips green
# once both the swallowed `lateral` flag and generator-TVF analysis land.
case("tbl-012", "table_expr", "correlated LATERAL table function over outer column (known gap)", "SELECT e.id, r.v FROM emp e, LATERAL explode(e.tags) AS r(v)")

# ── 15. Advanced predicates / SQL-specific operators ─────────────────────────
case("pr-001", "predicate_adv", "IS DISTINCT FROM", "SELECT * FROM emp WHERE dept_id IS DISTINCT FROM 10")
case("pr-002", "predicate_adv", "IS NOT DISTINCT FROM NULL", "SELECT * FROM emp WHERE manager_id IS NOT DISTINCT FROM NULL")
case("pr-003", "predicate_adv", "LIKE ANY", "SELECT * FROM emp WHERE name LIKE ANY ('A%', '%e')")
case("pr-004", "predicate_adv", "LIKE ALL", "SELECT * FROM emp WHERE name LIKE ALL ('%a%', '%e%')")
case("pr-005", "predicate_adv", "multi-column IN", "SELECT * FROM emp WHERE (dept_id, active) IN ((10, true), (20, false))")
case("pr-006", "predicate_adv", "IS TRUE / IS FALSE", "SELECT * FROM emp WHERE active IS TRUE AND (age > 100) IS FALSE")
case("pr-007", "predicate_adv", "lateral column alias (spark4)", "SELECT salary * 1.1 AS raised, raised - salary AS delta FROM emp", flags=("spark4",))
case("pr-008", "predicate_adv", "struct equality comparison", "SELECT * FROM emp WHERE named_struct('c', address.city) = named_struct('c', 'Vienna')")
case("pr-009", "predicate_adv", "NOT with IN subquery", "SELECT * FROM emp WHERE NOT (dept_id IN (SELECT dept_id FROM dept WHERE country = 'DE'))")
case("pr-010", "predicate_adv", "boolean column directly", "SELECT * FROM emp WHERE active")

# ── 16. PIVOT / UNPIVOT ──────────────────────────────────────────────────────
case("pv-001", "pivot", "PIVOT single aggregate", "SELECT * FROM (SELECT dept_id, active, salary FROM emp) PIVOT (avg(salary) FOR active IN (true AS act, false AS inact))")
case("pv-002", "pivot", "PIVOT count", "SELECT * FROM (SELECT dept_id, active FROM emp) PIVOT (count(*) FOR active IN (true AS t, false AS f))")
case("pv-003", "pivot", "PIVOT multiple aggregates", "SELECT * FROM (SELECT dept_id, active, salary FROM emp) PIVOT (avg(salary) AS a, max(salary) AS m FOR active IN (true AS act))")
case("pv-004", "pivot", "UNPIVOT (spark4)", "SELECT id, metric, val FROM (SELECT id, age, salary FROM emp) UNPIVOT (val FOR metric IN (age, salary))", flags=("spark4",))
case("pv-005", "pivot", "PIVOT on dept_id", "SELECT * FROM (SELECT active, dept_id, salary FROM emp) PIVOT (avg(salary) FOR dept_id IN (10, 20, 30))")
case("pv-006", "pivot", "stack() unpivot form", "SELECT id, stack(2, 'age', age, 'salary', salary) AS (metric, value) FROM emp")

# ── 17. Typed literals & intervals ───────────────────────────────────────────
case("lit-001", "typed_literal", "DATE literal", "SELECT DATE '2026-01-15' AS d")
case("lit-002", "typed_literal", "TIMESTAMP literal", "SELECT TIMESTAMP '2026-01-15 10:30:00' AS ts")
case("lit-003", "typed_literal", "INTERVAL day literal", "SELECT hire_date + INTERVAL '90' DAY AS later FROM emp")
case("lit-004", "typed_literal", "INTERVAL year-to-month", "SELECT INTERVAL '1-2' YEAR TO MONTH AS ym")
case("lit-005", "typed_literal", "INTERVAL day-to-second", "SELECT INTERVAL '1 02:30:00' DAY TO SECOND AS dts")
case("lit-006", "typed_literal", "make_interval / make_dt_interval", "SELECT make_interval(1, 2, 0, 5) iv, make_dt_interval(1, 2, 30, 0) dti FROM emp LIMIT 1")
case("lit-007", "typed_literal", "DECIMAL literal arithmetic", "SELECT 100.25 * 3.142 AS prod")
case("lit-008", "typed_literal", "timestamp - timestamp -> interval", "SELECT (TIMESTAMP '2026-06-30 00:00:00' - TIMESTAMP '2026-06-01 00:00:00') AS diff_iv")
case("lit-009", "typed_literal", "string escape literal", "SELECT 'line1\\nline2' AS s, 'tab\\there' AS t")
case("lit-010", "typed_literal", "INTERVAL arithmetic in WHERE", "SELECT * FROM emp WHERE last_login > current_timestamp() - INTERVAL '30' DAY", flags=("nondeterministic",))

# ── 18. Numeric tower × non-exotic built-in functions ────────────────────────
# Each function applied across the supported numeric types (short/int/bigint/
# float/double/decimal) to pin Spark's per-type RESULT TYPE and coercion rules —
# the ADR-005 divergent slice where DuckDB most often disagrees. The oracle
# compares the resolved schema; these are schema_only by intent.
case("num-001", "numeric_tower", "ceil over int/bigint/float/double (-> bigint)", "SELECT ceil(a) ca, ceil(lng) cl, ceil(f) cf, ceil(x) cx FROM nums", flags=("schema_only",))
case("num-002", "numeric_tower", "ceil/floor over decimal (-> decimal, scale 0)", "SELECT ceil(d1) cd1, floor(d2) fd2, ceil(d3) cd3 FROM nums", flags=("schema_only",))
case("num-003", "numeric_tower", "ceil with target scale (-> decimal) (spark4)", "SELECT ceil(x, 2) cx2, floor(d1, 1) fd1 FROM nums", flags=("schema_only", "spark4"))
case("num-004", "numeric_tower", "round over int/float/double", "SELECT round(a, 1) ra, round(f, 1) rf, round(x, 1) rx FROM nums", flags=("schema_only",))
case("num-005", "numeric_tower", "round/bround over decimal (precision/scale adjust)", "SELECT round(d1, 1) rd1, bround(d2, 2) bd2, round(d3, 3) rd3 FROM nums", flags=("schema_only",))
case("num-006", "numeric_tower", "round integral keeps integral type", "SELECT round(sml, -1) rs, round(lng, -2) rl FROM nums", flags=("schema_only",))
case("num-007", "numeric_tower", "abs preserves type across tower", "SELECT abs(sml) asm, abs(a) ai, abs(lng) al, abs(f) af, abs(x) ax, abs(d1) ad1 FROM nums", flags=("schema_only",))
case("num-008", "numeric_tower", "signum always double; negative preserves type", "SELECT signum(a) sg_i, signum(d1) sg_d, negative(a) ng_i, negative(d1) ng_d FROM nums", flags=("schema_only",))
case("num-009", "numeric_tower", "sqrt/exp/ln over int/decimal (-> double, coercion)", "SELECT sqrt(a) sq_i, sqrt(d1) sq_d, exp(a) ex_i, ln(d1) ln_d FROM nums", flags=("schema_only",))
case("num-010", "numeric_tower", "pow/log over mixed numeric args (-> double)", "SELECT pow(a, d2) p1, power(d1, 2) p2, log(a, lng) lg FROM nums", flags=("schema_only",))
case("num-011", "numeric_tower", "trig over int/float (-> double)", "SELECT sin(a) si, cos(f) co, atan2(a, d1) at FROM nums", flags=("schema_only",))
case("num-012", "numeric_tower", "mod over int/bigint/decimal", "SELECT mod(a, b) m_ii, mod(lng, a) m_li, mod(d1, d2) m_dd, pmod(a, b) pm FROM nums", flags=("schema_only",))
case("num-013", "numeric_tower", "% operator over float/double/decimal", "SELECT f % 2 m_f, x % 2 m_x, d1 % d2 m_d FROM nums", flags=("schema_only",))
case("num-014", "numeric_tower", "greatest/least over mixed numeric (-> widened)", "SELECT greatest(sml, a, lng) g1, greatest(a, x) g2, least(a, d1) g3, greatest(f, x) g4 FROM nums", flags=("schema_only",))
case("num-015", "numeric_tower", "coalesce/nullif numeric unification", "SELECT coalesce(a, lng) c1, coalesce(a, x) c2, coalesce(d2, d1) c3, nullif(a, b) n1 FROM nums", flags=("schema_only",))
case("num-016", "numeric_tower", "CASE branches unify int and decimal and double", "SELECT CASE WHEN a > 0 THEN a WHEN a = 0 THEN d1 ELSE x END AS unified FROM nums", flags=("schema_only",))
case("num-017", "numeric_tower", "short+int / short+bigint promotion", "SELECT sml + a AS si, sml + lng AS sl, sml * sml AS ss FROM nums", flags=("schema_only",))
case("num-018", "numeric_tower", "float+double / float+int / float+decimal", "SELECT f + x AS fx, f + a AS fa, f + d1 AS fd FROM nums", flags=("schema_only",))
case("num-019", "numeric_tower", "decimal+/-*÷ high-precision (38-digit boundary)", "SELECT d3 + d1 AS s, d3 * d2 AS p, d3 / d2 AS q, d3 - d1 AS df FROM nums", flags=("schema_only",))
case("num-020", "numeric_tower", "unary minus preserves type", "SELECT -a AS na, -d1 AS nd, -f AS nf, -sml AS ns FROM nums", flags=("schema_only",))
case("num-021", "numeric_tower", "cast up the tower (short->int->bigint->double)", "SELECT cast(sml AS int) si, cast(a AS bigint) ab, cast(lng AS double) ld, cast(f AS double) fd FROM nums", flags=("schema_only",))
case("num-022", "numeric_tower", "cast to/from decimal across tower", "SELECT cast(a AS decimal(10,2)) ad, cast(f AS decimal(10,4)) fd, cast(d1 AS double) dd, cast(d3 AS bigint) db FROM nums", flags=("schema_only",))
case("num-023", "numeric_tower", "try_cast narrowing overflow (-> null) (spark4)", "SELECT try_cast(lng AS int) ti, try_cast(d3 AS decimal(5,2)) td FROM nums", flags=("schema_only", "spark4"))
case("num-024", "numeric_tower", "sum over short/int/bigint (-> bigint)", "SELECT sum(sml) ss, sum(a) sa, sum(lng) sl FROM nums", flags=("schema_only",))
case("num-025", "numeric_tower", "sum over float/double (-> double) and decimal (-> widened precision)", "SELECT sum(f) sf, sum(x) sx, sum(d1) sd1, sum(d3) sd3 FROM nums", flags=("schema_only",))
case("num-026", "numeric_tower", "avg over int/decimal (Spark result-type rules)", "SELECT avg(a) aa, avg(lng) al, avg(d1) ad1, avg(d2) ad2 FROM nums", flags=("schema_only",))
case("num-027", "numeric_tower", "min/max preserve input type across tower", "SELECT min(sml) ms, max(a) ma, min(f) mf, max(d1) md FROM nums", flags=("schema_only",))
case("num-028", "numeric_tower", "stddev/variance always double across tower", "SELECT stddev(a) si, stddev(d1) sd, variance(lng) vl, var_pop(f) vf FROM nums", flags=("schema_only",))
case("num-029", "numeric_tower", "sum(DISTINCT)/avg(DISTINCT) over decimal", "SELECT sum(DISTINCT d1) sd, avg(DISTINCT a) aa FROM nums", flags=("schema_only",))
case("num-030", "numeric_tower", "bitwise AND/OR/XOR over short/int/bigint", "SELECT a & b ab, a | b ob, sml ^ a xb, lng & a lb FROM nums", flags=("schema_only",))
case("num-031", "numeric_tower", "shift/bit_count over integral tower", "SELECT shiftleft(a, 2) shl, shiftright(lng, 1) shr, bit_count(a) bc FROM nums", flags=("schema_only",))
case("num-032", "numeric_tower", "hex/bin over int vs bigint", "SELECT hex(a) ha, hex(lng) hl, bin(a) ba FROM nums", flags=("schema_only",))


# ---------------------------------------------------------------------------
# Coverage summary + optional self-check runner
# ---------------------------------------------------------------------------

def coverage() -> Dict[str, int]:
    counts: Dict[str, int] = {}
    for c in CASES:
        counts[c.category] = counts.get(c.category, 0) + 1
    return counts  # insertion order == frequency order


def run(spark: SparkSession, only: Tuple[str, ...] = (), collect: bool = False):
    """Execute every case against `spark`; reference side of the differential oracle.

    Repeat against the thunderduck session and diff `schema` (and `rows` when
    collect=True). `nondeterministic` cases should be compared by schema only.
    """
    build_inputs(spark)
    report = []
    for c in CASES:
        if only and c.category not in only:
            continue
        entry = {"id": c.id, "category": c.category, "flags": c.flags}
        try:
            df = spark.sql(c.sql)
            entry["schema"] = df.schema.json()
            if collect and "nondeterministic" not in c.flags:
                entry["rows"] = [r.asDict(recursive=True) for r in df.limit(1000).collect()]
            entry["status"] = "ok"
        except Exception as exc:  # surface parse/analysis errors per-case (esp. spark4 grammar)
            entry["status"] = "error"
            entry["error"] = f"{type(exc).__name__}: {exc}"
        report.append(entry)
    return report


if __name__ == "__main__":
    print(f"Total SQL cases: {len(CASES)}")
    print("Categories (frequency order):")
    for cat, n in coverage().items():
        print(f"  {cat:16s} {n:3d}")
    # Against a live Spark:
    #   spark = SparkSession.builder.master("local[*]").getOrCreate()
    #   import json; print(json.dumps(run(spark, collect=True), indent=2, default=str))