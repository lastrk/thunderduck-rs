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
import os
import sys
from pathlib import Path

# Same sys.path shim the other differential tests use, so we can import
# `utils.dataframe_diff` and `differential.dataframe_corpus`.
sys.path.insert(0, str(Path(__file__).parent.parent))

import pytest

from utils.dataframe_diff import (
    DataFrameDiff,
    SideOutcome,
    assert_dataframes_equal,
    capture_outcome,
    collect_both,
    reconcile_error_parity,
)
from utils import golden
from differential.dataframe_corpus import CASES, Case

CORPUS = "dataframe"


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


def _canonical_df(df, rows):
    """Build a fresh DataFrame from already-collected rows, sorted by `repr`.

    PySpark's `Row.__repr__` produces a stable string for nested arrays / maps /
    structs that `df.orderBy(...)` cannot sort by directly. The expensive
    `collect()` is done by the caller (concurrently, via `collect_both`); this
    only does the cheap sort + local `createDataFrame`, preserving the schema.
    """
    return df.sparkSession.createDataFrame(sorted(rows, key=repr), df.schema)


def _is_schema_only(case: Case) -> bool:
    return "schema_only" in case.flags or "nondeterministic" in case.flags


def _compare(ref_df, td_df, case: Case) -> None:
    """Diff a reference DataFrame vs τ's — shared by live and golden modes.

    Schema-only for `schema_only`/`nondeterministic` cases; otherwise collect
    both concurrently (bounded by DIFFERENTIAL_TIMEOUT — in golden mode `ref_df`
    is a local relation, so its collect is instant) and row-diff after
    `repr`-sort canonicalization, honoring the per-case epsilon override.
    """
    if _is_schema_only(case):
        _assert_schema_equal(ref_df, td_df, case.id)
        return
    extra = {"epsilon": case.epsilon} if case.epsilon is not None else {}
    timeout = int(os.environ.get("DIFFERENTIAL_TIMEOUT", "10"))
    ref_rows, td_rows = collect_both(ref_df, td_df, timeout)
    assert_dataframes_equal(
        _canonical_df(ref_df, ref_rows),
        _canonical_df(td_df, td_rows),
        query_name=case.id,
        **extra,
    )


@pytest.mark.differential
@pytest.mark.parametrize("case", CASES, ids=_case_id)
def test_case(case: Case, request, corpus_inputs_thunderduck, tpc_view_switcher):
    """One pytest test per Case, under the active oracle mode (see utils.golden).

    - golden (default): diff τ against the recorded golden — no Spark.
    - live: diff τ against a live Spark reference.
    - record: capture the live Spark reference into the golden file.

    Schema is always compared; `nondeterministic`/`schema_only` cases skip the
    row comparison; everything else is row-compared after `repr`-sort
    canonicalization.
    """
    # `deferred` cases are known-divergent and INTENTIONALLY not fixed — each
    # carries its reason in the corpus comment and in
    # docs/future_work/. Skipped rather than left red so the corpus signal
    # stays meaningful: a red case should mean "τ regressed or is missing a
    # feature", not "we already decided this one waits". pytest still reports
    # them as skips, so they cannot silently disappear.
    if "deferred" in case.flags:
        pytest.skip(f"deferred (see corpus comment / docs/future_work): {case.description}")

    mode = golden.oracle_mode()

    # tpch/tpcds cases: re-point the benchmark-colliding temp views (e.g.
    # `customer` exists in both benchmarks with different schemas). Golden-safe:
    # touches the reference session only in live/record. No-op otherwise.
    tpc_view_switcher(case.category)

    td_df = case.build(corpus_inputs_thunderduck)

    # ── record: capture the Spark reference into the golden, then stop ──────
    if mode == "record":
        if case.expected_error is not None:
            return  # error-parity cases carry their expected class in the corpus
        inputs_ref = request.getfixturevalue("corpus_inputs_reference")
        ref_df = case.build(inputs_ref)
        golden.record_reference(CORPUS, case.id, ref_df, schema_only=_is_schema_only(case))
        return

    # ── ADR-006 tri-state error-parity ─────────────────────────────────────
    if case.expected_error is not None:
        timeout = int(os.environ.get("DIFFERENTIAL_TIMEOUT", "10"))
        td = capture_outcome(td_df, timeout, "Thunderduck")
        if mode == "live":
            inputs_ref = request.getfixturevalue("corpus_inputs_reference")
            ref_df = case.build(inputs_ref)
            ref = capture_outcome(ref_df, timeout, "Spark Reference")
        else:  # golden: the reference is "Spark raises the declared class"
            ref = SideOutcome(
                error=RuntimeError(f"golden: Spark raises [{case.expected_error}]"),
                error_class=case.expected_error,
            )
        outcome = reconcile_error_parity(ref, td, case.id, expected_class=case.expected_error)
        if outcome is None:
            return  # both threw the matching class → PASS
        # Both returned values → row diff (reachable only in live mode; in golden
        # mode the reference "threw", so reconcile can only PASS or raise).
        ref_rows, td_rows = outcome
        assert_dataframes_equal(
            ref_df.sparkSession.createDataFrame(sorted(ref_rows, key=repr), ref_df.schema),
            td_df.sparkSession.createDataFrame(sorted(td_rows, key=repr), td_df.schema),
            query_name=case.id,
        )
        return

    # ── normal / schema-only ────────────────────────────────────────────────
    if mode == "live":
        inputs_ref = request.getfixturevalue("corpus_inputs_reference")
        ref_df = case.build(inputs_ref)
        _compare(ref_df, td_df, case)
    else:  # golden
        epsilon = case.epsilon if case.epsilon is not None else 1e-6
        if not golden.assert_golden_match(CORPUS, case.id, td_df, epsilon=epsilon):
            pytest.fail(
                f"no golden for {case.id}; record it: "
                f"tests/scripts/run-differential-tests.sh --record core -k {case.id}"
            )
