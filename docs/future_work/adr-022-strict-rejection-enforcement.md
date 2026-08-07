# Design issue: ADR-022's category-3 strict rejections are specified but unenforceable

**Status**: documented finding, not scheduled
**Date**: 2026-08-07 (analysis on `fix/tau-error-category-and-todf-lineage`)
**Area**: `tests/integration/differential/{sql,dataframe}_corpus.py` and their
runners, `tests/integration/utils/dataframe_diff.py`,
`crates/core/src/parser_v2/mod.rs`,
`crates/core/src/transpiler_v2/error.rs`,
`crates/connect-server/src/error.rs`, ADR-022 Amendment 1

> **⚠ Read this first: the hazard is the arbiter, not the plumbing.**
> The tempting implementation — "τ strict-rejects whatever `sqlparser` rejects"
> — is wrong, and a 36-case survey against live Spark 4.1.1 proves it
> (`tasks/tau-error-class-audit-2026-08.md`). `sqlparser`'s grammar fails as a
> proxy for the standard in **both** directions: it rejects valid Spark (HiveQL
> `TRANSFORM`, `CREATE TABLE ... USING parquet`) and accepts malformed SQL that
> Spark rejects (`SELECT id + FROM emp`). A detector built on it would tell
> users their valid queries are malformed — strictly worse than today's honest
> "not implemented in Thunderduck". ADR-022 Amendment 1 therefore makes its
> **enumerated register** the authority, and any implementation must keep it
> that way.

## Current behavior

ADR-022 Amendment 1 (commit `6e0ad9f`) added error **category 3 — strict
rejections**: inputs Spark *accepts* that τ deliberately rejects as malformed
under the dialect Spark documents itself as following. The amendment specified
two mechanisms that do not exist, so the policy is currently documentation
only.

**1. Register entry #1 is unguarded.** The register's sole entry is
`SELECT * FROM emp WHERE`, which Spark accepts by parsing `WHERE` as a table
alias and returning every row unfiltered. τ does reject it — but for the wrong
reason and with the wrong category:

```
StatusCode.UNIMPLEMENTED
details = "τ: unsupported proto shape `sql::parse_error`:
           sql parser error: Expected: ..., found: EOF"
```

That is a Thunderduck-boundary error, i.e. "τ has not implemented this". The
input is not unimplemented — it is malformed, and τ rejects it on purpose. The
category is wrong in exactly the way ADR-022 Amendment 1 exists to fix.

The cause is that all three parse-failure sites flatten every `sqlparser` error
into one boundary error, with no way to distinguish a registered malformation
from a genuine grammar gap:

```rust
// parser_v2/mod.rs:56, parser_v2/mod.rs:103, v2_lowering.rs:4789
Parser::parse_sql(&dialect, parse_input).map_err(|e| EmissionError::Unsupported {
    kind: UnsupportedKind::ProtoShape,
    name: "sql::parse_error".to_owned(),
    reason: e.to_string(),
})?
```

**2. The corpus cannot express a deliberate divergence.** Case flags today are
`cosmetic`, `nondeterministic`, `schema_only`, `spark4`. There is no way to say
"Spark returns rows, τ errors, and that is correct". Such a case can only be
permanently red — indistinguishable at a glance from a regression. This
falsifies ADR-022's own stated property, *"every red case is a τ bug or an
unimplemented feature"*; the amendment updated that bullet to name a
`divergent` flag, which is vapour until built.

## The fix

Two phases, deliberately sequenced witness-first so the born-red → green
transition is visible in history (the pattern used for the A1/C1 witnesses).

### Phase 1 — the `divergent` mechanism (Python, harness only)

Both corpus runners share a three-branch shape (`record` → `expected_error` →
normal diff), so the change is symmetric across
`test_sql_corpus_differential.py` and `test_dataframe_corpus_differential.py`.

