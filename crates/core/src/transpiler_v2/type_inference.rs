//! τ's TypeInferenceEngine — Spark-compatible type inference.
//!
//! Owned by τ (INV10: τ imports only `DataType`, `StructField`, `StructType`
//! from `crate::types`).

use super::name_fold::eq_fold;
use super::schema::{Attribute, ResolvedSchema};
use crate::types::{DataType, StructField, StructType};

/// Date-returning Spark functions. Single home of the roster consulted by
/// [`TypeInferenceEngine::function_return_type`]'s Date guard arm, and by
/// emission.rs's `date_typed_functions_return_date_in_duckdb` audit test,
/// which mechanically checks that each of these renders to a DuckDB
/// expression whose runtime type is DATE.
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

    /// Look up the type of a qualified column reference in `schema`.
    pub fn qualified_column_type(
        name: &str,
        qualifier: Option<&str>,
        schema: &ResolvedSchema,
    ) -> DataType {
        Self::qualified_column_info(name, qualifier, schema).0
    }

    /// Look up the nullability of a qualified column reference in `schema`.
    pub fn qualified_column_nullable(
        name: &str,
        qualifier: Option<&str>,
        schema: &ResolvedSchema,
    ) -> bool {
        Self::qualified_column_info(name, qualifier, schema).1
    }

    /// Shared resolver behind [`Self::column_type`] / [`Self::column_nullable`]:
    /// `(data_type, nullable)` of `name` in `schema`. Not found →
    /// `(Unresolved, true)`. Thin wrapper over [`Self::column_info_in`] scoped
    /// to the whole schema.
    fn column_info(name: &str, schema: &ResolvedSchema) -> (DataType, bool) {
        Self::column_info_in(name, &schema.fields).unwrap_or((DataType::Unresolved, true))
    }

    /// Slice-scoped sibling of [`Self::column_info`]: look up `name`
    /// case-insensitively within `fields` (rather than a whole [`StructType`]),
    /// returning `None` when not found instead of the `(Unresolved, true)`
    /// sentinel. Thin wrapper over [`Self::resolve_in`], discarding the
    /// resolved attribute — callers that need it (an `ExprId` to stamp) call
    /// `resolve_in` directly instead of re-walking `fields` a second time.
    pub(super) fn column_info_in(name: &str, fields: &[Attribute]) -> Option<(DataType, bool)> {
        Self::resolve_in(name, fields).map(|(dt, nullable, _)| (dt, nullable))
    }

    /// THE single home of the name-lookup ORDER shared by every
    /// [`super::analyzer`] resolver tier: exact case-insensitive name match
    /// first, then (for a dotted `"struct.field"` path) the struct column
    /// named by the first segment. Returns the matched attribute alongside
    /// its `(data_type, nullable)` so callers that need identity (an
    /// `ExprId` to stamp on a `ColumnReference`) get it from the SAME walk
    /// that produced the type — no paired second lookup, no hand-maintained
    /// re-basing when `fields` is a sub-slice of a larger schema (the
    /// returned `&Attribute` borrows from the caller's own backing storage,
    /// whatever range it was sliced from).
    ///
    /// Lets callers restrict a name-only lookup to a contiguous sub-range of
    /// a merged join schema (the alias→field-range map built by
    /// [`super::analyzer`]'s `QualifierScopes`). The dot-notation branch
    /// supports exactly ONE level of struct-field nesting (no recursion) —
    /// the qualified path ([`Self::qualified_column_info`] /
    /// [`Self::struct_qualifier_info`]) is the one that recurses. Struct-field
    /// nullability ORs in the parent column's nullability (a NULL struct makes
    /// every field read NULL); in the dotted case the returned attribute is
    /// the struct COLUMN, not the nested field (mirroring the pre-existing
    /// `field_index` behavior this replaces).
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

    /// Shared resolver behind [`Self::qualified_column_type`] /
    /// [`Self::qualified_column_nullable`]. Tries `qualifier` as a struct
    /// column first (with recursive nested-field resolution), then falls back
    /// to the unqualified lookup. `pub(super)` so [`super::analyzer`]'s
    /// `resolve_column` can call it once per fallback site instead of the
    /// `qualified_column_type` + `qualified_column_nullable` pair (each of
    /// which re-runs this same resolution end-to-end). Thin wrapper over
    /// [`Self::qualified_resolve_in`], discarding the resolved attribute.
    pub(super) fn qualified_column_info(
        name: &str,
        qualifier: Option<&str>,
        schema: &ResolvedSchema,
    ) -> (DataType, bool) {
        let (dt, nullable, _) = Self::qualified_resolve_in(name, qualifier, schema);
        (dt, nullable)
    }

    /// Identity-carrying sibling of [`Self::qualified_column_info`]: same
    /// struct-precedence-then-fallback resolution, additionally surfacing the
    /// resolved top-level [`Attribute`] (for its `ExprId`) when the
    /// UNQUALIFIED fallback is the one that matched. `None` in the third slot
    /// means either the name was not found at all, OR the struct-qualifier
    /// arm matched — the resolved field then lives INSIDE a struct column,
    /// not as its own top-level attribute, so there is no id to stamp
    /// (matching `resolve_column` tier-(d)'s existing choice to stamp `None`
    /// in that case).
    pub(super) fn qualified_resolve_in<'a>(
        name: &str,
        qualifier: Option<&str>,
        schema: &'a ResolvedSchema,
    ) -> (DataType, bool, Option<&'a Attribute>) {
        if let Some(q) = qualifier {
            if let Some((dt, nullable)) = Self::struct_qualifier_info(name, q, schema) {
                return (dt, nullable, None);
            }
        }
        match Self::resolve_in(name, &schema.fields) {
            Some((dt, nullable, attr)) => (dt, nullable, Some(attr)),
            None => (DataType::Unresolved, true, None),
        }
    }

    /// The struct-qualifier arm of [`Self::qualified_column_info`], extracted
    /// so [`super::analyzer`]'s `resolve_column` can run it standalone: struct
    /// precedence (a qualifier naming a top-level STRUCT column) must win over
    /// relation-alias scope resolution. `None` means `qualifier` does not name
    /// a struct column with a matching field — callers fall through to
    /// alias-scope or legacy name-only resolution.
    pub(super) fn struct_qualifier_info(
        name: &str,
        qualifier: &str,
        schema: &ResolvedSchema,
    ) -> Option<(DataType, bool)> {
        let f = schema.field_by_name(qualifier)?;
        if let DataType::Struct(st) = &f.data_type {
            if let Some(ff) = st.field_by_name(name) {
                return Some((ff.data_type.clone(), f.nullable || ff.nullable));
            }
            if let Some((dt, nullable)) = Self::resolve_nested_field(name, st) {
                return Some((dt, f.nullable || nullable));
            }
        }
        None
    }

    /// Recursively resolve a dotted `path` inside a struct, returning
    /// `(data_type, nullable)`. Nullability ORs in every ancestor field's
    /// nullability along the path.
    fn resolve_nested_field(path: &str, st: &StructType) -> Option<(DataType, bool)> {
        let dot_pos = path.find('.')?;
        let head = &path[..dot_pos];
        let tail = &path[dot_pos + 1..];
        let field = st.field_by_name(head)?;
        if let DataType::Struct(inner_st) = &field.data_type {
            if let Some(ff) = inner_st.field_by_name(tail) {
                Some((ff.data_type.clone(), field.nullable || ff.nullable))
            } else {
                let (dt, inner_nullable) = Self::resolve_nested_field(tail, inner_st)?;
                Some((dt, field.nullable || inner_nullable))
            }
        } else {
            None
        }
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
        use DataType::*;
        // Names without a spec row echo the argument's type — byte-identical
        // to the old default match arm.
        let Some(spec) = agg_spec_lower(&name.to_lowercase()) else {
            return arg_type.clone();
        };
        match spec.ret {
            AggRet::Long => Long,
            AggRet::Double => Double,
            AggRet::Integer => Integer,
            AggRet::Byte => Byte,
            AggRet::Boolean => Boolean,
            AggRet::ArgType => arg_type.clone(),
            AggRet::ArrayOfArg => Array(Box::new(arg_type.clone()), false),

            // SUM family: integer types → Long, float → Double, decimal → wider.
            // `try_sum` mirrors `sum`.
            AggRet::SumLike => match arg_type {
                Byte | Short | Integer | Long => Long,
                Float => Double,
                Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 10).min(38) as u8;
                    Decimal {
                        precision: p,
                        scale: *scale,
                    }
                }
                _ => arg_type.clone(),
            },

            // AVG family: integer types → Double, decimal → wider.
            // `try_avg` mirrors `avg`.
            AggRet::AvgLike => match arg_type {
                Byte | Short | Integer | Long => Double,
                Float | Double => Double,
                Decimal { precision, scale } => {
                    let p = (*precision as u16 + 4).min(38) as u8;
                    let s = (*scale + 4).min(18).min(p);
                    Decimal {
                        precision: p,
                        scale: s,
                    }
                }
                _ => arg_type.clone(),
            },
        }
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
        agg_spec_lower(name_lower).is_some_and(|s| s.null == AggNull::NonNullable)
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
        agg_spec_lower(name_lower).is_some_and(|s| s.null == AggNull::AlwaysNullable)
    }

    /// Return type of a window function given the optional argument type.
    ///
    /// `name` is expected to be canonical lowercase.
    pub fn window_return_type(name: &str, arg_type: Option<&DataType>) -> DataType {
        match name {
            "row_number" | "rank" | "dense_rank" | "ntile" => DataType::Integer,
            "percent_rank" | "cume_dist" => DataType::Double,
            "lag" | "lead" | "first_value" | "last_value" | "nth_value" => {
                arg_type.cloned().unwrap_or(DataType::Long)
            }
            agg => Self::aggregate_return_type(agg, arg_type.unwrap_or(&DataType::Unresolved)),
        }
    }

    /// Is this window function non-nullable (ranking + COUNT).
    ///
    /// `name` is expected to be canonical lowercase.
    pub fn window_is_non_nullable(name: &str) -> bool {
        matches!(
            name,
            "row_number"
                | "rank"
                | "dense_rank"
                | "ntile"
                | "percent_rank"
                | "cume_dist"
                | "count"
                | "count_distinct"
        )
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

    /// Infer the return type of a scalar/table function.
    ///
    /// Receives the FULL list of argument `(type, nullable)` pairs (`args`),
    /// so this is the single home for every return-type rule that depends on
    /// argument types and/or per-argument nullability alone — including the
    /// multi-arg widening folds (`coalesce` / `greatest` / `least`),
    /// arity-branch selection (`nvl2` / `if` / `iif`, `aggregate` / `reduce`),
    /// decimal widening (`mod` / `pmod`), and the nullability-widening array /
    /// map constructors (`array`, `map` / `create_map` — O11). Arms that need
    /// the argument *expressions themselves* (literal schemas, literal
    /// scales, struct field naming) stay in
    /// `Expression::function_call_data_type`, which pre-empts this resolver —
    /// a legitimate second home, not drift, because `(DataType, bool)` pairs
    /// cannot carry a literal value or an expression shape.
    ///
    /// Arms that only need argument TYPES read the derived `arg_types` view;
    /// arms that only need the first argument read `arg_types.first()`; arms
    /// that need nothing at all (hash / grouping) ignore `arg_types`/`args`.
    ///
    /// **τ seed:** returns `DataType::Unresolved` for anything the
    /// aggregate roster does not handle.
    /// Unsupported functions return `DataType::Unresolved` so the boundary
    /// guard can reject them instead of mis-typing the projection.
    pub fn function_return_type(name: &str, args: &[(DataType, bool)]) -> DataType {
        use DataType::*;
        // Type-only view: every arm whose return type depends on argument
        // TYPES alone reads this slice unchanged. The array / map arms below
        // also read `args` directly for per-argument nullability
        // (containsNull / valueContainsNull). Plan-time clone only —
        // negligible next to the `data_type()`/`nullable()` walks that built
        // `args` in the first place.
        let arg_types_owned: Vec<DataType> = args.iter().map(|(t, _)| t.clone()).collect();
        let arg_types: &[DataType] = &arg_types_owned;
        let first_arg_type = arg_types.first();
        // The most common per-arm reduction: the first argument's type, or
        // the arm-specific `default` when the call has no arguments.
        let first_arg_or = |default: DataType| first_arg_type.cloned().unwrap_or(default);
        let name_lower = name.to_lowercase();
        match name_lower.as_str() {
            "hash" | "murmur3" => Integer,
            "xxhash64" => Long,

            // Grouping indicators.
            "grouping" => Byte,
            "grouping_id" => Long,

            // Spark's `coalesce(a, b, c, ...)` (and aliases `nvl` / `ifnull`)
            // plus `greatest` / `least` return the least-common (widening)
            // type across all args (e.g.
            // `coalesce(decimal(9,2), decimal(2,2)) → decimal(10,2)`). An
            // empty arg list yields `Unresolved` — byte-identical to the old
            // path, where `function_call_data_type`'s `!is_empty()` guard fell
            // through to this resolver's weaker first-arg arm with a `None`
            // first arg (`first_arg_type.cloned().unwrap_or(Unresolved)`).
            "coalesce" | "nvl" | "ifnull" | "greatest" | "least" => match arg_types.split_first() {
                Some((first, rest)) => rest
                    .iter()
                    .fold(first.clone(), |acc, dt| Self::promote_numeric(&acc, dt)),
                None => Unresolved,
            },
            // Spark's `nvl2(cond, ifNotNull, ifNull)` and `if(cond, then,
            // else)` / `iif(...)` derive their return type from the branch
            // args (not the condition): the type of `args[1]`. Guarded on
            // arity 3 to stay byte-identical — any other arity fell through
            // the old `f.args.len() == 3` guard to this resolver's default
            // (`Unresolved`, since these names had no first-arg arm).
            "nvl2" | "if" | "iif" if arg_types.len() == 3 => arg_types[1].clone(),
            // Spark's `aggregate(arr, init, (acc, x) -> f [, finish])` /
            // `reduce` / `list_reduce` fold the array with `init` as the seed;
            // the result type is the seed/accumulator type (`args[1]`).
            // Guarded on arity ≥ 2 to stay byte-identical — a shorter arg list
            // fell through the old `f.args.len() >= 2` guard to this resolver's
            // default (`Unresolved`).
            "aggregate" | "reduce" | "list_reduce" if arg_types.len() >= 2 => arg_types[1].clone(),

            // Delegate to aggregate_return_type for known aggregates (the
            // `delegate` column of `AGG_SPECS`).
            n if agg_spec_lower(n).is_some_and(|s| s.delegate) => {
                Self::aggregate_return_type(n, first_arg_type.unwrap_or(&Unresolved))
            }

            // Most string functions return String; length family returns
            // Integer; regexp / like family returns Boolean.
            "concat" | "concat_ws" | "upper" | "lower" | "trim" | "ltrim" | "rtrim" | "btrim"
            | "substr" | "substring" | "left" | "right" | "lpad" | "rpad" | "replace"
            | "regexp_replace" | "regexp_extract" | "translate" | "initcap" | "space" | "repeat"
            | "overlay" | "format_string" | "format_number" | "base64" | "unbase64"
            | "url_encode" | "url_decode" | "encode" | "decode" | "soundex" | "sentences"
            | "split_part" | "substring_index" => String,
            // `regexp_extract_all(str, pattern[, group])` returns Array<String>.
            // Spark 4.x.
            "regexp_extract_all" => Array(Box::new(String), true),
            "length" | "char_length" | "character_length" | "octet_length" | "bit_length"
            | "levenshtein" | "instr" | "locate" | "position" | "ascii" | "unicode"
            | "find_in_set" | "regexp_count" | "regexp_instr" => Integer,
            "like" | "ilike" | "rlike" | "regexp_like" | "contains" | "startswith"
            | "starts_with" | "endswith" | "ends_with" | "isnull" | "isnotnull" | "isnan"
            | "eqnullsafe" => Boolean,
            "split" => DataType::Array(Box::new(String), false),
            "sha" | "sha1" | "sha2" | "md5" => String,
            // Spark's `crc32(binary)` returns BIGINT (unsigned CRC widened
            // to Long). Emission side lives in the `spark_crc32` session
            // macro (registered by `DuckDbSession::spawn` — bit-exact
            // `java.util.zip.CRC32` emulation with a 256-entry lookup
            // table); dispatch arm at `emission.rs` remaps `crc32` →
            // `spark_crc32`.
            "crc32" => Long,
            // Spark's `elt(idx, s1, s2, ...)` returns the type of the
            // picked argument. Return String as the common shape; nullability
            // follows the default rules.
            "elt" => String,
            // Spark's `parse_url(url, part[, key])` returns STRING.
            // (`url_encode`/`url_decode` already covered by the String
            // fold above; `find_in_set` covered by the Integer fold.)
            "parse_url" => String,

            // Most math functions on numeric return Double.
            "sqrt" | "cbrt" | "exp" | "expm1" | "ln" | "log" | "log10" | "log2" | "log1p"
            | "pow" | "power" | "sin" | "cos" | "tan" | "asin" | "acos" | "atan" | "atan2"
            | "sinh" | "cosh" | "tanh" | "asinh" | "acosh" | "atanh" | "degrees" | "radians"
            | "e" | "pi" | "hypot" | "rand" | "randn" | "random" => Double,
            // abs / nullif preserve the first-arg type; ceil/floor return
            // Long (Spark rule); signum returns Double. (round/bround are
            // resolved earlier by the `function_call_data_type` pre-pass,
            // which reads the scale literal; coalesce / nvl / ifnull /
            // greatest / least are typed once by the widening arm above —
            // never here.)
            "abs" | "nullif" => first_arg_or(Unresolved),
            "ceil" | "ceiling" | "floor" => {
                Self::ceil_floor_type(first_arg_type.unwrap_or(&Unresolved), None)
            }
            "sign" | "signum" => Double,
            // `negative`/`negate` map to Spark's `UnaryMinus`, whose
            // `dataType` equals the child type (int→int, decimal→decimal).
            "negative" | "negate" => first_arg_or(Unresolved),
            // Spark's `positive(x)` (`UnaryPositive`) is the identity —
            // `dataType` equals the child type, mirroring `negative` above.
            "positive" => first_arg_or(Unresolved),
            "factorial" => Long,
            // `mod(a, b)` / `pmod(a, b)` — when BOTH operands are Decimal,
            // Spark's `Remainder`/`Pmod` result decimal type widens per
            // `decimal_mod_type` (scale = max, precision = min(int digits) +
            // scale). Every other operand shape (int/int, bigint/int,
            // wrong arity — including zero args, which yield `Integer`)
            // keeps the first-arg type, byte-identical to the old
            // `function_call_data_type` pre-empt + first-arg fall-through.
            "mod" | "pmod" => match arg_types {
                [Decimal {
                    precision: p1,
                    scale: s1,
                }, Decimal {
                    precision: p2,
                    scale: s2,
                }] => Self::decimal_mod_type(*p1, *s1, *p2, *s2),
                _ => first_arg_or(Integer),
            },
            // `nanvl(a, b)` returns the type of the first argument (Spark:
            // both args must be Float/Double; return matches).
            "nanvl" => first_arg_or(Double),
            // `try_divide(a, b)` — Spark returns Double for integral / Float
            // inputs, Decimal for Decimal inputs (widened per
            // `decimal_div_type(p1,s1,p2,s2)`). This resolver only sees the
            // first arg's type, so the Decimal-input case cannot be computed
            // correctly here. Return `Unresolved` as a placeholder so the
            // ADR-022 boundary guard trips honestly rather than silently
            // mis-typing the projection.
            // TODO: needs multi-arg dispatch (both operand types) to compute
            // the widened Decimal via `Self::decimal_div_type`.
            "try_divide" => match first_arg_type {
                Some(Decimal { .. }) => Unresolved,
                _ => Double,
            },
            "bin" | "hex" => String,
            "unhex" => Binary,
            "conv" => String,
            "shiftleft" | "shiftright" | "shiftrightunsigned" | "bitwise_and" | "bitwise_or"
            | "bitwise_xor" | "bitwise_not" | "bit_count" | "bit_length_arg" | "bitwise_or_agg"
            | "&" | "|" | "^" | "bitwiseand" | "bitwiseor" | "bitwisexor" => first_arg_or(Integer),
            // Spark's `bit_get(x, pos)`/`getbit(x, pos)` return the bit at
            // 0-indexed `pos` (from the LSB) of the integral `x`, as a Byte
            // (TINYINT) — independent of `x`'s own width.
            "bit_get" | "getbit" => Byte,

            "current_timestamp" | "now" => Timestamp,
            // `date_trunc(fmt, ts_or_date)` returns Timestamp when the
            // second arg is Timestamp, Date when the second arg is Date.
            // Without the second arg's type at this call site, default to
            // Timestamp (the common case).
            "date_trunc" => Timestamp,
            "months_between" => Double,
            "to_timestamp" | "from_utc_timestamp" | "to_utc_timestamp" | "make_timestamp" => {
                Timestamp
            }
            // Date-returning Spark functions — single-homed in
            // `DATE_RETURNING_FNS` (also the sample roster for
            // `date_typed_functions_return_date_in_duckdb` in emission.rs).
            // Includes `make_date(year, month, day)` (three-arg integer
            // form).
            n if DATE_RETURNING_FNS.contains(&n) => Date,
            // `from_unixtime(secs[, fmt])` returns String in Spark (default
            // format `yyyy-MM-dd HH:mm:ss`), not Timestamp. `dayname`/
            // `monthname` return the day-of-week / month name as String
            // (DuckDB-native — emission passes them through unchanged).
            // `to_char(x, fmt)` formats `x` (date/timestamp form, per the
            // as a String, mirroring `date_format` below.
            "from_unixtime" | "date_format" | "date_part" | "dayname" | "monthname" | "to_char" => {
                String
            }
            // `unix_timestamp` returns Long (BIGINT) in Spark; the other
            // date-field extractors (`year`, `month`, `hour`, …) return
            // Integer. Keep them separate.
            "unix_timestamp" | "unix_micros" | "unix_millis" | "unix_seconds" => Long,
            "year" | "month" | "day" | "dayofmonth" | "dayofweek" | "dayofyear" | "weekofyear"
            | "hour" | "minute" | "second" | "quarter" | "week" | "datediff" | "extract" => Integer,
            // Spark's `timestampadd(unit, quantity, ts)` returns TIMESTAMP;
            // `timestampdiff(unit, start, end)` returns BIGINT (Long). The
            // leading UNIT is a string literal (demoted in the SparkSQL
            // lowering / proto converter), not a column.
            "timestampadd" => Timestamp,
            "timestampdiff" => Long,

            // Spark's `array(a, b, ...)` / `make_array` / `list_value` /
            // `list` — element type = findWiderCommonType (`unify_types`)
            // over all args; `containsNull` = any arg nullable. Single home
            // (moved from `Expression::function_call_data_type`, O11) — the
            // widened `(DataType, bool)` signature now carries per-arg
            // nullability, so the expression-level pre-pass fast path is no
            // longer needed. Empty call → `Array<Unresolved, true>`,
            // byte-identical to the prior defensive stub.
            "array" | "list_value" | "make_array" | "list" => match args.split_first() {
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
            // Spark's `map(k1, v1, ...)` / `create_map` — key/value type =
            // unify (`unify_types`) over the even/odd-index args;
            // `value_nullable` = any value arg nullable. Single home (moved
            // from `Expression::function_call_data_type`, O11). Malformed
            // (empty / odd arity) falls through to the `Map<String, String,
            // true>` arm below, identical to the prior pre-pass fallthrough.
            "map" | "create_map" if !args.is_empty() && args.len().is_multiple_of(2) => {
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
            // `str_to_map(str, pair_delim, kv_delim)` returns the same
            // `Map<VARCHAR, VARCHAR>`. Session macro `str_to_map` (see
            // `runtime/session.rs`) provides the DuckDB translation.
            //
            // `map`/`create_map` are now single-homed above (O11); this
            // unguarded arm serves only `map_from_entries` (no fast path) and
            // `str_to_map`, plus malformed `map`/`create_map` calls
            // (empty/odd-arity) that fall through the guarded arm above —
            // an honest-but-approximate `Map<String, String, true>` fallback.
            "map" | "create_map" | "map_from_entries" | "str_to_map" => DataType::Map {
                key: Box::new(String),
                value: Box::new(String),
                value_nullable: true,
            },
            // Spark's `map_from_arrays(keys, values)` (`MapFromArrays`)
            // derives the key type from the KEYS array's element type and
            // the value type + `valueContainsNull` from the VALUES array's
            // element type + `containsNull` flag (verified against Spark
            // 4.1.1: `MapFromArrays.dataType`) — distinct from
            // `map`/`create_map` above, which alternate key/value args and
            // widen across pairs. Both operand facts here (elem type +
            // containsNull) live inside `DataType::Array`'s own shape, so
            // `arg_types` alone is sufficient: this arm has been
            // `map_from_arrays`'s single home since before O11. A malformed
            // (non-2-arity or non-`Array`-typed) call falls through to the
            // `Map<String, String, true>` default above.
            "map_from_arrays" => match arg_types {
                // Exactly two Array args —
                // `f.args.len() == 2` guard; a 3+-arity call is malformed
                // (Spark rejects it as WRONG_NUM_ARGS) and takes the
                // fallback below, never a derived type.
                [Array(key_ty, _), Array(val_ty, value_contains_null)] => DataType::Map {
                    key: key_ty.clone(),
                    value: val_ty.clone(),
                    value_nullable: *value_contains_null,
                },
                _ => DataType::Map {
                    key: Box::new(String),
                    value: Box::new(String),
                    value_nullable: true,
                },
            },
            // `map_concat(m1, m2, ...)` merges maps left-to-right; result
            // type is the (unified) map type of the arguments. τ takes the
            // first-arg type as an approximation — Spark rejects
            // mixed-key/value-type inputs earlier, so the first-arg type
            // matches the result type on any well-typed input.
            "map_concat" => first_arg_or(Unresolved),

            // Return type = first-arg type (the collection) for filters;
            // transform / zip_with produce a NEW array but at this
            // resolver we approximate with first-arg type. Downstream
            // downstream schema validation will surface any element-type mismatch.
            // NOTE: `aggregate` / `reduce` / `list_reduce` are intentionally
            // NOT in this bucket — their return type is the fold-seed type
            // (arg[1]), not the array type. See
            // `Expression::function_call_data_type`'s fast-path.
            // NOTE: `reverse` is polymorphic — `reverse(str)→String`,
            // `reverse(array)→same array type`. First-arg-type covers both, so
            // it must NOT be added to the String-function group above (doing so
            // would mistype `reverse(array)` as String).
            "transform" | "list_transform" | "filter" | "list_filter" | "list_reverse"
            | "zip_with" | "list_zip" | "map_filter"
            | "map_zip_with" | "sort_array" | "list_sort" | "array_distinct" | "list_distinct"
            | "list_intersect" | "array_union" | "list_concat_unique"
            | "array_except" | "array_repeat" | "reverse" | "shuffle"
            | "arrays_zip" | "slice" | "list_slice"
            // Spark map higher-order functions preserve the outer Map type
            // (element-nullability details are approximated).
            | "transform_values" | "transform_keys" => first_arg_or(Unresolved),
            // Spark's `array_intersect(a, b)` returns `Array<T>` with
            // `containsNull = leftContainsNull AND rightContainsNull` per
            // Catalyst's `ArrayIntersect` — a NULL in the output requires
            // BOTH inputs to contain NULL. Both args' element type is
            // available in `arg_types`, so compute the AND directly rather
            // than approximating. When the second arg is
            // a non-nullable array literal (`rightContainsNull=false`), so
            // the AND still collapses to `containsNull=false`, matching the
            // prior hardcoded stamp. When either arg's type isn't a
            // resolved `Array` (should not happen for a well-typed call),
            // conservatively fall back to the old `containsNull=false`
            // stamp.
            "array_intersect" => match arg_types {
                [Array(_, left_contains_null), Array(_, right_contains_null), ..] => {
                    Self::rewrap_array(first_arg_type, Some(*left_contains_null && *right_contains_null))
                }
                _ => Self::rewrap_array(first_arg_type, Some(false)),
            },
            // `array_position(arr, item)` returns the 1-based index of the
            // first match, or 0 if not found. Spark returns `Long` (BIGINT)
            // regardless of the array element type.
            "array_position" | "list_position" => Long,
            "array_max" | "list_max" | "array_min" | "list_min" => {
                // Element type of the array — reduce Array<T> → T.
                match first_arg_type {
                    Some(DataType::Array(inner, _)) => (**inner).clone(),
                    _ => Unresolved,
                }
            }
            "array_join" | "list_string_agg" => String,
            // `arrays_overlap(a, b)` → Boolean. Spark returns Boolean.
            "arrays_overlap" | "list_has_any" => Boolean,
            // `flatten(Array<Array<T>>)` reduces one level of nesting →
            // Array<T>. Preserve inner containsNull flag.
            "flatten" | "list_flatten" => match first_arg_type {
                Some(DataType::Array(outer_inner, _)) => match outer_inner.as_ref() {
                    DataType::Array(inner, contains_null) => {
                        Array(inner.clone(), *contains_null)
                    }
                    _ => (**outer_inner).clone(),
                },
                _ => Unresolved,
            },
            "exists" | "list_any" | "forall" | "list_all" | "array_contains" | "list_contains"
            | "map_contains_key" => Boolean,

            // `size`, `cardinality`, `array_size`, `map_size` — always
            // return Integer regardless of the collection type.
            "size" | "cardinality" | "array_size" | "map_size" => Integer,

            // `sequence(start, stop[, step])` returns Array<T> where T is
            // the first arg's type (Long by default).
            "sequence" => Array(Box::new(first_arg_or(Long)), false),

            // Spark's `explode(arr)` / `explode_outer(arr)` emit one row
            // per element and return the array's element type. Emission
            // handles the row-multiplying `UNNEST(...)` in SELECT context.
            //
            // `posexplode_val` / `posexplode_pos` are internal FunctionCall
            // shapes synthesized by the converter when it splits a
            // multi-name `Alias(names=[pos, val], inner=posexplode(arr))`
            // into two projections.
            //
            // `element_at(coll, k)` shares the exact same reduction: Array →
            // element type; Map → value type (always nullable — missing key
            // returns NULL).
            "explode" | "explode_outer" | "posexplode_val" | "element_at" => match first_arg_type {
                Some(DataType::Array(elem, _)) => (**elem).clone(),
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },
            "posexplode_pos" => Integer,
            // Synthetic `map_explode_key(m)` / `map_explode_val(m)` — see the
            // v2 relation converter's alias-splitter (map-007). Return the
            // map's key / value type respectively.
            "map_explode_key" => match first_arg_type {
                Some(DataType::Map { key, .. }) => (**key).clone(),
                _ => Unresolved,
            },
            "map_explode_val" => match first_arg_type {
                Some(DataType::Map { value, .. }) => (**value).clone(),
                _ => Unresolved,
            },
            // Synthetic `stack_col(v1, v2, ..., vN)` — one per output
            // column of Spark's `stack(N, ...) AS (...)`. Analyzer pre-pass
            // (`expand_stack_projections`) fans a wrapped
            // `stack_multi_alias(...)` projection out into K per-column
            // `stack_col` calls with N row-values apiece. Every arg shares
            // a type in Spark (Stack.checkInputDataTypes coerces across
            // rows); take the first arg's type as the column type.
            // Emission maps `stack_col(v1, ..., vN)` to `UNNEST([v1, ..., vN])`.
            "stack_col" => first_arg_or(Unresolved),

            // `array_append` / `array_prepend`: Spark stamps containsNull
            // = true (a NULL element may be appended).
            // `array_insert(arr, pos, val)` — Spark stamps `containsNull=true`
            // (out-of-range positive `pos` pads the gap with NULLs). Return
            // the same element type as the input array.
            "array_append" | "array_prepend" | "append_element" | "prepend_element"
            | "array_insert" => Self::rewrap_array(first_arg_type, Some(true)),
            // `array_compact` removes NULL elements → containsNull=false.
            "array_compact" => Self::rewrap_array(first_arg_type, Some(false)),
            // `array_remove` preserves the input's containsNull flag.
            "array_remove" => Self::rewrap_array(first_arg_type, None),

            // (`element_at` rides the explode arm above — same Array/Map
            // reduction.)
            // `map_keys(Map<K, V>) → Array<K>`. Spark stamps
            // containsNull=true on the returned array (matches the
            // reference `ArrayType(StringType(), True)` — the map-keys
            // ArrayType is defensively nullable in Spark's schema even
            // though map keys are non-null in the data model).
            "map_keys" => match first_arg_type {
                Some(DataType::Map { key, .. }) => Array(key.clone(), true),
                _ => Unresolved,
            },
            // `map_values(Map<K, V>) → Array<V>` — inherits map's
            // value_nullable.
            "map_values" => match first_arg_type {
                Some(DataType::Map {
                    value,
                    value_nullable,
                    ..
                }) => Array(value.clone(), *value_nullable),
                _ => Unresolved,
            },
            // `map_entries(Map<K, V>) → Array<Struct{key: K NOT NULL,
            // value: V nullable}>`, containsNull=false.
            "map_entries" => match first_arg_type {
                Some(DataType::Map {
                    key,
                    value,
                    value_nullable,
                }) => {
                    let entry_struct = DataType::Struct(StructType::new(vec![
                        StructField::not_null("key", (**key).clone()),
                        StructField::new("value", (**value).clone(), *value_nullable),
                    ]));
                    Array(Box::new(entry_struct), false)
                }
                _ => Unresolved,
            },

            // `get_json_object(json_str, path)` returns String (nullable
            // when the path doesn't match).
            "get_json_object" | "json_extract_scalar" | "json_extract_string" => String,
            // `to_json(struct)` — Spark returns String; nullability follows
            // the argument (a NULL struct produces NULL, a non-null struct
            // produces a non-null JSON string). The default
            // `function_call_nullable` fallback (`any(arg.nullable)`) is
            // correct here — no override needed.
            "to_json" => String,
            // `schema_of_json(json_str)` — Spark returns a DDL schema String.
            // Requires the `thdck_spark_funcs` extension (`spark_schema_of_json`);
            // remapped at emission time.
            "schema_of_json" => String,
            // `to_csv(struct)` — Spark returns String. DuckDB has no native
            // `to_csv`; τ emits `concat_ws(',', CAST(f1 AS VARCHAR), ...)`
            // when the argument is a `struct(...)` literal. Nullability
            // follows argument nullability.
            "to_csv" => String,
            // `json_object_keys(jsonStr)` — Spark returns `Array<String>` of
            // the top-level object's keys (NULL if the input isn't a JSON
            // object). Emission remaps to DuckDB's native `json_keys`, which
            // already returns `VARCHAR[]`.
            "json_object_keys" => Array(Box::new(String), true),
            // Synthetic per-key `FunctionCall` produced by the
            // analyzer's Project pre-pass for `F.json_tuple` (the sole
            // choke point creating this name: see
            // `analyzer::expand_json_tuple_projections`, which already
            // enforces arity ≥ 2 pre-expansion and stamps exactly 2 args
            // — json expr + one literal key — per expanded call). Return
            // type is always `String` per Spark's `JsonTuple.elementSchema`
            // — an arity-only rule (no per-arg *expression* nullability
            // needed), so its single home is this resolver, not the
            // `function_call_data_type` pre-pass. The exact-2 slice pattern
            // mirrors `map_from_arrays`'s arity guard above; anything else
            // is malformed and falls through to the shared `Unresolved`
            // default.
            "json_tuple_field" => match arg_types {
                [_, _] => String,
                _ => Unresolved,
            },

            // `array_agg` is routed through the aggregate delegation list
            // above (unified with `collect_list`/`collect_set`), so it never
            // falls through here. Removed the scalar arm to eliminate the
            // divergent behavior where `array_agg(Array<T>)` incorrectly
            // returned `Array<T>` instead of `Array<Array<T>>` (Spark: an
            // aggregate over `Array<T>` yields `Array<Array<T>>`).
            // `histogram_numeric(col, nb) → Array<Struct{x: Double
            // (nullable), y: Double (nullable)}>` (containsNull=true) per
            // Spark 4's HistogramNumeric schema. The inner struct fields
            // and the outer array are all reported nullable=true via the
            // agent-observed reference schema.
            "histogram_numeric" => {
                let bin_struct = DataType::Struct(StructType::new(vec![
                    StructField::nullable("x", Double),
                    StructField::nullable("y", Double),
                ]));
                Array(Box::new(bin_struct), true)
            }

            // `F.window(ts, duration)` — tumbling time-window. Spark's
            // `TimeWindow.dataType` is a fixed
            // `Struct{start: TimestampType, end: TimestampType}` with both
            // fields nullable (Spark's `StructField` default). The struct
            // itself is nullable iff `ts` is nullable — that's handled by the
            // default `any(arg.nullable)` fallback in
            // `Expression::function_call_nullable`.
            "window" => DataType::Struct(StructType::new(vec![
                StructField::nullable("start", Timestamp),
                StructField::nullable("end", Timestamp),
            ])),

            // `input_file_name()` returns String (empty for in-memory).
            "input_file_name" | "input_file_block_start" | "input_file_block_length" => String,

            "typeof" => String,
            "spark_partition_id" => Integer,
            "monotonically_increasing_id" => Long,

            // Spark's `make_dt_interval(days[, hours[, mins[, secs]]])`
            // returns a `DayTimeIntervalType`.
            "make_dt_interval" | "try_make_dt_interval" => DayTimeInterval,
            // `make_ym_interval(years[, months])` returns
            // `YearMonthIntervalType`.
            "make_ym_interval" | "try_make_ym_interval" => YearMonthInterval,
            // `make_interval(years, months, weeks, days[, hours, mins, secs])`
            // returns `CalendarIntervalType` in Spark 4.1.
            "make_interval" | "try_make_interval" => Interval,

            // τ seed: everything else is unresolved.
            _ => Unresolved,
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

/// Return-type rule of an aggregate function (the `ret` column of
/// [`AGG_SPECS`]). Fixed-type variants map directly to a `DataType`;
/// `SumLike` / `AvgLike` carry Spark's integer-widening / decimal-widening
/// bodies in [`TypeInferenceEngine::aggregate_return_type`]; `ArgType`
/// echoes the argument's type — which is also the behavior of any name
/// absent from the table, so `ArgType` rows are byte-identical to the old
/// default match arm.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggRet {
    /// Fixed `Long` (COUNT family, `grouping_id`, approx-distinct family).
    Long,
    /// Fixed `Double` (stddev/variance, corr/covar/regr, percentile family).
    Double,
    /// Fixed `Integer` (scalar-wrapper size functions seen as aggregates).
    Integer,
    /// Fixed `Byte` (`grouping`).
    Byte,
    /// Fixed `Boolean` (bool_and/bool_or + Spark aliases).
    Boolean,
    /// Same type as the argument (min/max/first/last, bit aggregates, ...).
    ArgType,
    /// SUM family: integer types → Long, float → Double, decimal → wider.
    SumLike,
    /// AVG family: integer types → Double, decimal → wider.
    AvgLike,
    /// `Array(arg_type, containsNull = false)` (collect_list/collect_set).
    ArrayOfArg,
}

/// Nullability class of an aggregate function (the `null` column of
/// [`AGG_SPECS`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AggNull {
    /// Always non-nullable (COUNT family, grouping, collect family).
    NonNullable,
    /// Returns NULL for an empty group (SUM/AVG/MIN/MAX, statistics, ...).
    AlwaysNullable,
    /// In neither nullability predicate (scalar-wrapper size functions).
    Neither,
}

/// One row of the aggregate spec table — the single source of truth that
/// replaced five parallel hand-maintained lists (`aggregate_return_type`
/// match arms, the two nullability predicates, `function_return_type`'s
/// aggregate-delegation arm, and the `AGGREGATE_NAMES` classifier const).
///
/// Roster membership is observable behavior: keep classifier, delegation, and
/// nullability flags aligned with the parser and emitter contracts.
struct AggSpec {
    /// Lowercase canonical function name.
    name: &'static str,
    /// Return-type rule (see [`AggRet`]).
    ret: AggRet,
    /// Nullability class (see [`AggNull`]).
    null: AggNull,
    /// In the SparkSQL classifier roster:
    /// `parser_v2::v2_lowering::is_aggregate_function_name` and emission's
    /// `is_aggregate_name` treat this name as an aggregate.
    classifier: bool,
    /// `function_return_type` delegates this name to
    /// [`TypeInferenceEngine::aggregate_return_type`].
    delegate: bool,
}

/// Shorthand row constructor keeping [`AGG_SPECS`] readable.
const fn agg(
    name: &'static str,
    ret: AggRet,
    null: AggNull,
    classifier: bool,
    delegate: bool,
) -> AggSpec {
    AggSpec {
        name,
        ret,
        null,
        classifier,
        delegate,
    }
}

/// The flat aggregate spec table. INV10-compliant — lives under the
/// `transpiler_v2` tree that τ's front-ends are allowed to consume.
///
/// Column order: `(name, ret, null, classifier, delegate)`.
/// `rustfmt::skip` keeps the one-row-per-line tabular layout readable.
#[rustfmt::skip]
const AGG_SPECS: &[AggSpec] = &[
    // COUNT family (non-nullable).
    agg("count", AggRet::Long, AggNull::NonNullable, true, true),
    agg("count_distinct", AggRet::Long, AggNull::NonNullable, true, true),
    agg("count_if", AggRet::Long, AggNull::NonNullable, true, true),
    // GROUPING / GROUPING_ID (non-nullable; `function_return_type` resolves
    // them via dedicated arms rather than aggregate delegation).
    agg("grouping", AggRet::Byte, AggNull::NonNullable, true, false),
    agg("grouping_id", AggRet::Long, AggNull::NonNullable, true, false),
    // SUM family (always-nullable). `try_sum` mirrors `sum` — τ's analyzer
    agg("sum", AggRet::SumLike, AggNull::AlwaysNullable, true, true),
    agg("sum_distinct", AggRet::SumLike, AggNull::AlwaysNullable, true, true),
    agg("try_sum", AggRet::SumLike, AggNull::AlwaysNullable, true, true),
    // AVG family. `try_avg` mirrors `avg`.
    agg("avg", AggRet::AvgLike, AggNull::AlwaysNullable, true, true),
    agg("mean", AggRet::AvgLike, AggNull::AlwaysNullable, true, true),
    agg("try_avg", AggRet::AvgLike, AggNull::AlwaysNullable, true, true),
    // MIN / MAX / first / last / any_value — same type as argument.
    agg("min", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("max", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("first", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("last", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("first_value", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("last_value", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("any_value", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    // STDDEV / VARIANCE / SKEWNESS / KURTOSIS → Double.
    // Drift (verbatim): `std` is NOT in the classifier roster.
    agg("stddev", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("stddev_samp", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("std", AggRet::Double, AggNull::AlwaysNullable, false, true),
    agg("stddev_pop", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("variance", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("var_samp", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("var_pop", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("skewness", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("kurtosis", AggRet::Double, AggNull::AlwaysNullable, true, true),
    // Correlation / covariance / regression family → Double.
    agg("corr", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("covar_samp", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("covar_pop", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_slope", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_r2", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_intercept", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_avgx", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_avgy", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_sxx", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_sxy", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("regr_syy", AggRet::Double, AggNull::AlwaysNullable, true, true),
    // `regr_count` is always nullable and otherwise falls through to its
    // argument type; it is not a classifier or delegation entry.
    agg("regr_count", AggRet::ArgType, AggNull::AlwaysNullable, false, false),
    // Percentile / median → Double.
    agg("percentile", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("percentile_approx", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("approx_percentile", AggRet::Double, AggNull::AlwaysNullable, true, true),
    agg("median", AggRet::Double, AggNull::AlwaysNullable, true, true),
    // `mode` falls through to its argument type.
    agg("mode", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    // `max_by(x, y)` / `min_by(x, y)` — the value of `x` at the row where
    // `y` is max/min. Return type = the type of the FIRST arg (`x`), which
    // is exactly what `AggRet::ArgType` resolves to via the aggregate
    // delegation arm's `first_arg_type`. Always-nullable (empty group, an
    // all-NULL `y` column, or a NULL `x` at the extreme `y` row all yield
    // NULL, matching DuckDB's `arg_max_null`/`arg_min_null` which this pair
    // renders to — see `render_aggregate`).
    agg("max_by", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("min_by", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    // `nth_value` is handled by `window_return_type` and is always nullable.
    agg("nth_value", AggRet::ArgType, AggNull::AlwaysNullable, false, false),
    // collect_list / collect_set / array_agg → Array (non-nullable).
    // `array_agg` is a Spark 4.x alias of `collect_list`; it is not a
    // classifier entry because the SQL front-end handles it separately.
    agg("collect_list", AggRet::ArrayOfArg, AggNull::NonNullable, true, true),
    agg("collect_set", AggRet::ArrayOfArg, AggNull::NonNullable, true, true),
    agg("array_agg", AggRet::ArrayOfArg, AggNull::NonNullable, false, true),
    // approx_count_distinct → Long; these names are not classifier entries.
    agg("approx_count_distinct", AggRet::Long, AggNull::NonNullable, false, true),
    agg("count_approx_distinct", AggRet::Long, AggNull::NonNullable, false, true),
    // Bit aggregates → same type as arg.
    agg("bit_and", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("bit_or", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    agg("bit_xor", AggRet::ArgType, AggNull::AlwaysNullable, true, true),
    // Bool aggregates → Boolean.
    // `any` / `some` / `all` are Spark aliases of `bool_or` /
    // `bool_or` / `bool_and` — registering them in the classifier roster lets
    // the SparkSQL parser classify bare-aggregate fragments (`F.expr("any(x)")`)
    // as aggregates so they route to `lower_aggregate_select`, and lets
    // emission's `is_aggregate_name` route them to `render_aggregate` where
    // the name-remap already lives.
    agg("bool_and", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    agg("every", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    agg("bool_or", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    agg("any", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    agg("some", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    agg("all", AggRet::Boolean, AggNull::AlwaysNullable, true, true),
    // Scalar-wrapper size functions when they appear as aggregates — a
    // return-type arm only (no nullability class, no classifier/delegation).
    agg("size", AggRet::Integer, AggNull::Neither, false, false),
    agg("cardinality", AggRet::Integer, AggNull::Neither, false, false),
    agg("map_size", AggRet::Integer, AggNull::Neither, false, false),
    agg("array_size", AggRet::Integer, AggNull::Neither, false, false),
];

/// Look up the spec row for an already-lowercased aggregate name.
fn agg_spec_lower(name_lower: &str) -> Option<&'static AggSpec> {
    AGG_SPECS.iter().find(|s| s.name == name_lower)
}

/// Case-insensitive membership test against the classifier roster (the
/// `classifier` column of [`AGG_SPECS`]. Used by the SparkSQL front-end
/// (`parser_v2::v2_lowering::is_aggregate_function_name`) and by emission's
/// `is_aggregate_name` to decide whether a bare function name is an
/// aggregate. Table names are all-lowercase ASCII, so case-insensitive byte
/// comparison matches without allocating a lowercased `String`.
///
/// N5 note: `is_aggregate_function_name`'s `function_call_has_aggregate`
/// call site runs over the raw pre-lowering `sqlparser` AST (as-written
/// user casing, not yet N5-canonicalized), so this genuinely stays
/// case-insensitive — it is not a redundant N5 site.
pub(crate) fn is_aggregate_classifier_name(name: &str) -> bool {
    AGG_SPECS
        .iter()
        .any(|s| s.classifier && s.name.eq_ignore_ascii_case(name))
}

/// Classifier-roster names (test-only iterator for the symmetric-omission
/// mechanical checks in this module and `expression.rs`).
#[cfg(test)]
pub(crate) fn aggregate_classifier_names() -> impl Iterator<Item = &'static str> {
    AGG_SPECS.iter().filter(|s| s.classifier).map(|s| s.name)
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

/// The 3-name hash family — non-nullable regardless of args.
#[cfg(test)]
pub(crate) const HASH_FAMILY_NAMES: &[&str] = &["hash", "murmur3", "xxhash64"];

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Every aggregate classifier name belongs to exactly one of
    /// `aggregate_is_non_nullable` or `aggregate_is_always_nullable`.
    #[test]
    fn every_aggregate_return_type_name_appears_in_a_nullability_predicate() {
        for name in aggregate_classifier_names() {
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

    /// AGG_SPECS single-table invariants: every row name is unique (no
    /// shadowed lookups) and all-lowercase ASCII (the `_lower` predicates
    /// and `eq_ignore_ascii_case` classifier lookups rely on it).
    #[test]
    fn agg_specs_names_are_unique_and_lowercase() {
        let mut seen = std::collections::HashSet::new();
        for spec in AGG_SPECS {
            assert!(
                seen.insert(spec.name),
                "duplicate AGG_SPECS row for `{}`",
                spec.name,
            );
            assert!(
                spec.name
                    .chars()
                    .all(|c| !c.is_ascii_uppercase() && !c.is_whitespace()),
                "AGG_SPECS name `{}` must be lowercase ASCII with no whitespace",
                spec.name,
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

    /// `try_sum` and `try_avg` belong to the aggregate classifier roster so
    /// `is_aggregate_function_name` recognizes them as aggregate functions.
    #[test]
    fn aggregate_names_contains_try_sum_and_try_avg() {
        assert!(
            is_aggregate_classifier_name("try_sum"),
            "classifier roster must contain try_sum (checklist §1.4)",
        );
        assert!(
            is_aggregate_classifier_name("try_avg"),
            "classifier roster must contain try_avg (checklist §1.4)",
        );
    }

    /// `try_divide` is a scalar function — it must NOT be in the classifier
    /// roster.
    #[test]
    fn aggregate_names_does_not_contain_try_divide() {
        assert!(
            !is_aggregate_classifier_name("try_divide"),
            "classifier roster must NOT contain try_divide (scalar per checklist §4.1)",
        );
    }

    /// No name appears in BOTH the aggregate-classifier roster and the
    /// nondeterministic roster — the two are disjoint predicate domains, and
    /// a name in both would make `contains_aggregate_call` /
    /// `contains_nondeterministic_call` racy about which fallback trigger
    /// fires first for a given Sort key.
    #[test]
    fn aggregate_classifier_and_nondeterministic_rosters_are_disjoint() {
        for name in aggregate_classifier_names() {
            assert!(
                !is_nondeterministic_fn_name(name),
                "`{name}` is in both the aggregate-classifier roster and the \
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

    /// Both `try_sum` and `try_avg` must be in the always-nullable roster
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
    fn murmur3_return_type_is_integer() {
        assert_eq!(frt("murmur3", &[DataType::String]), DataType::Integer);
    }

    #[test]
    fn nanvl_returns_first_arg_type() {
        assert_eq!(frt("nanvl", &[DataType::Double]), DataType::Double);
        assert_eq!(frt("nanvl", &[DataType::Float]), DataType::Float);
    }

    #[test]
    fn negative_preserves_arg_type() {
        // `negative`/`negate` map to Spark's UnaryMinus: dataType == child.
        assert_eq!(frt("negative", &[DataType::Integer]), DataType::Integer);
        assert_eq!(frt("negative", &[dec(10, 2)]), dec(10, 2));
        assert_eq!(frt("negate", &[DataType::Integer]), DataType::Integer);
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

    /// `explode(Array<T>)`, `explode_outer(Array<T>)`, and
    /// `posexplode_val(Array<T>)` return the element type T.
    #[test]
    fn explode_returns_array_element_type() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(frt("explode", std::slice::from_ref(&arr)), DataType::String);
        assert_eq!(
            frt("explode_outer", std::slice::from_ref(&arr)),
            DataType::String
        );
        assert_eq!(frt("posexplode_val", &[arr]), DataType::String);
    }

    /// `posexplode_pos(arr)` returns the 0-indexed synthetic position as an
    /// Integer.
    #[test]
    fn posexplode_pos_returns_integer() {
        let arr = DataType::Array(Box::new(DataType::String), true);
        assert_eq!(frt("posexplode_pos", &[arr]), DataType::Integer);
    }

    #[test]
    fn sequence_returns_array_of_first_arg() {
        assert_eq!(
            frt("sequence", &[DataType::Integer]),
            DataType::Array(Box::new(DataType::Integer), false)
        );
        // No arg → default element type is Long (start/stop unknown).
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
        // Spark 4 reports the histogram bin struct fields and the outer
        // array as nullable=true (containsNull=true).
        let expected = DataType::Array(
            Box::new(DataType::Struct(StructType::new(vec![
                StructField::nullable("x", DataType::Double),
                StructField::nullable("y", DataType::Double),
            ]))),
            true,
        );
        assert_eq!(frt("histogram_numeric", &[DataType::Double]), expected);
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
    fn max_by_returns_first_arg_type_via_aggregate_delegation() {
        // Return type = type of `x` (the value column), not `y` (the
        // ordering column) — mirrors DuckDB's `arg_max_null(x, y)`.
        assert_eq!(
            frt("max_by", &[DataType::String, DataType::Integer]),
            DataType::String
        );
    }

    #[test]
    fn min_by_returns_first_arg_type_via_aggregate_delegation() {
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
    fn array_agg_returns_array_of_elem_via_aggregate_delegation() {
        // `array_agg` is unified with `collect_list`/`collect_set` in the
        // aggregate delegation list — the scalar arm was removed to avoid
        // the divergent case where `array_agg(Array<T>)` incorrectly
        // stayed at `Array<T>` instead of widening to `Array<Array<T>>`.
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
        assert_eq!(frt("list_position", &[arr]), DataType::Long);
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
        assert_eq!(frt("make_dt_interval", &[]), DataType::DayTimeInterval);
        assert_eq!(frt("try_make_dt_interval", &[]), DataType::DayTimeInterval);
        // Case-insensitive dispatch.
        assert_eq!(frt("MAKE_DT_INTERVAL", &[]), DataType::DayTimeInterval);
    }

    #[test]
    fn make_ym_interval_returns_year_month_interval() {
        assert_eq!(frt("make_ym_interval", &[]), DataType::YearMonthInterval);
        assert_eq!(
            frt("try_make_ym_interval", &[]),
            DataType::YearMonthInterval
        );
    }

    #[test]
    fn make_interval_returns_calendar_interval() {
        // Spark 4.1: `make_interval(1, 2, 0, 5)` returns
        // `CalendarIntervalType` (our `DataType::Interval`).
        assert_eq!(frt("make_interval", &[]), DataType::Interval);
        assert_eq!(frt("try_make_interval", &[]), DataType::Interval);
    }
}
