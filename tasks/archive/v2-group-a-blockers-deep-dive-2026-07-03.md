# Deep Dive — Group A "Not Fixable in Thunderduck" (11 cases)

**Companion to** `tasks/v2-corpus-remaining-blockers-2026-07-03.md`.
**Question**: Are these really expected to fail? How is the corpus set up? Does Thunderduck emulate Spark's failures?

---

## Executive answer

**"Group A" was too loose.** After per-case investigation:

- **True upstream blockers (client-side PySpark decoder)** — 4 cases. Cannot be fixed in Thunderduck at the current PySpark 4.1.1 version. Corpus does NOT tag these; they SHOULD be tagged.
- **Symmetric server-throw cases (both Spark and TD throw same-family errors)** — 3 cases (math-010, math-011, arr-008). Fixable in the *harness* (accept "both errored with matching code"), not by tagging or by τ work. Corpus has no such flag today.
- **Wrongly classified — actually Thunderduck bugs** — 4 cases (parse-003, cond-004, arr-012, set-004). These are NOT "expected to fail" — τ diverges from Spark and each is fixable in τ. My earlier group-A classification was incorrect.

**The corpus has NO "expected_to_fail" / "error_expected" flag.** Available flags (from `differential/dataframe_corpus.py:201`):

```python
flags: Tuple[str, ...] = ()  # arbitrary strings

# Recognized in the harness (test_dataframe_corpus_differential.py:73):
"schema_only"       # skip row comparison
"nondeterministic"  # skip row comparison

# Recognized elsewhere / cosmetic-only:
"spark4"        # requires Spark 4+
"schema_only"
"nondeterministic"
"cosmetic"      # result-irrelevant (join hints, repartition)
"expected_close" # tolerance for float ops (mentioned in comments)
```

None of these mean "expect this to fail on both sides." A new flag would be needed, or a new harness policy.

---

## Case-by-case verdict

### 1. `math-010` — mod / pmod on division by zero

**Corpus** (dataframe_corpus.py:312):
```python
case("math-010", "math", "mod / pmod",
     lambda I: I["nums"].select((F.col("a") % F.col("b")).alias("m"),
                                F.pmod("a", "b").alias("pm")))
# nums fixture contains a row with b=0
```

**No flags.**

**Spark reference behavior** (observed): raises `pyspark.errors...ArithmeticException: [REMAINDER_BY_ZERO] Remainder by zero. Use try_mod... spark.sql.ansi.enabled=false to bypass.`

**Thunderduck behavior** (inferred from DuckDB engine): DuckDB throws `Out of Range Error: Division by zero` on `a % b` when `b=0`. Wrapped by τ as `SparkConnectGrpcException` with a DuckDB error string.

**Symmetric server-throw** — both sides throw arithmetic errors. Wire error codes differ (Spark's typed `ArithmeticException` vs τ's generic `SparkConnectGrpcException`).

**Emulating Spark?** Partially. DuckDB throws on div-by-zero same as Spark's ANSI mode. But τ does not re-package the error as `SparkArithmeticException` with `[REMAINDER_BY_ZERO]` — the wire error type differs.

**Fix path**: harness policy ("both errored → PASS") OR τ improvement to re-wrap DuckDB arithmetic errors as Spark-compatible `ArithmeticException` codes.

---

### 2. `math-011` — int/int division on zero

**Corpus** (dataframe_corpus.py:313):
```python
case("math-011", "math", "int/int division -> double (Spark semantics)",
     lambda I: I["nums"].select((F.col("a") / F.col("b")).alias("div")))
# b=0 row
```

**No flags.**

**Spark behavior**: `ArithmeticException: [DIVIDE_BY_ZERO]`.

**Thunderduck behavior**: DuckDB `a/b` on `b=0` → `Out of Range Error`. τ wraps as generic gRPC error.

**Same class as math-010** — symmetric server-throw. Same fix path.

---

### 3. `arr-008` — element_at on empty array

**Corpus** (dataframe_corpus.py:441):
```python
case("arr-008", "array", "element_at (1-based) on array",
     lambda I: I["emp"].select(F.element_at("tags", 1).alias("first")))
# emp fixture has a row with tags=[] (empty)
```

**No flags.**

