"""Conformance corpus differential tests — the `core` suite.

Drives `dataframe_corpus.CASES` against the Spark reference and Thunderduck:

- Schema is always compared (column names, types, nullability — strict).
- Row values are also compared *after* canonicalization (sort by `repr(row)`,
  float tolerance via the existing `assert_dataframes_equal`), unless the case
  is flagged `schema_only` or `nondeterministic`.

The corpus's docstring is explicit that canonicalization belongs in the harness,
not in production τ (ADR-015). That's exactly what this file implements.

This is the **DataFrame-API only** conformance gate. SQL-front-end cases
(`spark.sql(...)`, CTEs, correlated subqueries) live in a separate corpus.

Running:
    cargo test -p thunderduck-connect-server --test differential core    -- --ignored --nocapture
    cargo test -p thunderduck-connect-server --test differential core_v2 -- --ignored --nocapture
"""
import sys
from pathlib import Path

# Same sys.path shim the other differential tests use, so we can import
# `utils.dataframe_diff` and `differential.dataframe_corpus`.
sys.path.insert(0, str(Path(__file__).parent.parent))

import pytest

from utils.dataframe_diff import DataFrameDiff, assert_dataframes_equal
from differential.dataframe_corpus import CASES, Case


def _case_id(case: Case) -> str:
    """Pytest ID for each case — surfaces failures as e.g. `test_case[chain-001]`."""
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
def test_case(case: Case, corpus_inputs_reference, corpus_inputs_thunderduck):
    """One pytest test per Case in the corpus.

    Schema is always compared. For `nondeterministic` / `schema_only` cases the
    row comparison is skipped; everything else is row-compared after
    `repr`-sort canonicalization.
    """
    ref_df = case.build(corpus_inputs_reference)
    td_df = case.build(corpus_inputs_thunderduck)

    schema_only = "schema_only" in case.flags or "nondeterministic" in case.flags
    if schema_only:
        _assert_schema_equal(ref_df, td_df, case.id)
        return

    assert_dataframes_equal(
        _canonicalize_rows(ref_df),
        _canonicalize_rows(td_df),
        query_name=case.id,
    )
