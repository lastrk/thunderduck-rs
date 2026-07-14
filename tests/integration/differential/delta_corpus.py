"""
thunderduck — Delta Lake read/write conformance corpus
======================================================

The **command / state-diff** counterpart to the query-only DataFrame and SQL
corpora. Per ADR-011 side-effecting statements (writes, MERGE) are validated by a
*state-diff oracle* — read-after-write logical-row comparison through both engines
— not by diffing a query result set. This corpus is that oracle's case registry
for the Delta Lake read + write surface, across **both** front-ends (DataFrame API
and Spark SQL).

What this covers (the full target surface, per the integration investigation)
------------------------------------------------------------------------------
- **Read**: `read.format("delta").load(path)` / `SELECT * FROM delta.`path`` from
  a local filesystem path (and, opt-in, S3), including a type-rich table and a
  partitioned table (partition-column typing is ADR-013's exposed divergence
  surface).
- **Write** (`df.write` and the SQL equivalents):
    1. parquet `mode(*).save`                          (non-Delta baseline)
    2. delta `mode("overwrite")`                       (FullOverwrite)
    3. delta `mode("overwrite").option("replaceWhere")`(OverwriteNoveltyRange/Custom)
    4. delta `mode("append")`                          (Append)
    5. delta `mode("overwrite")` into an empty path    (bootstrap-create)
- **MERGE** — expressed as SQL `MERGE INTO` (a temp-view source), which is the
  Spark-Connect-supported and τ-reachable form (the Python `DeltaTable.merge`
  builder accesses `SparkContext` and breaks over Connect). The three builder
  shapes map 1:1 to clause sets:
    - Upsert     — `WHEN MATCHED UPDATE` + `WHEN NOT MATCHED INSERT`
    - InsertOnly — `WHEN NOT MATCHED INSERT`
    - Custom     — per-clause update / delete / insert with clause conditions

Target-coverage semantics (ADR-017 / ADR-022)
---------------------------------------------
Delta writes are DuckDB-version-gated: today the extension supports read + blind
`append` into an *attached, pre-existing* table only; overwrite / replaceWhere /
bootstrap-create / MERGE are typed rejections pending duckdb-delta support (the
cross-repo dev loop exists to lift them). Cases for the not-yet-supported shapes
carry `expected_unsupported=<revisit-trigger>`; the driver maps that to a **strict
xfail**, so they:
  - do NOT count as hard failures while the gap is real, and
  - flip to a loud `xpass` (test failure) the moment τ + the extension implement
    them — the signal to drop the flag.

The oracle model (implemented in `test_delta_corpus_differential.py`)
---------------------------------------------------------------------
Delta tables can only be *created* by the reference (delta-spark) engine — τ's
create-on-write is out of scope (ADR-017). So the harness seeds every pre-existing
table (both engines' isolated copies) with the reference session, then runs the
action under test on the respective engine and reads the final state back through
that same engine. This mirrors reality: τ appends to a Delta table another writer
created (INV9: writable ⇒ attached + pre-existing).

Flags
-----
  "s3"          : requires a configured Delta-S3 endpoint (env
                  THUNDERDUCK_DELTA_S3_ENDPOINT); skipped otherwise. A local-FS
                  twin always runs so the shape is covered without minio.
  "schema_only" : compare schema only.

Conventions
-----------
  Reused compact, type-diverse inputs (see PEOPLE_* / TYPES_*). Pinned to Spark
  4.1.1 + delta-spark 4.3.0 (Delta Connect). The `delta.`path`` SQL syntax and the
  `format("delta")` reader require the reference server's DeltaCatalog +
  DeltaSparkSessionExtension (see tests/scripts/start-spark-4.1.1-reference.sh).
"""

from __future__ import annotations

import datetime
from dataclasses import dataclass
from decimal import Decimal
from typing import Callable, List, Optional, Tuple

from pyspark.sql import DataFrame, SparkSession
from pyspark.sql.types import (
    BooleanType, DateType, DecimalType, DoubleType, IntegerType, LongType,
    StringType, StructField, StructType, TimestampType,
)


