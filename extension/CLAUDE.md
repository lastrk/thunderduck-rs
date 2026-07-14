# CLAUDE.md — thdck_spark_funcs

Rules and context for `extension/` — the in-tree **thdck_spark_funcs** DuckDB
extension: Apache Spark-compatible scalar and aggregate functions, so that
queries thunderduck-rs pushes down to DuckDB produce **bit-for-bit identical**
results. This directory was absorbed from the (since-retired) standalone
`nubank/thunderduck-duckdb-extension` repository — see `README.md`'s
Provenance section. It is C++ living inside a Rust repo: the root `CLAUDE.md`
governs process (plan mode, verification-before-done); **this file** governs
everything C++ under `extension/`.

> The `## Quality Gate` and `## Spark-Parity Contract` sections below are
> load-bearing — any change under `extension/` is held to them.

## Project Overview

The extension registers, under `namespace duckdb`, the following functions:

| Function | Signature | Notes |
|----------|-----------|-------|
| `spark_xxhash64` | `(VARIADIC ANY) → BIGINT` | Bit-identical to Spark `xxhash64`. Seed 42. |
| `spark_hash` | `(VARIADIC ANY) → INTEGER` | Bit-identical to Spark `hash` (Murmur3). Seed 42. |
| `spark_decimal_div` | `(DECIMAL, DECIMAL) → DECIMAL` | Spark 4.1 division precision/scale rules, ROUND_HALF_UP. |
| `spark_try_divide` | `(numeric, numeric) → DOUBLE/DECIMAL` | Spark `try_divide`; ÷0/NULL → NULL; DECIMAL path uses Spark decimal rules. |
| `spark_sum` | aggregate | Spark `sum` overflow/precision semantics. |
| `spark_try_sum` | aggregate | Spark `try_sum`; overflow → NULL. |
| `spark_avg` | aggregate | Spark `avg`. |
| `spark_try_avg` | aggregate | Spark `try_avg`; integer/float → DOUBLE; DECIMAL path reuses `spark_avg`. |
| `spark_skewness` | aggregate | Spark `skewness`. |
| `spark_schema_of_json` | `(VARCHAR) → VARCHAR` | JSON schema extraction. |
`spark_xxhash64` and `spark_hash` share a single templated implementation via
**tag dispatch** (`SparkXxh64Tag` / `SparkMurmur3Tag`) — one code path, two
algorithms. Changes to the shared path affect both functions.

- **Language / standard:** C++11 (enforced — see conventions below).
- **DuckDB:** pinned submodule at **v1.5.4** (`duckdb/`) — see `BUILD_PINS.toml` (this directory) for the authoritative pin.
- **CI scaffolding:** `extension-ci-tools/` submodule (Makefile templates).
- **Downstream consumer:** thunderduck-rs itself — the built binary is adopted
  into `../extensions/vendored/` and embedded into the server via
  `include_bytes!` (`crates/core/build.rs`). See `docs/thunderduck-integration.md`
  for the version-lock contract and release flow.

## Layout

```
src/
  thdck_spark_funcs_extension.cpp   # entry point: Load() registers all functions
  include/
    spark_hash.hpp                  # hash validation + tag-dispatch exec
    spark_murmur3.hpp  spark_xxh64.hpp   # hash primitives
    spark_aggregates.hpp            # SUM / AVG / SKEWNESS states + ops
    decimal_division.hpp  spark_precision.hpp  wide_integer.hpp
    spark_schema_of_json.hpp
test/sql/*.test                     # SQLLogicTest files (the test suite)
```

The extension is **header-heavy**: one translation unit
(`thdck_spark_funcs_extension.cpp`) includes the headers under `src/include/`,
where the real logic lives.

## Build & Test

```bash
make debug             # debug build (fast iteration) -> build/debug/
make release           # release build (-O3)          -> build/release/
make test              # == test_release
./build/release/test/unittest "test/*"   # run the SQLLogicTest suite directly
./build/debug/test/unittest   "test/*"   # same, against the debug build
make clean             # remove build/
```

The preferred entry point is `../scripts/dev/build-extension.sh [--init] [--smoke]`
(run from the repo root): it initializes the two submodules, asserts the
three-way version lock (submodule tag == `BUILD_PINS.toml` == `duckdb` crate),
wraps `make release`, checks the built binary's footer, and with `--smoke`
also runs `make test` plus the swap-in proof against thunderduck-rs's
`extension_loader` tests. All-platform release builds are CI-only
(`../.github/workflows/extension-release.yml`, manual dispatch).

Tests are **SQLLogicTest** `.test` files in `test/sql/`. Parity functions must
include **golden values taken from Spark itself** (not approximations) — e.g.
`spark_xxhash64('Spark', array(123), 2) = 5602566077635097486`.

Do **not** edit `CMakeLists.txt` build flags, the `duckdb/` submodule, or
`extension_config.cmake` to accomplish a feature — the build is driven by
DuckDB's extension macros.

## Quality Gate

Run after every implementation and after every review fix, in order. Stop and
fix on the first failure.

1. **Build:** `make release`
   (for fast local iteration `make debug` is acceptable, but the gate is not
   green until `make release` succeeds).
2. **Format:** `make format-check`
   (auto-fix with `make format`; config is `.clang-format`).
