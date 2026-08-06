# τ error-class audit — review A1 + C1 (2026-08-06)

Closes the two findings from `tau-architecture-review-checklist.md` that carried
product consequence and were screened out of the 2026-08-04 landing by its
LOC-threshold filter (see that file's line "**A1 and C1 are the review's two
findings with product consequence and are still open**").

## Method — observed, never guessed

Every class token below was obtained by **running the repro against live Apache
Spark 4.1.1** (local[1], `spark.sql.ansi.enabled=true`, via the vendored distro
at `.spark/spark-4.1.1`) and reading `getCondition()`. Tokens were then checked
against Spark's own authoritative catalogue — 1244 conditions, extracted with:

```
unzip -p .spark/spark-4.1.1/jars/spark-common-utils_2.13-4.1.1.jar \
      error/error-conditions.json
```

This mattered. All three sites that previously carried a *prose* pseudo-class
were wrong or incomplete:

| Site | Prose token it carried | What Spark actually raises |
|---|---|---|
| LATERAL + NATURAL | `UNSUPPORTED_FEATURE` | **`INCOMPATIBLE_JOIN_TYPES`** |
| LATERAL + USING | `UNSUPPORTED_FEATURE` | **`UNSUPPORTED_FEATURE.LATERAL_JOIN_USING`** (subclass) |
| recursive CTE `UNION` | `UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE` | same ✓ (the only correct one) |

No condition among the 1244 mentions a natural join at all, so the
`UNSUPPORTED_FEATURE` token for LATERAL+NATURAL was fabricated.

## A1 — the category defect

`AnalyzerError::Other` sat under the `// ── Spark-emulated ──` header, but the
bridge keyed on `spark_class().is_some()`; `Other` returned `None`, so it fell to
`EmissionError::Unsupported { name: "analyzer-spark-emulated" }` →
`Status::unimplemented`. ADR-022 category 1 (Spark rejects it too) was reported
as category 2 (τ gap) for ~30 distinct rejection sites.

The message was also unrecoverable by the differential oracle. Its regexes are
`^\s*\[([A-Z][A-Z0-9_.]*)\]` and an unanchored twin
(`tests/integration/utils/dataframe_diff.py`). Run against the real τ message
both return `None`: the anchored one because the message opens with
`τ emission error:`, the unanchored one because hyphens fall outside the token
character class, so `[SPARK-EMULATED]` does not match either. Those cases were
*structurally incapable* of passing an error-class comparison.

**Fix.** Branch the bridge on a new `AnalyzerError::category()` (one exhaustive
match) rather than on the class. Add `AnalyzerError::SparkEmulated { class,
reason }` for sites whose class is established, and `AnalyzerError::Internal`
for τ bugs. `EmissionError::SparkEmulated.class` became
`Option<&'static str>` so a classless Spark-emulated error renders prefix-free
instead of inventing a token — it still exits `INVALID_ARGUMENT`, because the
*category* decides the status, not the presence of a class.

## Classification of the production sites

**Assigned a real, observed token (18):**

| Trigger | Spark 4.1.1 class |
|---|---|
| `toDF` arity mismatch | `ASSIGNMENT_ARITY_MISMATCH` |
| recursive CTE column-list arity | `ASSIGNMENT_ARITY_MISMATCH` |
| recursive CTE anchor/recursive arity | `ASSIGNMENT_ARITY_MISMATCH` |
| LATERAL + NATURAL | `INCOMPATIBLE_JOIN_TYPES` |
| LATERAL + USING | `UNSUPPORTED_FEATURE.LATERAL_JOIN_USING` |
| recursive CTE `UNION` (not ALL) | `UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE` |
| `unionByName` column-name mismatch | `_LEGACY_ERROR_TEMP_1201` (see note) |
| set-op arity mismatch | `NUM_COLUMNS_MISMATCH` |
| `inline` / `json_tuple` / `stack` arity | `WRONG_NUM_ARGS.WITHOUT_SUGGESTION` (3 sites) |
| `stack` first arg not a positive int | `DATATYPE_MISMATCH.UNEXPECTED_INPUT_TYPE` |
| `stack` alias-count mismatch | `UDTF_ALIAS_NUMBER_MISMATCH` |
| scalar subquery returning >1 column | `INVALID_SUBQUERY_EXPRESSION.SCALAR_SUBQUERY_RETURN_MORE_THAN_ONE_OUTPUT_COLUMN` |
| unpivot with no value columns | `UNPIVOT_REQUIRES_VALUE_COLUMNS` |
| ragged / mismatched `VALUES` | `INVALID_INLINE_TABLE.NUM_COLUMNS_MISMATCH` (2 sites) |
| SQL `UNPIVOT` with an empty value list | `PARSE_SYNTAX_ERROR` (see note) |

**Left classless — `Other`, correctly (4):** Spark raises a bare
`IllegalArgumentException` with no condition attached.

- `NATURAL <LeftSemi|LeftAnti> JOIN` → Spark: `requirement failed: Unsupported
  natural join type LeftSemi`. τ's message is already **byte-identical**.
- invalid regex in `colRegex` → `Unclosed character class near index 4`.
- `allowMissingColumns` without by-name matching, and set-op with <1 child —
  not reachable from the public API (defensive against a malformed proto).

**Reclassified as τ-internal → `Status::internal` (3):**

- `union-of-names produced orphan name` — a broken analyzer invariant.
- `stack_multi_alias` alias slots not string literals — `stack_multi_alias` is a
  τ *synthetic*; Spark has no such function, so no Spark class can exist.
- empty `VALUES` — unreachable from SQL (no syntax yields zero rows).

### Notes on two judgment calls

- **`_LEGACY_ERROR_TEMP_1201`** is emulated verbatim per an explicit decision.
  It is one of Spark's own un-migrated placeholders and will break if Spark
  renames the condition — revisit when the Spark pin moves.
- **`PARSE_SYNTAX_ERROR`** for empty-`IN` SQL `UNPIVOT` is a class from Spark's
  *parser*, surfaced here from the analyzer because `sqlparser` accepts the
  shape Spark's grammar rejects. Class-correct, layer-odd.

## Bonus finding — 5 sites where τ was over-strict (all fixed)

The audit turned up the mirror image of C1: **τ rejecting input Spark accepts.**
Each was verified twice — Spark analysed it *and* produced rows.

| τ guard (removed) | Spark 4.1.1 |
|---|---|
| `dropFields("X")` for absent X | Accepted; struct returned unchanged |
| unpivot id/value overlap | Accepted → `id, k, v` |
| unpivot variable name collides with an id | Accepted → `id:bigint, id:string, v:double` |
| unpivot value name collides with an id | Accepted → `id, k, id` |
| unpivot variable == value name | Accepted → `id, same:string, same:double` |

Two comments in the code asserted the opposite — *"Spark itself rejects overlap
between ids and values"* and *"the stamped output schema would carry two fields
with the same name — Spark rejects this"*. **Both are false.** Spark permits
duplicate names in an output schema; it only rejects an ambiguous *reference*.

Emission needed no change: `apply_update_fields_ops` and
`update_fields_data_type` already silently ignored a missing drop target, which
turns out to be exactly Spark's behaviour, so the comments calling that path
"unreachable" were updated rather than the code. The now-unused
`validate_update_fields_ops` was deleted.

## C1 — `analyze_to_df` omitted the lineage clear

`WithColumnsRenamed` clears `source_quals` on renamed slots; `analyze_to_df`
did the identical clone-and-rename and did not, while its comment claimed
parity ("same as WithColumnsRenamed"). There was exactly **one**
`source_quals.clear()` in the 14k-line file. Since both stamp
`TypedOp::WithColumnsRenamed` — for which `RelScope::of` yields zero scope — a
qualified ref fell to the tier that resolves via `source_quals`, so
`df.alias("t").toDF("y", ..).select("t.y")` resolved in τ and raises
`UNRESOLVED_COLUMN` in Spark. Leniency-only: τ over-accepted, never returned
wrong data.

Fixed with one unconditional clear (`toDF` renames every column). The SQL
`FROM t AS x(a, b)` shape is unaffected because `v2_lowering` wraps `ToDf` in an
`AliasedRelation`, whose `seed_source_quals` re-seeds every slot afterwards —
pinned by a dedicated ordering test so the clear can never be hoisted above the
seed.

## Third gap, surfaced while building the witnesses

`pretty_name` has no `UpdateFields` arm, so an unaliased `dropFields(...)`
projection is auto-named `expr` where Spark names it
`update_fields(s, dropfield())`. Data matches; only the generated column name
diverges. This is the same family as the already-deferred `prettyname-004`
(Window arm) and the `CaseWhen` arm that landed in PR #25 — it was invisible
before because τ *rejected* these projections outright, so no case could reach
the naming path.

Carried as `errcls-006`, **deferred** and not in the baseline. `errcls-004`
carries an explicit alias so the over-strictness fix is witnessed independently.

## Verification

- `cargo fmt` clean; `cargo clippy --workspace --all-targets -- -D warnings`
  zero; `cargo test --workspace` 1391 passed / 0 failed. The two C1 behavioural
  tests were confirmed to **fail** with the fix reverted (the ordering test
  passes either way by design — it guards a future regression, not this one).
- **DataFrame corpus: 421 passed / 7 failed.** All 7 are documented deferrals —
  `sqlwrap-001..005` and `prettyname-004` (both pre-existing, from PR #25) plus
  the new `errcls-006`. **Zero regressions.**
- **SQL corpus: 420 passed / 0 failed** — fully green, including the 4 new
  witnesses.
- Reference side: all 9 witnesses' expected classes were observed against live
  Spark 4.1.1 before being declared; the two acceptance cases were confirmed to
  return rows. τ side verified through the real Connect server via the golden
  oracle. Goldens for `errcls-004`/`005`/`006` recorded with
  `run-differential-tests.sh --record core`.

Witness index: `tests/integration/error_class_witness_manifest.json`.
