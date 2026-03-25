# ADR-07: Expression System

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

← [Back to Architecture Overview](../architecture.md)
