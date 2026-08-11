# Phase 4 live function registry analysis — 2026-08-11

## Decision

Proceed with a bounded vertical prototype, not a wholesale 254-function
migration.

The code already contains one successful live registry: the 65-row aggregate
table. It is read by return-type inference, nullability inference, aggregate
classification, the SQL frontend, the analyzer, and emission. Extending that
pattern to ordinary functions can remove real authorities. Replacing the whole
function dispatcher with data cannot: most of scalar emission is irreducible
Spark-parity code, and moving those bodies behind rows would add an enum and an
interpreter without deleting the behavior.

The prototype is a go only if it makes migrated functions single-source and
deletes more production Rust than it adds. Full migration remains conditional
on that result.

Before implementation, amend ADR-009. Its current final decision explicitly
selects hand-written name matches and says that adopting a live interpreter
reopens the ADR. A registry implementation without that amendment would make
the code and the authoritative architecture disagree.

## Branch and evidence

- Branch: `refactor/live-function-registry`
- Stacked base: `refactor/interval-field-span` at `937d76db`
- SCIP snapshot: exact for `937d76db`
- User-owned worktree state excluded from this analysis:
  `.claude/settings.local.json`, `.agents/`, `.codex/`, and `AGENTS.md`

SCIP confirms the production ownership boundaries rather than only textual
matches:

- `SUPPORTED_FUNCTIONS` has seven references across the catalog module and
  catalog-operation tests; production exposure is through
  `is_supported_function` and `supported_function_names` in
  `connect-server::catalog_ops`.
- `TypeInferenceEngine::function_return_type` has one production caller,
  `Expression::function_call_data_type`.
- `render_function_call_dispatch` has one caller, `render_function_call`.
- `AGG_SPECS` has four direct readers in `type_inference.rs`, while its
  classifier accessor has production consumers in the SQL frontend, analyzer,
  and emitter.
- The three inference-facing literal/schema helpers in `emission.rs` have
  production callers in `expression.rs`; this is a real dependency inversion,
  not a similarly named helper.

## Current authorities

| Authority | Measured shape | Production role | Finding |
|---|---:|---|---|
| `function_catalog.rs` | 254 names; 283 pre-test lines | `functionExists`, `getFunction`, `listFunctions` | Catalog-only roster; dispatch does not consult it |
| `AGG_SPECS` | 65 rows | aggregate type, nullability, delegation, and classification | Successful live registry, but its two booleans admit contradictory kinds |
| `function_return_type` | 503-line function | ordinary scalar/collection/date type rules | Mixes reusable rules with expression-specific special cases |
| `function_call_data_type` | 193-line function | literal- and expression-shape-sensitive type rules | Correct home for genuine expression-sensitive cases |
| `function_call_nullable` | 131-line region | scalar and aggregate nullability | Repeats ordinary families and carries special semantic rules |
| scalar function emission | 1,785-line dispatcher | scalar SQL emission | Dominated by special Spark semantics; only a minority is tabular |
| aggregate emission | 240-line renderer | aggregate SQL emission | Repeats native, rename, extension, and DISTINCT outcomes already adjacent to aggregate specs |
| generator classification | 8 public spellings | both frontends lower calls to structured generators | Separate name authority from catalog despite Phase 3's structured IR |
| session substrate | 33 SQL macro definitions | direct SQL and selected emitted targets | Bodies are live implementations, not registry data |
| extension substrate | 10 exported C++ names | mandatory Spark-parity functions | τ currently emits nine; the existing `extension_targets()` is empty and has no production caller |

The broad affected regions total thousands of lines, but that is not the
available deletion budget. The 1,785-line scalar dispatcher mostly contains
arity-, type-, literal-, and error-sensitive Spark behavior that must remain
hand-written. The realistic win is moderate: consolidate names and ordinary
rules, delete simple mapping lists, and leave special bodies intact.

## Demonstrated drift

The catalog roster is not a live support denominator. The following functions
are handled by both inference and emission but absent from the 254-name catalog:

- `approx_count_distinct`
- `bit_get`
- `btrim`
- `json_object_keys`
- `max_by`
- `min_by`
- `substring_index`
- `try_avg`
- `try_sum`
- `any_value`

Additional names appear in one or more semantic paths while remaining absent
from the catalog, including `array_agg`, `approx_percentile`, `to_char`,
`date_trunc`, `array_size`, `map_size`, and several newer regexp functions.
Presence in one path does not prove end-to-end support; that uncertainty is the
problem. Today there is no artifact from which catalog exposure and a complete
live route can both be derived.