# ---------------------------------------------------------------------------
# Inputs — compact but type-diverse; enough to exercise the Delta-type -> Spark
# type mapping (ADR-012/013) without the full corpus's nested-type weight.
# ---------------------------------------------------------------------------

def _d(s: str) -> datetime.date:
    return datetime.date.fromisoformat(s)


def _ts(s: str) -> datetime.datetime:
    return datetime.datetime.fromisoformat(s)


# Primary keyed table: the MERGE/append target and the basic read source.
PEOPLE_SCHEMA = StructType([
    StructField("id", LongType(), False),
    StructField("name", StringType(), True),
    StructField("dept", StringType(), True),
    StructField("salary", DoubleType(), True),
    StructField("active", BooleanType(), True),
])
PEOPLE_ROWS = [
    (1, "Alice", "infra", 95000.0, True),
    (2, "Bob",   "infra", 120000.0, True),
    (3, "Carol", "data",  78000.0, False),
    (4, "Dan",   "data",  64000.0, True),
]

# Append payload: brand-new keys (5,6) — used by append cases.
PEOPLE_APPEND_ROWS = [
    (5, "Eve",   "ml", 88000.0, True),
    (6, "Frank", "ml", 105000.0, False),
]

# MERGE source: overlaps (2 update, 3 update) + a new key (7 insert). Row for
# id=4 marks a delete in the Custom shape (flagged via a sentinel salary<0).
PEOPLE_MERGE_ROWS = [
    (2, "Bob",     "infra", 130000.0, True),    # matched -> update
    (3, "Carol",   "data",  80000.0,  True),    # matched -> update (also un-deactivate)
    (4, "Dan",     "data",  -1.0,     False),   # matched -> delete (Custom)
    (7, "Grace",   "ml",    72000.0,  True),    # not matched -> insert
]

# Type-coverage table for reads: pins Delta -> Spark type fidelity.
TYPES_SCHEMA = StructType([
    StructField("i32", IntegerType(), True),
    StructField("i64", LongType(), True),
    StructField("f64", DoubleType(), True),
    StructField("dec", DecimalType(12, 2), True),
    StructField("s", StringType(), True),
    StructField("b", BooleanType(), True),
    StructField("day", DateType(), True),
    StructField("ts", TimestampType(), True),
])
TYPES_ROWS = [
    (1, 9_000_000_000, 3.5, Decimal("100.25"), "x", True, _d("2020-01-01"), _ts("2026-06-01T09:15:00")),
    (-3, 1, -1.0, Decimal("0.00"), None, False, _d("2001-04-23"), None),
]


# ---------------------------------------------------------------------------
# Seed / write helpers (executed by the reference delta-spark session)
# ---------------------------------------------------------------------------

def seed_delta(rows, schema, partition_by=None):
    """Return a seed closure that overwrites a Delta table at `path` with `rows`."""
    def _seed(session: SparkSession, path: str) -> None:
        w = session.createDataFrame(rows, schema).write.format("delta").mode("overwrite")
        if partition_by:
            w = w.partitionBy(*partition_by)
        w.save(path)
    return _seed


def _read_delta_df(session: SparkSession, path: str) -> DataFrame:
    return session.read.format("delta").load(path)


def _read_delta_sql(session: SparkSession, path: str) -> DataFrame:
    return session.sql(f"SELECT * FROM delta.`{path}`")


def _read_parquet_df(session: SparkSession, path: str) -> DataFrame:
    return session.read.format("parquet").load(path)


# ---------------------------------------------------------------------------
# Case registry
# ---------------------------------------------------------------------------

@dataclass(frozen=True)
class DeltaCase:
    id: str
    category: str            # delta-read | delta-write-* | delta-merge-*
    description: str
    kind: str                # "read" | "write"
    frontend: str            # "df" | "sql"
    # read : run(session, path) -> DataFrame        (the query under test)
    # write: run(session, path) -> DataFrame        (action + read-back of final state)
    run: Callable[[SparkSession, str], DataFrame]
    # Pre-existing table seeder (run on the reference session into each engine's
    # isolated path). None for bootstrap-create (no pre-existing table).
    seed: Optional[Callable[[SparkSession, str], None]] = None
    flags: Tuple[str, ...] = ()
    # Revisit-trigger reason -> strict xfail. None = expected-green target.
    expected_unsupported: Optional[str] = None


