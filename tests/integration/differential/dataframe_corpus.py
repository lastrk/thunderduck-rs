"""
thunderduck — DataFrame transpiler conformance corpus
======================================================

A *biased but breadth-covering* sample of chained Spark DataFrame expressions,
used to drive the differential oracle (ADR-015) and to serve as the practical
coverage denominator for the emission table (ADR-009) during the reimplementation
of tau against the new ADR set.

What this is
------------
- ~325 chained DataFrame expressions, each <= 20 chained operations, grouped by
  feature family. Each case is an executable `build(I) -> DataFrame` over a fixed
  set of well-typed input DataFrames `I`. This corpus is DataFrame-API only; the
  SQL front end (raw `spark.sql(...)`, correlated subqueries, CTEs, etc.) is
  intentionally out of scope here and exercised by a separate corpus.
- It is a *biased sample*: deliberately over-weighted toward the cases where Spark
  and DuckDB disagree on TYPE and NULLABILITY (ADR-005's divergent slice), because
  that is thunderduck's actual risk surface — not toward uniform coverage of every
  function. Functions whose result type/nullability is "obvious" get one or two
  representatives; the type-promotion / nullability-propagation / set-widening
  cases get dense coverage.
- It is NOT an exhaustive enumeration of the API. The universe of valid chains is
  unbounded; this is a curated cross-section that touches every major family and
  the known divergence corners.

How to use it (two oracle modes, per ADR-015)
---------------------------------------------
  inputs = build_inputs(spark)                       # reference Spark session
  for c in CASES:
      df = c.build(inputs)
      ref_schema = df.schema                          # (a) AnalyzePlan / schema diff
      ref_rows   = df.collect()                        # (b) result differential
  ...then run the same `c.build` against thunderduck's session and diff:
    - schema diff validates the divergent slice (types + nullability) in isolation,
      AND front-end agreement (INV7) when the same logic is also expressed in SQL.
    - result diff validates end-to-end semantics after test-side canonicalization
      (ordering, float tolerance, map-key ordering) — there is no production
      canonicalizer (ADR-015); canonicalization lives in the harness.

Flags
-----
  "nondeterministic" : value differs run-to-run (rand, current_*, monotonically_*).
                       Use SCHEMA diff only, or seed/freeze in the harness.
  "schema_only"      : compare schema only (e.g. ordering-sensitive without a total
                       order, or physical-plan-only ops).
  "spark4"           : relies on Spark 3.5/4.x surface (try_*, offset, etc.).
  "cosmetic"         : result-irrelevant op (repartition/hint/coalesce) — exercises
                       the ADR-001 "result-irrelevant cosmetic" carve-out; expect
                       identical results, transpiler may legally drop/ignore it.

Conventions
-----------
  F = pyspark.sql.functions ; W = pyspark.sql.window.Window
  Input DataFrames in I: "emp", "dept", "emp2", "nums".
  Pinned to Spark 4.1.1 (ADR-016). Some try_* use F.expr where the native PySpark
  binding name is version-fragile; these are flagged "spark4".
"""

from __future__ import annotations

import datetime
from dataclasses import dataclass, field
from decimal import Decimal
from typing import Callable, Dict, List, Optional, Tuple

from pyspark.sql import DataFrame, SparkSession
from pyspark.sql import functions as F
from pyspark.sql.window import Window as W
from pyspark.sql.types import (
    ArrayType, BooleanType, DateType, DecimalType, DoubleType, IntegerType,
    LongType, MapType, StringType, StructField, StructType, TimestampType,
)

NAN = float("nan")


# ---------------------------------------------------------------------------
# Input DataFrames — explicit schemas, deliberate nulls / NaN / edge values
# ---------------------------------------------------------------------------