A focused pinned-Spark 4.1.1 probe confirmed `functionExists = true` for all
ten functions above and for `array_agg`, `approx_percentile`, `to_char`,
`date_trunc`, and `array_size`. It returned false for `map_size`, confirming the
opposite hazard too: internal compatibility spellings in inference must not be
blindly promoted into the public catalog.

The inverse is intentional in some cases: catalog names such as `explode`,
`inline`, `json_tuple`, and `stack` lower to the structured generator IR, while
`cast` lowers to syntax-specific IR. A registry therefore needs a closed
callable kind, not an assumption that every public function becomes a scalar
`FunctionCall`.

The aggregate table also exposes invalid combinations:

- `std`, `array_agg`, `approx_count_distinct`, and several other aggregate
  rows delegate aggregate typing but are not aggregate-classifier rows.
- `nth_value` and collection size functions live in `AGG_SPECS` only to reuse
  one rule, although they are not ordinary aggregates.

Independent `classifier` and `delegate` booleans encode those contradictions
instead of making function kind explicit.

The same Spark probe analyzed `std(id)`, `array_agg(id)`,
`approx_count_distinct(id)`, and `try_sum(id)` as Catalyst `Aggregate` nodes.
The existing false classifier flags are therefore implementation drift, not a
Spark distinction the new representation should preserve.

## Representative cross-layer matrix

| Spark spelling | Catalog | Current kind source | Type/nullability | Emission substrate |
|---|---|---|---|---|
| `abs` | yes | implicit scalar | first argument / any argument | native DuckDB fallback |
| `btrim` | no | implicit scalar | string / any argument | rename to `trim`; Phase 1 removed the shadow macro |
| `hash` | yes | implicit scalar | integer / non-null | extension `spark_hash` |
| `count` | yes | `AGG_SPECS` | long / non-null | native aggregate |
| `try_sum` | no | `AGG_SPECS` | sum-like / always nullable | extension `spark_try_sum` |
| `substring_index` | no | implicit scalar | string / any argument | irreducible special renderer |
| `array_remove` | yes | implicit scalar | array / argument-derived | live session macro with the same public name |
| `crc32` | yes | implicit scalar | long / argument-derived | renamed session macro `spark_crc32` |
| `explode` | yes | `generator.rs` | generator-owned output | structured `Generator`, not scalar emission |
| `cast` | yes | frontend syntax | cast target | structured `Cast`, not scalar emission |

This is the minimum shape a useful prototype must cover: native, rename,
extension, session substrate, aggregate, special, and non-scalar lowering.

## Recommended representation

Keep one row per accepted Spark spelling. Do not add alias canonicalization:
the spelling remains observable in errors and output naming, and both
frontends already canonicalize only ASCII case.

Use a closed kind whose payload contains only fields that kind can consume:

```text
FunctionSpec {
    name,
    implementation,
}

FunctionImplementation =
    Scalar { type_rule, null_rule, emit_rule }
  | Aggregate { type_rule, null_rule, emit_rule }
  | Generator { kind, outer }
  | LoweredSpecial(LoweredFunction)

EmitRule =
    Native
  | Rename(DuckFunction)
  | Extension(ExtensionFunction)
  | Session(SessionFunction)
  | Special(SpecialEmit)
```

Use enums and const data, with a sorted slice and binary search. Do not add a
proc macro, `build.rs` code generation, function pointers, trait objects, or a
new dependency.

`TypeRule`, `NullRule`, and `EmitRule` should express only repeated behavior.
Expression-sensitive rules stay in exhaustive special handlers. A special row
must name a closed enum variant so the registry remains the name-to-handler
authority; the handler body stays in its natural inference or emission module.

Do not put these fields into the first prototype:

- **Aliases.** Use one row per public spelling until semantic alias
  canonicalization is separately justified.
- **Arity.** Arity is currently enforced inside special lowering/emission
  paths. Adding a field before moving validation and deleting those checks
  creates two authorities and can change Spark error precedence.
- **Optional visibility flags.** Public Spark spellings belong in the
  registry. Internal DuckDB/extension targets belong in their closed target
  enums, not as callable function rows.

## Extension and session boundaries

The complete extension target set cannot be derived only from function rows:
`spark_decimal_div` is emitted by binary-operator handling. Centralize target
spelling in a closed `ExtensionFunction` enum used by both function and
operator emission. Validate every production-used variant against
`duckdb_functions()` after loading the mandatory extension. The extension may
export additional functions such as `spark_sum`; the invariant is that every
emitted target exists, not that every export must be emitted.