CASES: List[DeltaCase] = []


def case(id, category, description, kind, frontend, run,
         seed=None, flags=(), expected_unsupported=None):
    CASES.append(DeltaCase(
        id, category, description, kind, frontend, run,
        seed, tuple(flags), expected_unsupported,
    ))


# Revisit-trigger strings (ADR-017) — shared so the roadmap reads consistently.
_GATE_OVERWRITE = "ADR-017: overwrite needs delete/truncate in duckdb-delta; revisit when it ships delete"
_GATE_CREATE = "ADR-017: CREATE-on-write DDL is duckdb-delta future work; revisit when confirmed in a pinned build"
_GATE_MERGE = "ADR-017: duckdb-delta lacks MERGE; cross-repo dev-loop target; revisit when MERGE ships"


# ── 1. Reads ────────────────────────────────────────────────────────────────
case("dl-read-001", "delta-read", "basic read of a seeded Delta table (df)",
     "read", "df", _read_delta_df, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))
case("dl-read-002", "delta-read", "basic read of a seeded Delta table (sql delta.`path`)",
     "read", "sql", _read_delta_sql, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))
case("dl-read-003", "delta-read", "type-fidelity read: int/long/double/decimal/date/ts/bool (df)",
     "read", "df", _read_delta_df, seed=seed_delta(TYPES_ROWS, TYPES_SCHEMA))
case("dl-read-004", "delta-read", "type-fidelity read (sql)",
     "read", "sql", _read_delta_sql, seed=seed_delta(TYPES_ROWS, TYPES_SCHEMA))
case("dl-read-005", "delta-read", "partitioned read — partition-column typing (df)",
     "read", "df", _read_delta_df,
     seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA, partition_by=["dept"]))
case("dl-read-006", "delta-read", "partitioned read (sql)",
     "read", "sql", _read_delta_sql,
     seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA, partition_by=["dept"]))
case("dl-read-007", "delta-read", "projection + filter over a Delta read (df)",
     "read", "df", lambda s, p: _read_delta_df(s, p).select("id", "salary").filter("salary > 80000"),
     seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))
case("dl-read-008", "delta-read", "aggregate over a Delta read (sql)",
     "read", "sql",
     lambda s, p: s.sql(f"SELECT dept, count(*) c, avg(salary) a FROM delta.`{p}` GROUP BY dept"),
     seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))
# S3 twins (opt-in): identical shape, exercised only when an endpoint is configured.
case("dl-read-009-s3", "delta-read", "basic read from S3 (df) — opt-in",
     "read", "df", _read_delta_df, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA), flags=("s3",))


# ── 2. Writes — parquet full recreate (the user's write form #1, non-Delta) ───
# "Full recreate" writes fresh to a target that need not pre-exist, so seed=None:
# each engine writes from scratch to its own path and reads its own output back.
# (Seeding a Spark parquet *directory* and then having τ overwrite the same path
# as a single file is an ill-posed cross-engine oracle — a shared transaction
# log is exactly what Delta adds and plain parquet lacks.)
def _w_parquet_overwrite_df(s, p):
    s.createDataFrame(PEOPLE_APPEND_ROWS, PEOPLE_SCHEMA).write.format("parquet").mode("overwrite").save(p)
    return _read_parquet_df(s, p)


case("dl-write-pq-overwrite-001", "delta-write-parquet", "parquet full recreate / overwrite (df)",
     "write", "df", _w_parquet_overwrite_df, seed=None)


# ── 3. Writes — delta append (target-green after the τ read+append increment) ─
def _w_delta_append_df(s, p):
    s.createDataFrame(PEOPLE_APPEND_ROWS, PEOPLE_SCHEMA).write.format("delta").mode("append").save(p)
    return _read_delta_df(s, p)


def _w_delta_append_sql(s, p):
    s.sql(f"INSERT INTO delta.`{p}` VALUES "
          "(5,'Eve','ml',88000.0,true),(6,'Frank','ml',105000.0,false)")
    return _read_delta_sql(s, p)


