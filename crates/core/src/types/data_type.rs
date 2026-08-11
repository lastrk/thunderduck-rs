/// A field in Spark's ANSI day-time interval type, ordered from coarsest to finest.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum DayTimeField {
    Day = 0,
    Hour = 1,
    Minute = 2,
    Second = 3,
}

impl DayTimeField {
    /// Decode Spark Connect's day-time field number.
    pub fn from_proto(value: i32) -> Option<Self> {
        [Self::Day, Self::Hour, Self::Minute, Self::Second]
            .get(value as usize)
            .copied()
    }

    /// Encode Spark Connect's day-time field number.
    pub const fn to_proto(self) -> i32 {
        self as i32
    }

    const fn type_name(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Hour => "hour",
            Self::Minute => "minute",
            Self::Second => "second",
        }
    }
}

/// A field in Spark's ANSI year-month interval type, ordered from coarsest to finest.
#[repr(i32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum YearMonthField {
    Year = 0,
    Month = 1,
}

impl YearMonthField {
    /// Decode Spark Connect's year-month field number.
    pub fn from_proto(value: i32) -> Option<Self> {
        [Self::Year, Self::Month].get(value as usize).copied()
    }

    /// Encode Spark Connect's year-month field number.
    pub const fn to_proto(self) -> i32 {
        self as i32
    }

    const fn type_name(self) -> &'static str {
        match self {
            Self::Year => "year",
            Self::Month => "month",
        }
    }
}

/// Represents all Spark SQL data types.
///
/// This mirrors Spark's DataType hierarchy as a flat enum.
/// Compound types use `Box<DataType>` to avoid infinite size.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DataType {
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
    YearMonthInterval {
        start: YearMonthField,
        end: YearMonthField,
    },
    DayTimeInterval {
        start: DayTimeField,
        end: DayTimeField,
    },
    /// Spark `CalendarIntervalType`, whose value may mix interval families.
    Interval,
    Null,
    /// Type could not be statically resolved; treated as VARCHAR at generation time.
    Unresolved,

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
    /// Spark's default `DayTimeIntervalType(DAY, SECOND)`.
    pub const fn day_time_full() -> Self {
        Self::DayTimeInterval {
            start: DayTimeField::Day,
            end: DayTimeField::Second,
        }
    }

    /// Spark's default `YearMonthIntervalType(YEAR, MONTH)`.
    pub const fn year_month_full() -> Self {
        Self::YearMonthInterval {
            start: YearMonthField::Year,
            end: YearMonthField::Month,
        }
    }

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
            DataType::Interval
                | DataType::YearMonthInterval { .. }
                | DataType::DayTimeInterval { .. }
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
            DataType::YearMonthInterval { start, end } => {
                write!(f, "interval {}", start.type_name())?;
                if start != end {
                    write!(f, " to {}", end.type_name())?;
                }
                Ok(())
            }
            DataType::DayTimeInterval { start, end } => {
                write!(f, "interval {}", start.type_name())?;
                if start != end {
                    write!(f, " to {}", end.type_name())?;
                }
                Ok(())
            }
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
        assert_eq!(
            DataType::DayTimeInterval {
                start: DayTimeField::Hour,
                end: DayTimeField::Second,
            }
            .to_string(),
            "interval hour to second"
        );
        assert_eq!(
            DataType::YearMonthInterval {
                start: YearMonthField::Month,
                end: YearMonthField::Month,
            }
            .to_string(),
            "interval month"
        );
    }

    #[test]
    fn interval_field_proto_numbers_round_trip() {
        for field in [
            DayTimeField::Day,
            DayTimeField::Hour,
            DayTimeField::Minute,
            DayTimeField::Second,
        ] {
            assert_eq!(DayTimeField::from_proto(field.to_proto()), Some(field));
        }
        for field in [YearMonthField::Year, YearMonthField::Month] {
            assert_eq!(YearMonthField::from_proto(field.to_proto()), Some(field));
        }
        assert_eq!(DayTimeField::from_proto(4), None);
        assert_eq!(YearMonthField::from_proto(2), None);
    }
}
