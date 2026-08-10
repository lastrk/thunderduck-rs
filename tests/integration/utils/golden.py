"""Golden-file (snapshot) oracle for the differential corpora.

The differential oracle (ADR-015) validates τ against a reference Apache Spark
4.1.1 result. That reference result is deterministic for a fixed (plan, Spark
version, input fixtures), so it can be captured once and cached in git. This
module is that cache: it (de)serializes a case's reference result to a per-case
JSON file under ``differential/goldens/{dataframe,sql}/<case-id>.json``.

Three oracle modes, selected by ``THUNDERDUCK_ORACLE`` (default ``golden``):

- ``golden``  — run τ only, reconstruct the reference from the golden, diff.
                No Spark is started.
- ``live``    — run Spark + τ and diff live (the full-authority oracle).
- ``record``  — run Spark for the selected cases and (over)write their goldens.

The golden is a *cache* of the live oracle, not a replacement: ``live`` remains
authoritative and is used to (re)record goldens whenever a case or its input
fixtures change.

Serialization is schema-driven: values are encoded/decoded by walking the Spark
``StructType`` alongside the value, so ``MapType`` and ``StructType`` (both dicts
after ``asDict``) are never confused, non-string map keys survive, and the
decoded rows are plain tuples/lists/dicts/scalars suitable for
``createDataFrame(rows, schema)`` — the same shape the corpora build inputs from.
JSON (not parquet) is used deliberately so goldens diff and review in git.
"""
from __future__ import annotations

import base64
import datetime
import json
import math
import os
from decimal import Decimal
from pathlib import Path
from typing import Any, Optional

from pyspark.sql.types import (
    ArrayType,
    BinaryType,
    DataType,
    DateType,
    DecimalType,
    MapType,
    StructType,
    TimestampType,
    TimestampNTZType,
)

SPARK_VERSION = "4.1.1"

_VALID_MODES = ("golden", "live", "record")

_GOLDENS_ROOT = Path(__file__).parent.parent / "differential" / "goldens"


def oracle_mode() -> str:
    """Resolve the oracle mode from ``THUNDERDUCK_ORACLE`` (default ``golden``)."""
    mode = os.environ.get("THUNDERDUCK_ORACLE", "golden").strip().lower()
    if mode not in _VALID_MODES:
        raise ValueError(
            f"THUNDERDUCK_ORACLE={mode!r} invalid; expected one of {_VALID_MODES}"
        )
    return mode


def golden_path(corpus: str, case_id: str) -> Path:
    """Path to a case's golden file. ``corpus`` is 'dataframe' or 'sql'."""
    return _GOLDENS_ROOT / corpus / f"{case_id}.json"


# Schema-driven value codec

def _encode(dt: DataType, v: Any) -> Any:
    """Encode a single value of Spark type ``dt`` to a JSON-safe form."""
    if v is None:
        return None
    if isinstance(dt, DecimalType):
        return {"$dec": str(v)}
    if isinstance(dt, DateType):
        return {"$date": v.isoformat()}
    if isinstance(dt, (TimestampType, TimestampNTZType)):
        return {"$ts": v.isoformat()}
    if isinstance(dt, BinaryType):
        raw = bytes(v) if isinstance(v, (bytearray, bytes)) else v
        return {"$bin": base64.b64encode(raw).decode("ascii")}
    if isinstance(dt, ArrayType):
        return [_encode(dt.elementType, e) for e in v]
    if isinstance(dt, MapType):
        return {
            "$map": [
                [_encode(dt.keyType, k), _encode(dt.valueType, mv)]
                for k, mv in v.items()
            ]
        }
    if isinstance(dt, StructType):
        # Index by POSITION, never by name: a struct/row can have duplicate
        # field names (join "slot witness" cases), and Row[name] returns the
        # FIRST match — which would silently record the first column's value for
        # every duplicate-named field. Positional access preserves each slot.
        if isinstance(v, dict):
            return {"$row": [_encode(f.dataType, v.get(f.name)) for f in dt.fields]}
        return {"$row": [_encode(f.dataType, v[i]) for i, f in enumerate(dt.fields)]}
    # DayTimeIntervalType collects as datetime.timedelta (value-based check —
    # the type is not always distinguishable via a stable public class here).
    if isinstance(v, datetime.timedelta):
        return {"$td": [v.days, v.seconds, v.microseconds]}
    # Atomic scalar. Floats: tag the non-JSON-representable specials.
    if isinstance(v, float):
        if math.isnan(v):
            return {"$f": "nan"}
        if math.isinf(v):
            return {"$f": "inf" if v > 0 else "-inf"}
    return v