def build_inputs(spark: SparkSession) -> Dict[str, DataFrame]:
    emp_schema = StructType([
        StructField("id", LongType(), False),
        StructField("name", StringType(), True),
        StructField("dept_id", IntegerType(), True),       # nullable: drives outer-join nullability
        StructField("manager_id", LongType(), True),
        StructField("age", IntegerType(), True),
        StructField("salary", DoubleType(), True),
        StructField("bonus", DecimalType(9, 2), True),     # decimal: drives precision/scale promotion
        StructField("hire_date", DateType(), True),
        StructField("last_login", TimestampType(), True),
        StructField("active", BooleanType(), True),
        StructField("score", DoubleType(), True),          # contains NaN and null: isnan / nanvl
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

    def d(s):  # date
        return datetime.date.fromisoformat(s)

    def ts(s):  # timestamp
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
        (40, "Security",       None,                  "Berlin", "DE"),   # no employees: drives anti/semi/outer
    ]
    dept = spark.createDataFrame(dept_rows, dept_schema)

    # union-compatible projection target (note: missing some emp columns; has an extra one)
    emp2_schema = StructType([
        StructField("id", LongType(), False),
        StructField("name", StringType(), True),
        StructField("dept_id", IntegerType(), True),
        StructField("age", IntegerType(), True),
        StructField("salary", DoubleType(), True),
        StructField("country", StringType(), True),   # extra column (allowMissingColumns path)
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
        StructField("d1", DecimalType(10, 2), True),
        StructField("d2", DecimalType(6, 3), True),
        StructField("lng", LongType(), True),
    ])
    nums_rows = [
        (7,   2,    3.5,  2.0,  Decimal("100.25"), Decimal("3.142"), 9000000000),
        (10,  0,   -1.0,  0.0,  Decimal("0.00"),   Decimal("0.001"), 1),
        (-3,  5,    NAN,  4.0,  Decimal("9999.99"), Decimal("12.500"), -42),
        (0,   None, 2.0,  None, None,               Decimal("1.000"), None),
    ]
    nums = spark.createDataFrame(nums_rows, nums_schema)

    # raw text payloads for parse/format families (JSON, CSV, URL, number strings)
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

    return {"emp": emp, "dept": dept, "emp2": emp2, "nums": nums, "raw": raw}


# ---------------------------------------------------------------------------
# Case registry
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class Case:
    id: str
    category: str
    description: str
    build: Callable[[Dict[str, DataFrame]], DataFrame]
    flags: Tuple[str, ...] = ()
    # Declared Spark error-class token (e.g. "DIVIDE_BY_ZERO"). When set, the
    # harness applies the ADR-006 tri-state error-parity comparator instead of a
    # value diff: both engines must raise this class to PASS. See
    # utils.dataframe_diff.reconcile_error_parity.
    expected_error: Optional[str] = None


CASES: List[Case] = []


def case(id, category, description, build, flags=(), expected_error=None):
    CASES.append(Case(id, category, description, build, tuple(flags), expected_error))


# ── 1. Projection & column manipulation ────────────────────────────────────
case("proj-001", "projection", "select by name", lambda I: I["emp"].select("id", "name", "age"))
case("proj-002", "projection", "select Column objects", lambda I: I["emp"].select(F.col("id"), F.col("salary")))
case("proj-003", "projection", "select with arithmetic + alias", lambda I: I["emp"].select((F.col("salary") * 1.1).alias("raise")))
case("proj-004", "projection", "select star", lambda I: I["emp"].select("*"))
case("proj-005", "projection", "select star plus computed", lambda I: I["emp"].select("*", (F.col("age") + 1).alias("age_next")))
case("proj-006", "projection", "selectExpr (SQL strings) — exercises INV7", lambda I: I["emp"].selectExpr("id", "age + 1 as age1", "upper(name) as up"))
case("proj-007", "projection", "withColumn new", lambda I: I["emp"].withColumn("decade", (F.col("age") / 10).cast("int")))
case("proj-008", "projection", "withColumn overwrite existing", lambda I: I["emp"].withColumn("salary", F.col("salary") * 2))
case("proj-009", "projection", "withColumns (batch)", lambda I: I["emp"].withColumns({"a1": F.col("age") + 1, "s2": F.col("salary") / 2}))
case("proj-010", "projection", "withColumnRenamed", lambda I: I["emp"].withColumnRenamed("salary", "comp"))
case("proj-011", "projection", "withColumnsRenamed (batch)", lambda I: I["emp"].withColumnsRenamed({"salary": "comp", "age": "yrs"}))
case("proj-012", "projection", "drop single", lambda I: I["emp"].drop("address"))
case("proj-013", "projection", "drop multiple", lambda I: I["emp"].drop("tags", "attrs", "address"))
case("proj-014", "projection", "drop then withColumn referencing survivor", lambda I: I["emp"].drop("bonus").withColumn("hi", F.col("salary") > 100000))
case("proj-015", "projection", "chained withColumn (5 deep)", lambda I: (I["emp"]
    .withColumn("c1", F.col("age") + 1).withColumn("c2", F.col("c1") * 2)
    .withColumn("c3", F.col("c2") - 3).withColumn("c4", F.col("c3") % 7)
    .withColumn("c5", F.col("c4").cast("string"))))

# ── 2. Filtering & predicates ──────────────────────────────────────────────
case("filt-001", "filter", "filter by Column predicate", lambda I: I["emp"].filter(F.col("age") > 30))
case("filt-002", "filter", "where by SQL string — INV7", lambda I: I["emp"].where("age > 30 and active = true"))
case("filt-003", "filter", "AND / OR / NOT combination", lambda I: I["emp"].filter((F.col("age") > 30) & (F.col("active")) | (~F.col("active"))))
case("filt-004", "filter", "isNull", lambda I: I["emp"].filter(F.col("dept_id").isNull()))
case("filt-005", "filter", "isNotNull", lambda I: I["emp"].filter(F.col("bonus").isNotNull()))
case("filt-006", "filter", "between", lambda I: I["emp"].filter(F.col("age").between(30, 45)))
case("filt-007", "filter", "isin literals", lambda I: I["emp"].filter(F.col("dept_id").isin(10, 20)))
case("filt-008", "filter", "isin list", lambda I: I["emp"].filter(F.col("name").isin(["Alice", "Bob", "Eve"])))
case("filt-009", "filter", "like", lambda I: I["emp"].filter(F.col("name").like("A%")))
case("filt-010", "filter", "ilike (case-insensitive)", lambda I: I["emp"].filter(F.col("name").ilike("a%")))
case("filt-011", "filter", "rlike (regex)", lambda I: I["emp"].filter(F.col("name").rlike("^[A-D]")))
case("filt-012", "filter", "startswith / endswith / contains", lambda I: I["emp"].filter(F.col("name").startswith("A") | F.col("name").endswith("e") | F.col("name").contains("ar")))
case("filt-013", "filter", "eqNullSafe (<=>)", lambda I: I["emp"].filter(F.col("dept_id").eqNullSafe(F.lit(None))))
case("filt-014", "filter", "two chained filters", lambda I: I["emp"].filter(F.col("age") > 25).filter(F.col("salary") < 130000))
case("filt-015", "filter", "filter on computed column", lambda I: I["emp"].withColumn("tenure_pos", F.col("age") - 18).filter(F.col("tenure_pos") > 10))

# ── 3. Literals, typed columns & casts (type-source stress) ─────────────────
case("cast-001", "cast", "lit int / string / bool / double", lambda I: I["emp"].select(F.lit(1).alias("i"), F.lit("x").alias("s"), F.lit(True).alias("b"), F.lit(3.14).alias("d")))
case("cast-002", "cast", "lit null typed via cast", lambda I: I["emp"].select(F.lit(None).cast("int").alias("ni")))
case("cast-003", "cast", "cast int->double", lambda I: I["emp"].select(F.col("age").cast("double").alias("aged")))
case("cast-004", "cast", "cast double->int (truncation)", lambda I: I["emp"].select(F.col("salary").cast("int").alias("sali")))
case("cast-005", "cast", "cast string->date", lambda I: I["emp"].select(F.lit("2026-01-15").cast("date").alias("dt")))
case("cast-006", "cast", "cast to decimal(scale)", lambda I: I["emp"].select(F.col("salary").cast(DecimalType(12, 4)).alias("sal_dec")))
case("cast-007", "cast", "cast decimal->double (loses exactness)", lambda I: I["emp"].select(F.col("bonus").cast("double").alias("bon_d")))
case("cast-008", "cast", "cast boolean->int", lambda I: I["emp"].select(F.col("active").cast("int").alias("act_i")))
case("cast-009", "cast", "cast timestamp->date", lambda I: I["emp"].select(F.col("last_login").cast("date").alias("ll_d")))
case("cast-010", "cast", "cast chain int->string->int", lambda I: I["emp"].select(F.col("age").cast("string").cast("int").alias("rt")))
case("cast-011", "cast", "cast in arithmetic (mixed type promote)", lambda I: I["emp"].select((F.col("age").cast("long") + F.lit(1)).alias("agel")))
case("cast-012", "cast", "try_cast bad string (spark4)", lambda I: I["emp"].select(F.expr("try_cast(name as int)").alias("tc")), flags=("spark4",))

# ── 4. Conditionals & null-handling (nullability stress) ────────────────────
case("cond-001", "conditional", "when/otherwise", lambda I: I["emp"].select(F.when(F.col("age") >= 40, "senior").otherwise("junior").alias("band")))
case("cond-002", "conditional", "chained when (multi-branch)", lambda I: I["emp"].select(F.when(F.col("age") < 30, "a").when(F.col("age") < 45, "b").otherwise("c").alias("band")))
case("cond-003", "conditional", "when WITHOUT otherwise -> nullable", lambda I: I["emp"].select(F.when(F.col("active"), F.col("salary")).alias("maybe_sal")))
case("cond-004", "conditional", "coalesce removes nullability when last is non-null", lambda I: I["emp"].select(F.coalesce(F.col("bonus"), F.lit(Decimal("0.00"))).alias("bonus0")))
case("cond-005", "conditional", "coalesce of two nullables stays nullable", lambda I: I["emp"].select(F.coalesce(F.col("bonus"), F.col("score").cast("decimal(9,2)")).alias("c")))
case("cond-006", "conditional", "nvl", lambda I: I["emp"].select(F.nvl(F.col("dept_id"), F.lit(-1)).alias("dept_or")))
case("cond-007", "conditional", "nvl2", lambda I: I["emp"].select(F.nvl2(F.col("bonus"), F.lit("has"), F.lit("none")).alias("flag")))
case("cond-008", "conditional", "ifnull", lambda I: I["emp"].select(F.ifnull(F.col("manager_id"), F.lit(0)).alias("mgr")))
case("cond-009", "conditional", "nullif (-> nullable)", lambda I: I["emp"].select(F.nullif(F.col("age"), F.lit(60)).alias("age_or_null")))
case("cond-010", "conditional", "isnan on NaN double", lambda I: I["emp"].select(F.isnan(F.col("score")).alias("is_nan")))
case("cond-011", "conditional", "nanvl (replace NaN)", lambda I: I["emp"].select(F.nanvl(F.col("score"), F.lit(0.0)).alias("score_clean")))
case("cond-012", "conditional", "na.fill scalar", lambda I: I["emp"].na.fill(0, subset=["dept_id"]))
case("cond-013", "conditional", "na.fill dict (per-column)", lambda I: I["emp"].na.fill({"dept_id": -1, "name": "?"}))
case("cond-014", "conditional", "na.drop any", lambda I: I["emp"].na.drop("any", subset=["dept_id", "bonus"]))
case("cond-015", "conditional", "na.drop thresh", lambda I: I["emp"].na.drop(thresh=2, subset=["dept_id", "bonus", "last_login"]))
case("cond-016", "conditional", "na.replace value", lambda I: I["emp"].na.replace({"Alice": "ALICE"}, subset=["name"]))

# ── 5. String functions ────────────────────────────────────────────────────
case("str-001", "string", "concat", lambda I: I["emp"].select(F.concat(F.col("name"), F.lit("@x")).alias("u")))
case("str-002", "string", "concat_ws", lambda I: I["emp"].select(F.concat_ws("-", F.col("name"), F.col("dept_id").cast("string")).alias("k")))
case("str-003", "string", "substring", lambda I: I["emp"].select(F.substring(F.col("name"), 1, 2).alias("ss")))
case("str-004", "string", "upper / lower / initcap", lambda I: I["emp"].select(F.upper("name").alias("u"), F.lower("name").alias("l"), F.initcap("name").alias("c")))
case("str-005", "string", "trim / ltrim / rtrim", lambda I: I["emp"].select(F.trim(F.lit("  x  ")).alias("t"), F.ltrim(F.lit("  x")).alias("lt"), F.rtrim(F.lit("x  ")).alias("rt")))
case("str-006", "string", "lpad / rpad", lambda I: I["emp"].select(F.lpad("name", 8, "*").alias("lp"), F.rpad("name", 8, ".").alias("rp")))
case("str-007", "string", "length / char_length", lambda I: I["emp"].select(F.length("name").alias("len")))
case("str-008", "string", "instr / locate", lambda I: I["emp"].select(F.instr(F.col("name"), "a").alias("i"), F.locate("a", F.col("name")).alias("loc")))
case("str-009", "string", "regexp_replace", lambda I: I["emp"].select(F.regexp_replace("name", "[aeiou]", "_").alias("rr")))
case("str-010", "string", "regexp_extract", lambda I: I["emp"].select(F.regexp_extract("name", r"^(.)", 1).alias("first_char")))
case("str-011", "string", "split -> array", lambda I: I["emp"].select(F.split(F.concat_ws(",", F.col("tags")), ",").alias("parts")))
case("str-012", "string", "translate", lambda I: I["emp"].select(F.translate("name", "aeiou", "AEIOU").alias("tr")))
case("str-013", "string", "overlay", lambda I: I["emp"].select(F.overlay(F.col("name"), F.lit("XX"), F.lit(1), F.lit(2)).alias("ov")))
case("str-014", "string", "repeat / reverse", lambda I: I["emp"].select(F.repeat(F.col("name"), 2).alias("rep"), F.reverse(F.col("name")).alias("rev")))
case("str-015", "string", "format_string", lambda I: I["emp"].select(F.format_string("%s=%d", F.col("name"), F.col("age")).alias("fs")))
case("str-016", "string", "format_number (-> string)", lambda I: I["emp"].select(F.format_number(F.col("salary"), 2).alias("fn")))
case("str-017", "string", "ascii / base64 / unbase64", lambda I: I["emp"].select(F.ascii("name").alias("asc"), F.base64(F.col("name").cast("binary")).alias("b64")))
case("str-018", "string", "levenshtein", lambda I: I["emp"].select(F.levenshtein(F.col("name"), F.lit("Alice")).alias("lev")))
case("str-019", "string", "soundex", lambda I: I["emp"].select(F.soundex("name").alias("sx")))
case("str-020", "string", "regexp_extract_all (-> array, spark4)", lambda I: I["emp"].select(F.expr("regexp_extract_all(name, '[a-z]', 0)").alias(" all_lc")), flags=("spark4",))

# ── 6. Math / numeric functions ────────────────────────────────────────────
case("math-001", "math", "abs / signum", lambda I: I["nums"].select(F.abs("a").alias("ab"), F.signum("a").alias("sg")))
case("math-002", "math", "round (scale) / bround", lambda I: I["nums"].select(F.round("x", 1).alias("r"), F.bround("x", 1).alias("br")))
case("math-003", "math", "ceil / floor", lambda I: I["nums"].select(F.ceil("x").alias("c"), F.floor("x").alias("f")))
case("math-004", "math", "sqrt / cbrt", lambda I: I["nums"].select(F.sqrt("y").alias("sq"), F.cbrt("y").alias("cb")))
case("math-005", "math", "exp / log / log10 / log2", lambda I: I["nums"].select(F.exp("y").alias("e"), F.log("y").alias("ln"), F.log10("y").alias("l10"), F.log2("y").alias("l2")))
case("math-006", "math", "pow / hypot", lambda I: I["nums"].select(F.pow("y", F.lit(2)).alias("p"), F.hypot("x", "y").alias("h")))
case("math-007", "math", "trig (sin/cos/tan/atan2)", lambda I: I["nums"].select(F.sin("y").alias("s"), F.cos("y").alias("c"), F.atan2("y", "x").alias("a2")))
case("math-008", "math", "degrees / radians", lambda I: I["nums"].select(F.degrees("y").alias("deg"), F.radians("y").alias("rad")))
case("math-009", "math", "greatest / least (null handling)", lambda I: I["nums"].select(F.greatest("a", "b", F.lit(4)).alias("g"), F.least("a", "b").alias("l")))
case("math-010", "math", "mod / pmod", lambda I: I["nums"].select((F.col("a") % F.col("b")).alias("m"), F.pmod("a", "b").alias("pm")), expected_error="REMAINDER_BY_ZERO")
case("math-011", "math", "int/int division -> double (Spark semantics)", lambda I: I["nums"].select((F.col("a") / F.col("b")).alias("div")), expected_error="DIVIDE_BY_ZERO")
case("math-012", "math", "bitwise and/or/xor + shifts", lambda I: I["nums"].select(F.col("a").bitwiseAND(F.col("b")).alias("and"), F.shiftleft(F.col("a"), 2).alias("shl")))
case("math-013", "math", "hex / unhex / conv", lambda I: I["nums"].select(F.hex("a").alias("hx"), F.conv(F.col("a").cast("string"), 10, 2).alias("bin")))
case("math-014", "math", "factorial", lambda I: I["nums"].select(F.factorial(F.lit(5)).alias("fact")))
case("math-015", "math", "rand seeded (nondeterministic family, seeded)", lambda I: I["nums"].select(F.rand(42).alias("r")), flags=("nondeterministic",))
case("math-016", "math", "try_divide -> null on /0 (spark4)", lambda I: I["nums"].select(F.expr("try_divide(a, b)").alias("td")), flags=("spark4",))

# ── 7. Date / time functions ───────────────────────────────────────────────
case("dt-001", "datetime", "current_date / current_timestamp", lambda I: I["emp"].select(F.current_date().alias("cd"), F.current_timestamp().alias("ct")), flags=("nondeterministic",))
case("dt-002", "datetime", "date_add / date_sub", lambda I: I["emp"].select(F.date_add("hire_date", 30).alias("plus"), F.date_sub("hire_date", 30).alias("minus")))
case("dt-003", "datetime", "datediff", lambda I: I["emp"].select(F.datediff(F.lit("2026-06-30").cast("date"), F.col("hire_date")).alias("days")))
case("dt-004", "datetime", "months_between", lambda I: I["emp"].select(F.months_between(F.lit("2026-06-30").cast("date"), F.col("hire_date")).alias("mb")))
case("dt-005", "datetime", "add_months", lambda I: I["emp"].select(F.add_months("hire_date", 6).alias("am")))
case("dt-006", "datetime", "year/quarter/month/week/day", lambda I: I["emp"].select(F.year("hire_date").alias("y"), F.quarter("hire_date").alias("q"), F.month("hire_date").alias("m"), F.dayofmonth("hire_date").alias("d")))
case("dt-007", "datetime", "dayofweek / dayofyear / weekofyear", lambda I: I["emp"].select(F.dayofweek("hire_date").alias("dw"), F.dayofyear("hire_date").alias("dy"), F.weekofyear("hire_date").alias("woy")))
case("dt-008", "datetime", "hour/minute/second from timestamp", lambda I: I["emp"].select(F.hour("last_login").alias("h"), F.minute("last_login").alias("mi"), F.second("last_login").alias("s")))
case("dt-009", "datetime", "to_date with format", lambda I: I["emp"].select(F.to_date(F.lit("15/01/2026"), "dd/MM/yyyy").alias("td")))
case("dt-010", "datetime", "to_timestamp with format", lambda I: I["emp"].select(F.to_timestamp(F.lit("2026-01-15 10:00"), "yyyy-MM-dd HH:mm").alias("tt")))
case("dt-011", "datetime", "date_format", lambda I: I["emp"].select(F.date_format("hire_date", "yyyy/MM").alias("df")))
case("dt-012", "datetime", "trunc (date) / date_trunc (ts)", lambda I: I["emp"].select(F.trunc("hire_date", "month").alias("tm"), F.date_trunc("hour", F.col("last_login")).alias("dth")))
case("dt-013", "datetime", "last_day / next_day", lambda I: I["emp"].select(F.last_day("hire_date").alias("ld"), F.next_day("hire_date", "Mon").alias("nd")))
case("dt-014", "datetime", "unix_timestamp / from_unixtime", lambda I: I["emp"].select(F.unix_timestamp("last_login").alias("uts"), F.from_unixtime(F.lit(1700000000)).alias("fut")))
case("dt-015", "datetime", "make_date / make_timestamp", lambda I: I["emp"].select(F.make_date(F.lit(2026), F.lit(2), F.lit(28)).alias("md")))
case("dt-016", "datetime", "extract / datepart (spark4)", lambda I: I["emp"].select(F.expr("extract(YEAR FROM hire_date)").alias("ey")), flags=("spark4",))
case("dt-017", "datetime", "timestamp tz convert (from/to_utc_timestamp)", lambda I: I["emp"].select(F.to_utc_timestamp(F.col("last_login"), "CET").alias("utc")))

# ── 8. Aggregations (groupBy + agg) ────────────────────────────────────────
case("agg-001", "aggregate", "count(*) global", lambda I: I["emp"].agg(F.count(F.lit(1)).alias("n")))
case("agg-002", "aggregate", "count(col) ignores nulls", lambda I: I["emp"].agg(F.count("bonus").alias("n_bonus")))
case("agg-003", "aggregate", "countDistinct", lambda I: I["emp"].agg(F.countDistinct("dept_id").alias("nd")))
case("agg-004", "aggregate", "approx_count_distinct", lambda I: I["emp"].agg(F.approx_count_distinct("dept_id").alias("acd")))
case("agg-005", "aggregate", "sum(int) -> long ; avg(int) -> double", lambda I: I["emp"].agg(F.sum("age").alias("sum_age"), F.avg("age").alias("avg_age")))
case("agg-006", "aggregate", "min / max", lambda I: I["emp"].agg(F.min("salary").alias("mn"), F.max("salary").alias("mx")))
case("agg-007", "aggregate", "sum(decimal) -> decimal promotion", lambda I: I["emp"].agg(F.sum("bonus").alias("sum_bonus")))
case("agg-008", "aggregate", "stddev / variance family", lambda I: I["emp"].agg(F.stddev("salary").alias("sd"), F.variance("salary").alias("var"), F.stddev_pop("salary").alias("sdp")))
case("agg-009", "aggregate", "skewness / kurtosis", lambda I: I["emp"].agg(F.skewness("salary").alias("sk"), F.kurtosis("salary").alias("ku")))
case("agg-010", "aggregate", "first / last (ignorenulls)", lambda I: I["emp"].agg(F.first("bonus", ignorenulls=True).alias("f"), F.last("bonus", ignorenulls=True).alias("l")))
case("agg-011", "aggregate", "collect_list / collect_set", lambda I: I["emp"].groupBy("dept_id").agg(F.collect_list("name").alias("names"), F.collect_set("active").alias("act_set")), flags=("schema_only",))
case("agg-012", "aggregate", "corr / covar", lambda I: I["emp"].agg(F.corr("age", "salary").alias("corr"), F.covar_samp("age", "salary").alias("cov")))
case("agg-013", "aggregate", "percentile_approx / median", lambda I: I["emp"].agg(F.percentile_approx("salary", 0.5).alias("p50"), F.median("salary").alias("med")))
case("agg-014", "aggregate", "mode", lambda I: I["emp"].groupBy("dept_id").agg(F.mode("active").alias("mode_active")))
case("agg-015", "aggregate", "groupBy single key", lambda I: I["emp"].groupBy("dept_id").agg(F.avg("salary").alias("avg_sal")))
case("agg-016", "aggregate", "groupBy multiple keys", lambda I: I["emp"].groupBy("dept_id", "active").agg(F.count(F.lit(1)).alias("n")))
case("agg-017", "aggregate", "groupBy on expression", lambda I: I["emp"].groupBy((F.col("age") >= 40).alias("senior")).agg(F.avg("salary").alias("avg_sal")))
case("agg-018", "aggregate", "multiple aggs with aliases", lambda I: I["emp"].groupBy("dept_id").agg(F.count(F.lit(1)).alias("n"), F.avg("age").alias("avg_age"), F.max("salary").alias("max_sal")))
case("agg-019", "aggregate", "agg over expression argument", lambda I: I["emp"].groupBy("dept_id").agg(F.sum(F.col("salary") + F.coalesce(F.col("bonus").cast("double"), F.lit(0.0))).alias("total_comp")))
case("agg-020", "aggregate", "count_if (conditional count)", lambda I: I["emp"].groupBy("dept_id").agg(F.count_if(F.col("active")).alias("n_active")))
case("agg-021", "aggregate", "bool aggs any / every (some/all)", lambda I: I["emp"].groupBy("dept_id").agg(F.expr("any(active)").alias("any_active"), F.expr("every(active)").alias("all_active")))
case("agg-022", "aggregate", "bit_and / bit_or / bit_xor", lambda I: I["nums"].agg(F.bit_and("a").alias("ba"), F.bit_or("a").alias("bo"), F.bit_xor("a").alias("bx")))
case("agg-023", "aggregate", "agg(count) immediately after filter", lambda I: I["emp"].filter(F.col("active")).groupBy("dept_id").agg(F.count(F.lit(1)).alias("n")))
case("agg-024", "aggregate", "aggregate then filter (HAVING analog)", lambda I: I["emp"].groupBy("dept_id").agg(F.avg("salary").alias("avg_sal")).filter(F.col("avg_sal") > 80000))

# ── 9. Grouping extensions (rollup / cube / pivot) ─────────────────────────
case("grp-001", "grouping", "rollup", lambda I: I["emp"].rollup("dept_id", "active").agg(F.count(F.lit(1)).alias("n")))
case("grp-002", "grouping", "cube", lambda I: I["emp"].cube("dept_id", "active").agg(F.count(F.lit(1)).alias("n")))
case("grp-003", "grouping", "rollup with grouping_id", lambda I: I["emp"].rollup("dept_id", "active").agg(F.grouping_id().alias("gid"), F.count(F.lit(1)).alias("n")))
case("grp-004", "grouping", "pivot with explicit values", lambda I: I["emp"].groupBy("dept_id").pivot("active", [True, False]).agg(F.count(F.lit(1)).alias("n")))
case("grp-005", "grouping", "pivot without values (eager)", lambda I: I["emp"].groupBy("active").pivot("dept_id").agg(F.avg("salary")))
case("grp-006", "grouping", "grouping() flag column", lambda I: I["emp"].cube("dept_id").agg(F.grouping("dept_id").alias("g"), F.count(F.lit(1)).alias("n")))

# ── 10. Window functions ───────────────────────────────────────────────────
case("win-001", "window", "row_number over partition+order", lambda I: I["emp"].withColumn("rn", F.row_number().over(W.partitionBy("dept_id").orderBy(F.col("salary").desc()))))
case("win-002", "window", "rank / dense_rank", lambda I: I["emp"].withColumn("rk", F.rank().over(W.partitionBy("dept_id").orderBy("salary"))).withColumn("drk", F.dense_rank().over(W.partitionBy("dept_id").orderBy("salary"))))
case("win-003", "window", "percent_rank / cume_dist", lambda I: I["emp"].withColumn("pr", F.percent_rank().over(W.orderBy("salary"))).withColumn("cd", F.cume_dist().over(W.orderBy("salary"))))
case("win-004", "window", "ntile(4)", lambda I: I["emp"].withColumn("q", F.ntile(4).over(W.orderBy("salary"))))
case("win-005", "window", "lag / lead with default", lambda I: I["emp"].withColumn("prev", F.lag("salary", 1, 0.0).over(W.partitionBy("dept_id").orderBy("hire_date"))).withColumn("next", F.lead("salary").over(W.partitionBy("dept_id").orderBy("hire_date"))))
case("win-006", "window", "nth_value", lambda I: I["emp"].withColumn("second", F.nth_value("salary", 2).over(W.partitionBy("dept_id").orderBy("salary"))))
case("win-007", "window", "first_value / last_value", lambda I: I["emp"].withColumn("fv", F.first("name").over(W.partitionBy("dept_id").orderBy("salary"))).withColumn("lv", F.last("name").over(W.partitionBy("dept_id").orderBy("salary"))))
case("win-008", "window", "running sum rowsBetween unbounded..current", lambda I: I["emp"].withColumn("run", F.sum("salary").over(W.partitionBy("dept_id").orderBy("hire_date").rowsBetween(W.unboundedPreceding, W.currentRow))))
case("win-009", "window", "moving avg rowsBetween(-1,1)", lambda I: I["emp"].withColumn("ma", F.avg("salary").over(W.partitionBy("dept_id").orderBy("hire_date").rowsBetween(-1, 1))))
case("win-010", "window", "rangeBetween", lambda I: I["emp"].withColumn("rb", F.sum("salary").over(W.partitionBy("dept_id").orderBy("age").rangeBetween(-5, 5))))
case("win-011", "window", "aggregate over partition (no order)", lambda I: I["emp"].withColumn("dept_avg", F.avg("salary").over(W.partitionBy("dept_id"))))
case("win-012", "window", "window then filter rn=1 (top-per-group)", lambda I: I["emp"].withColumn("rn", F.row_number().over(W.partitionBy("dept_id").orderBy(F.col("salary").desc()))).filter(F.col("rn") == 1))

# ── 11. Joins (nullability + disambiguation) ───────────────────────────────
case("join-001", "join", "inner join on column name", lambda I: I["emp"].join(I["dept"], on="dept_id", how="inner"))
case("join-002", "join", "inner join on condition", lambda I: I["emp"].join(I["dept"], I["emp"].dept_id == I["dept"].dept_id, "inner").select(I["emp"]["name"], I["dept"]["dept_name"]))
case("join-003", "join", "left outer -> right cols nullable", lambda I: I["emp"].join(I["dept"], on="dept_id", how="left"))
case("join-004", "join", "right outer", lambda I: I["emp"].join(I["dept"], on="dept_id", how="right"))
case("join-005", "join", "full outer -> both sides nullable", lambda I: I["emp"].join(I["dept"], on="dept_id", how="full"))
case("join-006", "join", "left_semi (no right cols)", lambda I: I["emp"].join(I["dept"], on="dept_id", how="left_semi"))
case("join-007", "join", "left_anti (rows with no match)", lambda I: I["emp"].join(I["dept"], on="dept_id", how="left_anti"))
case("join-008", "join", "cross join", lambda I: I["emp"].select("id", "dept_id").crossJoin(I["dept"].select(F.col("dept_id").alias("d2"), "country")))
case("join-009", "join", "self join (alias) on manager", lambda I: I["emp"].alias("e").join(I["emp"].alias("m"), F.col("e.manager_id") == F.col("m.id"), "left").select(F.col("e.name").alias("emp"), F.col("m.name").alias("mgr")))
case("join-010", "join", "join on list of keys", lambda I: I["emp"].select("dept_id", "active", "name").join(I["emp2"].select("dept_id", F.lit(True).alias("active"), F.col("name").alias("n2")), on=["dept_id", "active"], how="inner"))
case("join-011", "join", "join with complex condition (range)", lambda I: I["emp"].alias("e").join(I["dept"].alias("d"), (F.col("e.dept_id") == F.col("d.dept_id")) & (F.col("e.salary") > 70000), "inner").select("e.name", "d.dept_name"))
case("join-012", "join", "three-way join chain", lambda I: (I["emp"].join(I["dept"], on="dept_id", how="left")
    .join(I["emp2"].select(F.col("id").alias("e2id"), F.col("country").alias("c2")), I["dept"].country == F.col("c2"), "left")))
case("join-013", "join", "join then groupBy+agg", lambda I: I["emp"].join(I["dept"], on="dept_id").groupBy("dept_name").agg(F.avg("salary").alias("avg_sal")))
case("join-014", "join", "broadcast hint join (cosmetic)", lambda I: I["emp"].join(F.broadcast(I["dept"]), on="dept_id", how="inner"), flags=("cosmetic",))

# ── 12. Set operations (type widening) ──────────────────────────────────────
def _emp_proj(I):
    return I["emp"].select("id", "name", "dept_id", "age", "salary")

case("set-001", "setop", "union (positional)", lambda I: _emp_proj(I).union(I["emp2"].select("id", "name", "dept_id", "age", "salary")))
case("set-002", "setop", "unionAll (alias of union)", lambda I: _emp_proj(I).unionAll(I["emp2"].select("id", "name", "dept_id", "age", "salary")))
case("set-003", "setop", "unionByName (column name match)", lambda I: _emp_proj(I).unionByName(I["emp2"].select("salary", "age", "dept_id", "name", "id")))
case("set-004", "setop", "unionByName allowMissingColumns", lambda I: _emp_proj(I).unionByName(I["emp2"], allowMissingColumns=True))
case("set-005", "setop", "intersect", lambda I: _emp_proj(I).intersect(_emp_proj(I).filter(F.col("age") > 30)))
case("set-006", "setop", "intersectAll", lambda I: _emp_proj(I).intersectAll(_emp_proj(I)))
case("set-007", "setop", "exceptAll", lambda I: _emp_proj(I).exceptAll(_emp_proj(I).filter(F.col("age") < 30)))
case("set-008", "setop", "subtract", lambda I: _emp_proj(I).subtract(_emp_proj(I).filter(F.col("dept_id") == 10)))
case("set-009", "setop", "union with int/double widening", lambda I: I["nums"].select(F.col("a").alias("v")).union(I["nums"].select(F.col("x").alias("v"))))
case("set-010", "setop", "union then distinct then orderBy", lambda I: _emp_proj(I).union(I["emp2"].select("id", "name", "dept_id", "age", "salary")).distinct().orderBy("id"))

# ── 13. Ordering, limit, offset, distinct ───────────────────────────────────
case("ord-001", "ordering", "orderBy asc", lambda I: I["emp"].orderBy("salary"))
case("ord-002", "ordering", "orderBy desc", lambda I: I["emp"].orderBy(F.col("salary").desc()))
case("ord-003", "ordering", "sort multiple keys", lambda I: I["emp"].sort(F.col("dept_id").asc(), F.col("salary").desc()))
case("ord-004", "ordering", "asc_nulls_first / desc_nulls_last", lambda I: I["emp"].orderBy(F.col("dept_id").asc_nulls_first(), F.col("bonus").desc_nulls_last()))
case("ord-005", "ordering", "orderBy with ascending list", lambda I: I["emp"].orderBy(["dept_id", "salary"], ascending=[True, False]))
case("ord-006", "ordering", "sortWithinPartitions", lambda I: I["emp"].repartition(2, "dept_id").sortWithinPartitions("salary"), flags=("schema_only",))
case("ord-007", "ordering", "limit", lambda I: I["emp"].orderBy("id").limit(3))
case("ord-008", "ordering", "offset (spark4)", lambda I: I["emp"].orderBy("id").offset(2), flags=("spark4",))
case("ord-009", "ordering", "offset + limit (pagination)", lambda I: I["emp"].orderBy("id").offset(2).limit(3), flags=("spark4",))
case("ord-010", "ordering", "distinct", lambda I: I["emp"].select("dept_id", "active").distinct())
case("ord-011", "ordering", "dropDuplicates subset", lambda I: I["emp"].dropDuplicates(["dept_id"]))
case("ord-012", "ordering", "dropDuplicates all columns equivalent", lambda I: I["emp"].select("dept_id", "active").dropDuplicates())

# ── 14. Complex types: arrays ───────────────────────────────────────────────
case("arr-001", "array", "array() constructor", lambda I: I["emp"].select(F.array(F.col("age"), F.lit(0)).alias("arr")))
case("arr-002", "array", "array_contains", lambda I: I["emp"].select(F.array_contains("tags", "rust").alias("has_rust")))
case("arr-003", "array", "size (null vs empty)", lambda I: I["emp"].select(F.size("tags").alias("n_tags")))
case("arr-004", "array", "sort_array asc/desc", lambda I: I["emp"].select(F.sort_array("tags").alias("sorted"), F.sort_array("tags", asc=False).alias("rsorted")))
case("arr-005", "array", "array_distinct", lambda I: I["emp"].select(F.array_distinct(F.array("tags", "tags")).alias("d")), flags=("schema_only",))
case("arr-006", "array", "array_max / array_min", lambda I: I["emp"].select(F.array_max("tags").alias("mx"), F.array_min("tags").alias("mn")))
case("arr-007", "array", "array_position", lambda I: I["emp"].select(F.array_position("tags", "rust").alias("pos")))
case("arr-008", "array", "element_at (1-based) on array", lambda I: I["emp"].select(F.element_at("tags", 1).alias("first")), expected_error="INVALID_ARRAY_INDEX_IN_ELEMENT_AT")
case("arr-009", "array", "slice", lambda I: I["emp"].select(F.slice("tags", 1, 2).alias("sl")))
case("arr-010", "array", "array_join", lambda I: I["emp"].select(F.array_join("tags", ",", "NULL").alias("j")))
case("arr-011", "array", "arrays_overlap / array_union", lambda I: I["emp"].select(F.arrays_overlap("tags", F.array(F.lit("rust"))).alias("ov"), F.array_union("tags", F.array(F.lit("x"))).alias("u")))
case("arr-012", "array", "arrays_zip", lambda I: I["emp"].select(F.arrays_zip("tags", "tags").alias("z")))
case("arr-013", "array", "flatten", lambda I: I["emp"].select(F.flatten(F.array("tags", "tags")).alias("fl")))
case("arr-014", "array", "sequence", lambda I: I["emp"].select(F.sequence(F.lit(1), F.col("age").cast("int"), F.lit(10)).alias("seq")))
case("arr-015", "array", "explode (drops null/empty rows)", lambda I: I["emp"].select("id", F.explode("tags").alias("tag")))
case("arr-016", "array", "explode_outer (keeps null/empty)", lambda I: I["emp"].select("id", F.explode_outer("tags").alias("tag")))
case("arr-017", "array", "posexplode (adds pos col)", lambda I: I["emp"].select("id", F.posexplode("tags").alias("pos", "tag")))

# ── 15. Complex types: maps ─────────────────────────────────────────────────
case("map-001", "map", "create_map", lambda I: I["emp"].select(F.create_map(F.lit("k"), F.col("name")).alias("m")))
case("map-002", "map", "map_keys / map_values", lambda I: I["emp"].select(F.map_keys("attrs").alias("ks"), F.map_values("attrs").alias("vs")))
case("map-003", "map", "map_entries", lambda I: I["emp"].select(F.map_entries("attrs").alias("ents")))
case("map-004", "map", "element_at on map (-> value type, nullable)", lambda I: I["emp"].select(F.element_at("attrs", F.lit("team")).alias("team")))
case("map-005", "map", "map_from_arrays", lambda I: I["emp"].select(F.map_from_arrays(F.array(F.lit("a")), F.array(F.col("name"))).alias("m")))
case("map-006", "map", "map_concat", lambda I: I["emp"].select(F.map_concat("attrs", F.create_map(F.lit("extra"), F.lit("1"))).alias("m")))
case("map-007", "map", "explode map -> key,value cols", lambda I: I["emp"].select("id", F.explode("attrs").alias("k", "v")))

# ── 16. Complex types: structs ──────────────────────────────────────────────
case("struct-001", "struct", "struct() constructor", lambda I: I["emp"].select(F.struct("name", "age").alias("info")))
case("struct-002", "struct", "dot access nested field", lambda I: I["emp"].select(F.col("address.city").alias("city")))
case("struct-003", "struct", "getField", lambda I: I["emp"].select(F.col("address").getField("zip").alias("zip")))
case("struct-004", "struct", "deep nested access (struct in struct)", lambda I: I["emp"].select(F.col("address.geo.lat").alias("lat")))
case("struct-005", "struct", "withField (add/replace nested)", lambda I: I["emp"].withColumn("address", F.col("address").withField("country", F.lit("AT"))))
case("struct-006", "struct", "dropFields", lambda I: I["emp"].withColumn("address", F.col("address").dropFields("geo")))
case("struct-007", "struct", "named_struct of expressions", lambda I: I["emp"].select(F.expr("named_struct('hi', salary > 100000, 'agep1', age + 1)").alias("s")))
case("struct-008", "struct", "star-expand a struct", lambda I: I["emp"].select("id", "address.*"))

# ── 17. Higher-order functions ──────────────────────────────────────────────
case("hof-001", "hof", "transform (array map)", lambda I: I["emp"].select(F.transform("tags", lambda x: F.upper(x)).alias("up_tags")))
case("hof-002", "hof", "filter (array)", lambda I: I["emp"].select(F.filter("tags", lambda x: x.startswith("r")).alias("r_tags")))
case("hof-003", "hof", "aggregate (fold)", lambda I: I["emp"].select(F.aggregate("tags", F.lit(""), lambda acc, x: F.concat(acc, x)).alias("cat")))
case("hof-004", "hof", "exists", lambda I: I["emp"].select(F.exists("tags", lambda x: x == F.lit("rust")).alias("has_rust")))
case("hof-005", "hof", "forall", lambda I: I["emp"].select(F.forall("tags", lambda x: F.length(x) > 0).alias("all_nonempty")))
case("hof-006", "hof", "zip_with", lambda I: I["emp"].select(F.zip_with("tags", "tags", lambda a, b: F.concat(a, b)).alias("zw")))
case("hof-007", "hof", "transform with index (2-arg lambda)", lambda I: I["emp"].select(F.transform("tags", lambda x, i: F.concat(i.cast("string"), F.lit(":"), x)).alias("idx_tags")))
case("hof-008", "hof", "map_filter", lambda I: I["emp"].select(F.map_filter("attrs", lambda k, v: k == F.lit("team")).alias("only_team")))
case("hof-009", "hof", "transform_values", lambda I: I["emp"].select(F.transform_values("attrs", lambda k, v: F.upper(v)).alias("up_vals")))
case("hof-010", "hof", "transform_keys", lambda I: I["emp"].select(F.transform_keys("attrs", lambda k, v: F.concat(F.lit("attr_"), k)).alias("pref_keys")))

# ── 18. na / stats / misc DataFrame methods ─────────────────────────────────
case("misc-001", "misc", "describe (summary stats)", lambda I: I["emp"].describe("age", "salary"), flags=("schema_only",))
case("misc-002", "misc", "summary with percentiles", lambda I: I["emp"].select("salary").summary("count", "min", "25%", "75%", "max"), flags=("schema_only",))
case("misc-003", "misc", "fillna (DataFrame alias)", lambda I: I["emp"].fillna({"dept_id": 0}))
case("misc-004", "misc", "dropna (DataFrame alias)", lambda I: I["emp"].dropna(subset=["dept_id"]))
case("misc-005", "misc", "replace (DataFrame alias)", lambda I: I["emp"].replace([10, 20], [110, 120], subset=["dept_id"]))
case("misc-006", "misc", "crosstab", lambda I: I["emp"].crosstab("dept_id", "active"), flags=("schema_only",))
case("misc-007", "misc", "freqItems", lambda I: I["emp"].freqItems(["dept_id"], support=0.3), flags=("schema_only",))
case("misc-008", "misc", "repartition (cosmetic, result-irrelevant)", lambda I: I["emp"].repartition(4, "dept_id"), flags=("cosmetic",))
case("misc-009", "misc", "coalesce partitions (cosmetic)", lambda I: I["emp"].coalesce(1), flags=("cosmetic",))
case("misc-010", "misc", "hint (cosmetic)", lambda I: I["emp"].hint("broadcast"), flags=("cosmetic",))

# ── 19. Type-inference & nullability stress (the divergent slice) ───────────
case("type-001", "type_inference", "int + long -> long", lambda I: I["nums"].select((F.col("a") + F.col("lng")).alias("r")))
case("type-002", "type_inference", "int + double -> double", lambda I: I["nums"].select((F.col("a") + F.col("x")).alias("r")))
case("type-003", "type_inference", "decimal(10,2) + decimal(6,3) -> promoted decimal", lambda I: I["nums"].select((F.col("d1") + F.col("d2")).alias("r")))
case("type-004", "type_inference", "decimal * decimal -> scale sum", lambda I: I["nums"].select((F.col("d1") * F.col("d2")).alias("r")))
case("type-005", "type_inference", "decimal / decimal -> Spark decimal result type", lambda I: I["nums"].select((F.col("d1") / F.col("d2")).alias("r")))
case("type-006", "type_inference", "int / int -> double (NOT int)", lambda I: I["nums"].select((F.col("a") / F.lit(2)).alias("r")))
case("type-007", "type_inference", "div (integer division operator) keeps integral", lambda I: I["nums"].select(F.expr("a div 2").alias("r")))
case("type-008", "type_inference", "literal None typed via lit().cast()", lambda I: I["emp"].select(F.lit(None).cast("timestamp").alias("nt")))
case("type-009", "type_inference", "when/otherwise type unification (int vs double)", lambda I: I["emp"].select(F.when(F.col("active"), F.lit(1)).otherwise(F.lit(2.5)).alias("u")))
case("type-010", "type_inference", "coalesce(nullable, lit) -> non-nullable result", lambda I: I["emp"].select(F.coalesce(F.col("dept_id"), F.lit(0)).alias("dept_nn")))
case("type-011", "type_inference", "outer-join column becomes nullable even from non-null source", lambda I: I["dept"].join(I["emp"].select("dept_id", F.col("id").alias("eid")), on="dept_id", how="left"))
case("type-012", "type_inference", "explode element nullability follows array element type", lambda I: I["emp"].select(F.explode(F.array(F.lit(1), F.lit(None).cast("int"))).alias("v")))
case("type-013", "type_inference", "count(*) is non-nullable long; sum is nullable", lambda I: I["emp"].groupBy("dept_id").agg(F.count(F.lit(1)).alias("n_nonnull"), F.sum("bonus").alias("sum_nullable")))
case("type-014", "type_inference", "avg always double even on long input", lambda I: I["nums"].agg(F.avg("lng").alias("avg_lng")))
case("type-015", "type_inference", "string concat with null -> null (Spark) vs skip", lambda I: I["emp"].select(F.concat(F.col("name"), F.lit(None).cast("string")).alias("c")))
case("type-016", "type_inference", "concat_ws skips null (contrast with concat)", lambda I: I["emp"].select(F.concat_ws("-", F.col("name"), F.lit(None).cast("string")).alias("c")))
case("type-017", "type_inference", "greatest/least ignore nulls", lambda I: I["emp"].select(F.greatest(F.col("bonus"), F.lit(None).cast("decimal(9,2)")).alias("g")))
case("type-018", "type_inference", "map value type is nullable", lambda I: I["emp"].select(F.element_at("attrs", F.lit("missing")).alias("maybe")))
case("type-019", "type_inference", "set-op widening int ∪ decimal", lambda I: I["nums"].select(F.col("a").cast("decimal(5,0)").alias("v")).unionByName(I["nums"].select(F.col("d1").alias("v"))))
case("type-020", "type_inference", "array of mixed numeric literals -> least common type", lambda I: I["emp"].select(F.array(F.lit(1), F.lit(2.0), F.lit(3)).alias("mixed")))
case("type-021", "type_inference", "boolean predicate column is non-null when operands non-null", lambda I: I["emp"].select((F.col("age") > 30).alias("pred")))
case("type-022", "type_inference", "cast widening preserves null; narrowing may null (try)", lambda I: I["nums"].select(F.expr("try_cast(lng as int)").alias("maybe_overflow")), flags=("spark4",))

# ── 20. Deep integration chains (10–20 ops; the realistic biased sample) ────
case("chain-001", "integration", "filter→withColumn(when)→join→groupBy→agg→window→filter→order→select→limit (≈14 ops)", lambda I: (
    I["emp"]
      .filter(F.col("age").between(25, 60))
      .withColumn("comp", F.col("salary") + F.coalesce(F.col("bonus").cast("double"), F.lit(0.0)))
      .withColumn("band", F.when(F.col("comp") > 100000, "high").when(F.col("comp") > 75000, "mid").otherwise("low"))
      .join(I["dept"], on="dept_id", how="left")
      .groupBy("dept_name", "band")
      .agg(F.count(F.lit(1)).alias("n"), F.avg("comp").alias("avg_comp"))
      .withColumn("rk", F.row_number().over(W.partitionBy("dept_name").orderBy(F.col("avg_comp").desc())))
      .filter(F.col("rk") <= 2)
      .orderBy("dept_name", "rk")
      .select("dept_name", "band", "n", "avg_comp")
      .limit(20)))
case("chain-002", "integration", "explode tags→join→agg→pivot→fillna→order (≈10 ops)", lambda I: (
    I["emp"]
      .select("id", "dept_id", F.explode_outer("tags").alias("tag"))
      .join(I["dept"], on="dept_id", how="inner")
      .groupBy("dept_name")
      .pivot("tag")
      .agg(F.count(F.lit(1)))
      .na.fill(0)
      .orderBy("dept_name")))
case("chain-003", "integration", "union→distinct→withColumn(string fns)→filter(rlike)→window dedup→drop (≈12 ops)", lambda I: (
    _emp_proj(I)
      .unionByName(I["emp2"].select("id", "name", "dept_id", "age", "salary"))
      .distinct()
      .withColumn("nkey", F.lower(F.regexp_replace("name", r"\s+", "_")))
      .filter(F.col("nkey").rlike("^[a-z]"))
      .withColumn("rn", F.row_number().over(W.partitionBy("dept_id").orderBy(F.col("salary").desc())))
      .filter(F.col("rn") == 1)
      .drop("rn")
      .orderBy("dept_id")))
case("chain-004", "integration", "nested struct/map/array projection→hof→explode→agg (≈11 ops)", lambda I: (
    I["emp"]
      .withColumn("city", F.col("address.city"))
      .withColumn("up_tags", F.transform("tags", lambda x: F.upper(x)))
      .withColumn("team", F.element_at("attrs", F.lit("team")))
      .select("id", "city", "team", F.explode_outer("up_tags").alias("tag"))
      .filter(F.col("tag").isNotNull())
      .groupBy("city", "team")
      .agg(F.collect_set("tag").alias("tags"), F.count(F.lit(1)).alias("n"))
      .orderBy(F.col("n").desc(), "city")), flags=("schema_only",))
case("chain-005", "integration", "self-join hierarchy→date math→agg→having→order (≈13 ops)", lambda I: (
    I["emp"].alias("e")
      .join(I["emp"].alias("m"), F.col("e.manager_id") == F.col("m.id"), "left")
      .select(F.col("e.name").alias("emp"), F.col("m.name").alias("mgr"),
              F.col("e.dept_id").alias("dept_id"), F.col("e.hire_date").alias("hd"),
              F.col("e.salary").alias("sal"))
      .withColumn("tenure_days", F.datediff(F.lit("2026-06-30").cast("date"), F.col("hd")))
      .withColumn("tenure_yrs", (F.col("tenure_days") / 365.25))
      .groupBy("dept_id")
      .agg(F.avg("tenure_yrs").alias("avg_tenure"), F.max("sal").alias("max_sal"), F.count(F.lit(1)).alias("n"))
      .filter(F.col("n") >= 1)
      .orderBy(F.col("avg_tenure").desc())))
case("chain-006", "integration", "cube→grouping_id→cast→when→order (≈9 ops)", lambda I: (
    I["emp"]
      .join(I["dept"], on="dept_id", how="left")
      .cube("country", "active")
      .agg(F.grouping_id().alias("gid"), F.avg("salary").alias("avg_sal"), F.count(F.lit(1)).alias("n"))
      .withColumn("level", F.when(F.col("gid") == 0, "leaf").otherwise("subtotal"))
      .orderBy("gid", "country")))


# ===========================================================================
# EXPANSION — additional DataFrame-API families beyond the base set.
# (The SQL front end — correlated subqueries, CTEs, etc. — is a separate corpus.)
# ===========================================================================


# ── 21. unpivot / melt / stack (wide <-> long) ──────────────────────────────
case("piv-004", "pivot_unpivot", "DataFrame unpivot (wide->long)", lambda I: I["emp"].select("id", "age", "salary").unpivot(["id"], ["age", "salary"], "metric", "value"))
case("piv-005", "pivot_unpivot", "DataFrame melt (alias of unpivot)", lambda I: I["emp"].select("id", "age", "salary").melt(["id"], ["age", "salary"], "metric", "value"))
case("piv-006", "pivot_unpivot", "stack() to long form (age cast to DOUBLE to share a type with salary)", lambda I: I["emp"].select("id", F.expr("stack(2, 'age', CAST(age AS DOUBLE), 'salary', salary) as (metric, value)")))

# ── 22. JSON functions ──────────────────────────────────────────────────────
case("json-001", "json", "get_json_object (path extract)", lambda I: I["raw"].select(F.get_json_object("json_str", "$.a").alias("a")))
case("json-002", "json", "json_tuple (multi-field)", lambda I: I["raw"].select("id", F.json_tuple("json_str", "a", "e")))
case("json-003", "json", "from_json with explicit schema", lambda I: I["raw"].select(F.from_json("json_str", "a INT, b ARRAY<STRING>, e DOUBLE").alias("parsed")))
case("json-004", "json", "from_json then field access", lambda I: I["raw"].select(F.from_json("json_str", "a INT, c STRUCT<d:BOOLEAN>").getField("c").getField("d").alias("d")))
case("json-005", "json", "to_json (struct -> json string)", lambda I: I["emp"].select(F.to_json(F.struct("name", "age", "address")).alias("j")))
case("json-006", "json", "schema_of_json (infer schema string)", lambda I: I["raw"].select(F.schema_of_json(F.lit('{"a":1,"b":"x"}')).alias("schema")))
case("json-007", "json", "from_csv with schema", lambda I: I["raw"].select(F.from_csv("csv_str", "qty INT, label STRING, price DOUBLE").alias("rec")))
case("json-008", "json", "to_csv (struct -> csv string)", lambda I: I["emp"].select(F.to_csv(F.struct("id", "name", "age")).alias("c")))

# ── 23. Hashing / checksum functions ────────────────────────────────────────
case("hash-001", "hashing", "md5 / sha1 / crc32", lambda I: I["emp"].select(F.md5("name").alias("md5"), F.sha1("name").alias("sha1"), F.crc32(F.col("name").cast("binary")).alias("crc")))
case("hash-002", "hashing", "sha2 with bit length", lambda I: I["emp"].select(F.sha2("name", 256).alias("sha256")))
case("hash-003", "hashing", "hash / xxhash64 (multi-col)", lambda I: I["emp"].select(F.hash("name", "dept_id").alias("h"), F.xxhash64("name", "salary").alias("xx")))

# ── 24. Interval types & temporal arithmetic ────────────────────────────────
case("intv-001", "interval", "make_interval -> CalendarInterval", lambda I: I["emp"].select(F.expr("make_interval(1, 2, 0, 5)").alias("iv")))
case("intv-002", "interval", "make_ym_interval -> YearMonthInterval type", lambda I: I["emp"].select(F.expr("make_ym_interval(2, 3)").alias("ymi")))
case("intv-003", "interval", "make_dt_interval -> DayTimeInterval type", lambda I: I["emp"].select(F.expr("make_dt_interval(1, 2, 30, 0)").alias("dti")))
case("intv-004", "interval", "date + INTERVAL literal", lambda I: I["emp"].select((F.col("hire_date") + F.expr("INTERVAL 90 DAYS")).alias("later")))
case("intv-005", "interval", "timestamp difference -> DayTimeInterval", lambda I: I["emp"].select((F.col("last_login") - F.col("last_login")).alias("zero_iv")))
case("intv-006", "interval", "timestampadd / timestampdiff (spark4)", lambda I: I["emp"].select(F.expr("timestampadd(MONTH, 3, last_login)").alias("ta"), F.expr("timestampdiff(DAY, hire_date, current_date())").alias("td")), flags=("spark4", "nondeterministic"))

# ── 25. Newer array / map functions ─────────────────────────────────────────
case("arr2-001", "array_new", "array_append / array_prepend", lambda I: I["emp"].select(F.array_append("tags", "new").alias("ap"), F.array_prepend("tags", "first").alias("pp")))
case("arr2-002", "array_new", "array_insert (1-based position)", lambda I: I["emp"].select(F.array_insert("tags", 1, "head").alias("ai")))
case("arr2-003", "array_new", "array_compact (drop nulls)", lambda I: I["emp"].select(F.array_compact(F.array(F.col("name"), F.lit(None).cast("string"))).alias("ac")))
case("arr2-004", "array_new", "array_size (vs size: null->null differs)", lambda I: I["emp"].select(F.array_size("tags").alias("asz")))
case("arr2-005", "array_new", "array_remove / array_except / array_intersect", lambda I: I["emp"].select(F.array_remove("tags", "rust").alias("rm"), F.array_intersect("tags", F.array(F.lit("rust"))).alias("ix")))
case("map2-001", "map_new", "map_contains_key", lambda I: I["emp"].select(F.map_contains_key("attrs", F.lit("team")).alias("has_team")))
case("map2-002", "map_new", "str_to_map", lambda I: I["raw"].select(F.str_to_map(F.lit("a:1,b:2"), F.lit(","), F.lit(":")).alias("m")))

# ── 26. inline / explode-of-structs ─────────────────────────────────────────
case("inl-001", "inline", "inline (array<struct> -> columns)", lambda I: I["emp"].select("id", F.inline(F.array(F.struct(F.col("name"), F.col("age"))))))
case("inl-002", "inline", "inline_outer (keeps null/empty array rows)", lambda I: I["emp"].select("id", F.inline_outer(F.array(F.struct(F.col("dept_id"), F.col("salary"))))))

# ── 27. URL / number-format / set lookup parsing ────────────────────────────
case("parse-001", "parsing", "parse_url (host/query/protocol)", lambda I: I["raw"].select(F.expr("parse_url(url, 'HOST')").alias("host"), F.expr("parse_url(url, 'QUERY', 'q')").alias("q")), flags=("spark4",))
case("parse-002", "parsing", "url_encode / url_decode", lambda I: I["raw"].select(F.expr("url_encode('a b&c')").alias("enc")), flags=("spark4",))
case("parse-003", "parsing", "to_number with format -> decimal (row 2 mismatch throws ANSI)", lambda I: I["raw"].select(F.expr("to_number(num_str, '9,999.99')").alias("n")), flags=("spark4",), expected_error="INVALID_FORMAT.MISMATCH_INPUT")
case("parse-004", "parsing", "try_to_number (null on bad input)", lambda I: I["raw"].select(F.expr("try_to_number(num_str, '999.99')").alias("n")), flags=("spark4",))
# Grouping-separator value-parity witnesses (guard against the DuckDB
# `try_cast` regression where '1,234.56' silently drops to NULL because
# DuckDB's numeric cast does not strip `,`).  See ADR-015.
# parse-003b uses a nullable column input (`num_str` filtered to the sole
# valid grouping row) rather than a string literal — Spark reports
# `to_number(<non-null literal>, <literal>)` as non-nullable, which is a
# separate τ nullability inference gap outside the scope of the input-strip
# fix.  Value parity is what this witness locks in.
case("parse-003b", "parsing", "to_number(num_str, '9,999.99') on grouped input -> 1234.56", lambda I: I["raw"].filter(F.col("id") == 1).select(F.expr("to_number(num_str, '9,999.99')").alias("n")), flags=("spark4",))
case("parse-004b", "parsing", "try_to_number literal with grouping -> DECIMAL(6,2) 1234.56", lambda I: I["raw"].select(F.expr("try_to_number('1,234.56', '9,999.99')").alias("n")), flags=("spark4",))
case("parse-004c", "parsing", "try_to_number bogus input with grouping fmt -> NULL", lambda I: I["raw"].select(F.expr("try_to_number('bogus', '9,999.99')").alias("n")), flags=("spark4",))
case("parse-005", "parsing", "find_in_set", lambda I: I["emp"].select(F.expr("find_in_set('rust', concat_ws(',', tags))").alias("pos")))
case("parse-006", "parsing", "split_part (1-based field)", lambda I: I["raw"].select(F.expr("split_part(csv_str, ',', 2)").alias("field2")), flags=("spark4",))
case("parse-007", "parsing", "elt (1-based pick from args)", lambda I: I["emp"].select(F.expr("elt(2, 'a', name, 'c')").alias("picked")))

# ── 28. Metadata / id / nondeterministic functions ──────────────────────────
case("meta-001", "metadata", "monotonically_increasing_id", lambda I: I["emp"].select("id", F.monotonically_increasing_id().alias("mid")), flags=("nondeterministic",))
case("meta-002", "metadata", "spark_partition_id (cosmetic/physical)", lambda I: I["emp"].select(F.spark_partition_id().alias("pid")), flags=("nondeterministic", "schema_only"))
case("meta-003", "metadata", "typeof (runtime type string)", lambda I: I["emp"].select(F.expr("typeof(salary)").alias("t"), F.expr("typeof(bonus)").alias("tb")))
case("meta-004", "metadata", "input_file_name (empty for in-memory)", lambda I: I["emp"].select(F.input_file_name().alias("f")), flags=("nondeterministic", "schema_only"))

# ── 29. Sampling ────────────────────────────────────────────────────────────
case("samp-001", "sampling", "sample with seed", lambda I: I["emp"].sample(0.5, seed=11), flags=("nondeterministic",))
case("samp-002", "sampling", "sampleBy stratified", lambda I: I["emp"].sampleBy("dept_id", {10: 0.5, 20: 0.5, 30: 1.0}, seed=11), flags=("nondeterministic",))

# ── 30. Additional aggregates ───────────────────────────────────────────────
case("agg2-001", "aggregate_new", "any_value", lambda I: I["emp"].groupBy("dept_id").agg(F.any_value("name").alias("a")), flags=("schema_only",))
case("agg2-002", "aggregate_new", "array_agg (alias of collect_list)", lambda I: I["emp"].groupBy("dept_id").agg(F.array_agg("name").alias("names")), flags=("schema_only",))
case("agg2-003", "aggregate_new", "regression aggregates (regr_slope/regr_r2)", lambda I: I["emp"].agg(F.regr_slope("salary", "age").alias("slope"), F.regr_r2("salary", "age").alias("r2")))
case("agg2-004", "aggregate_new", "try_sum / try_avg (overflow-safe, spark4)", lambda I: I["nums"].agg(F.expr("try_sum(lng)").alias("ts"), F.expr("try_avg(a)").alias("ta")), flags=("spark4",))
case("agg2-005", "aggregate_new", "histogram_numeric", lambda I: I["emp"].agg(F.expr("histogram_numeric(salary, 3)").alias("hist")), flags=("schema_only",))
case("agg2-006", "aggregate_new", "count_if + filtered agg combination", lambda I: I["emp"].groupBy("dept_id").agg(F.count_if(F.col("salary") > 90000).alias("n_high"), F.avg(F.when(F.col("active"), F.col("salary"))).alias("avg_active_sal")))

# ── 31. Structural DataFrame methods ────────────────────────────────────────
case("struc-001", "structural", "toDF (positional rename all columns)", lambda I: I["dept"].toDF("d_id", "d_name", "d_budget", "d_loc", "d_country"))
case("struc-002", "structural", "colRegex (select by pattern)", lambda I: I["emp"].select(I["emp"].colRegex("`.*_id`")))
case("struc-003", "structural", "repartitionByRange (cosmetic)", lambda I: I["emp"].repartitionByRange(3, "salary"), flags=("cosmetic",))
case("struc-004", "structural", "withMetadata (column metadata, schema-affecting)", lambda I: I["emp"].withMetadata("salary", {"unit": "USD"}), flags=("schema_only",))
case("struc-005", "structural", "selectExpr with star and exclude-like rebuild", lambda I: I["emp"].selectExpr("id", "name", "age + 1 AS age1", "salary / 1000 AS sal_k"))
case("struc-006", "structural", "reduce HOF (alias of aggregate, spark4)", lambda I: I["emp"].select(F.expr("reduce(tags, '', (acc, x) -> concat(acc, x))").alias("cat")), flags=("spark4",))

# ── 32. Time-window aggregate (tumbling) ────────────────────────────────────
case("win2-002", "window_time", "tumbling time window aggregate", lambda I: I["emp"].filter(F.col("last_login").isNotNull()).groupBy(F.window("last_login", "1 day")).agg(F.count(F.lit(1)).alias("n")))


# ---------------------------------------------------------------------------
# Coverage summary + optional self-check runner
# ---------------------------------------------------------------------------

def coverage() -> Dict[str, int]:
    counts: Dict[str, int] = {}
    for c in CASES:
        counts[c.category] = counts.get(c.category, 0) + 1
    return dict(sorted(counts.items(), key=lambda kv: kv[0]))


def run(spark: SparkSession, only: Tuple[str, ...] = (), collect: bool = False):
    """Build every case against `spark`; optionally collect rows. Returns a report.

    Use as the reference side of the differential oracle (ADR-015), then repeat
    against the thunderduck session and diff `schema` (and `rows` when collect=True).
    Cases flagged 'nondeterministic' should be compared by schema only.
    """
    inputs = build_inputs(spark)
    report = []
    for c in CASES:
        if only and c.category not in only:
            continue
        entry = {"id": c.id, "category": c.category, "flags": c.flags}
        try:
            df = c.build(inputs)
            entry["schema"] = df.schema.json()
            if collect and "nondeterministic" not in c.flags:
                entry["rows"] = [r.asDict(recursive=True) for r in df.limit(1000).collect()]
            entry["status"] = "ok"
        except Exception as exc:  # surface build/analysis errors per-case
            entry["status"] = "error"
            entry["error"] = f"{type(exc).__name__}: {exc}"
        report.append(entry)
    return report


if __name__ == "__main__":
    print(f"Total cases: {len(CASES)}")
    for cat, n in coverage().items():
        print(f"  {cat:16s} {n:3d}")
    # To exercise against a live Spark:
    #   spark = SparkSession.builder.master("local[*]").getOrCreate()
    #   import json; print(json.dumps(run(spark, collect=True), indent=2, default=str))
