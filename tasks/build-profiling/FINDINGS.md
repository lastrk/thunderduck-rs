# Build profiling — baseline (single-threaded, `-j1`)

Machine: 10 cores, 15 GiB RAM, no swap. Toolchain: rustc/cargo 1.94.0.
Profile: `dev` (debug, unoptimized + debuginfo), relaxed mode (no `bundled-extension`).
All runs forced `-j1` so each `rustc`/`cc1plus` peak RSS == per-thread RAM footprint.

Harness (`tasks/build-profiling/`):
- `rustc_wrap.py` — RUSTC_WRAPPER; records per-crate wall + peak RSS via `os.wait4`/`getrusage`.
- `sampler.py` — polls `/proc` every 0.2 s for whole-build RSS + live compiler-process count.
- `run-profile.sh <label> [--clean] [-- <cargo args>]` — orchestrates a run.
- `analyze.py results/<label>` — summarizes a run.

## Headline numbers

| Scenario | Wall | Peak total RSS | Peak single process |
|---|---|---|---|
| **Cold build** (`--clean`, full tree) | **41 min** | **4.9 GiB** (final `ld`) | 4.3 GiB (`ld` / connect-server `rustc`) |
| **Edit connect-server** → rebuild bin | **100 s** | 4.6 GiB | 4.3 GiB (connect-server `rustc`) |
| **Edit core** → rebuild core + bin | **110 s** | 4.6 GiB | 4.3 GiB (connect-server `rustc`) |

## Cold build — where the 41 minutes go

Three roughly-equal thirds, two of which are **one-time** (DuckDB, cached after first build):

| Phase | Wall (approx) | Notes |
|---|---|---|
| DuckDB C++ compile (`cc1plus`) | ~18 min | 280 translation units, bundled amalgamation. **One-time.** |
| DuckDB `ar` archive + final `ld` link | ~18 min | `ar` rebuilds a **1.94 GB** `libduckdb.a` in chunks (serial); `ld` peaks **4.3 GiB**. |
| **Rust compilation (`rustc`)** | **~5 min** | All 339 crates. This is the only part the edit loop touches. |

`sum of rustc wall = 305 s`, but most of it is two crates (see below).

## Per-thread RAM ceilings (for sizing parallelism)

| Phase | Largest single process | Total (at `-j1`) |
|---|---|---|
| DuckDB C++ (`cc1plus`) | **2.06 GiB** (biggest TU) | 2.24 GiB |
| Rust (`rustc`) | **4.27 GiB** (connect-server bin) | 4.90 GiB |

Top Rust crates by RAM / time:

| crate | RSS | wall |
|---|---|---|
| `thunderduck_connect_server` (bin) | **4274 MiB** | **138 s** |
| `libduckdb_sys` (lib glue) | 2156 MiB | 7 s |
| `sqlparser` | 806 MiB | 12 s |
| `arrow_ord` | 502 MiB | 4 s |
| most other libs | <500 MiB | <4 s |

## The dev-loop bottleneck

**Every edit pays ~100 s rebuilding the `thunderduck-connect-server` binary at 4.3 GiB peak**, whether you
touched 1 line in the bin or in core. The bin `rustc` (codegen + monomorphization of the whole arrow/tonic/duckdb
surface) plus the final `ld` link of the 1.9 GB DuckDB static lib *is* the inner loop. core/lib itself is cheap (2 s).

## Where parallelism could be increased (currently `jobs = 2` in `.cargo/config.toml`)

The `jobs = 2` cap was set to avoid OOM during the DuckDB C++ build. On this 15 GiB box that is **far too
conservative**:

1. **DuckDB C++ phase** — largest TU = 2.06 GiB. Budget ~2 GiB/job → **6–7 parallel jobs** fit in 15 GiB.
   This phase is one-time but dominates first build / version bumps and CI.
2. **Rust lib phase** — almost every dependency crate is <500 MiB; 10 parallel jobs fit comfortably. At `-j1`
   the build spent **1347 s with exactly one compiler busy** — pure serialization, the biggest parallelism win.
3. **NOT parallelizable** (memory/architecture floor, leave serial):
   - Final `ld` link — single 4.3 GiB process.
   - connect-server bin `rustc` — single 4.3 GiB process, 138 s; the inner-loop long pole.
   - `ar` archiving of `libduckdb.a` — inherently serial; better *avoided* than parallelized.

### Implication: jobs cap should be phase-aware

A single global `jobs = 2` throttles the cheap Rust phase (could be 10) to protect against the expensive C++
phase (safe at 6). Raising the global cap to ~6 and relying on RAM headroom is the simple win; the C++ TU peak
(2 GiB) is the binding constraint, not the Rust crates.

## Candidate optimizations (not yet applied — for the next step)

- **Linker**: swap `ld` → `lld`/`mold` — directly attacks the ~18-min link tail and the 100 s inner-loop link.
- **Raise `jobs`** 2 → ~6 (RAM-justified above); consider letting the Rust phase use all cores.
- **Avoid rebuilding DuckDB**: prebuilt/system `libduckdb` instead of `bundled`, or `sccache`, kills the ~36-min
  one-time C+++archive cost on clean builds / version bumps / CI.
- **Shrink the bin rebuild**: `split-debuginfo = "unpacked"`, lower `debug` level, `codegen-units` tuning, or
  splitting connect-server so edits don't re-monomorphize the whole arrow/tonic surface.
- **`cargo check` / `cargo-nextest`** for the type-check / test inner loop (skips the 4.3 GiB codegen+link entirely).

## Caveats

- `cargo check` loop was **not** cleanly measured: a concurrent `cargo build --release` from another session held
  the build lock and wedged it. (First-from-cold `check` also re-checks the whole tree; only a *warm* incremental
  check is representative — expected to be a few seconds since it skips codegen + link.)
- `ar` time shows up as "idle" in the sampler (it watches compilers/linkers, not `ar`); confirmed live via `ps`.
