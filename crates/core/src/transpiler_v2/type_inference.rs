//! τ's TypeInferenceEngine — Spark-compatible type inference.
//!
//! Owned by τ (INV10: τ imports only `DataType`, `StructField`, `StructType`
//! from `crate::types`).

use super::function_registry::{self, NullRule, SpecialFunction, TypeRule};
use super::name_fold::eq_fold;
use super::schema::{Attribute, ResolvedSchema};
use crate::types::{DataType, StructField, StructType};

/// Date-returning Spark functions. Single home of the roster consulted by
/// [`TypeInferenceEngine::function_return_type`]'s Date guard arm, and by
/// emission.rs's `date_typed_functions_return_date_in_duckdb` audit test,
/// which mechanically checks that each of these renders to a DuckDB
/// expression whose runtime type is DATE.
#[cfg(test)]
pub(crate) const DATE_RETURNING_FNS: &[&str] = &[
    "add_months",
    "current_date",
    "date_add",
    "date_sub",
    "last_day",
    "make_date",
    "next_day",
    "to_date",
    "trunc",
];

/// τ's Spark-compatible type inference engine.
///
/// Unit struct with associated functions.
pub struct TypeInferenceEngine;

impl TypeInferenceEngine {
    /// Look up the type of `name` in `schema` (case-insensitive).
    /// Returns `DataType::Unresolved` if the column is not found.
    ///
    /// Supports dot-notation for struct fields: `"person.name"` resolves
    /// to the `name` field within the `person` struct column.
    pub fn column_type(name: &str, schema: &ResolvedSchema) -> DataType {
        Self::column_info(name, schema).0
    }

    /// Look up the nullability of `name` in `schema` (case-insensitive).
    /// Returns `true` (nullable) if not found — safe default.
    pub fn column_nullable(name: &str, schema: &ResolvedSchema) -> bool {
        Self::column_info(name, schema).1
    }

    fn column_info(name: &str, schema: &ResolvedSchema) -> (DataType, bool) {
        Self::column_info_in(name, &schema.fields).unwrap_or((DataType::Unresolved, true))
    }

    pub(super) fn column_info_in(name: &str, fields: &[Attribute]) -> Option<(DataType, bool)> {
        Self::resolve_in(name, fields).map(|(dt, nullable, _)| (dt, nullable))
    }