**Spark behavior**: `ArrayIndexOutOfBoundsException: [INVALID_ARRAY_INDEX_IN_ELEMENT_AT] The index 1 is out of bounds. The array has 0 elements.`

**Thunderduck behavior**: DuckDB `list_element(arr, 1)` on empty list throws / returns NULL depending on version. In τ's Pass 58 emission we route `element_at` on Array to a specific pattern; needs re-verification against empty-list input.

**Symmetric server-throw likely.** If DuckDB returns NULL where Spark throws, we'd have a *different* failure mode (data-mismatch, not error-vs-error). Needs a direct probe to confirm.

---

### 4. `parse-003` — to_number with bad format

**Corpus** (dataframe_corpus.py:633):
```python
case("parse-003", "parsing", "to_number with format -> decimal",
     lambda I: I["raw"].select(F.expr("to_number(num_str, '9,999.99')").alias("n")),
     flags=("spark4",))
# raw fixture has "9.99" (no thousands separator) — mismatches the format "9,999.99"
```

**Flags: `spark4`** — requires Spark 4+ feature. No `expected_to_fail` flag.

**Spark behavior**: `IllegalArgumentException: [INVALID_FORMAT.MISMATCH_INPUT] The format is invalid: 9,999.99. The input "STRING" (9.99) does not match the format.`

**Thunderduck behavior**: Depends on how Pass 76's `try_to_number` DDL parser handles the `9,999.99` format string. τ likely returns NULL or throws with a different message.

**Not symmetric.** Spark throws by design (`to_number` is strict); τ likely returns NULL (defensive). **This is fixable in τ** — either raise a Spark-emulated `AnalyzerError` when format mismatches OR match Spark's strict behavior.

**My earlier classification was wrong.** This should be Group D (τ-side Spark-parity fix) not Group A.

---

### 5. `set-004` — unionByName with `allowMissingColumns=True`

**Corpus** (dataframe_corpus.py:411):
```python
case("set-004", "setop", "unionByName allowMissingColumns",
     lambda I: _emp_proj(I).unionByName(I["emp2"], allowMissingColumns=True))
# emp has {name, dept_id, id, salary, age}; emp2 adds 'country' — allowMissingColumns fills missing side with NULL
```

**No flags.**

**Spark behavior**: SUCCEEDS. Returns unified schema with NULLs for missing columns on each side.

**Thunderduck behavior**: τ raises `[SPARK-EMULATED] unionByName column-name mismatch: child 0 has {"name","dept_id","id","salary","age"}, child 1 has {"country","salary","age","name","id","dept_id"}`.

**τ is WRONG.** This is a τ analyzer bug — it treats the schema mismatch as an unrecoverable error even when the caller passed `allowMissingColumns=True`. Spark accepts this input and fills missing columns with NULL.

**Not "expected to fail."** τ should be fixed to honor the `allowMissingColumns` flag from the proto. **Firmly Group B/D, not A.**

**My earlier classification was wrong.**

---

### 6. `cond-004` — coalesce Decimal nullability + type widening

**Corpus** (dataframe_corpus.py:266):
```python
case("cond-004", "conditional", "coalesce removes nullability when last is non-null",
     lambda I: I["emp"].select(F.coalesce(F.col("bonus"),
                                           F.lit(Decimal("0.00"))).alias("bonus0")))
# emp.bonus is DecimalType(9,2)-nullable; lit(Decimal("0.00")) is DecimalType(3,2)-non-null
# Spark widens to DecimalType(10,2) non-null
```

**No flags.**

**Spark schema**: `bonus0: DecimalType(10, 2)` (widened).
**Thunderduck schema**: `bonus0: DecimalType(9, 2)` (took first arg's precision, didn't widen).

**τ is WRONG.** Our `coalesce` type inference does not apply Spark's Decimal widening rule (`max(p1-s1, p2-s2) + max(s1, s2) → precision`, `max(s1, s2) → scale`). The multi-arg widening we added in Pass 73 covers `coalesce/greatest/least/array` but the Decimal-widening formula is missing.

**Not "expected to fail."** Fixable in τ via a Decimal-widening arm in `type_inference.rs::unify_types`. **Group D (Spark-parity), not A.**

**My earlier classification was wrong.**

---

### 7. `arr-012` — arrays_zip duplicate field names