case("dl-write-append-001", "delta-write-append", "delta append new rows (df)",
     "write", "df", _w_delta_append_df, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))
case("dl-write-append-002", "delta-write-append", "delta append new rows (sql INSERT INTO)",
     "write", "sql", _w_delta_append_sql, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA))


# ── 4. Writes — delta overwrite / replaceWhere / bootstrap (gated) ────────────
def _w_delta_overwrite_df(s, p):
    s.createDataFrame(PEOPLE_APPEND_ROWS, PEOPLE_SCHEMA).write.format("delta").mode("overwrite").save(p)
    return _read_delta_df(s, p)


def _w_delta_replacewhere_df(s, p):
    # Replace only the 'infra' partition-slice with new rows; 'data' rows survive.
    (s.createDataFrame([(8, "Ivan", "infra", 99000.0, True)], PEOPLE_SCHEMA)
      .write.format("delta").mode("overwrite").option("replaceWhere", "dept = 'infra'").save(p))
    return _read_delta_df(s, p)


def _w_delta_bootstrap_df(s, p):
    # Empty-table bootstrap: create a Delta table at a non-existent path.
    s.createDataFrame([], PEOPLE_SCHEMA).write.format("delta").mode("overwrite").save(p)
    return _read_delta_df(s, p)


case("dl-write-overwrite-001", "delta-write-overwrite", "delta full overwrite (df)",
     "write", "df", _w_delta_overwrite_df, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA),
     expected_unsupported=_GATE_OVERWRITE)
case("dl-write-replacewhere-001", "delta-write-overwrite", "delta overwrite + replaceWhere (df)",
     "write", "df", _w_delta_replacewhere_df, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA),
     expected_unsupported=_GATE_OVERWRITE)
case("dl-write-bootstrap-001", "delta-write-bootstrap", "empty-table bootstrap create (df)",
     "write", "df", _w_delta_bootstrap_df, seed=None,
     expected_unsupported=_GATE_CREATE)


# ── 5. MERGE (as SQL MERGE INTO; source = a temp view) — all gated ────────────
def _register_merge_source(s):
    s.createDataFrame(PEOPLE_MERGE_ROWS, PEOPLE_SCHEMA).createOrReplaceTempView("delta_merge_src")


def _w_merge_upsert_sql(s, p):
    _register_merge_source(s)
    s.sql(f"""
        MERGE INTO delta.`{p}` AS t
        USING delta_merge_src AS src
        ON t.id = src.id
        WHEN MATCHED THEN UPDATE SET *
        WHEN NOT MATCHED THEN INSERT *
    """)
    return _read_delta_sql(s, p)


def _w_merge_insert_only_sql(s, p):
    _register_merge_source(s)
    s.sql(f"""
        MERGE INTO delta.`{p}` AS t
        USING delta_merge_src AS src
        ON t.id = src.id
        WHEN NOT MATCHED THEN INSERT *
    """)
    return _read_delta_sql(s, p)


def _w_merge_custom_sql(s, p):
    _register_merge_source(s)
    s.sql(f"""
        MERGE INTO delta.`{p}` AS t
        USING delta_merge_src AS src
        ON t.id = src.id
        WHEN MATCHED AND src.salary < 0 THEN DELETE
        WHEN MATCHED THEN UPDATE SET t.salary = src.salary, t.active = src.active
        WHEN NOT MATCHED THEN INSERT *
    """)
    return _read_delta_sql(s, p)


case("dl-merge-upsert-001", "delta-merge-upsert", "MERGE upsert: matched update + notmatched insert",
     "write", "sql", _w_merge_upsert_sql, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA),
     expected_unsupported=_GATE_MERGE)
case("dl-merge-insert-only-001", "delta-merge-insert-only", "MERGE insert-only: notmatched insert",
     "write", "sql", _w_merge_insert_only_sql, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA),
     expected_unsupported=_GATE_MERGE)
case("dl-merge-custom-001", "delta-merge-custom", "MERGE custom: per-clause delete/update/insert",
     "write", "sql", _w_merge_custom_sql, seed=seed_delta(PEOPLE_ROWS, PEOPLE_SCHEMA),
     expected_unsupported=_GATE_MERGE)