Move the live INV6 check to the runtime/extension-loader test boundary. τ must
not import runtime under INV10, while runtime may consume the τ-owned target
enum for validation. Delete the empty `extension_targets()` hook and its
`#[allow(dead_code)]` rather than preserving another list.

Session macro bodies remain in runtime. They are executable implementations,
not declarative function metadata. `EmitRule::Session` records the dependency;
a session-startup test checks that each required target exists. Phase 1 already
removed ten macros whose accepted calls are fully rewritten, so Phase 4 should
not repeat that deletion audit or move macro SQL into τ.

## Literal/schema parser direction

First move the function-literal adapters out of `emission.rs` into a neutral
τ module consumed by inference and emission:

- number-format precision/scale parsing;
- Spark DDL schema parsing for `from_json`;
- the flat-schema restriction for `from_csv`.

The shared `types::spark_ddl` parser can remain the lower value/type parser.
The architectural defect is `expression.rs -> emission.rs`, not the existence
of a value-level DDL parser used by both Connect conversion and τ.

## Ordered prototype

Each step receives focused tests and independent review before the next.

1. Amend ADR-009 to permit an interpreted, τ-owned registry for ordinary
   function rules while retaining exhaustive handwritten special handlers.
2. Extract the neutral literal/schema adapters. This is a dependency-direction
   change only.
3. Move the true aggregate subset out of `type_inference.rs` into the registry.
   Replace `classifier`/`delegate` booleans with `FunctionImplementation::Aggregate`;
   remove migrated aggregate spellings from the legacy catalog and aggregate
   emission lists.
4. Migrate one vertical scalar cohort:
   `abs`, `btrim`, `hash`, `array_remove`, `crc32`, and `substring_index`.
   This exercises every ordinary emit route plus a special handler.
5. Make catalog exposure the union of registry rows and the still-unmigrated
   legacy roster. A migrated spelling may appear in only the registry; exact-set
   tests reject overlap.
6. Measure production LOC and authorities. Stop if the interpreter is not net
   negative, if a migrated name remains authoritative in a second list/match,
   or if adding an ordinary function still needs more than one semantic row.
7. Only after the gate passes, migrate further simple families and structured
   generator names in small batches. Do not convert special emission bodies
   merely to increase registry coverage.
8. Centralize extension/session target enums, activate live existence checks,
   and delete the empty extension-target stub.

The temporary legacy roster is acceptable only for unmigrated names. It must
be named as migration state, have an exact disjointness test, and shrink in
every migration commit. It must never carry semantic fields or duplicate a
migrated spelling.

## Go/no-go metrics

The prototype passes only when all of the following hold:

- every field in a migrated row has a production reader;
- catalog, kind, ordinary type, ordinary nullability, and ordinary emission
  use the row for migrated functions;
- no migrated spelling remains in a legacy behavior list or name match except
  inside its named special handler;
- the six-function scalar cohort plus aggregate move is net-negative in
  pre-test production Rust;
- an additional ordinary native or renamed function requires one row and no
  new dispatcher arm;
- unknown names no longer reach an open-ended native fallback once the full
  migration completes;
- INV3 and INV10 stay active and green;
- format, lint, workspace unit tests, and both differential corpora retain
  their exact pre-slice baseline.

Expected outcome: a moderate production-LOC reduction and a large reduction in
places-to-edit, not deletion of the special-function dispatcher. If the
prototype grows a second meta-language or fails the LOC gate, retain the
handwritten scalar dispatcher, keep the successful aggregate registry, derive
catalog consistency tests, and close Phase 4 as a measured no-go.

## Implementation result

The bounded prototype passed. The live registry contains 73 rows: 59
aggregates, six representative scalars, and eight generator spellings. It
replaced the aggregate flags/table, migrated-name catalog entries, simple
emission lists, and generator name maps. Closed extension/session target enums
now have live DuckDB existence tests, and the empty INV6 hook was deleted.

Using the same pre-test physical-line metric as the analysis, the seven
function-owned files decreased from 10,188 to 10,128 lines (−60). Workspace
format, strict Clippy, and unit tests pass. Differential results preserve the
baseline: DataFrame 422 passed with the same seven deferred cases; SQL 426
passed with two intentional skips. Future migration remains conditional on the
same one-row/no-second-authority/net-negative gate.
