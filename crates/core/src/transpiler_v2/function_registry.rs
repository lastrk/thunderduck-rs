//! Live implementation registry for supported Spark function spellings.

use super::generator::GeneratorKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TypeRule {
    ArrayElement,
    ArrayOfArgument,
    ArrayWithoutNulls,
    ArrayWithNulls,
    Average,
    Binary,
    Boolean,
    Byte,
    Date,
    Double,
    FirstArgument,
    HistogramNumeric,
    Integer,
    Long,
    MapEntries,
    MapFromArrays,
    MapKeys,
    MapValues,
    PreserveArray,
    SecondArgument,
    Sequence,
    String,
    StringArray,
    StringMap,
    Sum,
    Timestamp,
    WidenArguments,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NullRule {
    AllArguments,
    Always,
    AnyArgument,
    BranchArguments,
    Never,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DuckFunction {
    ArgMaxNull,
    ArgMinNull,
    BoolAnd,
    BoolOr,
    Coalesce,
    Count,
    EndsWith,
    KurtosisPop,
    ListContains,
    ListHasAny,
    ListMax,
    ListMin,
    ListSlice,
    MapContains,
    Printf,
    Sha1,
    StartsWith,
    Stddev,
    Substring,
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
            Self::Coalesce => "coalesce",
            Self::Count => "count",
            Self::EndsWith => "ends_with",
            Self::KurtosisPop => "kurtosis_pop",
            Self::ListContains => "list_contains",
            Self::ListHasAny => "list_has_any",
            Self::ListMax => "list_max",
            Self::ListMin => "list_min",
            Self::ListSlice => "list_slice",
            Self::MapContains => "map_contains",
            Self::Printf => "printf",
            Self::Sha1 => "sha1",
            Self::StartsWith => "starts_with",
            Self::Stddev => "stddev",
            Self::Substring => "substring",
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
pub(crate) enum ScalarEmission {
    Extension(ExtensionFunction),
    Native,
    Rename(DuckFunction),
    Session(SessionFunction),
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
pub(crate) enum SpecialFunction {
    AddMonths,
    Aggregate,
    Array,
    ArrayAppend,
    ArrayDistinct,
    ArrayExcept,
    ArrayIntersect,
    ArrayJoin,
    ArrayPosition,
    ArrayPrepend,
    ArrayUnion,
    ArraysZip,
    BitGet,
    BitwiseAnd,
    BitwiseOr,
    BitwiseXor,
    Bround,
    Cardinality,
    Ceil,
    Ceiling,
    Concat,
    ConcatWs,
    Conv,
    CreateMap,
    DateAdd,
    DateFormat,
    DateSub,
    Datediff,
    Dayofweek,
    ElementAt,
    Elt,
    EqNullSafe,
    Exists,
    Filter,
    FindInSet,
    Flatten,
    Floor,
    Forall,
    FromCsv,
    FromJson,
    FromUnixtime,
    FromUtcTimestamp,
    Getbit,
    Hex,
    Hypot,
    Ilike,
    Isnan,
    Isnotnull,
    Isnull,
    JsonObjectKeys,
    Lag,
    Lead,
    Like,
    Ln,
    Locate,
    Log,
    Log10,
    Log2,
    MakeDtInterval,
    MakeInterval,
    MakeYmInterval,
    Map,
    MapConcat,
    MapFilter,
    Mod,
    MonthsBetween,
    NamedStruct,
    Nanvl,
    Negative,
    Not,
    NthValue,
    Nvl2,
    Overlay,
    ParseUrl,
    Pmod,
    Positive,
    Reduce,
    Regexp,
    RegexpLike,
    RegexpReplace,
    Reverse,
    Rlike,
    Round,
    Sha2,
    Shiftleft,
    Shiftright,
    Sign,
    Signum,
    Size,
    SortArray,
    Split,
    Struct,
    SubstringIndex,
    Timestampadd,
    Timestampdiff,
    ToChar,
    ToCsv,
    ToDate,
    ToJson,
    ToNumber,
    ToTimestamp,
    ToUtcTimestamp,
    Transform,
    TransformKeys,
    TransformValues,
    Trunc,
    TryDivide,
    TryElementAt,
    TryMakeInterval,
    TryToNumber,
    Typeof,
    UnixTimestamp,
    UrlDecode,
    UrlEncode,
    Window,
    ZipWith,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LoweredFunction {
    Cast,
    When,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FunctionImplementation {
    Scalar(ScalarSpec),
    Aggregate(AggregateSpec),
    Generator(GeneratorSpec),
    Special(SpecialFunction),
    Lowered(LoweredFunction),
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

const fn special(name: &'static str, handler: SpecialFunction) -> FunctionSpec {
    FunctionSpec {
        name,
        implementation: FunctionImplementation::Special(handler),
    }
}

const fn lowered(name: &'static str, function: LoweredFunction) -> FunctionSpec {
    FunctionSpec {
        name,
        implementation: FunctionImplementation::Lowered(function),
    }
}

use AggregateEmission as AE;
use AggregateSpecial as AS;
use DuckFunction as DF;
use ExtensionFunction as EF;
use LoweredFunction as L;
use NullRule as N;
use ScalarEmission as SE;
use SessionFunction as SF;
use SpecialFunction as S;
use TypeRule as T;

#[rustfmt::skip]
const FUNCTION_SPECS: &[FunctionSpec] = &[
    special("&", S::BitwiseAnd),
    special("<=>", S::EqNullSafe),
    special("^", S::BitwiseXor),
    scalar("abs",                   T::FirstArgument,   N::AnyArgument, SE::Native),
    scalar("acos", T::Double, N::Always, SE::Native),
    scalar("acosh", T::Double, N::Always, SE::Native),
    special("add_months", S::AddMonths),
    special("aggregate", S::Aggregate),
    aggregate("all",               T::Boolean,         N::Always,      AE::Rename(DF::BoolAnd)),
    aggregate("any",               T::Boolean,         N::Always,      AE::Rename(DF::BoolOr)),
    aggregate("any_value",         T::FirstArgument,   N::Always,      AE::Native),
    aggregate("approx_count_distinct", T::Long,        N::Never,       AE::Native),
    aggregate("approx_percentile", T::Double,          N::Always,      AE::Special(AS::ApproxPercentile)),
    special("array", S::Array),
    aggregate("array_agg",         T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectList)),
    special("array_append", S::ArrayAppend),
    scalar("array_compact", T::ArrayWithoutNulls, N::AnyArgument, SE::Native),
    scalar("array_contains", T::Boolean, N::AnyArgument, SE::Rename(DF::ListContains)),
    special("array_distinct", S::ArrayDistinct),
    special("array_except", S::ArrayExcept),
    scalar("array_insert", T::ArrayWithNulls, N::AnyArgument, SE::Native),
    special("array_intersect", S::ArrayIntersect),
    special("array_join", S::ArrayJoin),
    scalar("array_max", T::ArrayElement, N::AnyArgument, SE::Rename(DF::ListMax)),
    scalar("array_min", T::ArrayElement, N::AnyArgument, SE::Rename(DF::ListMin)),
    special("array_position", S::ArrayPosition),
    special("array_prepend", S::ArrayPrepend),
    scalar("array_remove",          T::PreserveArray,   N::AnyArgument, SE::Session(SF::ArrayRemove)),
    scalar("array_repeat", T::ArrayOfArgument, N::AnyArgument, SE::Native),
    scalar("array_size", T::Integer, N::AnyArgument, SE::Native),
    special("array_union", S::ArrayUnion),
    scalar("arrays_overlap", T::Boolean, N::AnyArgument, SE::Rename(DF::ListHasAny)),
    special("arrays_zip", S::ArraysZip),
    scalar("ascii", T::Integer, N::AnyArgument, SE::Native),
    scalar("asin", T::Double, N::Always, SE::Native),
    scalar("asinh", T::Double, N::Always, SE::Native),
    scalar("atan", T::Double, N::Always, SE::Native),
    scalar("atan2", T::Double, N::AnyArgument, SE::Native),
    scalar("atanh", T::Double, N::Always, SE::Native),
    aggregate("avg",               T::Average,         N::Always,      AE::Special(AS::Average)),
    scalar("base64", T::String, N::AnyArgument, SE::Native),
    scalar("bin", T::String, N::AnyArgument, SE::Native),
    aggregate("bit_and",           T::FirstArgument,   N::Always,      AE::Native),
    scalar("bit_count", T::FirstArgument, N::AnyArgument, SE::Native),
    special("bit_get", S::BitGet),
    scalar("bit_length", T::Integer, N::AnyArgument, SE::Native),
    aggregate("bit_or",            T::FirstArgument,   N::Always,      AE::Native),
    aggregate("bit_xor",           T::FirstArgument,   N::Always,      AE::Native),
    special("bitwise_and", S::BitwiseAnd),
    scalar("bitwise_not", T::FirstArgument, N::AnyArgument, SE::Native),
    special("bitwise_or", S::BitwiseOr),
    special("bitwise_xor", S::BitwiseXor),
    special("bitwiseand", S::BitwiseAnd),
    special("bitwiseor", S::BitwiseOr),
    special("bitwisexor", S::BitwiseXor),
    aggregate("bool_and",          T::Boolean,         N::Always,      AE::Native),
    aggregate("bool_or",           T::Boolean,         N::Always,      AE::Native),
    special("bround", S::Bround),
    scalar("btrim",                T::String,          N::AnyArgument, SE::Rename(DF::Trim)),
    special("cardinality", S::Cardinality),
    lowered("cast", L::Cast),
    scalar("cbrt", T::Double, N::Always, SE::Native),
    special("ceil", S::Ceil),
    special("ceiling", S::Ceiling),
    scalar("char_length", T::Integer, N::AnyArgument, SE::Native),
    scalar("character_length", T::Integer, N::AnyArgument, SE::Native),
    scalar("coalesce", T::WidenArguments, N::AllArguments, SE::Native),
    aggregate("collect_list",      T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectList)),
    aggregate("collect_set",       T::ArrayOfArgument, N::Never,       AE::Session(SF::CollectSet)),
    special("concat", S::Concat),
    special("concat_ws", S::ConcatWs),
    scalar("contains", T::Boolean, N::AnyArgument, SE::Native),
    special("conv", S::Conv),
    aggregate("corr",              T::Double,          N::Always,      AE::Native),
    scalar("cos", T::Double, N::Always, SE::Native),
    scalar("cosh", T::Double, N::Always, SE::Native),
    scalar("cot", T::Double, N::Always, SE::Native),
    aggregate("count",             T::Long,            N::Never,       AE::Native),
    aggregate("count_distinct",    T::Long,            N::Never,       AE::Distinct(DF::Count)),
    aggregate("count_if",          T::Long,            N::Never,       AE::Special(AS::CountIf)),
    aggregate("covar_pop",         T::Double,          N::Always,      AE::Native),
    aggregate("covar_samp",        T::Double,          N::Always,      AE::Native),
    scalar("crc32",                T::Long,            N::AnyArgument, SE::Session(SF::Crc32)),
    special("create_map", S::CreateMap),
    scalar("csc", T::Double, N::Always, SE::Native),
    scalar("cume_dist", T::Double, N::Never, SE::Native),
    scalar("current_date", T::Date, N::AnyArgument, SE::Native),
    scalar("current_timestamp", T::Timestamp, N::AnyArgument, SE::Native),
    special("date_add", S::DateAdd),
    special("date_format", S::DateFormat),
    scalar("date_part", T::String, N::AnyArgument, SE::Native),
    special("date_sub", S::DateSub),
    scalar("date_trunc", T::Timestamp, N::AnyArgument, SE::Native),
    special("datediff", S::Datediff),
    scalar("day", T::Integer, N::AnyArgument, SE::Native),
    scalar("dayname", T::String, N::AnyArgument, SE::Native),
    scalar("dayofmonth", T::Integer, N::AnyArgument, SE::Native),
    special("dayofweek", S::Dayofweek),
    scalar("dayofyear", T::Integer, N::AnyArgument, SE::Native),
    scalar("decode", T::String, N::AnyArgument, SE::Native),
    scalar("degrees", T::Double, N::Always, SE::Native),
    scalar("dense_rank", T::Integer, N::Never, SE::Native),
    scalar("e", T::Double, N::AnyArgument, SE::Native),
    special("element_at", S::ElementAt),
    special("elt", S::Elt),
    scalar("encode", T::String, N::AnyArgument, SE::Native),
    scalar("ends_with", T::Boolean, N::AnyArgument, SE::Native),
    scalar("endswith", T::Boolean, N::AnyArgument, SE::Rename(DF::EndsWith)),
    special("eqnullsafe", S::EqNullSafe),
    aggregate("every",             T::Boolean,         N::Always,      AE::Rename(DF::BoolAnd)),
    special("exists", S::Exists),
    scalar("exp", T::Double, N::Always, SE::Native),
    generator("explode",           GeneratorKind::Explode,            false),
    generator("explode_outer",     GeneratorKind::Explode,            true),
    scalar("expm1", T::Double, N::Always, SE::Native),
    scalar("extract", T::Integer, N::AnyArgument, SE::Native),
    scalar("factorial", T::Long, N::Always, SE::Native),
    special("filter", S::Filter),
    special("find_in_set", S::FindInSet),
    aggregate("first",             T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    aggregate("first_value",       T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    special("flatten", S::Flatten),
    special("floor", S::Floor),
    special("forall", S::Forall),
    scalar("format_number", T::String, N::AnyArgument, SE::Native),
    scalar("format_string", T::String, N::Never, SE::Rename(DF::Printf)),
    special("from_csv", S::FromCsv),
    special("from_json", S::FromJson),
    special("from_unixtime", S::FromUnixtime),
    special("from_utc_timestamp", S::FromUtcTimestamp),
    scalar("get_json_object", T::String, N::AnyArgument, SE::Native),
    special("getbit", S::Getbit),
    scalar("greatest", T::WidenArguments, N::AllArguments, SE::Native),
    aggregate("grouping",          T::Byte,            N::Never,       AE::Native),
    aggregate("grouping_id",       T::Long,            N::Never,       AE::Native),
    scalar("hash",                 T::Integer,         N::Never,       SE::Extension(EF::Hash)),
    special("hex", S::Hex),
    aggregate("histogram_numeric", T::HistogramNumeric, N::Always, AE::Native),
    scalar("hour", T::Integer, N::AnyArgument, SE::Native),
    special("hypot", S::Hypot),
    scalar("if", T::SecondArgument, N::BranchArguments, SE::Native),
    scalar("ifnull", T::WidenArguments, N::AllArguments, SE::Rename(DF::Coalesce)),
    special("ilike", S::Ilike),
    scalar("initcap", T::String, N::AnyArgument, SE::Native),
    generator("inline",            GeneratorKind::Inline,             false),
    generator("inline_outer",      GeneratorKind::Inline,             true),
    scalar("input_file_block_length", T::String, N::AnyArgument, SE::Native),
    scalar("input_file_block_start", T::String, N::AnyArgument, SE::Native),
    scalar("input_file_name", T::String, N::AnyArgument, SE::Native),
    scalar("instr", T::Integer, N::AnyArgument, SE::Native),
    special("isnan", S::Isnan),
    special("isnotnull", S::Isnotnull),
    special("isnull", S::Isnull),
    special("json_object_keys", S::JsonObjectKeys),
    generator("json_tuple",        GeneratorKind::JsonTuple,          false),
    aggregate("kurtosis",          T::Double,          N::Always,      AE::Rename(DF::KurtosisPop)),
    special("lag", S::Lag),
    aggregate("last",              T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    scalar("last_day", T::Date, N::AnyArgument, SE::Native),
    aggregate("last_value",        T::FirstArgument,   N::Always,      AE::Special(AS::FirstLast)),
    special("lead", S::Lead),
    scalar("least", T::WidenArguments, N::AllArguments, SE::Native),
    scalar("left", T::String, N::AnyArgument, SE::Native),
    scalar("len", T::Integer, N::AnyArgument, SE::Native),
    scalar("length", T::Integer, N::AnyArgument, SE::Native),
    scalar("levenshtein", T::Integer, N::AnyArgument, SE::Native),
    special("like", S::Like),
    special("ln", S::Ln),
    special("locate", S::Locate),
    special("log", S::Log),
    special("log10", S::Log10),
    scalar("log1p", T::Double, N::Always, SE::Native),
    special("log2", S::Log2),
    scalar("lower", T::String, N::AnyArgument, SE::Native),
    scalar("lpad", T::String, N::AnyArgument, SE::Native),
    scalar("ltrim", T::String, N::AnyArgument, SE::Native),
    scalar("make_date", T::Date, N::AnyArgument, SE::Native),
    special("make_dt_interval", S::MakeDtInterval),
    special("make_interval", S::MakeInterval),
    scalar("make_timestamp", T::Timestamp, N::AnyArgument, SE::Native),
    special("make_ym_interval", S::MakeYmInterval),
    special("map", S::Map),
    special("map_concat", S::MapConcat),
    scalar("map_contains_key", T::Boolean, N::AnyArgument, SE::Rename(DF::MapContains)),
    scalar("map_entries", T::MapEntries, N::AnyArgument, SE::Native),
    special("map_filter", S::MapFilter),
    scalar("map_from_arrays", T::MapFromArrays, N::AnyArgument, SE::Native),
    scalar("map_from_entries", T::StringMap, N::Always, SE::Native),
    scalar("map_keys", T::MapKeys, N::AnyArgument, SE::Native),
    scalar("map_values", T::MapValues, N::AnyArgument, SE::Native),
    scalar("map_zip_with", T::FirstArgument, N::AnyArgument, SE::Native),
    aggregate("max",               T::FirstArgument,   N::Always,      AE::Native),
    aggregate("max_by",            T::FirstArgument,   N::Always,      AE::Rename(DF::ArgMaxNull)),
    scalar("md5", T::String, N::AnyArgument, SE::Native),
    aggregate("mean",              T::Average,         N::Always,      AE::Special(AS::Average)),
    aggregate("median",            T::Double,          N::Always,      AE::Native),
    aggregate("min",               T::FirstArgument,   N::Always,      AE::Native),
    aggregate("min_by",            T::FirstArgument,   N::Always,      AE::Rename(DF::ArgMinNull)),
    scalar("minute", T::Integer, N::AnyArgument, SE::Native),
    special("mod", S::Mod),
    aggregate("mode",              T::FirstArgument,   N::Always,      AE::Special(AS::Mode)),
    scalar("monotonically_increasing_id", T::Long, N::Never, SE::Native),
    scalar("month", T::Integer, N::AnyArgument, SE::Native),
    scalar("monthname", T::String, N::AnyArgument, SE::Native),
    special("months_between", S::MonthsBetween),
    special("named_struct", S::NamedStruct),
    special("nanvl", S::Nanvl),
    special("negative", S::Negative),
    scalar("next_day", T::Date, N::AnyArgument, SE::Native),
    special("not", S::Not),
    scalar("now", T::Timestamp, N::AnyArgument, SE::Native),
    special("nth_value", S::NthValue),
    scalar("ntile", T::Integer, N::Never, SE::Native),
    scalar("nullif", T::FirstArgument, N::AnyArgument, SE::Native),
    scalar("nvl", T::WidenArguments, N::AllArguments, SE::Rename(DF::Coalesce)),
    special("nvl2", S::Nvl2),
    scalar("octet_length", T::Integer, N::AnyArgument, SE::Native),
    special("overlay", S::Overlay),
    special("parse_url", S::ParseUrl),
    scalar("percent_rank", T::Double, N::Never, SE::Native),
    aggregate("percentile",        T::Double,          N::Always,      AE::Special(AS::Percentile)),
    aggregate("percentile_approx", T::Double,          N::Always,      AE::Special(AS::ApproxPercentile)),
    scalar("pi", T::Double, N::AnyArgument, SE::Native),
    special("pmod", S::Pmod),
    generator("posexplode",        GeneratorKind::PosExplode,         false),
    generator("posexplode_outer",  GeneratorKind::PosExplode,         true),
    scalar("position", T::Integer, N::AnyArgument, SE::Native),
    special("positive", S::Positive),
    scalar("pow", T::Double, N::AnyArgument, SE::Native),
    scalar("power", T::Double, N::AnyArgument, SE::Native),
    scalar("printf", T::String, N::Never, SE::Native),
    scalar("quarter", T::Integer, N::AnyArgument, SE::Native),
    scalar("radians", T::Double, N::Always, SE::Native),
    scalar("rand", T::Double, N::AnyArgument, SE::Native),
    scalar("randn", T::Double, N::AnyArgument, SE::Native),
    scalar("random", T::Double, N::AnyArgument, SE::Native),
    scalar("rank", T::Integer, N::Never, SE::Native),
    special("reduce", S::Reduce),
    special("regexp", S::Regexp),
    scalar("regexp_count", T::Integer, N::AnyArgument, SE::Native),
    scalar("regexp_extract", T::String, N::AnyArgument, SE::Native),
    scalar("regexp_extract_all", T::StringArray, N::AnyArgument, SE::Native),
    scalar("regexp_instr", T::Integer, N::AnyArgument, SE::Native),
    special("regexp_like", S::RegexpLike),
    special("regexp_replace", S::RegexpReplace),
    aggregate("regr_avgx",         T::Double,          N::Always,      AE::Native),
    aggregate("regr_avgy",         T::Double,          N::Always,      AE::Native),
    aggregate("regr_count",        T::Long,            N::Never,       AE::Native),
    aggregate("regr_intercept",    T::Double,          N::Always,      AE::Native),
    aggregate("regr_r2",           T::Double,          N::Always,      AE::Native),
    aggregate("regr_slope",        T::Double,          N::Always,      AE::Native),
    aggregate("regr_sxx",          T::Double,          N::Always,      AE::Native),
    aggregate("regr_sxy",          T::Double,          N::Always,      AE::Native),
    aggregate("regr_syy",          T::Double,          N::Always,      AE::Native),
    scalar("repeat", T::String, N::AnyArgument, SE::Native),
    scalar("replace", T::String, N::AnyArgument, SE::Native),
    special("reverse", S::Reverse),
    scalar("right", T::String, N::AnyArgument, SE::Native),
    scalar("rint", T::Double, N::Always, SE::Native),
    special("rlike", S::Rlike),
    special("round", S::Round),
    scalar("row_number", T::Integer, N::Never, SE::Native),
    scalar("rpad", T::String, N::AnyArgument, SE::Native),
    scalar("rtrim", T::String, N::AnyArgument, SE::Native),
    scalar("schema_of_json", T::String, N::AnyArgument, SE::Extension(EF::SchemaOfJson)),
    scalar("sec", T::Double, N::Always, SE::Native),
    scalar("second", T::Integer, N::AnyArgument, SE::Native),
    scalar("sentences", T::String, N::AnyArgument, SE::Native),
    scalar("sequence", T::Sequence, N::AnyArgument, SE::Native),
    scalar("sha", T::String, N::AnyArgument, SE::Rename(DF::Sha1)),
    scalar("sha1", T::String, N::AnyArgument, SE::Native),
    special("sha2", S::Sha2),
    special("shiftleft", S::Shiftleft),
    special("shiftright", S::Shiftright),
    scalar("shiftrightunsigned", T::FirstArgument, N::AnyArgument, SE::Native),
    scalar("shuffle", T::FirstArgument, N::AnyArgument, SE::Native),
    special("sign", S::Sign),
    special("signum", S::Signum),
    scalar("sin", T::Double, N::Always, SE::Native),
    scalar("sinh", T::Double, N::Always, SE::Native),
    special("size", S::Size),
    aggregate("skewness",          T::Double,          N::Always,      AE::Extension(EF::Skewness)),
    scalar("slice", T::FirstArgument, N::AnyArgument, SE::Rename(DF::ListSlice)),
    aggregate("some",              T::Boolean,         N::Always,      AE::Rename(DF::BoolOr)),
    special("sort_array", S::SortArray),
    scalar("soundex", T::String, N::AnyArgument, SE::Native),
    scalar("space", T::String, N::AnyArgument, SE::Native),
    scalar("spark_partition_id", T::Integer, N::Never, SE::Native),
    special("split", S::Split),
    scalar("split_part", T::String, N::AnyArgument, SE::Native),
    scalar("sqrt", T::Double, N::Always, SE::Native),
    generator("stack",             GeneratorKind::Stack,              false),
    scalar("starts_with", T::Boolean, N::AnyArgument, SE::Native),
    scalar("startswith", T::Boolean, N::AnyArgument, SE::Rename(DF::StartsWith)),
    aggregate("std",               T::Double,          N::Always,      AE::Rename(DF::Stddev)),
    aggregate("stddev",            T::Double,          N::Always,      AE::Native),
    aggregate("stddev_pop",        T::Double,          N::Always,      AE::Native),
    aggregate("stddev_samp",       T::Double,          N::Always,      AE::Native),
    scalar("str_to_map", T::StringMap, N::AnyArgument, SE::Native),
    special("struct", S::Struct),
    scalar("substr", T::String, N::AnyArgument, SE::Rename(DF::Substring)),
    scalar("substring", T::String, N::AnyArgument, SE::Native),
    special("substring_index", S::SubstringIndex),
    aggregate("sum",               T::Sum,             N::Always,      AE::Native),
    aggregate("sum_distinct",      T::Sum,             N::Always,      AE::Distinct(DF::Sum)),
    scalar("tan", T::Double, N::Always, SE::Native),
    scalar("tanh", T::Double, N::Always, SE::Native),
    special("timestampadd", S::Timestampadd),
    special("timestampdiff", S::Timestampdiff),
    special("to_char", S::ToChar),
    special("to_csv", S::ToCsv),
    special("to_date", S::ToDate),
    special("to_json", S::ToJson),
    special("to_number", S::ToNumber),
    special("to_timestamp", S::ToTimestamp),
    special("to_utc_timestamp", S::ToUtcTimestamp),
    special("transform", S::Transform),
    special("transform_keys", S::TransformKeys),
    special("transform_values", S::TransformValues),
    scalar("translate", T::String, N::AnyArgument, SE::Native),
    scalar("trim", T::String, N::AnyArgument, SE::Native),
    special("trunc", S::Trunc),
    scalar("try_add", T::WidenArguments, N::Always, SE::Native),
    aggregate("try_avg",           T::Average,         N::Always,      AE::Extension(EF::TryAverage)),
    special("try_divide", S::TryDivide),
    special("try_element_at", S::TryElementAt),
    special("try_make_interval", S::TryMakeInterval),
    scalar("try_multiply", T::WidenArguments, N::Always, SE::Native),
    scalar("try_subtract", T::WidenArguments, N::Always, SE::Native),
    aggregate("try_sum",           T::Sum,             N::Always,      AE::Extension(EF::TrySum)),
    special("try_to_number", S::TryToNumber),
    special("typeof", S::Typeof),
    scalar("unbase64", T::String, N::AnyArgument, SE::Native),
    scalar("unhex", T::Binary, N::AnyArgument, SE::Native),
    scalar("unix_micros", T::Long, N::AnyArgument, SE::Native),
    scalar("unix_millis", T::Long, N::AnyArgument, SE::Native),
    scalar("unix_seconds", T::Long, N::AnyArgument, SE::Native),
    special("unix_timestamp", S::UnixTimestamp),
    scalar("upper", T::String, N::AnyArgument, SE::Native),
    special("url_decode", S::UrlDecode),
    special("url_encode", S::UrlEncode),
    aggregate("var_pop",           T::Double,          N::Always,      AE::Native),
    aggregate("var_samp",          T::Double,          N::Always,      AE::Native),
    aggregate("variance",          T::Double,          N::Always,      AE::Native),
    scalar("week", T::Integer, N::AnyArgument, SE::Native),
    scalar("weekofyear", T::Integer, N::AnyArgument, SE::Native),
    lowered("when", L::When),
    special("window", S::Window),
    scalar("xxhash64", T::Long, N::Never, SE::Extension(EF::XxHash64)),
    scalar("year", T::Integer, N::AnyArgument, SE::Native),
    special("zip_with", S::ZipWith),
    special("|", S::BitwiseOr),
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
        FunctionImplementation::Aggregate(_)
        | FunctionImplementation::Generator(_)
        | FunctionImplementation::Lowered(_)
        | FunctionImplementation::Special(_) => None,
    }
}

pub(crate) fn aggregate_spec(name_lower: &str) -> Option<&'static AggregateSpec> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Aggregate(ref spec) => Some(spec),
        FunctionImplementation::Generator(_)
        | FunctionImplementation::Lowered(_)
        | FunctionImplementation::Scalar(_)
        | FunctionImplementation::Special(_) => None,
    }
}

pub(crate) fn generator_spec(name_lower: &str) -> Option<&'static GeneratorSpec> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Generator(ref spec) => Some(spec),
        FunctionImplementation::Aggregate(_)
        | FunctionImplementation::Lowered(_)
        | FunctionImplementation::Scalar(_)
        | FunctionImplementation::Special(_) => None,
    }
}

pub(crate) fn special_function(name_lower: &str) -> Option<SpecialFunction> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Special(function) => Some(function),
        FunctionImplementation::Aggregate(_)
        | FunctionImplementation::Generator(_)
        | FunctionImplementation::Lowered(_)
        | FunctionImplementation::Scalar(_) => None,
    }
}

pub(crate) fn lowered_function(name_lower: &str) -> Option<LoweredFunction> {
    match lookup(name_lower)?.implementation {
        FunctionImplementation::Lowered(function) => Some(function),
        FunctionImplementation::Aggregate(_)
        | FunctionImplementation::Generator(_)
        | FunctionImplementation::Scalar(_)
        | FunctionImplementation::Special(_) => None,
    }
}

pub(crate) fn generator_name(kind: GeneratorKind, outer: bool) -> Option<&'static str> {
    let matching = |spec: &&FunctionSpec| match spec.implementation {
        FunctionImplementation::Generator(generator) => generator.kind == kind,
        FunctionImplementation::Aggregate(_)
        | FunctionImplementation::Lowered(_)
        | FunctionImplementation::Scalar(_)
        | FunctionImplementation::Special(_) => false,
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
        assert!(is_aggregate("histogram_numeric"));
        assert!(scalar_spec("size").is_none());
        assert_eq!(generator_spec("explode_outer").map(|s| s.outer), Some(true));
        assert_eq!(
            generator_name(GeneratorKind::JsonTuple, true),
            Some("json_tuple")
        );
    }

    #[test]
    fn implementation_routes_are_closed_and_distinct() {
        assert!(matches!(
            lookup("abs").map(|spec| spec.implementation),
            Some(FunctionImplementation::Scalar(_))
        ));
        assert!(matches!(
            lookup("sum").map(|spec| spec.implementation),
            Some(FunctionImplementation::Aggregate(_))
        ));
        assert!(matches!(
            lookup("explode").map(|spec| spec.implementation),
            Some(FunctionImplementation::Generator(_))
        ));
        assert!(matches!(
            lookup("substring_index").map(|spec| spec.implementation),
            Some(FunctionImplementation::Special(_))
        ));
        assert!(matches!(
            lookup("cast").map(|spec| spec.implementation),
            Some(FunctionImplementation::Lowered(_))
        ));

        for name in [
            "cume_dist",
            "dense_rank",
            "ntile",
            "percent_rank",
            "rank",
            "row_number",
        ] {
            assert!(matches!(
                lookup(name).map(|spec| spec.implementation),
                Some(FunctionImplementation::Scalar(_))
            ));
        }
        for name in [
            "&",
            "<=>",
            "^",
            "bitwise_and",
            "bitwise_or",
            "bitwise_xor",
            "bitwiseand",
            "bitwiseor",
            "bitwisexor",
            "eqnullsafe",
            "|",
        ] {
            assert!(matches!(
                lookup(name).map(|spec| spec.implementation),
                Some(FunctionImplementation::Special(_))
            ));
        }
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