    /// Resolve a top-level name or one-level dotted struct path.
    pub(super) fn resolve_in<'a>(
        name: &str,
        fields: &'a [Attribute],
    ) -> Option<(DataType, bool, &'a Attribute)> {
        if let Some(f) = fields.iter().find(|f| eq_fold(&f.name, name)) {
            return Some((f.data_type.clone(), f.nullable, f));
        }
        if let Some(dot_pos) = name.find('.') {
            let struct_name = &name[..dot_pos];
            let field_name = &name[dot_pos + 1..];
            if let Some(f) = fields.iter().find(|f| eq_fold(&f.name, struct_name)) {
                if let DataType::Struct(st) = &f.data_type {
                    let (dt, field_nullable) = st
                        .field_by_name(field_name)
                        .map(|ff| (ff.data_type.clone(), ff.nullable))
                        .unwrap_or((DataType::Unresolved, true));
                    return Some((dt, f.nullable || field_nullable, f));
                }
            }
        }
        None
    }

    /// Promote two numeric types following Spark's rules.
    pub fn promote_numeric(left: &DataType, right: &DataType) -> DataType {
        use DataType::*;
        match (left, right) {
            (Unresolved, _) | (_, Unresolved) => Unresolved,
            (a, b) if a == b => a.clone(),
            (
                Decimal {
                    precision: p1,
                    scale: s1,
                },
                Decimal {
                    precision: p2,
                    scale: s2,
                },
            ) => Self::unify_decimal(*p1, *s1, *p2, *s2),
            (Double, _) | (_, Double) => Double,
            (Float, b) | (b, Float) if b.is_integral() => Double,
            (Float, Float) => Float,
            (Decimal { precision, scale }, b) | (b, Decimal { precision, scale })
                if b.is_integral() =>
            {
                // `is_integral()` guarantees `decimal_form` is `Some`; the
                // fallback (identity unification) is unreachable.
                let (p2, s2) = Self::decimal_form(b).unwrap_or((*precision, *scale));
                Self::unify_decimal(*precision, *scale, p2, s2)
            }
            (Long, _) | (_, Long) => Long,
            (Integer, _) | (_, Integer) => Integer,
            (Short, _) | (_, Short) => Short,
            (Byte, Byte) => Byte,
            _ => Double,
        }
    }

    /// Spark-compatible type unification (TypeCoercion.findTightestCommonType).
    pub fn unify_types(a: &DataType, b: &DataType) -> DataType {
        use DataType::*;
        match (a, b) {
            (Unresolved, _) => b.clone(),
            (_, Unresolved) => a.clone(),
            (Null, _) => b.clone(),
            (_, Null) => a.clone(),
            (x, y) if x == y => a.clone(),
            (x, y) if x.is_numeric() && y.is_numeric() => Self::promote_numeric(a, b),
            (Boolean, y) if y.is_numeric() => y.clone(),
            (x, Boolean) if x.is_numeric() => x.clone(),
            (Date, Timestamp) | (Timestamp, Date) => Timestamp,
            (
                DayTimeInterval {
                    start: left_start,
                    end: left_end,
                },
                DayTimeInterval {
                    start: right_start,
                    end: right_end,
                },
            ) => DayTimeInterval {
                start: (*left_start).min(*right_start),
                end: (*left_end).max(*right_end),
            },
            (
                YearMonthInterval {
                    start: left_start,
                    end: left_end,
                },
                YearMonthInterval {
                    start: right_start,
                    end: right_end,
                },
            ) => YearMonthInterval {
                start: (*left_start).min(*right_start),
                end: (*left_end).max(*right_end),
            },
            (x, y) if x.is_interval() && y.is_interval() => Interval,
            _ => String,
        }
    }

    /// Spark's decimal addition/subtraction result type — the shared bounds
    /// with a `+1` carry digit for the potential extra leading digit.
    pub fn decimal_add_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        Self::decimal_bounds(p1, s1, p2, s2, 1)
    }

    /// Spark's decimal multiplication result type.
    pub fn decimal_mul_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let raw_precision = p1 as i16 + p2 as i16 + 1;
        let raw_scale = s1 as i16 + s2 as i16;
        let (precision, scale) = Self::adjust_precision_scale(raw_precision, raw_scale);
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal division result type.
    pub fn decimal_div_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale_raw = 6i16.max(s1 as i16 + p2 as i16 + 1);
        let precision_raw = p1 as i16 - s1 as i16 + s2 as i16 + scale_raw;
        let (precision, scale) = Self::adjust_precision_scale(precision_raw, scale_raw);
        DataType::Decimal { precision, scale }
    }

    /// Spark's decimal modulo result type.
    pub fn decimal_mod_type(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).min(p2 as i16 - s2 as i16);
        let (precision, scale_out) =
            Self::adjust_precision_scale(int_digits + scale as i16, scale as i16);
        DataType::Decimal {
            precision,
            scale: scale_out,
        }
    }

    /// Return type of an aggregate function given its argument type.
    /// Follows Spark's semantics exactly.
    ///
    /// The input name is normalized here so direct callers may use any case.
    pub fn aggregate_return_type(name: &str, arg_type: &DataType) -> DataType {
        let lower = name.to_ascii_lowercase();
        let Some(spec) = function_registry::aggregate_spec(&lower) else {
            return arg_type.clone();
        };
        Self::registered_return_type(spec.result, &[(arg_type.clone(), true)])
    }

    /// Is this aggregate function always non-nullable?
    ///
    /// Case-insensitive wrapper — test-only convenience; production callers
    /// all use [`Self::aggregate_is_non_nullable_lower`] with a
    /// pre-lowercased name.
    ///
    /// The case-insensitive wrapper is retained for direct callers.
    #[cfg(test)]
    pub fn aggregate_is_non_nullable(name: &str) -> bool {
        Self::aggregate_is_non_nullable_lower(&name.to_lowercase())
    }

    /// Fast-path form of the aggregate non-nullability predicate (the
    /// case-insensitive wrapper `aggregate_is_non_nullable` is test-only).
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    pub fn aggregate_is_non_nullable_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "aggregate_is_non_nullable_lower requires pre-lowercased input; got `{name_lower}`",
        );
        function_registry::aggregate_spec(name_lower)
            .is_some_and(|spec| spec.nullability == NullRule::Never)
    }

    /// Aggregate functions that always return NULL for an empty group.
    ///
    /// Case-insensitive wrapper — test-only convenience; production callers
    /// all use [`Self::aggregate_is_always_nullable_lower`] with a
    /// pre-lowercased name.
    ///
    /// The case-insensitive wrapper is retained for direct callers.
    #[cfg(test)]
    pub fn aggregate_is_always_nullable(name: &str) -> bool {
        Self::aggregate_is_always_nullable_lower(&name.to_lowercase())
    }

    /// Fast-path form of the aggregate always-nullable predicate (the
    /// case-insensitive wrapper `aggregate_is_always_nullable` is test-only).
    ///
    /// **Precondition:** `name_lower` MUST already be lowercase. Debug builds
    /// `debug_assert!` this; release builds trust the contract to avoid an
    /// unnecessary allocation.
    pub fn aggregate_is_always_nullable_lower(name_lower: &str) -> bool {
        debug_assert!(
            name_lower.chars().all(|c| !c.is_ascii_uppercase()),
            "aggregate_is_always_nullable_lower requires pre-lowercased input; got `{name_lower}`",
        );
        function_registry::aggregate_spec(name_lower)
            .is_some_and(|spec| spec.nullability == NullRule::Always)
    }

    /// Return type of a window function given the optional argument type.
    ///
    /// `name` is expected to be canonical lowercase.
    pub fn window_return_type(name: &str, arg_type: Option<&DataType>) -> DataType {
        if let Some(spec) = function_registry::scalar_spec(name) {
            let args = arg_type
                .map(|data_type| vec![(data_type.clone(), true)])
                .unwrap_or_default();
            return Self::registered_return_type(spec.result, &args);
        }
        if matches!(
            function_registry::special_function(name),
            Some(SpecialFunction::Lag | SpecialFunction::Lead | SpecialFunction::NthValue)
        ) {
            return arg_type.cloned().unwrap_or(DataType::Long);
        }
        Self::aggregate_return_type(name, arg_type.unwrap_or(&DataType::Unresolved))
    }

    /// Is this window function non-nullable (ranking + COUNT).
    ///
    /// `name` is expected to be canonical lowercase.
    pub fn window_is_non_nullable(name: &str) -> bool {
        function_registry::scalar_spec(name).is_some_and(|spec| spec.nullability == NullRule::Never)
            || function_registry::aggregate_spec(name)
                .is_some_and(|spec| spec.nullability == NullRule::Never)
    }

    /// Spark-parity return type of `ceil`/`floor`.
    ///
    /// `target_scale`:
    /// - `None` → 1-arg `Ceil`/`Floor` (`inputTypes = {Double, Decimal, Long}`).
    /// - `Some(t)` → 2-arg `RoundCeil`/`RoundFloor`; the child is implicitly cast
    ///   to `Decimal` first (via Spark's `DecimalType.forType`), so the result is
    ///   always a `Decimal`.
    ///
    /// Rules pinned from `mathExpressions.scala` @ v4.1.1:
    /// - **1-arg**: `Decimal(p, 0)` → unchanged; `Decimal(p, s>0)` →
    ///   `Decimal(min(p - s + 1, 38), 0)`; everything else (integral / float /
    ///   double) → `Long`.
    /// - **2-arg** with the child's decimal form `(p, s)` and `ild = p - s + 1`:
    ///   `t < 0` → `Decimal(min(max(ild, -t + 1), 38), 0)`;
    ///   `t >= 0` → `ns = min(s, t)`, `Decimal(min(ild + ns, 38), ns)`.
    ///
    /// Unsupported input types return [`DataType::Unresolved`] (honest — trips the
    /// ADR-022 boundary guard rather than mis-typing the projection).
    pub fn ceil_floor_type(input: &DataType, target_scale: Option<i32>) -> DataType {
        use DataType::*;
        match target_scale {
            None => match input {
                Decimal { precision, scale } if *scale == 0 => Decimal {
                    precision: *precision,
                    scale: 0,
                },
                Decimal { precision, scale } => {
                    let p = (*precision as i32) - (*scale as i32) + 1;
                    Decimal {
                        precision: p.clamp(1, 38) as u8,
                        scale: 0,
                    }
                }
                // Every non-Decimal input → `Long` (Spark implicitly casts
                // integral / float / double / string to the `Long` result;
                // This preserves Spark's `Long` result for these inputs.
                _ => Long,
            },
            Some(t) => {
                let Some((p, s)) = Self::decimal_form(input).map(|(p, s)| (p as i32, s as i32))
                else {
                    return Unresolved;
                };
                let ild = p - s + 1;
                if t < 0 {
                    let precision = ild.max(-t + 1).clamp(1, 38) as u8;
                    Decimal {
                        precision,
                        scale: 0,
                    }
                } else {
                    let ns = s.min(t);
                    let precision = (ild + ns).clamp(1, 38) as u8;
                    Decimal {
                        precision,
                        scale: ns as u8,
                    }
                }
            }
        }
    }

    /// Spark's `DecimalType.forType` mapping: the exact-decimal form
    /// `(precision, scale)` of a numeric type — used to implicitly cast a
    /// 2-arg ceil/floor child to `Decimal` before rounding, to widen an
    /// integral operand against a `Decimal` in [`Self::promote_numeric`], and
    /// (via `expression::binary_data_type`) to cast a non-literal integral
    /// operand before applying a decimal arithmetic formula. Non-numeric
    /// inputs return `None` (→ `Unresolved`).
    pub(crate) fn decimal_form(dt: &DataType) -> Option<(u8, u8)> {
        use DataType::*;
        Some(match dt {
            Decimal { precision, scale } => (*precision, *scale),
            Byte => (3, 0),
            Short => (5, 0),
            Integer => (10, 0),
            Long => (20, 0),
            Float => (14, 7),
            Double => (30, 15),
            _ => return None,
        })
    }

    /// Infers registered rules and handwritten special-function return types.
    /// Expression-sensitive rules are resolved earlier by
    /// `Expression::function_call_data_type`; unsupported calls stay unresolved.
    pub fn function_return_type(name: &str, args: &[(DataType, bool)]) -> DataType {
        use DataType::*;
        let name_lower = name.to_ascii_lowercase();
        let registered_rule = function_registry::aggregate_spec(&name_lower)
            .map(|spec| spec.result)
            .or_else(|| function_registry::scalar_spec(&name_lower).map(|spec| spec.result));
        if let Some(rule) = registered_rule {
            return Self::registered_return_type(rule, args);
        }
        let Some(special) = function_registry::special_function(&name_lower) else {
            return DataType::Unresolved;
        };
        let first_arg_type = args.first().map(|(data_type, _)| data_type);
        let first_arg_or = |default: DataType| first_arg_type.cloned().unwrap_or(default);
        match special {
            // `nvl2(condition, if_not_null, if_null)` returns a branch type.
            SpecialFunction::Nvl2 if args.len() == 3 => args[1].0.clone(),
            // Higher-order folds return the seed/accumulator type.
            SpecialFunction::Aggregate | SpecialFunction::Reduce if args.len() >= 2 => {
                args[1].0.clone()
            }

            SpecialFunction::Concat
            | SpecialFunction::ConcatWs
            | SpecialFunction::RegexpReplace
            | SpecialFunction::Overlay
            | SpecialFunction::UrlEncode
            | SpecialFunction::UrlDecode
            | SpecialFunction::SubstringIndex => String,
            SpecialFunction::Locate | SpecialFunction::FindInSet => Integer,
            SpecialFunction::Like
            | SpecialFunction::Ilike
            | SpecialFunction::Rlike
            | SpecialFunction::RegexpLike
            | SpecialFunction::EqNullSafe
            | SpecialFunction::Isnull
            | SpecialFunction::Isnotnull
            | SpecialFunction::Isnan
            | SpecialFunction::Regexp
            | SpecialFunction::Not => Boolean,
            SpecialFunction::Split => DataType::Array(Box::new(String), false),
            SpecialFunction::Sha2 => String,
            SpecialFunction::Elt => String,
            SpecialFunction::ParseUrl => String,

            SpecialFunction::Ln
            | SpecialFunction::Log
            | SpecialFunction::Log10
            | SpecialFunction::Log2
            | SpecialFunction::Hypot => Double,
            SpecialFunction::Ceil | SpecialFunction::Ceiling | SpecialFunction::Floor => {
                Self::ceil_floor_type(first_arg_type.unwrap_or(&Unresolved), None)
            }
            SpecialFunction::Sign | SpecialFunction::Signum => Double,
            SpecialFunction::BitwiseAnd
            | SpecialFunction::BitwiseOr
            | SpecialFunction::BitwiseXor => first_arg_or(Unresolved),
            SpecialFunction::Negative => first_arg_or(Unresolved),
            SpecialFunction::Positive => first_arg_or(Unresolved),
            // Decimal remainder has its own widening formula.
            SpecialFunction::Mod | SpecialFunction::Pmod => match args {
                [(
                    Decimal {
                        precision: p1,
                        scale: s1,
                    },
                    _,
                ), (
                    Decimal {
                        precision: p2,
                        scale: s2,
                    },
                    _,
                )] => Self::decimal_mod_type(*p1, *s1, *p2, *s2),
                _ => first_arg_or(Integer),
            },
            SpecialFunction::Nanvl => first_arg_or(Double),
            // Decimal try_divide remains an honest boundary until both operands
            // participate in its decimal formula.
            SpecialFunction::TryDivide => match first_arg_type {
                Some(Decimal { .. }) => Unresolved,
                _ => Double,
            },
            SpecialFunction::Hex => String,
            SpecialFunction::Conv => String,
            SpecialFunction::Shiftleft | SpecialFunction::Shiftright => first_arg_or(Integer),
            SpecialFunction::BitGet | SpecialFunction::Getbit => Byte,

            SpecialFunction::MonthsBetween => Double,
            SpecialFunction::ToTimestamp
            | SpecialFunction::FromUtcTimestamp
            | SpecialFunction::ToUtcTimestamp => Timestamp,
            SpecialFunction::AddMonths
            | SpecialFunction::DateAdd
            | SpecialFunction::DateSub
            | SpecialFunction::ToDate
            | SpecialFunction::Trunc => Date,
            SpecialFunction::FromUnixtime
            | SpecialFunction::DateFormat
            | SpecialFunction::ToChar => String,
            SpecialFunction::UnixTimestamp => Long,
            SpecialFunction::Datediff | SpecialFunction::Dayofweek => Integer,
            SpecialFunction::Timestampadd => Timestamp,
            SpecialFunction::Timestampdiff => Long,

            // Array constructors widen element types and carry argument nullability.
            SpecialFunction::Array => match args.split_first() {
                Some(((first_ty, first_null), rest)) => {
                    let mut elem = first_ty.clone();
                    let mut contains_null = *first_null;
                    for (dt, n) in rest {
                        elem = Self::unify_types(&elem, dt);
                        contains_null = contains_null || *n;
                    }
                    Array(Box::new(elem), contains_null)
                }
                None => Array(Box::new(Unresolved), true),
            },
            // Map constructors widen alternating key/value arguments.
            SpecialFunction::Map | SpecialFunction::CreateMap
                if !args.is_empty() && args.len().is_multiple_of(2) =>
            {
                let mut key_ty = args[0].0.clone();
                let mut val_ty = args[1].0.clone();
                let mut value_nullable = args[1].1;
                let mut i = 2;
                while i < args.len() {
                    key_ty = Self::unify_types(&key_ty, &args[i].0);
                    val_ty = Self::unify_types(&val_ty, &args[i + 1].0);
                    value_nullable = value_nullable || args[i + 1].1;
                    i += 2;
                }
                DataType::Map {
                    key: Box::new(key_ty),
                    value: Box::new(val_ty),
                    value_nullable,
                }
            }
            // Preserve the existing defensive type for malformed constructors.
            SpecialFunction::Map | SpecialFunction::CreateMap => DataType::Map {
                key: Box::new(String),
                value: Box::new(String),
                value_nullable: true,
            },
            SpecialFunction::MapConcat => first_arg_or(Unresolved),

            // Collection-preserving higher-order functions use the input shape.
            SpecialFunction::Transform
            | SpecialFunction::Filter
            | SpecialFunction::ZipWith
            | SpecialFunction::MapFilter
            | SpecialFunction::SortArray
            | SpecialFunction::ArrayDistinct
            | SpecialFunction::ArrayUnion
            | SpecialFunction::ArrayExcept
            | SpecialFunction::Reverse
            | SpecialFunction::ArraysZip
            | SpecialFunction::TransformValues
            | SpecialFunction::TransformKeys => first_arg_or(Unresolved),
            // ArrayIntersect contains NULL only when both inputs can.
            SpecialFunction::ArrayIntersect => match args {
                [(Array(_, left_contains_null), _), (Array(_, right_contains_null), _), ..] => {
                    Self::rewrap_array(
                        first_arg_type,
                        Some(*left_contains_null && *right_contains_null),
                    )
                }
                _ => Self::rewrap_array(first_arg_type, Some(false)),
            },
            SpecialFunction::ArrayPosition => Long,
            SpecialFunction::ArrayJoin => String,
            SpecialFunction::Flatten => match first_arg_type {
                Some(DataType::Array(outer_inner, _)) => match outer_inner.as_ref() {
                    DataType::Array(inner, contains_null) => Array(inner.clone(), *contains_null),
                    _ => (**outer_inner).clone(),
                },
                _ => Unresolved,
            },
            SpecialFunction::Exists | SpecialFunction::Forall => Boolean,

            SpecialFunction::Size | SpecialFunction::Cardinality => Integer,

            SpecialFunction::ElementAt | SpecialFunction::TryElementAt => match first_arg_type {
                Some(DataType::Array(elem, _)) => (**elem).clone(),
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },

            // Appending or prepending can introduce a NULL element.
            SpecialFunction::ArrayAppend | SpecialFunction::ArrayPrepend => {
                Self::rewrap_array(first_arg_type, Some(true))
            }
            SpecialFunction::ToJson => String,
            SpecialFunction::ToCsv => String,
            SpecialFunction::JsonObjectKeys => Array(Box::new(String), true),

            // Time windows expose nullable start/end timestamp fields.
            SpecialFunction::Window => DataType::Struct(StructType::new(vec![
                StructField::nullable("start", Timestamp),
                StructField::nullable("end", Timestamp),
            ])),

            SpecialFunction::Typeof => String,

            SpecialFunction::MakeDtInterval => DataType::day_time_full(),
            SpecialFunction::MakeYmInterval => DataType::year_month_full(),
            SpecialFunction::MakeInterval | SpecialFunction::TryMakeInterval => Interval,

            // These handlers need expression literals or window context and
            // are resolved before this type-only fallback.
            SpecialFunction::Bround
            | SpecialFunction::Aggregate
            | SpecialFunction::FromCsv
            | SpecialFunction::FromJson
            | SpecialFunction::Lag
            | SpecialFunction::Lead
            | SpecialFunction::NamedStruct
            | SpecialFunction::Nvl2
            | SpecialFunction::NthValue
            | SpecialFunction::Reduce
            | SpecialFunction::Round
            | SpecialFunction::Struct
            | SpecialFunction::ToNumber
            | SpecialFunction::TryToNumber => Unresolved,
        }
    }

    fn registered_return_type(rule: TypeRule, args: &[(DataType, bool)]) -> DataType {
        use DataType::*;
        let first = args.first().map(|(data_type, _)| data_type);
        match rule {
            TypeRule::ArrayElement => match first {
                Some(DataType::Array(element, _)) => (**element).clone(),
                _ => Unresolved,
            },
            TypeRule::ArrayOfArgument => {
                Array(Box::new(first.cloned().unwrap_or(Unresolved)), false)
            }
            TypeRule::ArrayWithoutNulls => Self::rewrap_array(first, Some(false)),
            TypeRule::ArrayWithNulls => Self::rewrap_array(first, Some(true)),
            TypeRule::Average => match first {
                Some(Byte | Short | Integer | Long | Float | Double) => Double,
                Some(Decimal { precision, scale }) => {
                    let precision = (*precision as u16 + 4).min(38) as u8;
                    Decimal {
                        precision,
                        scale: (*scale + 4).min(18).min(precision),
                    }
                }
                Some(other) => other.clone(),
                None => Unresolved,
            },
            TypeRule::Binary => Binary,
            TypeRule::Boolean => Boolean,
            TypeRule::Byte => Byte,
            TypeRule::Date => Date,
            TypeRule::Double => Double,
            TypeRule::FirstArgument => first.cloned().unwrap_or(Unresolved),
            TypeRule::HistogramNumeric => Array(
                Box::new(Struct(StructType::new(vec![
                    StructField::nullable("x", first.cloned().unwrap_or(Unresolved)),
                    StructField::nullable("y", Double),
                ]))),
                true,
            ),
            TypeRule::Integer => Integer,
            TypeRule::Long => Long,
            TypeRule::MapEntries => match first {
                Some(Map {
                    key,
                    value,
                    value_nullable,
                }) => Array(
                    Box::new(Struct(StructType::new(vec![
                        StructField::not_null("key", (**key).clone()),
                        StructField::new("value", (**value).clone(), *value_nullable),
                    ]))),
                    false,
                ),
                _ => Unresolved,
            },
            TypeRule::MapFromArrays => match args {
                [(Array(key, _), _), (Array(value, value_nullable), _)] => Map {
                    key: key.clone(),
                    value: value.clone(),
                    value_nullable: *value_nullable,
                },
                _ => Map {
                    key: Box::new(String),
                    value: Box::new(String),
                    value_nullable: true,
                },
            },
            TypeRule::MapKeys => match first {
                Some(Map { key, .. }) => Array(key.clone(), true),
                _ => Unresolved,
            },
            TypeRule::MapValues => match first {
                Some(Map {
                    value,
                    value_nullable,
                    ..
                }) => Array(value.clone(), *value_nullable),
                _ => Unresolved,
            },
            TypeRule::PreserveArray => Self::rewrap_array(first, None),
            TypeRule::SecondArgument => args
                .get(1)
                .map(|(data_type, _)| data_type.clone())
                .unwrap_or(Unresolved),
            TypeRule::Sequence => Array(Box::new(first.cloned().unwrap_or(Long)), false),
            TypeRule::String => String,
            TypeRule::StringArray => Array(Box::new(String), true),
            TypeRule::StringMap => Map {
                key: Box::new(String),
                value: Box::new(String),
                value_nullable: true,
            },
            TypeRule::Sum => match first {
                Some(Byte | Short | Integer | Long) => Long,
                Some(Float | Double) => Double,
                Some(Decimal { precision, scale }) => Decimal {
                    precision: (*precision as u16 + 10).min(38) as u8,
                    scale: *scale,
                },
                Some(other) => other.clone(),
                None => Unresolved,
            },
            TypeRule::Timestamp => Timestamp,
            TypeRule::WidenArguments => match args.split_first() {
                Some(((first, _), rest)) => rest.iter().fold(first.clone(), |result, (next, _)| {
                    Self::promote_numeric(&result, next)
                }),
                None => Unresolved,
            },
        }
    }

    /// Rewrap an `Array<T>` argument type with the same element type and a
    /// per-function `containsNull` stamp: `Some(b)` forces the flag to `b`,
    /// `None` preserves the input array's flag. Non-array (or absent) inputs
    /// return `Unresolved`.
    fn rewrap_array(arg: Option<&DataType>, contains_null: Option<bool>) -> DataType {
        match arg {
            Some(DataType::Array(elem, arg_contains_null)) => {
                DataType::Array(elem.clone(), contains_null.unwrap_or(*arg_contains_null))
            }
            _ => DataType::Unresolved,
        }
    }

    /// Widest-operand decimal unification — the shared bounds with no carry
    /// digit (plain type unification cannot gain a leading digit).
    fn unify_decimal(p1: u8, s1: u8, p2: u8, s2: u8) -> DataType {
        Self::decimal_bounds(p1, s1, p2, s2, 0)
    }

    /// Shared decimal-bounds math behind [`Self::unify_decimal`] (`carry = 0`)
    /// and [`Self::decimal_add_type`] (`carry = 1`):
    /// `scale = max(s1, s2)`, `int_digits = max(p1 - s1, p2 - s2)`,
    /// `precision = min(int_digits + scale + carry, 38)`.
    fn decimal_bounds(p1: u8, s1: u8, p2: u8, s2: u8, carry: i16) -> DataType {
        let scale = s1.max(s2);
        let int_digits = (p1 as i16 - s1 as i16).max(p2 as i16 - s2 as i16);
        let precision = ((int_digits + scale as i16 + carry).min(38)) as u8;
        DataType::Decimal { precision, scale }
    }

    fn adjust_precision_scale(raw_precision: i16, raw_scale: i16) -> (u8, u8) {
        if raw_precision <= 38 {
            (raw_precision as u8, raw_scale as u8)
        } else {
            let int_digits = raw_precision - raw_scale;
            let min_scale = raw_scale.min(6);
            let scale = ((38i16 - int_digits).max(min_scale)).max(0);
            let precision = (int_digits + scale).clamp(0, 38);
            (precision as u8, scale as u8)
        }
    }
}

