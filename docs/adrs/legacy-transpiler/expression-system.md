# Expression System (legacy path)

> **Status: existing implementation — runs behind `--transpiler legacy` (the default).** The authoritative v2 architecture supersedes this file where they conflict; the two paths coexist, so do not delete the legacy path to make room for v2. This file's v2 successor is listed in the legacy→v2 map in [`../README.md`](../README.md); the v2 spine is [`../../thunderduck-rearchitect-ADRs.md`](../../thunderduck-rearchitect-ADRs.md).

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