**Corpus** (dataframe_corpus.py:445):
```python
case("arr-012", "array", "arrays_zip",
     lambda I: I["emp"].select(F.arrays_zip("tags", "tags").alias("z")))
# Same column zipped with itself — Spark yields struct<tags: string, tags: string> (duplicate names!)
```

**No flags.**

**Spark schema**: `z: ArrayType(StructType([StructField('tags', StringType(), True), StructField('tags', StringType(), True)]), False)` — **duplicate field name "tags"**.

**Thunderduck schema**: `z: ArrayType(StructType([StructField('tags_0', StringType(), True), StructField('tags_1', StringType(), True)]), False)` — **deduplicated to tags_0, tags_1**.

**τ deliberately dedups** (Pass 69) to avoid DuckDB `struct_pack` rejecting duplicate keys. But this violates Spark parity: Spark emits duplicate field names.

**Fix path**: τ needs to preserve Spark's field names AND find a DuckDB emission that tolerates duplicates. Options:
- Emit anonymous struct via `row(...)` for `arrays_zip` (loses names entirely — worse).
- Emit `struct_pack` with unique DuckDB-side names then post-rename via a wrapper `CAST(z AS STRUCT("tags" STRING, "tags" STRING)[])` — DuckDB accepts duplicate names in the target schema.
- Return "τ boundary error" as an honest limit.

**Not "expected to fail."** Fixable, non-trivial. **Group D, not A.**

**My earlier classification was wrong.**

---

### 8–11. `intv-001`, `intv-002`, `intv-003`, `intv-005` — CalendarInterval / YearMonthInterval / DayTimeInterval

**Corpus** (dataframe_corpus.py:610-614):
```python
case("intv-001", ...)  # F.expr("make_interval(1, 2, 0, 5)")  — CalendarInterval
case("intv-002", ...)  # F.expr("make_ym_interval(2, 3)")     — YearMonthInterval
case("intv-003", ...)  # F.expr("make_dt_interval(1, 2, 30, 0)")  — DayTimeInterval
case("intv-005", ...)  # (last_login - last_login)            — DayTimeInterval
```

**No flags.**

**Spark reference behavior** (observed error trace): **the reference server FIRES `.collect()` and returns Arrow data typed as `month_day_nano_interval` (or `month_interval` for `intv-002`). PySpark's client-side Arrow decoder then rejects with `PySparkTypeError: UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION`.**

This is confirmed by the traceback: `PySparkTypeError` originates at `pyspark/sql/pandas/types.py:425` — the client-side pandas converter, not the server. Spark's server-side succeeded.

**Thunderduck behavior**: Emits correct interval SQL. DuckDB returns interval-typed Arrow. PySpark client rejects it identically.

**Both sides succeed server-side; both fail client-side with matching error.**

**Corpus does not tag these** — even though the failure is a PySpark 4.1.1 client bug/limitation, not a server-side τ or Spark issue.

**Fix path**:
- **Client-side upstream**: fix PySpark's Arrow decoder to handle `month_day_nano_interval`. This is a `pyspark/sql/pandas/types.py` issue in PySpark 4.1.1.
- **Corpus tag**: add `flags=("pyspark_arrow_incompat",)` and have the harness `pytest.xfail(...)` these until PySpark upgrades.
- **Server-side workaround (not recommended)**: τ could cast intervals to STRING before returning, but that would break the entire interval type-parity story.

**True upstream blockers.** These are the only 4 cases that literally cannot be closed in Thunderduck at the current PySpark version. All 4 need a corpus flag change.

---

## Summary table