/// Spark 4.1.1 `Nondeterministic`-expression names relevant to sort-key
/// rebinding: `Rand`/`Randn`/`Uuid`/`Shuffle`/`MonotonicallyIncreasingID`
/// (confirmed against `randomExpressions.scala` / `misc.scala` /
/// `collectionOperations.scala` / `MonotonicallyIncreasingID.scala`), plus
/// `InputFileName` / `SparkPartitionID` (both `Nondeterministic` in
/// `Expression.scala`'s `misc.scala` neighborhood) and `random`, the
/// Spark-SQL alias spelling of `rand`. This is the roster relevant to this
/// fallback, NOT a claim of exhaustive coverage of every `Nondeterministic`
/// expression in Catalyst. `Expression.semanticEquals` is unconditionally
/// `false` whenever either side is nondeterministic (`deterministic &&
/// other.deterministic && ...`, `Analyzer.scala`); `analyzer::semantic_eq`
/// mirrors that exclusion.
const NONDETERMINISTIC_FN_NAMES: &[&str] = &[
    "rand",
    "random",
    "randn",
    "uuid",
    "shuffle",
    "monotonically_increasing_id",
    "input_file_name",
    "spark_partition_id",
];

/// Case-insensitive membership test against [`NONDETERMINISTIC_FN_NAMES`].
/// Used by `analyzer::contains_nondeterministic_call` to detect a Sort key
/// (or Sort-key restatement) that calls a nondeterministic function, so it
/// can be excluded from the `semantic_eq` rebind fallback.
///
/// N5 note: the sole production caller (`&f.name`) already supplies a
/// canonical lowercase name, but `is_nondeterministic_fn_name_membership_and_case_insensitivity`
/// exercises this function directly with `"RAND"` / `"Random"`, so the
/// case-insensitive lookup stays for direct-call robustness.
pub(crate) fn is_nondeterministic_fn_name(name: &str) -> bool {
    NONDETERMINISTIC_FN_NAMES
        .iter()
        .any(|n| n.eq_ignore_ascii_case(name))
}

