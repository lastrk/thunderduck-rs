# ADR-023 — Resolve references to qualifier lineage and ordinals

> **Retired 2026-08-10. Do not use this ADR as implementation guidance.**
> [ADR-024](../../thunderduck-rearchitect-ADRs.md#adr-024--τ-stores-attribute-identity-in-the-resolved-schema-references-bind-to-attributes-not-positions)
> replaced its attribute-binding representation, and
> [ADR-026](../../thunderduck-rearchitect-ADRs.md#adr-026--τ-mirrors-spark-connects-plan_id-tree-lookup)
> replaced its plan-ID clauses. The text below is retained only as design history.

**Former status:** Proposed; partially implemented before supersession
**Depends on:** ADR-005, ADR-006, ADR-021, ADR-022

## Historical decision

τ carried string qualifiers (`e.name`, `__td_jl.col`) from analysis into emitted
SQL. Wrapping a child under a new subquery alias could strand those qualifiers.
The design combined Calcite-style ordinal binding with Spark DataFrame
source-qualifier lineage so analysis could distinguish a projected-through
column from a newly aliased one.

Resolve each reference once at analysis time to qualifier lineage plus ordinal,
then regenerate any SQL qualifier from the current emission scope:

- one local qualifier binding resolves to its ordinal;
- multiple local bindings are ambiguous;
- no local binding resolves only through inherited source-qualifier lineage or
  a correlated outer scope; otherwise it is unknown;
- emitted names are uniquified for SQL safety, while resolved and wire schemas
  preserve Spark's duplicate names; and
- ordinal remaps occur only at structural boundaries such as joins, set ops,
  USING contraction, and `SelectBlock` merging.

The rejected alternatives were pure ordinals without lineage, emission-side
qualifier stripping, and carrying qualifier strings through later rewrites.
The intended migration was to delete stranded-qualifier repair machinery after
the ordinal/lineage resolver covered the relevant corpus witnesses.

## Retirement rationale

ADR-024 replaced ordinal-indexed parallel lineage with `Attribute`-owned
`ExprId` and source qualifiers. ADR-023 also incorrectly treated Spark Connect
`plan_id` as another qualifier-scope binding. ADR-026 instead mirrors Spark's
top-down tagged-plan search and ancestor-output filtering.