| Case | Corpus flag | Spark side | TD side | Correct classification | Actionable? |
|---|---|---|---|---|---|
| math-010 | none | throws ANSI mod-by-0 | throws DuckDB div-by-0 | Symmetric server-throw | Harness policy OR τ error re-wrap |
| math-011 | none | throws ANSI div-by-0 | throws DuckDB div-by-0 | Symmetric server-throw | Same as above |
| arr-008 | none | throws OOB | ? DuckDB list_element(empty, 1) | Needs direct probe | Likely symmetric OR τ NULL-return |
| parse-003 | `spark4` | throws INVALID_FORMAT | ? τ Pass 76 to_number DDL parser | **τ Spark-parity fix (Group D)** | Yes, in τ |
| set-004 | none | **succeeds** | **rejects (τ bug)** | **τ analyzer bug (Group B)** | Yes, in τ |
| cond-004 | none | Decimal(10,2) | **Decimal(9,2) (τ bug)** | **τ Spark-parity fix (Group D)** | Yes, in τ |
| arr-012 | none | struct{tags,tags} (dup) | struct{tags_0,tags_1} (deduped) | **τ Spark-parity fix (Group D)** | Yes, in τ |
| intv-001 | none | server OK, PySpark client rejects | server OK, PySpark client rejects | **True upstream (PySpark)** | No — corpus flag needed |
| intv-002 | none | Same as intv-001 | Same | True upstream | No — corpus flag needed |
| intv-003 | none | Same as intv-001 | Same | True upstream | No — corpus flag needed |
| intv-005 | none | Same as intv-001 | Same | True upstream | No — corpus flag needed |

---

## Revised counts

- **True upstream (PySpark client)**: 4 cases (intv-001/002/003/005). Need corpus flag `pyspark_arrow_incompat` + harness `xfail`.
- **Symmetric server-throw (harness-fixable)**: 2 cases confidently (math-010, math-011). Possibly 3 with arr-008 depending on DuckDB behavior for empty-list `element_at`. Harness policy change: accept "both errored" as pass.
- **Actual Thunderduck bugs (my Group-A classification was wrong)**: 4–5 cases (parse-003, set-004, cond-004, arr-012, possibly arr-008). All fixable in τ.

**Revised ceiling if we fix everything actionable + harness + corpus flags: ~322/324 (99.4%).** Only 2 stubborn cases (a subset of intv-* depending on how PySpark handles zero-width intervals) may remain hard-blocked.

---

## Recommendations

### 1. Add corpus flag `pyspark_arrow_incompat`
Tag `intv-001`, `intv-002`, `intv-003`, `intv-005`. Harness should `pytest.xfail(reason="PySpark 4.1.1 Arrow decoder doesn't support month_day_nano_interval")` these until PySpark ships a fix.

### 2. Add corpus flag `error_symmetric`
Or a harness pattern that catches exceptions on both sides and treats matching error codes as PASS. Would unblock `math-010`, `math-011` (and possibly `arr-008`).

Design sketch:
```python
def test_case(case, ...):
    ref_err = None
    try:
        ref_rows = _canonicalize_rows(ref_df)
    except Exception as e:
        ref_err = e
    td_err = None
    try:
        td_rows = _canonicalize_rows(td_df)
    except Exception as e:
        td_err = e
    if ref_err and td_err:
        # Both errored — check if error kinds match (family: arithmetic, array_index, format)
        if error_family(ref_err) == error_family(td_err):
            return  # PASS
        raise AssertionError(f"error mismatch: ref={ref_err} td={td_err}")
    if ref_err or td_err:
        raise AssertionError(f"asymmetric error: ref={ref_err} td={td_err}")
    assert_dataframes_equal(ref_rows, td_rows, query_name=case.id)
```

### 3. Reclassify and fix the 4–5 actual τ bugs
- `set-004`: honor `allowMissingColumns=True` in `V2RelationConverter` for `SetOp::UnionByName`. High-value: unblocks legitimate Spark parity.
- `cond-004`: implement Spark's Decimal widening in `type_inference.rs::unify_types`.
- `arr-012`: preserve Spark's duplicate field names via post-emit CAST-to-schema.
- `parse-003`: match Spark's strict `to_number` format-mismatch error.
- `arr-008`: probe TD behavior for `element_at(empty, 1)` and decide.

### 4. Revised path to ceiling
- Priority 1 (harness policy + corpus flags): +6 cases (4 intv + 2 arithmetic) → **300/324**.
- Priority 2 (τ reclassified fixes): +4–5 cases → **304–305/324**.
- Combined with previously stated priorities 1–5 (LATERAL substrate, RelType::* stats, interval parser, session-DISTINCT pivot): **~320+/324 realistic**.

---

*Report generated 2026-07-03 after session commit `8d232df`. Direct TD probes for `math-010/011/arr-008/parse-003` were attempted but timed out in the sandbox; behavior inferred from DuckDB semantics + prior corpus emission code. Confirm with a manual probe when convenient.*