3. **Tests:** `make test`  → `./build/release/test/unittest "test/*"` must be
   all-green. Any new or changed behavior **requires** a `test/sql/*.test`
   case; parity behavior requires a Spark-sourced golden.
4. **Static analysis (when C++ under `src/` changed):** `make tidy-check`.
   This reconfigures and runs clang-tidy and is slow; it may be skipped for
   test-only or docs-only changes — say so in your log when you skip it.

A single targeted test run: `./build/release/test/unittest "test/sql/<file>.test"`.

## Spark-Parity Contract

These invariants are the whole point of the extension. **Never** violate them;
a reviewer must flag any change that does.

- **NULL is skipped, not propagated.** Hash/aggregate functions use
  `FunctionNullHandling::SPECIAL_HANDLING`. Never "fix" NULLs with `COALESCE`
  or `WHERE ... IS NOT NULL`, and never switch to `DEFAULT_NULL_HANDLING`.
- **Hash functions return signed integers.** `spark_hash → INTEGER`,
  `spark_xxhash64 → BIGINT`. The result is a **bit reinterpret** of the
  unsigned accumulator, not a cast. Do **not** wrap results in `CAST` — DuckDB
  rejects unsigned→signed casts with the high bit set.
- **Initial seed is 42.** Empty argument list returns `42`.
- **Unsupported types fail at bind time, recursively.** Unsigned integer types
  (`UTINYINT`…`UBIGINT`, `HUGEINT`), `UUID`, `BIT`, `ENUM`, `UNION`, `VARINT`,
  and sub-microsecond time types are rejected — including when nested
  (`LIST<UTINYINT>`, `STRUCT(x UTINYINT)`, `MAP<INT, HUGEINT>`).
- **Decimal division uses Spark 4.1 rules.** Operands promote to
  `DECIMAL(38, scale)`; rounding is ROUND_HALF_UP; intermediate scaling can
  overflow 128 bits, so wide arithmetic (`__builtin_mul_overflow`, `Mul128` /
  `Div256By128`, `uint256_t`) is mandatory, not optional.

`README.md` is the canonical prose statement of this contract.

## C++11 + DuckDB Conventions

**C++11 only.** DuckDB v1.5.4 pins `CMAKE_CXX_STANDARD "11"`. Do not use
C++14/17/20 features: no `std::optional`, no structured bindings
(`auto [a, b]`), no `if constexpr`, no generic-lambda `auto` params, no
`std::make_unique`, no fold expressions, no `std::string_view`. "Modern C++"
here means **RAII, move semantics, templates / tag dispatch / CRTP,
const-correctness** — expressed within C++11 — plus DuckDB's own idioms.

- **Namespace:** all code in `namespace duckdb`.
- **Memory:** `make_uniq<T>()` (DuckDB's factory), `unique_ptr`,
  `optional_ptr` — never raw `new`/`delete`, never `std::make_unique`.
- **Types:** `Vector`, `DataChunk`, `LogicalType`, `PhysicalType`,
  `UnifiedVectorFormat`, `ValidityMask`, `SelectionVector`, `string_t`,
  `hugeint_t`, `idx_t`.
- **Vectorized execution — think in batches, not rows.** A function processes a
  whole `DataChunk`. Provide a **fast path** for flat, all-valid vectors
  (`FlatVector::GetData<T>()` + `__restrict`) and a **generic path** via
  `ToUnifiedFormat` that honors the **selection vector** and **validity mask**.
- **Errors:** throw DuckDB exceptions — `InvalidInputException`,
  `BinderException` (bind-time type rejection), `InternalException`,
  `NotImplementedException`, `OutOfRangeException`. Validate types in the bind
  function; keep the execution path hot.
- **Performance idioms already in the codebase:** `__restrict`,
  `__builtin_expect`, `__builtin_mul_overflow`, branchless arithmetic,
  bind-time constant hoisting (e.g. `pow10`). No external deps
  (`vcpkg.json` is empty) — use DuckDB's standard library, not Boost.
- **Strings:** write outputs with `StringVector::AddString` / `AddStringOrBlob`;
  never return a `string_t` pointing at a freed buffer.

## Documentation Policy (for the docs-updater agent)

Keep prose in sync with shipped code. Canonical docs, in order of authority:

1. **`README.md`** — the integration contract and function semantics. Update
   when a function's signature, NULL/seed/return-type behavior, or supported
   types change.
2. **`docs/thunderduck-integration.md`** — the Thunderduck bundling / version-lock
   guide. Update when build outputs, platform matrix, or the DuckDB version
   change.
3. **`CLAUDE.md`** (this file) — update the function table, Quality Gate, or
   parity contract when they change.

Apply the **minimum** edit needed; do not touch a doc a change does not affect.

## Per-Role Reading Guide

The origin repo's C++-specialized `.claude/agents/` were **not** imported —
thunderduck-rs's own agents (Rust-oriented) operate here. Whoever works under
`extension/` in a given role should load context accordingly:

| Role | Read |
|------|------|
| architect | `CLAUDE.md`, `README.md`, `docs/thunderduck-integration.md` |
| coder | `CLAUDE.md`, `README.md`, relevant `src/include/*.hpp` |
| reviewer | `CLAUDE.md`, `README.md` (parity contract) |
| perf | `CLAUDE.md` (build & vectorization), the changed sources |
| diagnostician | `CLAUDE.md`, `README.md`, Spark docs + `duckdb/` source |
| docs-updater | this Documentation Policy section |
