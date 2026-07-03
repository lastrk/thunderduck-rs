# v2 Corpus-Driven Pass Log

Append one entry per corpus-driven pass. Format per
`tasks/v2-corpus-driven-iteration-methodology.md` §Pass log.

## Session 2026-07-02 → 2026-07-03 (retroactive summary)

The first 57 passes ran under the earlier methodology iteration (before
this pass-log format was defined). Reconstructed from git commit trail
`4fb17b9`..`dd8d7e8` on branch `feat/v2-transpiler`. Corpus climb: 0 →
205 / 324 (63%). Detailed per-commit annotations in commit messages;
the summary below records the corpus deltas by pass number.

| Pass | Δ | Focus | Commit |
|------|----|-------|--------|
| E.0 + diagnostic | +25 | execute_streaming_query wiring; SingleRow subquery-safe; complex-type literals; timestamp construction; analyze_plan schema producer; NaN diff util | `4fb17b9` |
| 1 | +21 | WithColumns end-to-end + column-order contract | `27dadaa` |
| 2 | +9 | Aggregate operator + primitive function family | `47c478e` |
| 3 | +4 | ExpressionString via parser_v2::parse_expression | `b5491a7` |
| 4 | +9 | Join emission (8/14 cases) + USING dedup + explicit column list | `7fc7887` |
| 5 | +3 | DropColumns | `82fd174` |
| 6 | +7 | SetOp Union/Intersect/Except + widened CAST wrapper | `b0d499f` |
| 7 | +1 | Scalar function pass-through arm | `ce18fbb` |
| 8 | +2 | AliasedRelation + WithColumnsRenamed | `3989f66` |
| 9 | +9 | Deduplicate + cosmetic passthrough | `0e36303` |
| 10 | +1 | UNION BY NAME (analyzer + emission) | `93c4511` |
| **11** | **+30** | **Scalar function return types (~100 arms)** | `fec602d` |
| 12 | +5 | NA family (fill/drop/replace) | `ddadc31` |
| 13 | +1 | array/map/struct/locate/overlay remaps + GROUP BY alias-strip | `f648153` |
| 14 | +1 | date_add/date_sub → INTERVAL form | `a1c639f` |
| 15 | +1 | nvl/nvl2/ifnull/unix_timestamp/startswith | `60dc527` |
| 16 | +1 | array/list_* remaps + ExtractValue wiring | `129f080` |
| 17 | +3 | NaFill nullability tightening | `3510da2` |
| 18 | +1 | plan_id disambig in Join ON | `9b5ffbc` |
| 19 | +1 | USING-column donor rules for RIGHT/FULL | `f5f38a5` |
| 20 | 0 | ROLLUP/CUBE emission (test scaffolding fix `bf2b054`) | `1e7d1d5` |
| 21 | +2 | current_date DateType + datediff/months_between remaps | `9343653` |
| 22 | +1 | add_months + months_between return-type | `78733aa` |
| 23 | +1 | sha/sha1/sha2 + signum remaps | `ed05dcd` |
| 24 | +6 | isnull/like/eqNullSafe/split/bitwise remaps | `819681b` |
| 25 | +1 | overlay/nanvl/named_struct/map_contains | `2baa08f` |
| 26 | +2 | statistical aggregates (skewness/kurtosis/corr/covar/regr_/median) | `af8fb1c` |
| **27** | **+12** | **DataFrame-path aggregate grouping unfold** | `7dbf0f7` |
| 28 | +9 | Window expression wiring | `47eb957` |
| 29 | +3 | Lambda / HOF wiring | `d0496c9` |
| 30 | +1 | ceil/floor/signum/factorial types | `1dcf4da` |
| 31 | +2 | collect_list/set + approx_count_distinct non-null | `fb0b6d1` |
| 32 | +3 | Project-over-Join inlining for user aliases | `c6665a6` |
| 33 | +3 | count_if/grouping/grouping_id aggregates | `57c885f` |
| 34 | +2 | first/last/nth_value ignorenulls arg drop | `9d5e0bb` |
| 35 | +1 | Spark-parity truncation on float→int cast | `1d8e1c5` |
| 36 | +1 | regexp_replace global flag | `447d39e` |
| 37 | 0 | split element-nullability | `9158425` |
| 38 | 0 | allow zero-arg grouping/grouping_id | `d5e0a2c` |
| 39 | +1 | lag/lead nullability rule | `1b3fc7b` |
| 40 | +1 | String→(Date/Timestamp/numeric) cast nullability | `407d2f2` |
| 41 | +1 | spark_partition_id return type Integer | `2c81116` |
| 42 | 0 | typeof/spark_partition_id non-null | `deac7bd` |
| 43 | 0 | trunc(date, fmt) arg swap | `dc676d0` |
| 44 | +1 | always-nullable Spark scalars (factorial, url_encode, try_*) | `5a29af7` |
| 45 | +1 | date_format Spark→strftime token translation | `0ed222c` |
| 46 | +2 | Window frame spec parsing + emission | `24d2421` |
| 47 | +1 | dayofweek Sunday-index correction | `4aa5e52` |
| 48 | +1 | date_trunc returns Timestamp | `3024241` |
| 49 | +1 | ext6 extension remaps (spark_hash/xxhash64/try_divide/skewness) | `0145169` |
| 50 | +1 | kurtosis population formula | `d9aa9c5` |
| 51 | +2 | array/map constructor return types | `367ec8b` |
| 52 | +4 | array function return types + sort_array signature | `a0f4b1a` |
| 53 | 0 | lambda paren correction + aggregate fold init | `cf21a82` |
| 54 | +1 | ToDf positional rename | `2e491d7` |
| 55 | +2 | grouping_id/grouping populate group cols | `fd4d112` |
| 56 | 0 | percentile_approx/median/mode/any/every aggregates | `dd8d7e8` |

**Cumulative:** 0 → 205 / 324 (63%) across 57 recorded passes.

**Ground rules going forward (per iteration methodology):**
- Every new pass appends its own entry below this block.
- Include ADR citations, checklist §-anchors, layer(s) touched, compiler-warning delta, and commit SHA per methodology §Pass log.
- No pass is complete until findings = 0 (zero DEFER).

---

<!-- Add new passes below this line -->
