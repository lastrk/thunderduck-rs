# Dev Journal — 2026-03-21 — Reference Gap Analysis Rewrite + Bug Investigation

**Date**: 2026-03-21
**Branch**: main
**Status**: Doc update only — no code changed. 665 passing, 4 failing (unchanged).

---

## Summary

`docs/reference-gap-analysis.md` was written on 2026-03-18 before Phase 3 and listed every
Critical/Important item as open. Phases 3 and 4 have since closed all of them. The document was
fully rewritten as an accurate 2026-03-21 snapshot: 18 items moved to the closed-gaps table,
and 2 active bugs + 3 Phase 5 gaps + medium/low open items documented with file locations.

Additionally, the Java reference implementation was explored to understand the exact fix strategy
for the two remaining active bugs (Section 2 of the gap analysis). Findings documented below
for the upcoming fix session.

---

## Java reference investigation: Bug 1 — NAReplace NULL handling

**File**: `.reference/connect-server/src/main/java/com/thunderduck/connect/converter/RelationConverter.java`
**Method**: `convertNAReplace()` lines 1936–1948

The Java implementation calls `Literal.toSQL()` to render the old-value, which returns the string
`"NULL"` for a null literal. It then uses a string comparison:

```java
if ("NULL".equals(oldVal)) {
    selectList.append(" WHEN ").append(quotedCol).append(" IS NULL THEN ").append(newVal);
} else {
    selectList.append(" WHEN ").append(quotedCol).append(" = ").append(oldVal).append(" THEN ").append(newVal);
}
```

**Rust approach**: The fix must operate at the AST level (per architecture rules — no SQL string
inspection). In the Rust converter, detect `Expression::Literal(Literal { value: None, .. })`
(null literal) and route to `IS NULL` instead of `= NULL` in `gen_na_replace`.

---

## Java reference investigation: Bug 2 — Empty LocalRelation join produces wrong column count

**File**: `.reference/core/src/main/java/com/thunderduck/generator/SQLGenerator.java`
**Method**: `generateEmptyValues()` lines 3297–3330, called from `visitLocalDataRelation()` lines 3272–3287

When a `LocalDataRelation` has zero rows, the Java generator emits a schema-preserving SQL:

```sql
-- With schema "id INT, value STRING"
SELECT CAST(NULL AS INT) AS "id", CAST(NULL AS VARCHAR) AS "value" WHERE FALSE
```

Each column gets an explicit `CAST(NULL AS <duckdb_type>) AS "colname"` projection. The `WHERE FALSE`
ensures zero rows but the column list is fully typed and named — DuckDB infers the output schema
from the SELECT list, not from any data.

Without this, a bare `SELECT * FROM (VALUES (NULL)) AS t WHERE FALSE` collapses to a single
anonymous column, losing all schema information when the empty relation participates in a join.

**Rust approach**: In `gen_local_relation`, when `rows.is_empty()`, generate the explicit
`SELECT CAST(NULL AS type) AS "col", ... WHERE FALSE` form using the `schema` field of
`LocalRelation`. This requires `StructType` field iteration and type-to-DuckDB-SQL mapping.

---

## Documents changed

- `docs/reference-gap-analysis.md` — full rewrite (2026-03-18 → 2026-03-21)
