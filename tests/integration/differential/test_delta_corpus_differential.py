"""Delta Lake corpus differential tests — the `delta` suite (state-diff oracle).

Drives `delta_corpus.CASES` through the ADR-011 **state-diff oracle**: for writes
the correctness signal is the *table state after the write*, read back and diffed
between engines — not a query result set.

Oracle mechanics
----------------
- **Isolation**: each case gets a unique scratch dir per engine under a
  session-scoped tmp root, so both engines write independently and never collide.
- **Seeding**: Delta tables can only be *created* by the reference (delta-spark)
  engine — τ's create-on-write is out of scope (ADR-017, INV9: a writable Delta
  table must be attached + pre-existing). So the case's `seed` runs on the
  reference session into *each* engine's isolated path; the action under test then
  runs on the respective engine.
- **Read cases**: one shared path, seeded once by the reference; both engines read
  it; schema + rows diffed.
- **Write cases**: reference writes+reads-back its path; τ writes+reads-back its
  path; the two final states are diffed.
- **Comparison** reuses `utils.dataframe_diff.assert_dataframes_equal` + the
  `repr`-sort canonicalization from the DataFrame-corpus driver (ADR-015:
  canonicalization is test-side, never in production τ).

Expected-red roadmap (ADR-017)
------------------------------
Cases with `expected_unsupported` are marked **strict xfail**: they stay green-
as-xfail while the DuckDB-Delta gap is real, and flip to a loud failure (xpass)
the moment τ implements them — the signal to drop the flag. To audit the
reference side of xfail cases (they must be valid Spark), run with `--runxfail`.

Running
-------
    ./tests/scripts/run-differential-tests.sh differential/test_delta_corpus_differential.py -v
"""
import os
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent.parent))

import pytest

from utils.dataframe_diff import DataFrameDiff, assert_dataframes_equal
from differential.delta_corpus import CASES, DeltaCase


def _params():
    """Parametrize with per-case strict-xfail marks for the gated shapes."""
    out = []
    for c in CASES:
        marks = []
        if c.expected_unsupported:
            marks.append(pytest.mark.xfail(reason=c.expected_unsupported, strict=True))
        out.append(pytest.param(c, id=c.id, marks=marks))
    return out


def _assert_schema_equal(ref_df, td_df, name: str) -> None:
    diff = DataFrameDiff()
    match, message = diff._compare_schemas(ref_df.schema, td_df.schema, ignore_nullable=False)
    if not match:
        raise AssertionError(f"{name} schema mismatch:\n{message}")


def _canonicalize_rows(df):
    """Collect + repr-sort so unordered results diff deterministically."""
    rows = sorted(df.collect(), key=repr)
    return df.sparkSession.createDataFrame(rows, df.schema)


def _diff(ref_df, td_df, name: str, schema_only: bool) -> None:
    _assert_schema_equal(ref_df, td_df, name)
    if schema_only:
        return
    assert_dataframes_equal(
        _canonicalize_rows(ref_df),
        _canonicalize_rows(td_df),
        query_name=name,
    )


@pytest.fixture(scope="session")
def delta_scratch_dir(tmp_path_factory):
    """Session-scoped scratch root for isolated per-case Delta/parquet tables."""
    return tmp_path_factory.mktemp("delta_corpus")


@pytest.mark.differential
@pytest.mark.parametrize("case", _params())
def test_delta_case(case: DeltaCase, spark_reference, spark_thunderduck, delta_scratch_dir):
    """One test per Delta corpus case, via the state-diff oracle."""
    if "s3" in case.flags and not os.environ.get("THUNDERDUCK_DELTA_S3_ENDPOINT"):
        pytest.skip("S3 endpoint not configured (THUNDERDUCK_DELTA_S3_ENDPOINT)")

    schema_only = "schema_only" in case.flags
    base = Path(delta_scratch_dir) / case.id

    if case.kind == "read":
        # One shared table, seeded once by the reference; both engines read it.
        path = str(base / "table")
        case.seed(spark_reference, path)
        ref_df = case.run(spark_reference, path)
        td_df = case.run(spark_thunderduck, path)
        _diff(ref_df, td_df, case.id, schema_only)
        return

    # write: each engine gets its own isolated path; both pre-existing tables are
    # seeded by the reference (only delta-spark can create a Delta table).
    ref_path = str(base / "ref")
    td_path = str(base / "td")
    if case.seed is not None:
        case.seed(spark_reference, ref_path)
        case.seed(spark_reference, td_path)

    ref_state = case.run(spark_reference, ref_path)
    td_state = case.run(spark_thunderduck, td_path)
    _diff(ref_state, td_state, case.id, schema_only)
