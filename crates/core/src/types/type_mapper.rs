use std::borrow::Cow;

use super::DataType;

/// Maps Spark `DataType` to DuckDB SQL type strings (used in CAST / DDL).
///
/// There is intentionally no DuckDB → Spark string parser here: schema
/// inference goes through DuckDB's typed Arrow API
/// (`crates/core/src/runtime/schema_inferrer.rs`), which is case-preserving
/// and handles STRUCT / LIST / MAP correctly. A previous string-based
/// `from_duckdb` parser was removed as dead code (it had no production
/// callers, did not handle STRUCT, and was the same shape that caused the
/// Java reference's `e174e6c` field-name case-mangling bug).
pub struct TypeMapper;

impl TypeMapper {
    /// Convert a Spark `DataType` to the DuckDB SQL type string used in CAST/DDL.
    pub fn to_duckdb(dt: &DataType) -> Cow<'static, str> {
        match dt {
            DataType::Boolean => Cow::Borrowed("BOOLEAN"),
            DataType::Byte => Cow::Borrowed("TINYINT"),
            DataType::Short => Cow::Borrowed("SMALLINT"),
            DataType::Integer => Cow::Borrowed("INTEGER"),
            DataType::Long => Cow::Borrowed("BIGINT"),
            DataType::Float => Cow::Borrowed("FLOAT"),
            DataType::Double => Cow::Borrowed("DOUBLE"),
            DataType::Decimal { precision, scale } => {
                Cow::Owned(format!("DECIMAL({precision},{scale})"))
            }
            DataType::String => Cow::Borrowed("VARCHAR"),
            DataType::Binary => Cow::Borrowed("BLOB"),
            DataType::Date => Cow::Borrowed("DATE"),
            DataType::Timestamp => Cow::Borrowed("TIMESTAMP WITH TIME ZONE"),
            DataType::TimestampNtz => Cow::Borrowed("TIMESTAMP"),
            DataType::YearMonthInterval => Cow::Borrowed("INTERVAL"),
            DataType::DayTimeInterval => Cow::Borrowed("INTERVAL"),
            DataType::Interval => Cow::Borrowed("INTERVAL"),
            DataType::Null => Cow::Borrowed("NULL"),
            DataType::Unresolved => Cow::Borrowed("VARCHAR"),
            DataType::Array(elem, _) => Cow::Owned(format!("{}[]", Self::to_duckdb(elem))),
            DataType::Map { key, value, .. } => Cow::Owned(format!(
                "MAP({}, {})",
                Self::to_duckdb(key),
                Self::to_duckdb(value)
            )),
            DataType::Struct(_) => Cow::Borrowed("STRUCT"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_renders_with_precision_and_scale() {
        assert_eq!(
            TypeMapper::to_duckdb(&DataType::Decimal {
                precision: 18,
                scale: 4
            }),
            "DECIMAL(18,4)"
        );
    }

    #[test]
    fn array_appends_brackets() {
        let dt = DataType::Array(Box::new(DataType::Integer), true);
        assert_eq!(TypeMapper::to_duckdb(&dt), "INTEGER[]");
    }
}
