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
    tests/scripts/differential-progress.sh    # full suite + progress row
"""
import os
import sys
from pathlib import Path

# Same sys.path shim the other differential tests use, so we can import
# `utils.dataframe_diff` and `differential.sql_corpus`.
sys.path.insert(0, str(Path(__file__).parent.parent))

import pytest

from utils.dataframe_diff import (
    DataFrameDiff,
    SideOutcome,
    assert_dataframes_equal,
    capture_outcome,
    collect_both,
    reconcile_error_parity,
    spark_error_class,
)
from utils import golden
from differential.sql_corpus import CASES, Case

CORPUS = "sql"


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


def _canonical_df(df, rows):
    """Build a fresh DataFrame from already-collected rows, sorted by `repr`.

    PySpark's `Row.__repr__` produces a stable string for nested arrays / maps /
    structs that `df.orderBy(...)` cannot sort by directly. The expensive
    `collect()` is done by the caller (concurrently, via `collect_both`); this
    only does the cheap sort + local `createDataFrame`, preserving the schema.
    """
    return df.sparkSession.createDataFrame(sorted(rows, key=repr), df.schema)


def _sql_outcome(spark, sql: str, timeout: int, name: str):
    """Evaluate one engine end-to-end for an `expected_error` case.

    Unlike `capture_outcome` (collect-time only), this also captures EAGER
    analysis-time errors raised by `spark.sql(...)` itself — Spark's Connect
    session analyzes eagerly, so classes like UNRESOLVED_COLUMN surface at
    `.sql()`, before any DataFrame exists (e.g. sq-023). Returns
    `(df_or_None, SideOutcome)`; `df` is None iff `.sql()` itself threw.
    """
    try:
        df = spark.sql(sql)
    except Exception as exc:  # noqa: BLE001 — any analysis error becomes an outcome
        return None, SideOutcome(error=exc, error_class=spark_error_class(exc))
    return df, capture_outcome(df, timeout, name)


def _is_schema_only(case: Case) -> bool:
    return "schema_only" in case.flags or "nondeterministic" in case.flags


def _compare(ref_df, td_df, case: Case) -> None:
    """Diff a reference DataFrame vs τ's — shared by live and golden modes."""
    if _is_schema_only(case):
        _assert_schema_equal(ref_df, td_df, case.id)
        return
    timeout = int(os.environ.get("DIFFERENTIAL_TIMEOUT", "10"))
    ref_rows, td_rows = collect_both(ref_df, td_df, timeout)
    assert_dataframes_equal(
        _canonical_df(ref_df, ref_rows),
        _canonical_df(td_df, td_rows),
        query_name=case.id,
    )


@pytest.mark.differential
@pytest.mark.parametrize("case", CASES, ids=_case_id)
def test_case(
    case: Case,
    request,
    spark_thunderduck,
    sql_corpus_thunderduck,
    tpc_view_switcher,
):
    """One pytest test per Case in the SQL corpus, under the active oracle mode.

    - golden (default): diff τ against the recorded golden — no Spark.
    - live: diff τ against a live Spark reference.
    - record: capture the live Spark reference into the golden file.

    Runs `case.sql` through `spark.sql(...)` on the τ session (temp views
    registered by `sql_corpus_thunderduck`). Schema is always compared;
    `nondeterministic`/`schema_only` cases skip the row comparison; everything
    else is row-compared after `repr`-sort canonicalization.
    """
    mode = golden.oracle_mode()

    # tpch/tpcds cases: re-point the benchmark-colliding temp views. Golden-safe
    # (touches the reference session only in live/record).
    tpc_view_switcher(case.category)

    def _ref_sql(sql):
        """Register reference temp views (once) and run the SQL on Spark."""
        request.getfixturevalue("sql_corpus_reference")
        return request.getfixturevalue("spark_reference").sql(sql)

    # ── record: capture the Spark reference into the golden, then stop ──────
    if mode == "record":
        if case.expected_error is not None:
            return  # error-parity cases carry their expected class in the corpus
        ref_df = _ref_sql(case.sql)
        golden.record_reference(CORPUS, case.id, ref_df, schema_only=_is_schema_only(case))
        return

    # ── ADR-006 tri-state error-parity (also captures eager .sql() errors) ──
    if case.expected_error is not None:
        timeout = int(os.environ.get("DIFFERENTIAL_TIMEOUT", "10"))
        td_df, td = _sql_outcome(spark_thunderduck, case.sql, timeout, "Thunderduck")
        if mode == "live":
            request.getfixturevalue("sql_corpus_reference")
            ref_df, ref = _sql_outcome(
                request.getfixturevalue("spark_reference"), case.sql, timeout, "Spark Reference"
            )
        else:  # golden: the reference is "Spark raises the declared class"
            ref_df = None
            ref = SideOutcome(
                error=RuntimeError(f"golden: Spark raises [{case.expected_error}]"),
                error_class=case.expected_error,
            )
        outcome = reconcile_error_parity(ref, td, case.id, expected_class=case.expected_error)
        if outcome is None:
            return  # both threw the matching class → PASS
        # Both returned values → row diff (live mode only; ref_df non-None).
        ref_rows, td_rows = outcome
        assert_dataframes_equal(
            ref_df.sparkSession.createDataFrame(sorted(ref_rows, key=repr), ref_df.schema),
            td_df.sparkSession.createDataFrame(sorted(td_rows, key=repr), td_df.schema),
            query_name=case.id,
        )
        return

    # ── normal / schema-only ────────────────────────────────────────────────
    td_df = spark_thunderduck.sql(case.sql)
    if mode == "live":
        ref_df = _ref_sql(case.sql)
        _compare(ref_df, td_df, case)
    else:  # golden
        if not golden.assert_golden_match(CORPUS, case.id, td_df):
            pytest.fail(
                f"no golden for {case.id}; record it: "
                f"tests/scripts/run-differential-tests.sh --record sql_v2 -k {case.id}"
            )