/// The 11-name correlation / covariance / regression family.
#[cfg(test)]
pub(crate) const CORR_FAMILY_NAMES: &[&str] = &[
    "corr",
    "covar_samp",
    "covar_pop",
    "regr_slope",
    "regr_r2",
    "regr_intercept",
    "regr_avgx",
    "regr_avgy",
    "regr_sxx",
    "regr_sxy",
    "regr_syy",
];

/// Public hash functions with non-nullable results.
#[cfg(test)]
pub(crate) const HASH_FAMILY_NAMES: &[&str] = &["hash", "xxhash64"];

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{DayTimeField, YearMonthField};

    fn dec(p: u8, s: u8) -> DataType {
        DataType::Decimal {
            precision: p,
            scale: s,
        }
    }

    /// One-line wrapper for [`TypeInferenceEngine::function_return_type`],
    /// pairing each type with a conservative `nullable = true` default. Every
    /// existing call site exercises type-only arms that ignore `.1`, so this
    /// keeps them unchanged after the O11 signature widen.
    fn frt(name: &str, args: &[DataType]) -> DataType {
        let pairs: Vec<(DataType, bool)> = args.iter().map(|t| (t.clone(), true)).collect();
        TypeInferenceEngine::function_return_type(name, &pairs)
    }

    /// Nullability-sensitive sibling of [`frt`] for the arms that read
    /// per-argument nullability directly (`array`, `map`/`create_map` — O11).
    fn frt_n(name: &str, args: &[(DataType, bool)]) -> DataType {
        TypeInferenceEngine::function_return_type(name, args)
    }

    /// One-line wrapper for [`TypeInferenceEngine::aggregate_return_type`].
    fn agg_rt(name: &str, arg: &DataType) -> DataType {
        TypeInferenceEngine::aggregate_return_type(name, arg)
    }

    #[test]
    fn interval_unification_widens_each_family_to_the_union_span() {
        let day_minute = DataType::DayTimeInterval {
            start: DayTimeField::Day,
            end: DayTimeField::Minute,
        };
        let hour_second = DataType::DayTimeInterval {
            start: DayTimeField::Hour,
            end: DayTimeField::Second,
        };
        assert_eq!(
            TypeInferenceEngine::unify_types(&day_minute, &hour_second),
            DataType::day_time_full()
        );

        let year = DataType::YearMonthInterval {
            start: YearMonthField::Year,
            end: YearMonthField::Year,
        };
        let month = DataType::YearMonthInterval {
            start: YearMonthField::Month,
            end: YearMonthField::Month,
        };
        assert_eq!(
            TypeInferenceEngine::unify_types(&year, &month),
            DataType::year_month_full()
        );
    }

    #[test]
    fn ceil_floor_1arg_decimal_scale0() {
        // Decimal(p, 0) is unchanged.
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(12, 0), None),
            dec(12, 0)
        );
    }

    #[test]
    fn ceil_floor_1arg_decimal_positive_scale() {
        // Decimal(p, s>0) → Decimal(min(p - s + 1, 38), 0).
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(10, 2), None),
            dec(9, 0)
        );
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(6, 3), None),
            dec(4, 0)
        );
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(38, 6), None),
            dec(33, 0)
        );
    }

    #[test]
    fn ceil_floor_1arg_non_decimal_is_long() {
        for input in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
        ] {
            assert_eq!(
                TypeInferenceEngine::ceil_floor_type(&input, None),
                DataType::Long,
                "ceil/floor({input:?}) must be Long",
            );
        }
    }

    #[test]
    fn ceil_floor_1arg_non_numeric_still_long() {
        // Any non-Decimal input (including String / Unresolved) resolves to
        // Long.
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&DataType::String, None),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&DataType::Unresolved, None),
            DataType::Long
        );
    }

    #[test]
    fn ceil_floor_2arg_decimal_positive_scale() {
        // floor(decimal(10,2), 1) → decimal(10,1).
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(10, 2), Some(1)),
            dec(10, 1)
        );
    }

    #[test]
    fn ceil_floor_2arg_double_scale() {
        // ceil(double, 2): double→(30,15), ild=16, ns=2 → decimal(18,2).
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&DataType::Double, Some(2)),
            dec(18, 2)
        );
    }

    #[test]
    fn ceil_floor_2arg_integral_scale() {
        // Long→(20,0), ild=21, ns=min(0,2)=0 → decimal(21,0).
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&DataType::Long, Some(2)),
            dec(21, 0)
        );
    }

    #[test]
    fn ceil_floor_2arg_negative_scale() {
        // t<0: decimal(10,2), ild=9, -t+1=4 → decimal(max(9,4),0)=decimal(9,0).
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&dec(10, 2), Some(-3)),
            dec(9, 0)
        );
    }

    #[test]
    fn ceil_floor_2arg_unsupported_is_unresolved() {
        assert_eq!(
            TypeInferenceEngine::ceil_floor_type(&DataType::String, Some(2)),
            DataType::Unresolved
        );
    }

    #[test]
    fn count_if_returns_long() {
        for arg in [
            DataType::Boolean,
            DataType::Integer,
            DataType::String,
            DataType::Null,
        ] {
            assert_eq!(
                agg_rt("count_if", &arg),
                DataType::Long,
                "count_if({arg:?}) must return Long",
            );
        }
    }

    #[test]
    fn count_if_is_non_nullable() {
        assert!(TypeInferenceEngine::aggregate_is_non_nullable("count_if"));
    }

    #[test]
    fn count_if_case_insensitive() {
        assert_eq!(agg_rt("COUNT_IF", &DataType::Boolean), DataType::Long);
        assert!(TypeInferenceEngine::aggregate_is_non_nullable("Count_If"));
    }

    #[test]
    fn corr_family_returns_double() {
        for name in CORR_FAMILY_NAMES {
            assert_eq!(
                agg_rt(name, &DataType::Integer),
                DataType::Double,
                "{name}(Integer) must return Double",
            );
        }
    }

    #[test]
    fn corr_family_is_always_nullable() {
        for name in CORR_FAMILY_NAMES {
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
        }
    }

    #[test]
    fn stddev_variance_family_still_double() {
        for name in [
            "stddev",
            "stddev_samp",
            "std",
            "stddev_pop",
            "variance",
            "var_samp",
            "var_pop",
            "skewness",
            "kurtosis",
        ] {
            assert_eq!(
                agg_rt(name, &DataType::Integer),
                DataType::Double,
                "{name}(Integer) must return Double",
            );
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
        }
    }

    #[test]
    fn sum_integer_returns_long() {
        assert_eq!(agg_rt("sum", &DataType::Integer), DataType::Long);
    }

    #[test]
    fn sum_decimal_widens_precision() {
        assert_eq!(agg_rt("sum", &dec(10, 2)), dec(20, 2));
    }

    #[test]
    fn avg_integer_returns_double() {
        assert_eq!(agg_rt("avg", &DataType::Long), DataType::Double);
    }

    /// Every aggregate registry name belongs to exactly one of
    /// `aggregate_is_non_nullable` or `aggregate_is_always_nullable`.
    #[test]
    fn every_aggregate_return_type_name_appears_in_a_nullability_predicate() {
        for name in function_registry::aggregate_names() {
            let non_null = TypeInferenceEngine::aggregate_is_non_nullable(name);
            let always_null = TypeInferenceEngine::aggregate_is_always_nullable(name);
            assert!(
                non_null ^ always_null,
                "aggregate `{name}` must appear in exactly one of \
                 aggregate_is_non_nullable ({non_null}) XOR \
                 aggregate_is_always_nullable ({always_null})",
            );
        }
    }

    /// §8.4 — corr/covar/regr family must be `→ Double` AND
    /// `aggregate_is_always_nullable` AND NOT `aggregate_is_non_nullable`.
    #[test]
    fn corr_family_is_symmetric_across_return_type_and_nullability_predicates() {
        for name in CORR_FAMILY_NAMES {
            assert_eq!(
                agg_rt(name, &DataType::Integer),
                DataType::Double,
                "{name} return-type must be Double",
            );
            assert!(
                TypeInferenceEngine::aggregate_is_always_nullable(name),
                "{name} must be aggregate_is_always_nullable",
            );
            assert!(
                !TypeInferenceEngine::aggregate_is_non_nullable(name),
                "{name} must NOT be aggregate_is_non_nullable",
            );
        }
    }

    #[test]
    fn column_lookup_case_insensitive() {
        let schema = ResolvedSchema::minted(StructType::new(vec![
            StructField::nullable("id", DataType::Long),
            StructField::not_null("code", DataType::String),
        ]));
        assert_eq!(
            TypeInferenceEngine::column_type("id", &schema),
            DataType::Long
        );
        assert_eq!(
            TypeInferenceEngine::column_type("ID", &schema),
            DataType::Long
        );
        assert!(!TypeInferenceEngine::column_nullable("code", &schema));
        assert_eq!(
            TypeInferenceEngine::column_type("missing", &schema),
            DataType::Unresolved
        );
    }

    /// `try_sum` and `try_avg` are aggregate registry entries.
    #[test]
    fn aggregate_names_contains_try_sum_and_try_avg() {
        assert!(
            function_registry::is_aggregate("try_sum"),
            "registry must classify try_sum as an aggregate (checklist §1.4)",
        );
        assert!(
            function_registry::is_aggregate("try_avg"),
            "registry must classify try_avg as an aggregate (checklist §1.4)",
        );
    }

    /// `try_divide` is scalar, not aggregate.
    #[test]
    fn aggregate_names_does_not_contain_try_divide() {
        assert!(
            !function_registry::is_aggregate("try_divide"),
            "registry must not classify try_divide as aggregate (checklist §4.1)",
        );
    }

    /// No name appears in BOTH the aggregate registry and the
    /// nondeterministic roster — the two are disjoint predicate domains, and
    /// a name in both would make `contains_aggregate_call` /
    /// `contains_nondeterministic_call` racy about which fallback trigger
    /// fires first for a given Sort key.
    #[test]
    fn aggregate_registry_and_nondeterministic_roster_are_disjoint() {
        for name in function_registry::aggregate_names() {
            assert!(
                !is_nondeterministic_fn_name(name),
                "`{name}` is in both the aggregate registry and the \
                 nondeterministic roster",
            );
        }
    }

    /// Membership + case-insensitivity for [`is_nondeterministic_fn_name`].
    #[test]
    fn is_nondeterministic_fn_name_membership_and_case_insensitivity() {
        assert!(is_nondeterministic_fn_name("RAND"));
        assert!(is_nondeterministic_fn_name("Random"));
        assert!(!is_nondeterministic_fn_name("sum"));
    }

    /// `try_sum` return-type must mirror `sum` (Integer → Long, Double → Double,
    /// Decimal → widened).
    #[test]
    fn aggregate_return_type_try_sum_matches_sum_path() {
        for arg in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            dec(10, 2),
        ] {
            assert_eq!(
                agg_rt("try_sum", &arg),
                agg_rt("sum", &arg),
                "try_sum({arg:?}) must return the same type as sum({arg:?})",
            );
        }
    }

    /// `try_avg` return-type must mirror `avg`.
    #[test]
    fn aggregate_return_type_try_avg_matches_avg_path() {
        for arg in [
            DataType::Byte,
            DataType::Short,
            DataType::Integer,
            DataType::Long,
            DataType::Float,
            DataType::Double,
            dec(10, 2),
        ] {
            assert_eq!(
                agg_rt("try_avg", &arg),
                agg_rt("avg", &arg),
                "try_avg({arg:?}) must return the same type as avg({arg:?})",
            );
        }
    }

    /// Both `try_sum` and `try_avg` must use the always-nullable rule
    /// (empty groups return NULL, and `try_*` variants surface arithmetic
    /// overflows as NULL in place of errors).
    #[test]
    fn try_sum_and_try_avg_are_always_nullable() {
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("try_sum"));
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("try_avg"));
        assert!(!TypeInferenceEngine::aggregate_is_non_nullable("try_sum"));
        assert!(!TypeInferenceEngine::aggregate_is_non_nullable("try_avg"));
    }

    #[test]
    fn hash_return_type_is_integer() {
        assert_eq!(frt("hash", &[DataType::Integer]), DataType::Integer);
    }

    #[test]
    fn xxhash64_return_type_is_long() {
        assert_eq!(frt("xxhash64", &[DataType::Integer]), DataType::Long);
    }

    #[test]
    fn nanvl_returns_first_arg_type() {
        assert_eq!(frt("nanvl", &[DataType::Double]), DataType::Double);
        assert_eq!(frt("nanvl", &[DataType::Float]), DataType::Float);
    }

    #[test]
    fn negative_preserves_arg_type() {
        // `negative` maps to Spark's UnaryMinus: dataType == child.
        assert_eq!(frt("negative", &[DataType::Integer]), DataType::Integer);
        assert_eq!(frt("negative", &[dec(10, 2)]), dec(10, 2));
    }

    #[test]
    fn try_divide_returns_double_for_integers() {
        assert_eq!(frt("try_divide", &[DataType::Integer]), DataType::Double);
        assert_eq!(frt("try_divide", &[DataType::Long]), DataType::Double);
    }

    /// Spark's
    /// `positive(x)` (`UnaryPositive`) is the identity — dataType == child,
    /// mirroring `negative` above.
    #[test]
    fn positive_preserves_arg_type() {
        assert_eq!(frt("positive", &[DataType::Double]), DataType::Double);
        assert_eq!(frt("positive", &[DataType::Integer]), DataType::Integer);
    }

    /// Spark's
    /// `bit_get`/`getbit(x, pos)` return the bit as a Byte (TINYINT),
    /// independent of `x`'s own type.
    #[test]
    fn bit_get_returns_byte() {
        assert_eq!(
            frt("bit_get", &[DataType::Integer, DataType::Integer]),
            DataType::Byte
        );
        assert_eq!(
            frt("getbit", &[DataType::Long, DataType::Integer]),
            DataType::Byte
        );
    }

    /// The DATE form returns String regardless of the
    /// input's own type, mirroring `date_format`.
    #[test]
    fn to_char_returns_string() {
        assert_eq!(
            frt("to_char", &[DataType::Date, DataType::String]),
            DataType::String
        );
    }

    #[test]
    fn try_divide_returns_unresolved_for_decimal_until_multi_arg_dispatch() {
        // `try_divide(Decimal, Decimal)` widens per `decimal_div_type`, but
        // this resolver only sees the first arg's type. Until multi-arg
        // dispatch lands, the Decimal branch must return `Unresolved` so the
        // ADR-022 boundary guard trips instead of silently mis-typing.
        assert_eq!(frt("try_divide", &[dec(10, 2)]), DataType::Unresolved);
    }

    #[test]
    fn size_scalar_returns_integer() {
        assert_eq!(
            frt("size", &[DataType::Array(Box::new(DataType::String), true)]),
            DataType::Integer
        );
        assert_eq!(
            frt(
                "cardinality",
                &[DataType::Array(Box::new(DataType::Integer), true)]
            ),
            DataType::Integer
        );
    }

    #[test]
    fn sequence_returns_array_of_first_arg() {
        assert_eq!(
            frt("sequence", &[DataType::Integer]),
            DataType::Array(Box::new(DataType::Integer), false)
        );
        assert_eq!(
            frt("sequence", &[]),
            DataType::Array(Box::new(DataType::Long), false)
        );
    }

    #[test]
    fn get_json_object_returns_string() {
        assert_eq!(
            frt("get_json_object", &[DataType::String]),
            DataType::String
        );
    }

    /// `to_json(struct)` returns String.
    #[test]
    fn to_json_returns_string() {
        // Argument here is a StructType (from `struct(...)`) — the resolver
        // only takes the first-arg type; the return is always String.
        let s = DataType::Struct(StructType::new(vec![]));
        assert_eq!(frt("to_json", &[s]), DataType::String);
    }

    /// `schema_of_json(json_str)` returns String.
    #[test]
    fn schema_of_json_returns_string() {
        assert_eq!(frt("schema_of_json", &[DataType::String]), DataType::String);
    }

    /// `to_csv(struct)` returns String.
    #[test]
    fn to_csv_returns_string() {
        let s = DataType::Struct(StructType::new(vec![]));
        assert_eq!(frt("to_csv", &[s]), DataType::String);
    }

    #[test]
    fn element_at_on_map_returns_value_type() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(frt("element_at", &[m]), DataType::Long);
    }

    #[test]
    fn element_at_on_array_returns_element_type() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(frt("element_at", &[a]), DataType::String);
    }

    #[test]
    fn map_keys_returns_array_of_key_type_with_contains_null_true() {
        // Spark stamps `ArrayType(K, containsNull=True)` on map_keys.
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            frt("map_keys", &[m]),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn map_values_returns_array_of_value_type() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(
            frt("map_values", &[m]),
            DataType::Array(Box::new(DataType::Long), true)
        );
    }

    #[test]
    fn map_entries_returns_array_of_struct() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        let expected = DataType::Array(
            Box::new(DataType::Struct(StructType::new(vec![
                StructField::not_null("key", DataType::String),
                StructField::new("value", DataType::Long, true),
            ]))),
            false,
        );
        assert_eq!(frt("map_entries", &[m]), expected);
    }

    #[test]
    fn map_contains_key_returns_boolean() {
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::Long),
            value_nullable: true,
        };
        assert_eq!(frt("map_contains_key", &[m]), DataType::Boolean);
    }

    #[test]
    fn array_append_preserves_element_but_forces_containsnull_true() {
        let a = DataType::Array(Box::new(DataType::String), false);
        assert_eq!(
            frt("array_append", std::slice::from_ref(&a)),
            DataType::Array(Box::new(DataType::String), true)
        );
        assert_eq!(
            frt("array_prepend", &[a]),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn array_compact_forces_containsnull_false() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            frt("array_compact", &[a]),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_remove_returns_first_arg_type() {
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(frt("array_remove", std::slice::from_ref(&a)), a);
    }

    #[test]
    fn array_intersect_contains_null_is_left_and_right() {
        // Spark's `ArrayIntersect`: containsNull = leftContainsNull AND
        // rightContainsNull. Both args nullable-element → true (matches
        // where both
        // `arr1`/`arr2` are `ArrayType(IntegerType(), True)`).
        let left = DataType::Array(Box::new(DataType::String), true);
        let right_nullable = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            frt("array_intersect", &[left.clone(), right_nullable]),
            DataType::Array(Box::new(DataType::String), true)
        );
        // Right arg non-nullable-element → AND collapses to false — matches
        // whose second arg is a non-nullable array
        // literal.
        let right_non_nullable = DataType::Array(Box::new(DataType::String), false);
        assert_eq!(
            frt("array_intersect", &[left, right_non_nullable]),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_intersect_falls_back_to_contains_null_false_without_a_second_array_arg() {
        // Defensive fallback (should not happen for a well-typed Spark
        // call): when the second arg isn't a resolved `Array` type, keep
        // the previous conservative `containsNull=false` stamp rather than
        // panicking or guessing.
        let a = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            frt("array_intersect", &[a]),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn map_from_arrays_derives_key_and_value_types_from_array_args() {
        // Spark's `MapFromArrays.dataType`: key = keys array's element type,
        // value + valueContainsNull = values array's element type +
        // containsNull. This resolver is the single home for the rule.
        let keys = DataType::Array(Box::new(DataType::String), false);
        let values = DataType::Array(Box::new(DataType::Integer), false);
        assert_eq!(
            frt("map_from_arrays", &[keys, values]),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: false,
            }
        );
    }

    #[test]
    fn map_from_arrays_value_nullable_follows_values_array_contains_null() {
        let keys = DataType::Array(Box::new(DataType::String), false);
        let values = DataType::Array(Box::new(DataType::Integer), true);
        assert_eq!(
            frt("map_from_arrays", &[keys, values]),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn map_from_arrays_falls_back_to_string_string_map_without_two_array_args() {
        // Malformed / non-Array-typed call: honest-but-approximate fallback,
        // same as the pre-move hard-coded default.
        assert_eq!(
            frt("map_from_arrays", &[DataType::String]),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn map_from_arrays_three_args_is_malformed_and_falls_back() {
        // Three or more args are malformed (Spark WRONG_NUM_ARGS); use the
        // fallback rather than deriving from the first two arrays.
        let arr = || DataType::Array(Box::new(DataType::Integer), false);
        assert_eq!(
            frt("map_from_arrays", &[arr(), arr(), arr()]),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn array_constructor_homogeneous_non_null_args_is_non_nullable() {
        // `array(1, 2, 3)` with every arg non-nullable → containsNull=false.
        assert_eq!(
            frt_n(
                "array",
                &[
                    (DataType::Integer, false),
                    (DataType::Integer, false),
                    (DataType::Integer, false),
                ],
            ),
            DataType::Array(Box::new(DataType::Integer), false)
        );
    }

    #[test]
    fn array_constructor_any_nullable_arg_sets_contains_null_true() {
        assert_eq!(
            frt_n(
                "array",
                &[(DataType::Integer, false), (DataType::Integer, true)],
            ),
            DataType::Array(Box::new(DataType::Integer), true)
        );
    }

    #[test]
    fn array_constructor_widens_mixed_numeric_types() {
        // `array(1, 2.0, 3)` → Array<Double> (findWiderCommonType, not
        // first-arg-only).
        assert_eq!(
            frt_n(
                "array",
                &[
                    (DataType::Integer, false),
                    (DataType::Double, false),
                    (DataType::Integer, false),
                ],
            ),
            DataType::Array(Box::new(DataType::Double), false)
        );
    }

    #[test]
    fn array_constructor_empty_call_is_unresolved_and_nullable() {
        assert_eq!(
            frt_n("array", &[]),
            DataType::Array(Box::new(DataType::Unresolved), true)
        );
    }

    #[test]
    fn map_constructor_non_null_values_reports_value_nullable_false() {
        // `map('a', 1, 'b', 2)` — no nullable value arg → value_nullable=false.
        assert_eq!(
            frt_n(
                "map",
                &[
                    (DataType::String, false),
                    (DataType::Integer, false),
                    (DataType::String, false),
                    (DataType::Integer, false),
                ],
            ),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: false,
            }
        );
    }

    #[test]
    fn map_constructor_nullable_value_arg_sets_value_nullable_true() {
        assert_eq!(
            frt_n(
                "create_map",
                &[
                    (DataType::String, false),
                    (DataType::Integer, false),
                    (DataType::String, false),
                    (DataType::Integer, true),
                ],
            ),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn map_constructor_widens_heterogeneous_key_and_value_types() {
        assert_eq!(
            frt_n(
                "map",
                &[
                    (DataType::Integer, false),
                    (DataType::Integer, false),
                    (DataType::Long, false),
                    (DataType::Double, false),
                ],
            ),
            DataType::Map {
                key: Box::new(DataType::Long),
                value: Box::new(DataType::Double),
                value_nullable: false,
            }
        );
    }

    #[test]
    fn map_constructor_odd_arity_falls_back_to_string_string_map() {
        // Malformed (odd arity: k1, v1, k2 with no matching v2) falls through
        // to the hard-coded Map<String, String, true> arm — same fallback as
        // pre-O11.
        assert_eq!(
            frt_n(
                "map",
                &[
                    (DataType::String, false),
                    (DataType::Integer, false),
                    (DataType::String, false),
                ],
            ),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn array_insert_stamps_contains_null_true() {
        // `array_insert(arr, pos, val)` — Spark stamps
        // `containsNull=true` (out-of-range positive pos pads with NULLs).
        let a = DataType::Array(Box::new(DataType::String), false);
        assert_eq!(
            frt("array_insert", &[a]),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn str_to_map_returns_map_of_string_to_string() {
        // `str_to_map` returns Map<VARCHAR, VARCHAR> with
        // nullable values (missing tokens produce NULL values).
        assert_eq!(
            frt("str_to_map", &[DataType::String]),
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }
        );
    }

    #[test]
    fn map_concat_returns_first_arg_type() {
        // `map_concat` preserves the first argument's map type.
        let m = DataType::Map {
            key: Box::new(DataType::String),
            value: Box::new(DataType::String),
            value_nullable: true,
        };
        assert_eq!(frt("map_concat", std::slice::from_ref(&m)), m);
    }

    #[test]
    fn histogram_numeric_returns_array_of_bin_struct() {
        let expected = DataType::Array(
            Box::new(DataType::Struct(StructType::new(vec![
                StructField::nullable("x", DataType::Integer),
                StructField::nullable("y", DataType::Double),
            ]))),
            true,
        );
        assert_eq!(frt("histogram_numeric", &[DataType::Integer]), expected);
    }

    #[test]
    fn input_file_name_returns_string() {
        assert_eq!(frt("input_file_name", &[]), DataType::String);
    }

    #[test]
    fn split_part_returns_string() {
        assert_eq!(frt("split_part", &[DataType::String]), DataType::String);
    }

    #[test]
    fn btrim_returns_string() {
        assert_eq!(frt("btrim", &[DataType::String]), DataType::String);
        assert_eq!(
            frt("btrim", &[DataType::String, DataType::String]),
            DataType::String
        );
    }

    #[test]
    fn substring_index_returns_string() {
        assert_eq!(
            frt(
                "substring_index",
                &[DataType::String, DataType::String, DataType::Integer]
            ),
            DataType::String
        );
    }

    #[test]
    fn dayname_returns_string() {
        assert_eq!(frt("dayname", &[DataType::Date]), DataType::String);
    }

    #[test]
    fn monthname_returns_string() {
        assert_eq!(frt("monthname", &[DataType::Date]), DataType::String);
    }

    #[test]
    fn regexp_extract_all_returns_array_of_string() {
        assert_eq!(
            frt("regexp_extract_all", &[DataType::String]),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn json_object_keys_returns_array_of_string() {
        assert_eq!(
            frt("json_object_keys", &[DataType::String]),
            DataType::Array(Box::new(DataType::String), true)
        );
    }

    #[test]
    fn max_by_registry_rule_returns_first_arg_type() {
        // Return type = type of `x` (the value column), not `y` (the
        // ordering column) — mirrors DuckDB's `arg_max_null(x, y)`.
        assert_eq!(
            frt("max_by", &[DataType::String, DataType::Integer]),
            DataType::String
        );
    }

    #[test]
    fn min_by_registry_rule_returns_first_arg_type() {
        assert_eq!(
            frt("min_by", &[DataType::String, DataType::Integer]),
            DataType::String
        );
    }

    #[test]
    fn max_by_min_by_are_always_nullable() {
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("max_by"));
        assert!(TypeInferenceEngine::aggregate_is_always_nullable("min_by"));
    }

    #[test]
    fn array_agg_returns_array_of_elem_from_registry_rule() {
        assert_eq!(
            frt("array_agg", &[DataType::String]),
            DataType::Array(Box::new(DataType::String), false)
        );
    }

    #[test]
    fn array_agg_of_array_returns_array_of_array() {
        // Spark semantics: aggregating `Array<T>` yields `Array<Array<T>>`.
        let inner = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            frt("array_agg", std::slice::from_ref(&inner)),
            DataType::Array(Box::new(inner), false)
        );
    }

    /// `array_position(arr, item)` returns Long, not the array's element
    /// type. Spark's return is BIGINT (Long) — 1-based index or 0 if not
    /// found.
    #[test]
    fn array_position_returns_long() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(
            frt("array_position", std::slice::from_ref(&arr)),
            DataType::Long
        );
    }

    /// `arrays_overlap(a, b)` returns Boolean regardless of the array
    /// element type.
    #[test]
    fn arrays_overlap_returns_boolean() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(frt("arrays_overlap", &[arr]), DataType::Boolean);
    }

    /// `flatten(Array<Array<T>>)` unwraps one level of nesting → `Array<T>`,
    /// preserving the inner containsNull flag.
    #[test]
    fn flatten_reduces_one_array_level() {
        let inner = DataType::Array(Box::new(DataType::String), true);
        let outer = DataType::Array(Box::new(inner.clone()), false);
        assert_eq!(frt("flatten", &[outer]), inner);
    }

    /// `F.window(ts, duration)` → `Struct{start: Timestamp, end: Timestamp}`
    /// with both fields nullable (Spark's `TimeWindow.dataType` uses
    /// `StructField` default nullable). Return type is arg-independent so
    /// `first_arg_type = None` returns the same fixed struct.
    #[test]
    fn window_returns_struct_of_two_nullable_timestamps() {
        let expected = DataType::Struct(StructType::new(vec![
            StructField::nullable("start", DataType::Timestamp),
            StructField::nullable("end", DataType::Timestamp),
        ]));
        assert_eq!(frt("window", &[DataType::Timestamp]), expected);
        // Arg-independent — no arg-type context should give the same schema.
        assert_eq!(frt("window", &[]), expected);
        // Case-insensitive dispatch.
        assert_eq!(frt("Window", &[DataType::Timestamp]), expected);
    }

    #[test]
    fn make_dt_interval_returns_day_time_interval() {
        // Spark's `make_dt_interval(1, 2, 30, 0)` yields
        // `DayTimeIntervalType`.
        assert_eq!(frt("make_dt_interval", &[]), DataType::day_time_full());
        // Case-insensitive dispatch.
        assert_eq!(frt("MAKE_DT_INTERVAL", &[]), DataType::day_time_full());
    }

    #[test]
    fn make_ym_interval_returns_year_month_interval() {
        assert_eq!(frt("make_ym_interval", &[]), DataType::year_month_full());
    }

    #[test]
    fn make_interval_returns_calendar_interval() {
        // Spark 4.1: `make_interval(1, 2, 0, 5)` returns
        // `CalendarIntervalType` (our `DataType::Interval`).
        assert_eq!(frt("make_interval", &[]), DataType::Interval);
        assert_eq!(frt("try_make_interval", &[]), DataType::Interval);
    }
}
