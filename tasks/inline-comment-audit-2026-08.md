# Inline-comment audit

**Status:** Complete — approved for publication after review and verification

**Branch:** `refactor/inline-comment-audit`

**Base:** `bff14bd` (`refactor: remove unreachable transpiler paths`)

**Recorded:** 2026-08-09

## Objective

Keep comments only where they preserve information the code cannot state
locally: architectural boundaries, Spark/DuckDB parity, safety constraints,
wire contracts, portability, and surprising test setup. Delete narration,
decorative separators, review history, pass numbers, status ledgers, and test
comments that merely repeat the assertion. Shorten retained rationale and check
it against the implementation while editing.

This is an isolated comment-only branch. It must not alter executable tokens,
identifiers, diagnostics, test expectations, SQL statements/results, shebangs,
or file modes. Comments embedded in SQL string literals are in scope; only
their comment text may change, never the executable SQL token stream.

## Scope

The audit was partitioned into independent path-owned waves:

- `crates/core/src/transpiler_v2/analyzer.rs`;
- `crates/core/src/transpiler_v2/emission.rs`;
- the remaining `crates/core/src/transpiler_v2/` modules;
- the remaining first-party `crates/core/src/` modules;
- `crates/connect-server/src/`;
- first-party Python and shell under `tests/` and `scripts/`;
- `extension/src/` and comments in `extension/test/` SQLLogicTest files.

Generated protobuf code, vendored dependencies and binaries, third-party
submodules, golden/data files, and SQL query bodies are excluded. Documentation
is excluded except for references made stale by this audit.

## Decision rule

Retain or restore a comment when removing it would hide one of these facts:

1. why an apparently simpler implementation is wrong;
2. a non-local invariant or ownership boundary;
3. a Spark-parity rule not apparent from Rust/C++ types;
4. unsafe, overflow, alignment, vector-selection, or wire-format behavior;
5. an operational contract such as readiness ownership or platform variance;
6. why a test input reaches a specific optimizer, merge, or overflow path.

Move historical provenance to ADRs/tasks rather than keeping it beside live
code. Prefer one compact rationale at the decision point over repeated comments
at every caller or assertion.

## Measured result

Counts below cover the 111 changed code/test/tool files and count standalone
comment lines using the same pattern before and after. They are a directional
size measure, not a claim about semantic value.

| Partition | Files | Before | After | Removed |
|---|---:|---:|---:|---:|
| Rust | 38 | 14,831 | 10,907 | 3,924 |
| Extension C++ | 10 | 515 | 120 | 395 |
| Extension SQLLogicTest | 17 | 906 | 402 | 504 |
| Python and shell | 46 | 1,838 | 1,195 | 643 |
| **Total** | **111** | **18,090** | **12,624** | **5,466** |

Excluding this task record but including the stale-reference correction in
`extension/README.md`, the audit changes 112 files with 1,150 inserted and
6,921 deleted lines (net −5,771).

## Review corrections

Independent domain reviews found no behavior defect, but they did catch places
where the first pass removed too much or crossed the comment-only boundary. The
branch now:

- restores the hidden-sort placement/order invariant and the two original N5/N8
  diagnostic strings;
- keeps concise rationale for decimal wire normalization, LocalRelation schema
  precedence, interval `RawSql`, partial plan-ID traversal, async schema
  discovery, and gRPC error mapping;
- preserves the shared Spark DDL additive-grammar contract;
- documents Spark-process readiness ownership and GNU/macOS and conda naming
  portability;
- restores extension formulas and safety notes for decimal precision, 256-bit
  arithmetic, aggregate-state alignment, vector selection/validity, NaN bits,
  skewness merge/finalize behavior, and optimizer-path tests;
- restores formatter-required C++ namespace-closing labels;
- repairs malformed rustdoc and the README reference to the shortened skewness
  implementation note.

## Verification

- `git diff --check` — passed.
- Changed Rust files: `rustfmt --check --edition 2021` — passed.
- `cargo fmt --check` — passed.
- `cargo clippy -- -D warnings` — passed after repairing one empty rustdoc line.
- Changed Python files: `python3 -m py_compile` — passed.
- Changed shell files: `bash -n` — passed.
- Aggregate non-comment diff scan — only trailing comments and embedded SQL
  comments remain; domain reviewers found no executable or expected-output
  change.
- `cargo test -j1 --features bundled` — passed: 134 connect-server tests and
  1,265 core tests, with only the repository's declared ignored suites. The
  serial retry bounded peak memory after the first parallel link was killed.
- `cargo build -j1 --release --features bundled` — passed.
- Extension `make release` — passed.
- Extension `make format-check` — passed with the repository's documented
  version-check override and system `clang-format-18`.
- Extension `make test` — passed: 547 assertions in 24 SQLLogicTest cases.
- Extension `make tidy-check TIDY_BINARY=clang-tidy-18` — passed.
- DataFrame corpus — 421 passed with the same seven baseline-red cases:
  `errcls-006`, `sqlwrap-001` through `sqlwrap-005`, and `prettyname-004`.
- SQL corpus — 424 passed and two declared skips.
- Witness gate — all 829 baseline-pass cases remain green, zero regressions,
  and 14/14 witness flips remain green.

The user approved committing and publishing this branch on 2026-08-09. Merge
remains subject to PR review.
