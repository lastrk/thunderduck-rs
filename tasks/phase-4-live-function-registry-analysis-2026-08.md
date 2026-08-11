# Phase 4 live function registry — 2026-08-11

## Decision

The bounded prototype was net-negative and proved that one live registry could
own callable identity without turning Spark-specific behavior into a template
language. The full cleanup therefore migrates every public spelling while
preserving five distinct implementation routes:

```text
FunctionImplementation =
    Scalar(ordinary interpreted rules)
  | Aggregate(ordinary interpreted rules)
  | Generator(structured generator)
  | Special(closed handwritten handler)
  | Lowered(frontend-owned syntax/IR)
```

`Special` is a permanent semantic distinction, not a migration backlog. A
function belongs there when its type, nullability, or SQL body depends on
Spark-specific expression shape, literals, arity-sensitive error behavior, or
another rule that is clearer as Rust than as registry data.

## Final shape

`function_registry.rs` contains one sorted `FunctionSpec` row per observable
Spark spelling. A row owns its public name and exactly one route:

- `Scalar` and `Aggregate` carry closed type, nullability, and emission rules.
- `Generator` carries the structured generator kind and outer flag.
- `Special` carries one `SpecialFunction` discriminant. Type inference,
  nullability, and emission exhaustively match the enum.
- `Lowered` identifies syntax that the frontend must convert to structured IR;
  it cannot reach scalar emission.

The registry has no callbacks, function pointers, trait objects, aliases,
arity metadata, proc macro, generated source, or test-only rows. Arity remains
with the semantic handler so Spark's error precedence stays intact.

## Result

The live registry owns all 352 supported Spark and Connect spellings:

| Route | Rows | Meaning |
|---|---:|---|
| Scalar | 159 | ordinary interpreted rules |
| Aggregate | 60 | ordinary aggregate rules |
| Generator | 8 | structured generator lowering |
| Special | 123 | closed handwritten handlers |
| Lowered | 2 | frontend-owned syntax/IR |

The cleanup deleted:

- the 204-name legacy catalog and registry/catalog union;
- aggregate classifier flags and parallel aggregate emission lists;
- raw-name ordinary type and nullability families;
- migrated scalar emission arms and permissive unknown-name native fallback;
- duplicate generator-name maps and the empty extension-target hook.

`function_catalog.rs` is now only the catalog view of the registry. Unknown
functions stop at a Thunderduck boundary. Adding an ordinary function is one
row; adding a special handler produces compiler exhaustiveness errors at all
three semantic consumers.

`histogram_numeric` is classified as an aggregate, matching Spark 4.1.1, and
its histogram `x` field preserves the input type. `cast` and `when` remain
lowered; higher-order, format-sensitive, interval, JSON, and other genuinely
custom functions remain special. Ranking/window functions use ordinary scalar
rules; Connect's symbolic null-safe and bitwise spellings use closed special
handlers instead of an open native fallback.

## Acceptance gates

- Every registry field has a production reader.
- No public spelling or function kind has a second authority.
- Closed target enums retain live extension/session existence checks.
- Pre-test production Rust is net-negative from the Phase 4 prototype commit.
- Format, strict Clippy, workspace tests, and both differential corpora preserve
  their prior-green baselines.

The full cleanup removes another 31 pre-test production lines from the
prototype's five affected modules, for a cumulative Phase 4 reduction of 91
production lines.

Verification is green: format, strict workspace Clippy, all workspace tests,
and both complete differential corpora. DataFrame finishes 422 pass / 7 known
deferred; SQL finishes 426 pass / 2 skips; the 829-case prior-green oracle has
zero regressions.
