/// Represents all Spark SQL data types.
///
/// This mirrors Spark's DataType hierarchy as a flat enum.
/// Compound types use `Box<DataType>` to avoid infinite size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataType {
    // ── Scalar ────────────────────────────────────────────────────
    Boolean,
    Byte,
    Short,
    Integer,
    Long,
    Float,
    Double,
    Decimal {
        precision: u8,
        scale: u8,
    },
    String,
    Binary,
    Date,
    Timestamp,
    TimestampNtz,
    YearMonthInterval,
    DayTimeInterval,
    /// Generic interval — used when the sub-kind (year-month vs day-time) is
    /// not statically known. Produced by `Expression::Interval` (which can
    /// carry any combination of months/days/microseconds) and by `INTERVAL`
    /// literals parsed from raw Spark SQL.
    Interval,
    Null,
    /// Type could not be statically resolved; treated as VARCHAR at generation time.
    Unresolved,

    // ── Compound ─────────────────────────────────────────────────
    /// Array type. Second field is `contains_null` (whether elements may be null).
    Array(Box<DataType>, bool),
    Map {
        key: Box<DataType>,
        value: Box<DataType>,
        value_nullable: bool,
    },
    Struct(crate::types::StructType),
}

impl DataType {
    /// Returns true if this type is numeric (integer or floating-point).
    pub fn is_numeric(&self) -> bool {
        matches!(
            self,
            DataType::Byte
                | DataType::Short
                | DataType::Integer
                | DataType::Long
                | DataType::Float
                | DataType::Double
                | DataType::Decimal { .. }
        )
    }

    /// Returns true if this is an exact integer type (no decimal part).
    pub fn is_integral(&self) -> bool {
        matches!(
            self,
            DataType::Byte | DataType::Short | DataType::Integer | DataType::Long
        )
    }

    /// Returns true if this is any interval type (generic, year-month, or day-time).
    pub fn is_interval(&self) -> bool {
        matches!(
            self,
            DataType::Interval | DataType::YearMonthInterval | DataType::DayTimeInterval
        )
    }

    /// Returns true if this type or any nested type is `Unresolved`.
    /// Used to detect when static schema inference is incomplete.
    pub fn contains_unresolved(&self) -> bool {
        match self {
            DataType::Unresolved => true,
            DataType::Array(elem, _) => elem.contains_unresolved(),
            DataType::Map { key, value, .. } => {
                key.contains_unresolved() || value.contains_unresolved()
            }
            DataType::Struct(s) => s.fields.iter().any(|f| f.data_type.contains_unresolved()),
            _ => false,
        }
    }
}

impl std::fmt::Display for DataType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DataType::Boolean => write!(f, "boolean"),
            DataType::Byte => write!(f, "byte"),
            DataType::Short => write!(f, "short"),
            DataType::Integer => write!(f, "integer"),
            DataType::Long => write!(f, "long"),
            DataType::Float => write!(f, "float"),
            DataType::Double => write!(f, "double"),
            DataType::Decimal { precision, scale } => write!(f, "decimal({precision},{scale})"),
            DataType::String => write!(f, "string"),
            DataType::Binary => write!(f, "binary"),
            DataType::Date => write!(f, "date"),
            DataType::Timestamp => write!(f, "timestamp"),
            DataType::TimestampNtz => write!(f, "timestamp_ntz"),
            DataType::YearMonthInterval => write!(f, "year_month_interval"),
            DataType::DayTimeInterval => write!(f, "day_time_interval"),
            DataType::Interval => write!(f, "interval"),
            DataType::Null => write!(f, "null"),
            DataType::Unresolved => write!(f, "unresolved"),
            DataType::Array(elem, _) => write!(f, "array<{elem}>"),
            DataType::Map { key, value, .. } => write!(f, "map<{key},{value}>"),
            DataType::Struct(st) => write!(f, "struct<{}>", st.field_names().join(",")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numeric_classification() {
        assert!(DataType::Integer.is_numeric());
        assert!(DataType::Double.is_numeric());
        assert!(DataType::Decimal {
            precision: 10,
            scale: 2
        }
        .is_numeric());
        assert!(!DataType::String.is_numeric());
        assert!(!DataType::Boolean.is_numeric());
    }

    #[test]
    fn display() {
        assert_eq!(DataType::Long.to_string(), "long");
        assert_eq!(
            DataType::Decimal {
                precision: 18,
                scale: 4
            }
            .to_string(),
            "decimal(18,4)"
        );
        assert_eq!(
            DataType::Array(Box::new(DataType::Integer), true).to_string(),
            "array<integer>"
        );
    }
}