def _decode(dt: DataType, v: Any) -> Any:
    """Decode a JSON value of Spark type ``dt`` into a createDataFrame-ready form."""
    if v is None:
        return None
    if isinstance(v, dict):
        if "$dec" in v:
            return Decimal(v["$dec"])
        if "$date" in v:
            return datetime.date.fromisoformat(v["$date"])
        if "$ts" in v:
            return datetime.datetime.fromisoformat(v["$ts"])
        if "$bin" in v:
            return base64.b64decode(v["$bin"])
        if "$f" in v:
            return {"nan": math.nan, "inf": math.inf, "-inf": -math.inf}[v["$f"]]
        if "$td" in v:
            d, s, us = v["$td"]
            return datetime.timedelta(days=d, seconds=s, microseconds=us)
        if "$map" in v:
            return {
                _decode(dt.keyType, k): _decode(dt.valueType, mv)
                for k, mv in v["$map"]
            }
        if "$row" in v:
            # Positional tuple in struct field order (createDataFrame-friendly).
            return tuple(
                _decode(f.dataType, cell) for f, cell in zip(dt.fields, v["$row"])
            )
    if isinstance(dt, ArrayType):
        return [_decode(dt.elementType, e) for e in v]
    return v


def _encode_row(schema: StructType, row: Any) -> list:
    """Encode one collected Row into a positional list matching ``schema``.

    Accesses by POSITION (``row[i]``), never by name — join results can carry
    duplicate column names, and ``Row[name]`` returns the first match, which
    would corrupt every duplicate-named slot.
    """
    return [_encode(f.dataType, row[i]) for i, f in enumerate(schema.fields)]


def _decode_row(schema: StructType, cells: list) -> tuple:
    """Decode one stored row (positional list) into a createDataFrame tuple."""
    return tuple(_decode(f.dataType, c) for f, c in zip(schema.fields, cells))


# Read / write

def write_golden(
    corpus: str,
    case_id: str,
    *,
    schema: StructType,
    rows: Optional[list],
) -> None:
    """Write a case's golden. ``rows=None`` records a schema-only golden."""
    path = golden_path(corpus, case_id)
    path.parent.mkdir(parents=True, exist_ok=True)
    doc = {
        "id": case_id,
        "spark_version": SPARK_VERSION,
        "kind": "schema_only" if rows is None else "rows",
        "schema": schema.jsonValue(),
    }
    if rows is not None:
        doc["rows"] = [_encode_row(schema, r) for r in rows]
    # Serialize fully before touching the file, then write atomically via a temp
    # + rename, so an encode error can never leave a corrupt/partial golden.
    payload = json.dumps(doc, indent=1, ensure_ascii=False, sort_keys=False) + "\n"
    tmp = path.with_suffix(path.suffix + ".tmp")
    tmp.write_text(payload)
    tmp.replace(path)


class Golden:
    """A decoded golden: its kind, reference schema, and (for rows) decoded tuples."""

    def __init__(self, kind: str, schema: StructType, rows: Optional[list]):
        self.kind = kind
        self.schema = schema
        self.rows = rows  # list[tuple] for kind=='rows', else None


