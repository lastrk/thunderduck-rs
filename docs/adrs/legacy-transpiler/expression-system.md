# Expression System (SUPERSEDED)

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as a historical reference to the pre-τ architecture. ADR index: [`../README.md`](../README.md) · τ spine: [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

**Decision: Rust `enum` (not `Box<dyn Trait>`)**

The expression set is closed (all types known at compile time). Enum variants are zero-allocation, exhaustively matchable, and avoid vtable dispatch overhead.

```rust
pub enum Expression {
    Literal(Literal),
    ColumnReference(ColumnReference),
    UnresolvedColumn(UnresolvedColumn),
    Binary(BinaryExpression),
    Unary(UnaryExpression),
    FunctionCall(FunctionCall),
    Cast(CastExpression),
    CaseWhen(CaseWhenExpression),
    Window(WindowFunction),
    Alias(AliasExpression),
    Star,
    InSubquery(InSubquery),
    ExistsSubquery(ExistsSubquery),
    ScalarSubquery(ScalarSubquery),
    Lambda(LambdaExpression),
    LambdaVariable(LambdaVariableExpression),
    RawSql(RawSqlExpression),
    ArrayLiteral(ArrayLiteralExpression),
    MapLiteral(MapLiteralExpression),
    StructLiteral(StructLiteralExpression),
    Between(BetweenExpression),
}
```

Key methods (implemented via `match`):

| Method | Purpose |
|--------|---------|
| `to_sql(&self) -> String` | Generates SQL text for DuckDB. **Never** implement via `Display` or `Debug` — those are for humans. |
| `data_type(&self, schema: &StructType) -> DataType` | Type inference |
| `nullable(&self) -> bool` | Null propagation |

**Rejected alternative**: `Box<dyn ExpressionTrait>` — heap allocation per node, no exhaustiveness, no benefit for a closed set.

---

← [Back to ADR Index](../README.md)
