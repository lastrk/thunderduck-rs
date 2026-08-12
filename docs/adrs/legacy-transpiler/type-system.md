# Type System (SUPERSEDED)

> **SUPERSEDED — DO NOT USE AS GUIDANCE — HISTORICAL REFERENCE ONLY.**
> This ADR describes the retired legacy v1 transpiler. The corresponding Rust modules were deleted on 2026-07-05. Kept in-tree as historical reference only. Active ADR index: [`../README.md`](../README.md).

**Decision: `DataType` enum mirroring Spark's type hierarchy**

```rust
pub enum DataType {
    Boolean,
    Byte,
    Short,
    Integer,
    Long,
    Float,
    Double,
    Decimal { precision: u8, scale: u8 },
    String,
    Binary,
    Date,
    Timestamp,
    TimestampNtz,
    YearMonthInterval,
    DayTimeInterval,
    Array(Box<DataType>),
    Map { key: Box<DataType>, value: Box<DataType>, value_nullable: bool },
    Struct(StructType),
    Null,
    Unresolved,
}

pub struct StructType {
    pub fields: Vec<StructField>,
}

pub struct StructField {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
}
```

`TypeInferenceEngine` centralises all type promotion rules (e.g., `Integer + Double → Double`, `SUM(Integer) → Long`, `COUNT → Long non-nullable`) following Spark semantics exactly.

---

← [Back to ADR Index](../README.md)
