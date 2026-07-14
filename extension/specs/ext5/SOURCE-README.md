# DuckDB extension specifications

Handoff artifacts for the `thunderduck-duckdb-extension` project (a separate
repo, not this one). When a v2-transpiler slice identifies a `spark_*`
function it needs but which is not yet present in the pinned
`thdck_spark_funcs` release (currently `ext4` per ADR-020), the slice writes
a specification here and defers the dependent corpus cases to a future
"`ext<N+1>` release slice".

A separate session working on the extension project consumes the specs,
implements the functions against the DuckDB extension SDK, ships a new
release, and this repo pins the new binary and consumes the newly-available
functions.

## File naming

One spec per function, named `spark_<name>.md`. Examples:
- `spark_hash.md` — Spark's `hash(...)` (Murmur3-32, signed INT, seed 42).
- `spark_xxhash64.md` — Spark's `xxhash64(...)` (xxHash64, signed BIGINT, seed 42).
- `spark_decimal_div.md` — decimal division with `ROUND_HALF_UP`.

## Content template

Every spec file MUST contain the following sections (see
`tasks/v2-slice-d-initial-prompt.md` §"Specification template" for the
authoritative template):

- **Function name** — the DuckDB-side symbol.
- **Spark equivalent** — the exact Spark function / semantic being replicated.
- **Signature** — input types, return type, variadic vs. fixed-arity, aggregate vs. scalar.
- **Semantic contract** — what makes Spark's behavior distinct from DuckDB's native form.
- **Corpus test cases** — the exact case IDs this function unblocks.
- **Reference implementation pointer** — Spark source and/or legacy `SqlGenerator::gen_expr` arm.
- **Dependencies** — DuckDB internals, other `spark_*` functions, or other DuckDB extensions.
- **Testing notes** — a minimal SQL test that exercises the function once implemented.

## Lifecycle

1. A v2-transpiler slice identifies a missing function.
2. The slice's coder writes `tasks/duckdb-extension-specs/spark_<name>.md`.
3. The slice's DEFER list (in `tasks/v2-adr-readiness-map.md`) gains a
   "Pending C++ extension work" heading naming each spec file and the
   corpus case IDs each unblocks.
4. The extension session consumes the specs and ships `ext<N+1>`.
5. A follow-up "`ext<N+1>` release slice" in this repo:
   - Pins the new binary in `crates/core/build.rs` / `crates/core/src/runtime/`.
   - Adds the newly-available functions to
     `emission::extension_targets()` and the appropriate arms in
     `emission::render_function_call` / `render_aggregate`.
   - Removes the corresponding spec files from this directory (moved to
     `tasks/duckdb-extension-specs/archived/<ext-N+1>/` for history).
   - Removes the DEFER carryover from the readiness map.
6. INV6's mechanical check picks up the new function names automatically.

## Anti-patterns

- **Do not write specs speculatively.** Only write a spec when a
  corpus-driven need is identified. Slice ADRs govern what functions we
  need; speculative extension work is outside ADR-010's scope.
- **Do not leave a spec file without a DEFER entry in the readiness map.**
  The pair is the load-bearing contract; a spec without a DEFER is
  orphaned work, and a DEFER without a spec is un-actioned work.
- **Do not implement the C++ side in this repo.** The extension project is
  intentionally separate; PRs that touch `crates/core/build.rs` beyond
  the release pin are scope creep.