- `Case` gains `divergent_error: Optional[str]` carrying **τ's** expected class
  (not Spark's), with `flags=("divergent",)` for greppability. The runner should
  assert the two co-occur — one without the other is a corpus authoring error
  and must fail loudly rather than silently skip.
- A new runner branch, placed after `expected_error` and before the normal diff:
  - PASS iff τ raised **and** its class equals `divergent_error`.
  - FAIL if τ returned rows (the divergence silently vanished) **or** raised a
    different class (drifted). Both directions matter; a flag that only catches
    one is half a guard.
  - `live`: additionally assert Spark returns rows. If Spark starts erroring,
    the register entry's premise is void and the case must fail.
  - `golden`: assert a `rows`-kind golden exists (`golden.read_golden`) —
    standing evidence Spark accepted at record time.
  - `record`: record Spark's rows as normal; do **not** skip the way
    `expected_error` cases do.
- Shared helper `assert_registered_divergence(...)` in `dataframe_diff.py`
  beside the existing `reconcile_error_parity`, reusing `capture_outcome` /
  `_sql_outcome` and `spark_error_class`.
- Register entry #1 lands as a corpus case, **born red**.

### Phase 2 — τ emits a strict rejection (Rust)

- `EmissionError::StrictRejection { rule: &'static str, message: String }`,
  Display `[THUNDERDUCK_STRICT.{rule}] {message}`, mapped to
  `Status::invalid_argument`. The `TranspilerV2Emission` match in
  `connect-server/src/error.rs` is exhaustive, so the compiler forces the arm.
- A τ-owned token namespace is correct here precisely *because* Spark raises
  nothing — inventing a Spark class would repeat the A1 mistake. Verified safe:
  none of Spark's 1244 conditions starts with `THUNDERDUCK`.
- Centralise the three parse-failure sites behind one
  `classify_parse_failure(sql, &ParserError) -> EmissionError` that is
  **default-safe**: it returns today's boundary error unless the input
  positively matches a registered pattern. Default-safe is the whole design —
  see the warning at the top.
- Entry #1's detector is necessarily token-level, since `sqlparser` failed and
  there is no AST to inspect: tokenize under `SparkDialect`, take the last
  significant token, fire only if it is a `Word` in a const allow-list.

## Secondary observations

- **A research step gates Phase 2.** The allow-list must be empirical, not
  guessed. Spark accepts `FROM emp WHERE` (alias) but rejects
  `FROM emp GROUP BY` (`PARSE_SYNTAX_ERROR`) — so a naive "trailing reserved
  word" rule would wrongly capture a category-1 case. Probe live Spark across
  the reserved-word list to find which trailing keywords it really accepts as an
  alias. Only verified ones may enter the list, and **each needs its own row in
  ADR-022's register** — the ADR states that an unregistered strict rejection is
  a bug, not an application of policy. Expect the register to grow.
- **Keep the allow-list and the register in lockstep.** Worth a comment on the
  const pointing at the ADR and vice versa, so editing one surfaces the other.
- **No `AnalyzerError` counterpart is needed** while entry #1 is parse-stage.
  When the first analyzer-stage strict rejection lands, add an
  `ErrorCategory::StrictRejection` variant — the `ErrorCategory` doc comment in
  `analyzer.rs` already warns against overloading `Internal`, which would report
  a deliberate policy decision as a τ bug.
- **Opposite defect, tracked separately.** τ currently *accepts*
  `SELECT id + FROM emp`, which Spark rejects
  (`UNRESOLVED_COLUMN.WITHOUT_SUGGESTION`). Amendment 1 obliges τ to tighten
  here, so the policy creates work rather than only ratifying today's behaviour.
  Recorded in ADR-022 as a known non-conformance; different mechanism (too
  lenient, not too strict) and should not ride along with this.

## Verification sketch (when implemented)

- Unit: `classify_parse_failure` upgrades entry #1 and — more importantly —
  **does not** upgrade the negative cases: trailing `GROUP BY`, keyword typo,
  unbalanced paren, unterminated string, empty `UNPIVOT (… IN ())`.
- **Critical interaction:** `parseerr-001..005` in `sql_corpus.py` are
  *category-1* witnesses expecting `PARSE_SYNTAX_ERROR`. The detector must not
  capture any of them, or it silently converts documented category-1 cases into
  category-3 ones. Re-run `-k parseerr` and confirm all five still fail for
  their original reason.
- Both corpora, no previously-green regression. Baselines at the time of
  writing: DataFrame 421 passed / 7 failed, SQL 420 passed / 6 failed (all
  documented deferrals).
- Disable the detector once and confirm the divergent case goes red. A flag that
  cannot fail is worthless.
