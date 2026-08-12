# ADR-016 — Pinned reference version and ANSI-mode configuration; coverage claims are version-and-config-scoped

**Status:** Proposed
**Depends on:** ADR-010
**Depended on by:** ADR-014, ADR-015, ADR-017, ADR-018, ADR-022, ADR-026

**Context.** Spark's semantics (type coercion, nullability, function behavior, SQL grammar) evolve across versions. They also branch on runtime configuration — most critically `spark.sql.ansi.enabled`, which selects between two materially different execution semantics for arithmetic, array indexing, cast overflow, string-to-number parsing, and several other primitives. Coverage claims and inference fidelity are only meaningful against a fixed semantics *and* a fixed config profile.

**Decision.** Everything is pegged to Apache Spark 4.1.1 **running in ANSI SQL mode** (`spark.sql.ansi.enabled=true`, the Spark 4.x default). Under ANSI mode:
- Division / remainder by zero raises `SparkArithmeticException [DIVIDE_BY_ZERO]` / `[REMAINDER_BY_ZERO]` instead of returning NULL.
- `element_at` / array subscript on out-of-bounds indices raises `SparkArrayIndexOutOfBoundsException [INVALID_ARRAY_INDEX_IN_ELEMENT_AT]` instead of returning NULL.
- Numeric-cast overflow raises `SparkNumberFormatException [CAST_INVALID_INPUT]` instead of returning NULL / silently truncating.
- `to_number` on a format-input mismatch raises `SparkIllegalArgumentException [INVALID_FORMAT.MISMATCH_INPUT]`.
- Interval-related conversions surface their strict Spark types (`YearMonthInterval`, `DayTimeInterval`) rather than being coerced to a permissive representation.

Callers who want the non-ANSI semantics use Spark's opt-in `try_*` families (`try_divide`, `try_mod`, `try_element_at`, `try_cast`, `try_to_number`, …) or `NULLIF(x, 0)` guards — those are explicit, τ-emittable, and NULL-returning. **τ matches ANSI Spark by default and matches `try_*` when the caller wrote `try_*`.** τ MUST NOT silently rewrite an ANSI arithmetic path to a NULL-returning wrapper.

The pinned-artifact set also includes DuckDB and its extensions, with a floor of **DuckDB ≥ v1.5.3** where the Iceberg write path (ADR-018) is used (required for MERGE, ALTER, and Iceberg v3 deletion vectors). A version *or config* bump re-derives the coverage denominators (ADR-009, ADR-014) and re-runs both suites (ADR-015). The pinning policy is bump-and-re-run, with the pinned version and `spark.sql.ansi.enabled=true` both hard CI checks against the Spark image under test.

**Error emulation contract (interaction with ADR-022).** When ANSI semantics call for a strict-throw and τ's emission delegates to DuckDB, τ MUST surface the failure as a Spark-emulated error carrying Spark's error-class code (e.g. `DIVIDE_BY_ZERO`, `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`), not as an opaque `SparkConnectGrpcException` wrapping a DuckDB error string. DuckDB's engine-level throws MUST be caught in the runtime layer and re-wrapped with Spark's error taxonomy before crossing the wire. This is what makes the differential oracle (ADR-015) able to compare error paths symmetrically — "both errored with matching class" is a legitimate PASS mode; "τ errored with a different class" is a divergence.

**Consequences.**
- (+) Coverage claims and parity guarantees are precise and auditable against a definite semantics *and* a definite runtime config.
- (+) The ANSI pin eliminates a whole category of ambiguity: `a / 0` has exactly one correct answer (throw with `DIVIDE_BY_ZERO`), not two.
- (+) The differential oracle (ADR-015) becomes tri-state on any given case: **both-succeed** (compare rows), **both-throw with matching error class** (PASS), or **anything else** (fail). The harness must implement this tri-state — see ADR-015 refinement hooks.
- (−) A version *or* config bump is a planned, multi-day effort (regenerate denominators, re-run suites, reconcile any new divergences in coercion/nullability/SQL-grammar/error-class), not a silent change.
- (−) Re-wrapping DuckDB engine errors as Spark-classed errors is a real runtime-layer responsibility; τ owns the mapping table `DuckDB error kind → Spark error class` (arithmetic, array-index, cast, format, decimal-overflow).
- (neutral) A `SparkConfig { version: "4.1.1", ansi_enabled: true, ... }` constant documents the contract and preserves room for config-conditional behavior if ever needed (default: none; all unconditional on the pinned config).

**Non-goals.** Non-ANSI mode is out of scope. If a user needs `spark.sql.ansi.enabled=false` semantics, they call `try_*` explicitly. τ does not offer a "relaxed mode" runtime switch — parallel to ADR-020's elimination of "relaxed extension mode."

**Refinement hooks.**
- Establish the version-and-config-bump runbook.
- Enumerate the `DuckDB error kind → Spark error class` mapping table exhaustively (arithmetic, array-index, numeric cast, decimal overflow, string-to-number format, JSON parse). This is a τ runtime-layer artifact and should be locked with unit tests plus corpus witnesses (the ANSI-throw cases: math-010, math-011, arr-008, parse-003, cast overflow variants, …).
- Decide whether any transformation ever needs to be version-conditional or config-conditional (default: none; all unconditional on the pinned config).
- Update the differential harness (ADR-015) to implement the tri-state error comparison: both-succeed / both-throw-matching-class / divergence.

---