def read_golden(corpus: str, case_id: str) -> Optional[Golden]:
    """Load a case's golden, or None if it does not exist."""
    path = golden_path(corpus, case_id)
    if not path.exists():
        return None
    doc = json.loads(path.read_text())
    schema = StructType.fromJson(doc["schema"])
    kind = doc["kind"]
    rows = None
    if kind == "rows":
        rows = [_decode_row(schema, cells) for cells in doc["rows"]]
    return Golden(kind, schema, rows)


# Driver helpers (shared by both corpus drivers)

def record_reference(corpus: str, case_id: str, ref_df, *, schema_only: bool) -> None:
    """Record a case's reference result from a live Spark ``ref_df`` (record mode).

    Schema-only cases (nondeterministic / distribution-only) store just the
    schema. Everything else stores the canonicalized (``repr``-sorted) rows.
    """
    if schema_only:
        write_golden(corpus, case_id, schema=ref_df.schema, rows=None)
    else:
        rows = sorted(ref_df.collect(), key=repr)
        write_golden(corpus, case_id, schema=ref_df.schema, rows=rows)


def _rowify(v: Any) -> Any:
    """Recursively normalize a collected value to golden decoded shape.

    Spark ``Row`` (a tuple subclass) → plain tuple; lists/tuples/dicts recurse.
    This makes τ's collected rows structurally identical to the decoded golden
    rows (nested struct = tuple, array = list, map = dict), so ``repr``-sort
    keys align and positional comparison is exact — crucially it does NOT go
    through ``asDict()``, so duplicate column names (join "slot witness" cases)
    are compared by position rather than silently collapsed to last-wins.
    """
    from pyspark.sql import Row

    if isinstance(v, Row):
        return tuple(_rowify(x) for x in v)
    if isinstance(v, list):
        return [_rowify(x) for x in v]
    if isinstance(v, tuple):
        return tuple(_rowify(x) for x in v)
    if isinstance(v, dict):
        return {k: _rowify(x) for k, x in v.items()}
    return v


def assert_golden_match(corpus: str, case_id: str, td_df, *, epsilon: float = 1e-6):
    """Diff τ's DataFrame against a recorded golden (golden mode). No Spark.

    Compares the golden's recorded schema (exactly as Spark produced it) against
    τ's output schema, and — unless the golden is schema-only — the golden's rows
    against τ's collected rows, positionally and after ``repr``-sort
    canonicalization, reusing ``DataFrameDiff._values_equal`` for float/decimal
    epsilon + NaN handling. Raises ``AssertionError`` on any mismatch. Returns
    ``False`` if the golden is absent (the caller fails loudly with a record
    hint), else ``True``.
    """
    from utils.dataframe_diff import DataFrameDiff, collect_with_timeout

    g = read_golden(corpus, case_id)
    if g is None:
        return False

    diff = DataFrameDiff(epsilon=epsilon)
    ok, msg = diff._compare_schemas(g.schema, td_df.schema, ignore_nullable=False)
    if not ok:
        raise AssertionError(f"{case_id} schema mismatch (golden vs τ):\n{msg}")
    if g.kind == "schema_only":
        return True

    timeout = int(os.environ.get("DIFFERENTIAL_TIMEOUT", "10"))
    td_rows = [_rowify(r) for r in collect_with_timeout(td_df, timeout, "Thunderduck")]
    ref = sorted((tuple(r) for r in g.rows), key=repr)
    td = sorted(td_rows, key=repr)
    if len(ref) != len(td):
        raise AssertionError(
            f"{case_id} row count mismatch: golden={len(ref)}, τ={len(td)}"
        )
    names = [f.name for f in g.schema.fields]
    for i, (rr, tr) in enumerate(zip(ref, td)):
        for j, (rv, tv) in enumerate(zip(rr, tr)):
            if not diff._values_equal(rv, tv):
                col = names[j] if j < len(names) else f"#{j}"
                raise AssertionError(
                    f"{case_id} row {i} col {j} ({col}): golden={rv!r} != τ={tv!r}"
                )
    return True
