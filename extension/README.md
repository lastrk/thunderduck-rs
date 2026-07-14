# thunderduck-duckdb-extension

A [DuckDB](https://duckdb.org/) extension that implements Apache Spark-compatible functions, including decimal division, aggregate functions (e.g. skewness), `schema_of_json()`, and bit-parity hash functions (`spark_xxhash64`, `spark_hash`).

This extension is part of the [ThunderDuck](https://github.com/lastrk/thunderduck) project.

## Provenance

This directory is an in-tree import of what was previously a standalone
repository. The extension's source of truth is now `extension/` in
thunderduck-rs; the original repos are retained only as historical archives:

- **Origin repo**: [`nubank/thunderduck-duckdb-extension`](https://github.com/nubank/thunderduck-duckdb-extension) (to be archived)
- **Mirror**: [`lastrk/thunderduck-duckdb-extension`](https://github.com/lastrk/thunderduck-duckdb-extension) (to be archived)
- **Imported at**: origin HEAD [`33e8d49`](https://github.com/nubank/thunderduck-duckdb-extension/tree/33e8d49) — the extension's own content (`src/`, `test/`, `specs/`, `docs/thunderduck-integration.md`, build files, this README, `CLAUDE.md`, `.gitignore`) copied verbatim; `.github/`, `.claude/`, `.nu/`, and the `duckdb`/`extension-ci-tools` submodule *contents* were excluded (the submodules themselves are re-added at `extension/duckdb` and `extension/extension-ci-tools`, see `extension/BUILD_PINS.toml`).
- **Deliberate deltas vs the import HEAD** (everything else is byte-identical to `33e8d49`):
  - `BUILD_PINS.toml` — new file, the authoritative version pins.
  - `CLAUDE.md` — stale `v1.5.0` references corrected to `v1.5.4`, and re-hosting edits: intro reframed for the in-tree location, downstream consumer corrected to thunderduck-rs (`include_bytes!` embed), pointer to `scripts/dev/build-extension.sh`, and the agent-loading section updated (the origin repo's `.claude/` cpp agents were not imported).
  - `docs/thunderduck-integration.md` — rewritten for the Rust host (the original described the JVM/JDBC consumer).
  - `.gitignore` — one entry appended (a three-line block: blank line, comment, `.cache/` — a local clangd-index artifact, not present upstream).
  - This README — this Provenance section.
- **Vendored binaries built at**: [`20616b3`](https://github.com/nubank/thunderduck-duckdb-extension/tree/20616b3) — the release binaries checked into `extensions/vendored/` at the thunderduck-rs repo root were built from this commit. The only delta between `20616b3` and the `33e8d49` import HEAD is a security-scan CI workflow addition; no extension source changed.
- See `docs/context/extension-archival-checklist.md` (thunderduck-rs) for the pre-archival gates and the archive commands to run once this import is the sole source of truth.

## Spark hash functions

`spark_xxhash64(VARIADIC ANY) → BIGINT` and `spark_hash(VARIADIC ANY) → INTEGER` produce **bit-for-bit identical** values to Apache Spark's `org.apache.spark.sql.functions.xxhash64` and `hash` for every supported input type. Parity is verified by the test suite against Spark documentation goldens (e.g. `xxhash64('Spark', array(123), 2) = 5602566077635097486`).

### Integration contract (read before wiring into a translator)

This section is the load-bearing one for any agent or human translating Spark plans into DuckDB SQL — for example, the strict-mode `FunctionRegistry` in Thunderduck and Thunderduck-RS. The behavior below is intentional and the in-tree tests pin it down; the inline comments at the top of `src/include/spark_hash.hpp` repeat this for source-grep discoverability.

**Signatures**

| Function | Signature | Return type | Initial seed |
|---|---|---|---|
| `spark_xxhash64` | `spark_xxhash64(VARIADIC ANY) → BIGINT` | signed two's-complement | `42` (Long) |
| `spark_hash` | `spark_hash(VARIADIC ANY) → INTEGER` | signed two's-complement | `42` (Int) |

Both functions accept **zero or more** arguments. `spark_xxhash64()` returns `42`; `spark_hash()` returns `42`. The seed is not user-exposed (Spark's SQL surface does not expose it either).

**NULL semantics — do NOT wrap callers in `coalesce` / `IFNULL` / null filters**

A NULL value at a given column/row leaves the running hash seed **unchanged** for that row. The column is skipped, not propagated. This matches Spark's `HashExpression.eval`: `if (value == null) continue`.

```
spark_xxhash64(1::INT, NULL::INT, 2::INT)  ==  spark_xxhash64(1::INT, 2::INT)
spark_xxhash64(NULL)                        ==  42  (initial seed, unchanged)
spark_xxhash64()                            ==  42
```

The extension achieves this by setting `FunctionNullHandling::SPECIAL_HANDLING`. Without that flag, DuckDB would short-circuit any row containing a NULL argument and return NULL for the whole row — the **opposite** of Spark's semantic.

When translating Spark's `xxhash64(c1, c2, ..., cN)`, emit `spark_xxhash64(c1, c2, ..., cN)` **directly**. Do **not** add `coalesce(c, default_value)` (a non-NULL `default_value` folds into the hash, while NULL skips — they are not equivalent), do **not** filter out NULL columns at plan time (the column count is data-dependent and Spark's intent is per-row NULL-skip).

The same NULL-skip semantic applies to **null elements inside nested types**:

- `LIST<T>` / `ARRAY<T,n>`: null elements skipped
- `STRUCT`: null field values skipped (field names do not enter the hash)
- `MAP`: null values skipped; null keys are a Spark-side error and so are these
- NULL at the whole-list / whole-struct / whole-map level skips the column entirely

**Return type — do NOT wrap in `CAST AS BIGINT` / `CAST AS INTEGER`**

These functions return signed `BIGINT` / `INTEGER` directly. Unlike the `hashfuncs.xxh64` / `murmurhash3_32` path that previously sat behind Spark's `xxhash64` / `hash` (which returns unsigned `UBIGINT` / `UINTEGER` and required `CAST(... AS BIGINT)` plus the `thdck_to_signed_long` macro to dodge `UINT64 → INT64` cast overflow), `spark_xxhash64` and `spark_hash` produce values already in the signed two's-complement domain that Spark uses. Drop the `CAST` wrapper and drop the `thdck_to_signed_*` macros — they are not needed and add planner noise. The TPC-H `pmod(xxhash64(c, salt), Long.MaxValue) - K` pattern works without any cast.

**Bit-parity guarantee**

Outputs are bit-for-bit identical to Spark's, including:

- Sign-extension of `TINYINT` / `SMALLINT` to int32 before `hashInt`.
- NaN canonicalization for `FLOAT` / `DOUBLE` (`0x7FC00000` / `0x7FF8000000000000`).
- `DECIMAL(p ≤ 18)` hashed as `hashLong(unscaledLong)`; `DECIMAL(p > 18)` hashed as `hashBytes(BigInteger.toByteArray(unscaledValue))`.
- `INTERVAL` hashed as three primitives in order: `hashInt(months)`, `hashInt(days)`, `hashLong(microseconds)`.
- Spark's `Murmur3_x86_32.hashUnsafeBytes` per-byte tail behavior (one `mixK1` / `mixH1` per tail byte — **not** the canonical MurmurHash3 packed tail).
- Initial seed `42` for both algorithms.

The tests under `test/sql/spark_xxhash64.test` and `test/sql/spark_hash.test` are the authoritative parity oracle. The two Spark documentation goldens (`xxhash64('Spark', array(123), 2) = 5602566077635097486` and `hash('Spark', array(123), 2) = -1321691492`) are both pinned there.

**Unsupported types (bind-time error)**

The following DuckDB types throw at bind time with `spark_xxhash64 / spark_hash: type %s has no Spark equivalent; cast to a Spark-supported type explicitly`:

`UTINYINT`, `USMALLINT`, `UINTEGER`, `UBIGINT`, `HUGEINT`, `TIME`, `TIME_TZ`, `TIMESTAMP_S`, `TIMESTAMP_MS`, `TIMESTAMP_NS`, `UUID`, `BIT`, `ENUM`, `UNION`, `VARINT`.

The check is recursive — `LIST<UTINYINT>`, `STRUCT(x UTINYINT)`, `MAP<INT, HUGEINT>` all fail. Spark itself has no unsigned or HUGEINT type, and its `TimestampType` is microsecond-precision only, so Spark-Connect plans should never produce these. Erroring loudly is preferred to silent divergence.

**Relaxed-mode policy (Thunderduck integration responsibility)**

The extension provides bit-parity native functions; mode dispatch is the integration layer's responsibility:

- **Strict mode**: map Spark `xxhash64(...)` → `spark_xxhash64(...)`, `hash(...)` → `spark_hash(...)`.
- **Relaxed mode**: throw `UnsupportedOperationException("xxhash64 / hash require strict-mode parity; not available in relaxed mode")` at translation time. Do **not** fall back to the `hashfuncs` community extension or any other not-bit-parity path — that is the failure mode that motivated this work.
