//! Live metadata for migrated Spark function spellings.

use super::generator::GeneratorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeRule {
    ArrayOfArgument,
    Average,
    Boolean,
    Byte,
    Double,
    FirstArgument,
    Integer,
    Long,
    PreserveArray,
    String,
    Sum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NullRule {
    Always,
    AnyArgument,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DuckFunction {
    ArgMaxNull,
    ArgMinNull,
    BoolAnd,
    BoolOr,
    Count,
    KurtosisPop,
    Stddev,
    Sum,
    Trim,
}

impl DuckFunction {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ArgMaxNull => "arg_max_null",
            Self::ArgMinNull => "arg_min_null",
            Self::BoolAnd => "bool_and",
            Self::BoolOr => "bool_or",
            Self::Count => "count",
            Self::KurtosisPop => "kurtosis_pop",
            Self::Stddev => "stddev",
            Self::Sum => "sum",
            Self::Trim => "trim",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ExtensionFunction {
    Average,
    DecimalDivide,
    Hash,
    SchemaOfJson,
    Skewness,
    TryAverage,
    TryDivide,
    TrySum,
    XxHash64,
}

impl ExtensionFunction {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 9] = [
        Self::Average,
        Self::DecimalDivide,
        Self::Hash,
        Self::SchemaOfJson,
        Self::Skewness,
        Self::TryAverage,
        Self::TryDivide,
        Self::TrySum,
        Self::XxHash64,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Average => "spark_avg",
            Self::DecimalDivide => "spark_decimal_div",
            Self::Hash => "spark_hash",
            Self::SchemaOfJson => "spark_schema_of_json",
            Self::Skewness => "spark_skewness",
            Self::TryAverage => "spark_try_avg",
            Self::TryDivide => "spark_try_divide",
            Self::TrySum => "spark_try_sum",
            Self::XxHash64 => "spark_xxhash64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionFunction {
    ArrayRemove,
    CollectList,
    CollectSet,
    Crc32,
}

impl SessionFunction {
    #[cfg(test)]
    pub(crate) const ALL: [Self; 4] = [
        Self::ArrayRemove,
        Self::CollectList,
        Self::CollectSet,
        Self::Crc32,
    ];

    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::ArrayRemove => "array_remove",
            Self::CollectList => "collect_list",
            Self::CollectSet => "collect_set",
            Self::Crc32 => "spark_crc32",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarSpecial {
    SubstringIndex,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ScalarEmission {
    Extension(ExtensionFunction),
    Native,
    Rename(DuckFunction),
    Session(SessionFunction),
    Special(ScalarSpecial),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CallableSpec<E> {
    pub(crate) result: TypeRule,
    pub(crate) nullability: NullRule,
    pub(crate) emission: E,
}

pub(crate) type ScalarSpec = CallableSpec<ScalarEmission>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggregateSpecial {
    ApproxPercentile,
    Average,
    CountIf,
    FirstLast,
    Mode,
    Percentile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggregateEmission {
    Distinct(DuckFunction),
    Extension(ExtensionFunction),
    Native,
    Rename(DuckFunction),
    Session(SessionFunction),
    Special(AggregateSpecial),
}

pub(crate) type AggregateSpec = CallableSpec<AggregateEmission>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct GeneratorSpec {
    pub(crate) kind: GeneratorKind,
    pub(crate) outer: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FunctionImplementation {
    Aggregate(AggregateSpec),
    Generator(GeneratorSpec),
    Scalar(ScalarSpec),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FunctionSpec {
    pub(crate) name: &'static str,
    pub(crate) implementation: FunctionImplementation,
}

const fn scalar(
    name: &'static str,
    result: TypeRule,
    nullability: NullRule,
    emission: ScalarEmission,
) -> FunctionSpec {
    FunctionSpec {
        name,
        implementation: FunctionImplementation::Scalar(CallableSpec {
            result,
            nullability,
            emission,
        }),
    }
}

const fn aggregate(
    name: &'static str,
    result: TypeRule,
    nullability: NullRule,
    emission: AggregateEmission,
) -> FunctionSpec {
    FunctionSpec {
        name,
        implementation: FunctionImplementation::Aggregate(CallableSpec {
            result,
            nullability,
            emission,
        }),
    }
}

const fn generator(name: &'static str, kind: GeneratorKind, outer: bool) -> FunctionSpec {
    FunctionSpec {
        name,
        implementation: FunctionImplementation::Generator(GeneratorSpec { kind, outer }),
    }
}

use AggregateEmission as AE;
use AggregateSpecial as AS;
use DuckFunction as DF;
use ExtensionFunction as EF;
use NullRule as N;
use ScalarEmission as SE;
use ScalarSpecial as SS;
use SessionFunction as SF;
use TypeRule as T;

#[rustfmt::skip]
const FUNCTION_SPECS: &[FunctionSpec] = &[
    scalar("abs",                   T::FirstArgument,   N::AnyArgument, SE::Native),
    aggregate("all",               T::Boolean,         N::Always,      AE::Rename(DF::BoolAnd)),
    aggregate("any",               T::Boolean,         N::Always,      AE::Rename(DF::BoolOr)),
    aggregate("any_value",         T::FirstArgument,   N::Always,      AE::Native),
    aggregate("approx_count_distinct", T::Long,        N::Never,       AE::Native),
    aggregate("approx_percentile", T::Double,          N::Always,      AE::Special(AS::ApproxPercentile)),
    aggregate("array_agg",         T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectList)),
    scalar("array_remove",          T::PreserveArray,   N::AnyArgument, SE::Session(SF::ArrayRemove)),
    aggregate("avg",               T::Average,         N::Always,      AE::Special(AS::Average)),
    aggregate("bit_and",           T::FirstArgument,   N::Always,      AE::Native),
    aggregate("bit_or",            T::FirstArgument,   N::Always,      AE::Native),
    aggregate("bit_xor",           T::FirstArgument,   N::Always,      AE::Native),
    aggregate("bool_and",          T::Boolean,         N::Always,      AE::Native),
    aggregate("bool_or",           T::Boolean,         N::Always,      AE::Native),
    scalar("btrim",                T::String,          N::AnyArgument, SE::Rename(DF::Trim)),
    aggregate("collect_list",      T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectList)),
    aggregate("collect_set",       T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectSet)),
    aggregate("corr",              T::Double,          N::Always,      AE::Native),
    aggregate("count",             T::Long,            N::Never,       AE::Native),
    aggregate("count_distinct",    T::Long,            N::Never,       AE::Distinct(DF::Count)),
    aggregate("count_if",          T::Long,            N::Never,       AE::Special(AS::CountIf)),
    aggregate("covar_pop",         T::Double,          N::Always,      AE::Native),
    aggregate("covar_samp",        T::Double,          N::Always,      AE::Native),
    scalar("crc32",                T::Long,            N::AnyArgument, SE::Session(SF::Crc32)),
    aggregate("every",             T::Boolean,         N::Always,      AE::Rename(DF::BoolAnd)),
    generator("explode",           GeneratorKind::Explode,            false),
    generator("explode_outer",     GeneratorKind::Explode,            true),
    aggregate("first",             T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    aggregate("first_value",       T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    aggregate("grouping",          T::Byte,            N::Never,       AE::Native),
    aggregate("grouping_id",       T::Long,            N::Never,       AE::Native),
    scalar("hash",                 T::Integer,         N::Never,       SE::Extension(EF::Hash)),
    generator("inline",            GeneratorKind::Inline,             false),
    generator("inline_outer",      GeneratorKind::Inline,             true),
    generator("json_tuple",        GeneratorKind::JsonTuple,          false),
    aggregate("kurtosis",          T::Double,          N::Always,      AE::Rename(DF::KurtosisPop)),
    aggregate("last",              T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    aggregate("last_value",        T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    aggregate("max",               T::FirstArgument,   N::Always,      AE::Native),
    aggregate("max_by",            T::FirstArgument,   N::Always,      AE::Rename(DF::ArgMaxNull)),
    aggregate("mean",              T::Average,         N::Always,      AE::Special(AS::Average)),
    aggregate("median",            T::Double,          N::Always,      AE::Native),
    aggregate("min",               T::FirstArgument,   N::Always,      AE::Native),
    aggregate("min_by",            T::FirstArgument,   N::Always,      AE::Rename(DF::ArgMinNull)),
    aggregate("mode",              T::FirstArgument,   N::Always,      AE::Special(AS::Mode)),
    aggregate("percentile",        T::Double,          N::Always,      AE::Special(AS::Percentile)),
    aggregate("percentile_approx", T::Double,          N::Always,      AE::Special(AS::ApproxPercentile)),
    generator("posexplode",        GeneratorKind::PosExplode,         false),
    generator("posexplode_outer",  GeneratorKind::PosExplode,         true),
    aggregate("regr_avgx",         T::Double,          N::Always,      AE::Native),
    aggregate("regr_avgy",         T::Double,          N::Always,      AE::Native),
    aggregate("regr_count",        T::Long,            N::Never,       AE::Native),
    aggregate("regr_intercept",    T::Double,          N::Always,      AE::Native),
    aggregate("regr_r2",           T::Double,          N::Always,      AE::Native),
    aggregate("regr_slope",        T::Double,          N::Always,      AE::Native),
    aggregate("regr_sxx",          T::Double,          N::Always,      AE::Native),
    aggregate("regr_sxy",          T::Double,          N::Always,      AE::Native),
    aggregate("regr_syy",          T::Double,          N::Always,      AE::Native),
    aggregate("skewness",          T::Double,          N::Always,      AE::Extension(EF::Skewness)),
    aggregate("some",              T::Boolean,         N::Always,      AE::Rename(DF::BoolOr)),
    generator("stack",             GeneratorKind::Stack,              false),
    aggregate("std",               T::Double,          N::Always,      AE::Rename(DF::Stddev)),
    aggregate("stddev",            T::Double,          N::Always,      AE::Native),
    aggregate("stddev_pop",        T::Double,          N::Always,      AE::Native),
    aggregate("stddev_samp",       T::Double,          N::Always,      AE::Native),
    scalar("substring_index",      T::String,          N::AnyArgument, SE::Special(SS::SubstringIndex)),
    aggregate("sum",               T::Sum,             N::Always,      AE::Native),
    aggregate("sum_distinct",      T::Sum,             N::Always,      AE::Distinct(DF::Sum)),
    aggregate("try_avg",           T::Average,         N::Always,      AE::Extension(EF::TryAverage)),
    aggregate("try_sum",           T::Sum,             N::Always,      AE::Extension(EF::TrySum)),
    aggregate("var_pop",           T::Double,          N::Always,      AE::Native),
    aggregate("var_samp",          T::Double,          N::Always,      AE::Native),
    aggregate("variance",          T::Double,          N::Always,      AE::Native),
];

pub(crate) fn lookup(name_lower: &str) -> Option<&'static FunctionSpec> {
    FUNCTION_SPECS
        .binary_search_by_key(&name_lower, |spec| spec.name)
        .ok()
        .map(|index| &FUNCTION_SPECS[index])
}

pub(crate) fn scalar_spec(name_lower: &str) -> Option<&'static ScalarSpec> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Scalar(ref spec) => Some(spec),
        FunctionImplementation::Aggregate(_) | FunctionImplementation::Generator(_) => None,
    }
}

pub(crate) fn aggregate_spec(name_lower: &str) -> Option<&'static AggregateSpec> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Aggregate(ref spec) => Some(spec),
        FunctionImplementation::Generator(_) | FunctionImplementation::Scalar(_) => None,
    }
}

pub(crate) fn generator_spec(name_lower: &str) -> Option<&'static GeneratorSpec> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Generator(ref spec) => Some(spec),
        FunctionImplementation::Aggregate(_) | FunctionImplementation::Scalar(_) => None,
    }
}

pub(crate) fn generator_name(kind: GeneratorKind, outer: bool) -> Option<&'static str> {
    let matching = |spec: &&FunctionSpec| match spec.implementation {
        FunctionImplementation::Generator(generator) => generator.kind == kind,
        _ => false,
    };
    FUNCTION_SPECS
        .iter()
        .filter(matching)
        .find_map(|spec| match spec.implementation {
            FunctionImplementation::Generator(generator) if generator.outer == outer => {
                Some(spec.name)
            }
            _ => None,
        })
        .or_else(|| {
            let mut matches = FUNCTION_SPECS.iter().filter(matching);
            let only = matches.next()?;
            matches.next().is_none().then_some(only.name)
        })
}

pub(crate) fn is_aggregate(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    aggregate_spec(&lower).is_some()
}

pub(crate) fn function_names() -> impl Iterator<Item = &'static str> {
    FUNCTION_SPECS.iter().map(|spec| spec.name)
}

#[cfg(test)]
pub(crate) fn aggregate_names() -> impl Iterator<Item = &'static str> {
    FUNCTION_SPECS.iter().filter_map(|spec| {
        matches!(spec.implementation, FunctionImplementation::Aggregate(_)).then_some(spec.name)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn specs_are_sorted_and_unique() {
        assert!(FUNCTION_SPECS
            .windows(2)
            .all(|pair| pair[0].name < pair[1].name));
    }

    #[test]
    fn kind_is_not_encoded_as_independent_flags() {
        assert!(is_aggregate("STD"));
        assert!(is_aggregate("array_agg"));
        assert!(is_aggregate("approx_count_distinct"));
        assert!(scalar_spec("size").is_none());
        assert_eq!(generator_spec("explode_outer").map(|s| s.outer), Some(true));
        assert_eq!(
            generator_name(GeneratorKind::JsonTuple, true),
            Some("json_tuple")
        );
    }

    #[test]
    fn corrected_spark_aggregate_facts_are_atomic_rows() {
        let regr_count = aggregate_spec("regr_count").unwrap();
        assert_eq!(regr_count.result, TypeRule::Long);
        assert_eq!(regr_count.nullability, NullRule::Never);
        assert_eq!(regr_count.emission, AggregateEmission::Native);

        let array_agg = aggregate_spec("array_agg").unwrap();
        assert_eq!(array_agg.result, TypeRule::ArrayOfArgument);
        assert_eq!(array_agg.nullability, NullRule::Never);
        assert_eq!(
            array_agg.emission,
            AggregateEmission::Session(SessionFunction::CollectList)
        );
    }
}
