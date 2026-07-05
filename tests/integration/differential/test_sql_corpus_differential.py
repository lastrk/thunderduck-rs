"""Spark SQL conformance corpus differential tests — the `sql_v2` suite.

Drives `sql_corpus.CASES` (raw `spark.sql("...")`) against the Spark reference
and Thunderduck:

- Schema is always compared (column names, types, nullability — strict).
- Row values are also compared *after* canonicalization (sort by `repr(row)`,
  float tolerance via the existing `assert_dataframes_equal`), unless the case
  is flagged `schema_only` or `nondeterministic`.

This is the **SQL-front-end** conformance gate — the counterpart to
`test_dataframe_corpus_differential.py` (the DataFrame-API `core`/`core_v2`
suite). Inputs (`emp`, `dept`, `emp2`, `nums`, `raw`) are registered as temp
views by the `sql_corpus_reference` / `sql_corpus_thunderduck` fixtures so the
SQL can reference them by name.

The τ SQL path is still maturing, so this suite is *expected to be partially
red* until more of the SQL surface lands — it is the fitness function that
drives the SQL implementation, exactly as `core_v2` did for the DataFrame API.

Running:
    cargo test -p thunderduck-connect-server --test differential sql_v2 -- --ignored --nocapture
    tests/scripts/run-differential-tests.sh sql_v2
    tests/scripts/v2-sql-progress.sh          # records a progress row
"""
import os
import sys
from pathlib import Path

# Same sys.path shim the other differential tests use, so we can import
# `utils.dataframe_diff` and `differential.sql_corpus`.
sys.path.insert(0, str(Path(__file__).parent.parent))

import pytest

from utils.dataframe_diff import DataFrameDiff, assert_dataframes_equal
from differential.sql_corpus import CASES, Case


def _case_id(case: Case) -> str:
    """Pytest ID for each case — surfaces failures as e.g. `test_case[sq-003]`."""
    return case.id


def _assert_schema_equal(ref_df, td_df, query_name: str) -> None:
    """Schema-only diff for nondeterministic / schema_only cases.

    Wraps the existing `DataFrameDiff._compare_schemas` since
    `assert_dataframes_equal` doesn't expose a schema-only mode.
    """
    diff = DataFrameDiff()
    match, message = diff._compare_schemas(ref_df.schema, td_df.schema, ignore_nullable=False)
    if not match:
        raise AssertionError(f"{query_name} schema mismatch:\n{message}")


def _canonicalize_rows(df):
    """Collect rows and sort by `repr` so unordered cases diff deterministically.

    PySpark's `Row.__repr__` produces a stable string for nested arrays / maps /
    structs that `df.orderBy(...)` cannot sort by directly. Returns a fresh
    DataFrame built from the sorted Python rows, preserving the schema.
    """
    rows = sorted(df.collect(), key=repr)
    return df.sparkSession.createDataFrame(rows, df.schema)


@pytest.mark.differential
@pytest.mark.parametrize("case", CASES, ids=_case_id)
def test_case(
    case: Case,
    spark_reference,
    spark_thunderduck,
    sql_corpus_reference,
    sql_corpus_thunderduck,
):
    """One pytest test per Case in the SQL corpus.

    Runs `case.sql` through `spark.sql(...)` on both sessions (the temp views the
    SQL references are registered by the `sql_corpus_*` fixtures). Schema is
    always compared. For `nondeterministic` / `schema_only` cases the row
    comparison is skipped; everything else is row-compared after `repr`-sort
    canonicalization. A per-case exception (parse / analysis / unimplemented on
    the τ side) surfaces as a test FAILURE for that case — the signal this
    corpus exists to drive.
    """
    ref_df = spark_reference.sql(case.sql)
    td_df = spark_thunderduck.sql(case.sql)

    schema_only = "schema_only" in case.flags or "nondeterministic" in case.flags
    if schema_only:
        _assert_schema_equal(ref_df, td_df, case.id)
        return

    assert_dataframes_equal(
        _canonicalize_rows(ref_df),
        _canonicalize_rows(td_df),
        query_name=case.id,
    )
