//! τ's `CommonAst` — the substrate-independent plan tree shared by every
//! front-end (Spark Connect proto, SparkSQL, future front-ends).
//!
//! **INV10:** this file imports ONLY value-level types from `crate::types`
//! (`StructType`) plus intra-τ modules. No `crate::logical`, `crate::parser`,
//! `crate::generator`, `crate::functions`, `crate::runtime`,
//! `crate::types::TypeInferenceEngine`.
//!
//! The wrapper `CommonAst { op: CommonOp }` exists so τ's analyzer can attach
//! resolution metadata (resolved schema, plan_id, etc.) without a source-wide
//! refactor. τ keeps the wrapper minimal.

use super::expression::{Expression, Literal, SortOrder};
use crate::types::StructType;

/// τ's canonical plan tree — a single wrapper around a [`CommonOp`] variant.
///
/// τ's analyzer extends this wrapper (e.g. `pub resolved_schema: Option<StructType>`).
/// τ keeps it as a thin wrapper so the extension is additive.
#[derive(Debug, Clone, PartialEq)]
pub struct CommonAst {
    /// The plan operator this node represents.
    pub op: CommonOp,
}

impl CommonAst {
    /// Construct a `CommonAst` wrapping the given [`CommonOp`].
    pub fn new(op: CommonOp) -> Self {
        Self { op }
    }
}

/// The canonical plan operator set shared by every τ front-end.
///
/// Every variant below is analyzed and emitted end-to-end (relational core,
/// Aggregate incl. Rollup/Cube/GroupingSets, Join, SetOp, WithColumns, NA
/// family, Unpivot, Pivot / Crosstab, Stat family, TableFunction, ...) with
/// one exception: `Unnest`, whose emission arm is still a
/// Thunderduck-boundary [`super::EmissionError::Unsupported`] per ADR-022.
/// Plan shapes with no variant here surface as
/// [`super::EmissionError::Unsupported`] (`kind: ProtoShape`) from the
/// front-ends. There is **no** opaque `Sql` variant — parser_v2 owns SQL
/// text (Open Decision 1 Option 1b).
#[derive(Debug, Clone, PartialEq)]
pub enum CommonOp {
    // ── Relational (structured) ──────────────────────────────────────────
    /// `SELECT projections FROM input`.
    Project {
        /// The input relation.
        input: Box<CommonAst>,
        /// The projection list — a sequence of expressions (with optional
        /// aliases) evaluated against the input schema.
        projections: Vec<Expression>,
    },

    /// `SELECT * FROM input WHERE condition`.
    Filter {
        /// The input relation.
        input: Box<CommonAst>,
        /// The predicate — a Boolean-valued expression.
        condition: Expression,
    },

    /// `SELECT * FROM input ORDER BY order [LIMIT limit] [OFFSET offset]`.
    ///
    /// The `Sort` variant carries optional `limit` / `offset` so an
    /// `OFFSET` alone (without a preceding `Sort` in the source plan) still
    /// lowers to a single node.
    Sort {
        /// The input relation.
        input: Box<CommonAst>,
        /// The sort keys (may be empty when representing a bare OFFSET).
        order: Vec<SortOrder>,
        /// Optional LIMIT — evaluated after sorting.
        limit: Option<i64>,
        /// Optional OFFSET — evaluated after sorting (and after LIMIT).
        offset: Option<i64>,
    },

    /// `SELECT * FROM input LIMIT limit [OFFSET offset]`.
    Limit {
        /// The input relation.
        input: Box<CommonAst>,
        /// The maximum number of rows to return.
        limit: i64,
        /// Optional OFFSET.
        offset: Option<i64>,
    },

    /// `SELECT aggregates FROM input GROUP BY grouping`.
    ///
    /// Covers plain GROUP BY plus Rollup / Cube / GroupingSets via
    /// `grouping_kind` / `grouping_sets`. Pivot is a separate variant
    /// ([`CommonOp::Pivot`]).
    ///
    /// # N7 — `aggregates` IS the complete output list, by construction
    ///
    /// `aggregates` always carries the full output projection: for the
    /// SparkSQL front-end (`lower_aggregate_select`) it's the whole SELECT
    /// list, grouping columns and aggregate calls alike; for the DataFrame
    /// front-end it's `grouping ++ agg_exprs`, matching Spark's
    /// `RelationalGroupedDataset.toDF` layout (verified empirically:
    /// `df.groupBy("k").agg(F.col("k"), F.sum("v"))` yields schema `[k, k,
    /// sum(v)]` — the grouping key restated, not deduplicated). Every
    /// construction site builds this list itself; there is no per-front-end
    /// flag and no fold-at-read-time — see [`grouped_aggregate`] for the
    /// DataFrame-shaped constructor.
    Aggregate {
        /// The input relation.
        input: Box<CommonAst>,
        /// The grouping expressions (may be empty for global aggregation).
        grouping: Vec<Expression>,
        /// The complete output list — see variant-level doc (N7).
        aggregates: Vec<Expression>,
        /// The grouping kind — GroupBy (default), Rollup, Cube, or
        /// GroupingSets (Pivot lives elsewhere).
        grouping_kind: GroupingKind,
        /// Per-set column membership for [`GroupingKind::GroupingSets`] —
        /// indices into the flat `grouping` list (first-appearance order). One
        /// inner vec per set; an empty inner vec is the empty set `()` (grand
        /// total). EMPTY for all other kinds and on the DataFrame
        /// `groupingSets` path (which stays a boundary error, ADR-022).
        grouping_sets: Vec<Vec<usize>>,
        /// SparkSQL `HAVING <pred>` — post-aggregation group filter. `None` for
        /// the DataFrame path (models post-agg filtering as a separate Filter
        /// over the Aggregate). Resolved against the aggregate INPUT schema
        /// (aggregate exprs + grouping keys bind to input columns). Does NOT
        /// resolve SELECT output-alias refs (`HAVING s > x`) — no corpus
        /// witness; future merged-scope enhancement.
        having: Option<Expression>,
    },

    // ── Leaves ───────────────────────────────────────────────────────────
    /// A relation with exactly one row and zero columns — the identity for
    /// `SELECT literal` with no `FROM`.
    SingleRow,

    /// A named table scan (`SELECT * FROM table`).
    TableScan {
        /// The table name (as written by the caller).
        table: String,
        /// Optional alias (e.g. `FROM t AS x`).
        alias: Option<String>,
    },

    /// An in-line `VALUES (...) AS t(col_names)` relation.
    Values {
        /// One expression list per row.
        rows: Vec<Vec<Expression>>,
        /// The column names for the resulting relation.
        column_names: Vec<String>,
    },

    /// A `createDataFrame` payload — Arrow-IPC rows parsed into
    /// `Expression::Literal` values. **No synthesized SQL** — the emission
    /// arm is responsible for rendering (per §2.1 anti-SQL anchor).
    LocalRelation {
        /// The declared schema (may include columns not present in `rows`).
        schema: StructType,
        /// One expression list per row (each element must be an
        /// `Expression::Literal`).
        rows: Vec<Vec<Expression>>,
    },

    /// A file-format scan (`spark.read.parquet(...)`, etc.).
    FileScan {
        /// The file format.
        format: FileFormat,
        /// One or more file paths / globs.
        paths: Vec<String>,
        /// Optional declared schema.
        schema: Option<StructType>,
        /// Format-specific options (e.g. `header=true`).
        options: Vec<(String, String)>,
    },

    /// A table-valued function call (`spark.table("...")` alternatives).
    TableFunction {
        /// The function name.
        name: String,
        /// The function arguments.
        args: Vec<Expression>,
        /// Whether to emit an ordinality column (`WITH ORDINALITY`).
        with_ordinality: bool,
    },

    /// `UNNEST(expr) [WITH ORDINALITY]`.
    Unnest {
        /// The array/map expression being unnested.
        expr: Expression,
        /// Whether to emit an ordinality column.
        with_ordinality: bool,
    },

    // ── Set operations ─────────────────────────────────────────
    /// A n-ary set operation: UNION / INTERSECT / EXCEPT.
    ///
    /// **τ's analyzer adds this variant.** Set-op widening (analyzer's downward
    /// sub-sweep, per rearchitect ADR-006) runs across `children` to compute
    /// the widened schema; the resolved schema is stamped by the analyzer.
    /// `UNION BY NAME` (`by_name = true`) is implemented (name-matched
    /// widening, optional `allow_missing_columns`); by-name INTERSECT /
    /// EXCEPT are unsupported by DuckDB itself and still surface as
    /// `AnalyzerError::PuntedOperator`.
    SetOp {
        /// The kind of set operation.
        kind: SetOpKind,
        /// Whether duplicates are preserved (`UNION ALL`) or removed
        /// (`UNION DISTINCT`).
        all: bool,
        /// Whether column matching is by-name (`UNION BY NAME`) or by-position.
        by_name: bool,
        /// Spark's `allowMissingColumns` flag on `unionByName`. Meaningful
        /// only when `by_name = true` and `kind = Union` (proto contract;
        /// PySpark's `DataFrame.unionByName` always ships `by_name = true`
        /// alongside). `false` for every other set-op form.
        allow_missing_columns: bool,
        /// The n-ary children of the set operation.
        children: Vec<CommonAst>,
    },

    /// `df.na.fill(values, subset=cols)`. `values` has length 1 (fill all
    /// subset cols with that scalar) or length == cols.len() (per-column).
    NaFill {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of column names (empty = all columns of matching type).
        cols: Vec<String>,
        /// Fill values (Literal expressions).
        values: Vec<Expression>,
    },

    /// `df.na.drop(how, subset=cols, thresh)`. Row is dropped if fewer than
    /// `min_non_nulls` of the subset cols are non-null. When `min_non_nulls`
    /// is None, all subset cols must be non-null (how="any").
    NaDrop {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of columns to inspect.
        cols: Vec<String>,
        /// Optional minimum non-null count.
        min_non_nulls: Option<i32>,
    },

    /// `df.na.replace([old], [new], subset=cols)`.
    NaReplace {
        /// The input relation.
        input: Box<CommonAst>,
        /// Subset of column names.
        cols: Vec<String>,
        /// (old, new) pairs.
        replacements: Vec<(Expression, Expression)>,
    },

    /// `df.unpivot(ids, values, variable_column_name, value_column_name)` /
    /// `df.melt(...)` (PySpark alias). Wide → long transformation.
    ///
    /// **Semantics:** produces `<ids>` columns unchanged plus two new columns:
    /// `variable_column_name` (STRING NOT NULL, the source column name) and
    /// `value_column_name` (Spark-widened common type across all `values`
    /// columns, nullable if any source value column is nullable).
    ///
    /// `ids` and `values` are column *names*. When `values` is empty, Spark
    /// defaults to "all non-id columns"; the analyzer materialises that
    /// expansion so the emission stage sees a fully-resolved column list.
    Unpivot {
        /// The input relation.
        input: Box<CommonAst>,
        /// Id columns — preserved verbatim in the output. Either supplied
        /// explicitly (DataFrame path) or derived from the input schema by
        /// the analyzer (SQL `UNPIVOT`, which lists only value columns).
        ids: UnpivotIds,
        /// Value columns to unpivot. Empty ⇒ analyzer expands to all
        /// non-id input columns per Spark semantics.
        values: Vec<String>,
        /// The name of the output variable column (source-column names).
        variable_column_name: String,
        /// The name of the output value column (source-column values).
        value_column_name: String,
    },

    /// `df.groupBy(...).pivot(col, [values]).agg(...)` — rotate rows to
    /// columns. Grouping columns remain as rows; the pivot column's distinct
    /// values (either supplied explicitly or discovered eagerly at runtime by
    /// DuckDB's PIVOT operator when `pivot_values` is empty) become new column
    /// headers, one per aggregate per pivot value.
    ///
    /// **Semantics:** the output schema is `<grouping> + <pivot_value_i × agg_j>`.
    /// When multiple aggregates are supplied, output column names follow
    /// Spark's `<pivot_value>_<agg_alias>` convention; when a single aggregate
    /// is supplied, output columns are named after the pivot values verbatim.
    /// Empty `pivot_values` (implicit / "eager discovery" per Spark) is
    /// resolved by the connect-server pre-pass (`resolve_implicit_pivots` in
    /// `service.rs` runs the eager `SELECT DISTINCT` against the live session
    /// and rewrites this node to the explicit-values shape *before*
    /// `analyze`); the analyzer itself — a pure stage with no session hook
    /// (INV10) — still rejects a residual empty list with
    /// `PuntedOperator("Pivot[implicit-values]")`.
    Pivot {
        /// The input relation.
        input: Box<CommonAst>,
        /// Grouping columns (preserved as rows). Either supplied explicitly
        /// (DataFrame `groupBy(...).pivot(...)`) or derived from the input
        /// schema by the analyzer (SQL `PIVOT`, which lists no grouping).
        grouping: PivotGrouping,
        /// The column whose distinct values become new column headers.
        pivot_column: Expression,
        /// Explicit list of pivot values (Literal expressions). Empty ⇒
        /// DuckDB eagerly discovers values at PIVOT execution.
        pivot_values: Vec<Expression>,
        /// One or more aggregate expressions applied per pivot cell.
        aggregates: Vec<Expression>,
    },

    /// `df.describe(col1, col2, ...)` — Spark's `describe` StatFunction. Emits
    /// a summary relation with a `summary` STRING NOT NULL column plus one
    /// STRING NULLABLE column per input `cols` entry. Empty `cols` ⇒ analyzer
    /// expands to all input columns in schema order (Spark default).
    Describe {
        /// The input relation.
        input: Box<CommonAst>,
        /// Explicit column names to describe. Empty ⇒ analyzer expands to
        /// all input columns per Spark semantics.
        cols: Vec<String>,
    },

    /// `df.summary(stat1, stat2, ...)` — Spark's `summary` StatFunction. Emits
    /// a summary relation with a `summary` STRING NOT NULL column plus one
    /// STRING NULLABLE column per input column (analyzer always expands
    /// because proto `StatSummary` has no `cols` field). Empty `statistics`
    /// ⇒ analyzer applies the eight Spark defaults (`count`, `mean`,
    /// `stddev`, `min`, `25%`, `50%`, `75%`, `max`).
    Summary {
        /// The input relation.
        input: Box<CommonAst>,
        /// Statistics to compute. Empty ⇒ analyzer applies the
        /// `DEFAULT_SUMMARY_STATS` list.
        statistics: Vec<String>,
    },

    /// `df.stat.freqItems(cols, support)` — Spark's `StatFunctions.freqItems`.
    /// Emits one `ARRAY<T>` output column per input col, named
    /// `{col}_freqItems`, where `T` matches the source column's declared
    /// [`crate::types::DataType`] (Spark parity per ADR-015).
    FreqItems {
        /// The input relation.
        input: Box<CommonAst>,
        /// The column names to search frequent items in.
        cols: Vec<String>,
        /// The minimum item frequency (`0.01` by default per PySpark client).
        /// `HAVING COUNT(*) >= support * total_rows` at emission.
        support: f64,
    },

    /// `df.stat.crosstab(col1, col2)` — Spark's `StatFunctions.crossTabulate`.
    /// The output column list is `DISTINCT(col2)` — unknowable at plan time —
    /// so this rides the same connect-server pre-pass as implicit Pivot:
    /// `resolve_implicit_pivots` (`service.rs`) discovers `col2`'s distinct
    /// buckets from the live session and
    /// `analyzer::crosstab_to_aggregate` desugars this node into a
    /// conditional-count `Aggregate` *before* `analyze`. The analyzer itself
    /// — a pure stage with no session hook (INV10) — still rejects a
    /// residual `Crosstab` node with
    /// `PuntedOperator("Crosstab[dynamic-values]")`.
    Crosstab {
        /// The input relation.
        input: Box<CommonAst>,
        /// The first column — distinct values become row keys.
        col1: String,
        /// The second column — distinct values become column headers.
        col2: String,
    },

    /// `df.dropDuplicates([cols])` / `df.distinct()`. `on_columns` empty ⇒
    /// dedupe on the full row (`SELECT DISTINCT *`). Non-empty ⇒
    /// `SELECT DISTINCT ON (cols) * FROM ...`.
    Deduplicate {
        /// The input relation.
        input: Box<CommonAst>,
        /// Optional subset of columns to dedupe on. Empty = all columns.
        on_columns: Vec<String>,
    },

    /// `df.alias(name)` — wrap the input in a named subquery alias.
    /// Semantically transparent for schemas (analyzer passes input schema
    /// through); the alias name is retained for scope-resolution in
    /// downstream operators.
    AliasedRelation {
        /// The input relation.
        input: Box<CommonAst>,
        /// The alias to apply.
        alias: String,
    },

    /// `df.toDF(new1, new2, ...)` — positional rename all columns.
    /// Analyzer must have input schema to zip positional names.
    ToDf {
        /// The input relation.
        input: Box<CommonAst>,
        /// The new positional names.
        column_names: Vec<String>,
    },

    /// `df.withColumnsRenamed({old: new, ...})` — rename columns.
    /// Column identity + types + nullability are preserved.
    WithColumnsRenamed {
        /// The input relation.
        input: Box<CommonAst>,
        /// Old-name → new-name renames (order preserved).
        renames: Vec<(String, String)>,
    },

    // ── Column-list extensions ───────────────────────────────────────────
    /// `df.drop(col1, col2, ...)` — remove named columns from the input.
    /// Analyzer produces an output schema equal to input minus the named
    /// columns (case-insensitive per Spark). Missing names are silently
    /// ignored per Spark semantics.
    DropColumns {
        /// The input relation.
        input: Box<CommonAst>,
        /// Column names to remove.
        drop_names: Vec<String>,
    },

    /// `df.withColumn(name, expr)` / `df.withColumns({...})` — add or replace
    /// per-name column assignments over `input`. Semantics: for each
    /// `(name, expr)`, if `name` matches an existing input column
    /// (case-insensitive per Spark), the column is *replaced*; otherwise the
    /// column is *appended*. Duplicate names within `assignments` are a
    /// Spark-emulated error surfaced by the analyzer.
    WithColumns {
        /// The input relation.
        input: Box<CommonAst>,
        /// One `(column_name, expression)` per proto `Alias`. Order matters:
        /// later assignments referencing an earlier assignment's name see the
        /// pre-assignment value (analyzer resolves against input schema, not
        /// intermediate state — matches Spark's `withColumn` semantics).
        assignments: Vec<(String, Expression)>,
    },

    // ── Sampling (Pass 83) ───────────────────────────────────────────────
    /// `df.sample(fraction, seed)` / `df.sample(withReplacement, fraction, seed)`.
    /// Schema-preserving.
    ///
    /// Mirrors proto `Sample` 1:1: `lower_bound` / `upper_bound` (the Spark
    /// client converts `fraction` to `[0.0, fraction]`), plus `with_replacement`
    /// and `seed`. Proto `deterministic_order` is a physical-execution hint
    /// dropped at conversion.
    ///
    /// `with_replacement = true` is a permanent Thunderduck-boundary case per
    /// ADR-022 — DuckDB has no row-level sampling with replacement. Emission
    /// surfaces `EmissionError::Unsupported` with `kind: Op`.
    Sample {
        /// The input relation.
        input: Box<CommonAst>,
        /// The inclusive lower bound of the sampling range.
        lower_bound: f64,
        /// The exclusive upper bound of the sampling range.
        upper_bound: f64,
        /// Whether rows may be sampled with replacement.
        with_replacement: bool,
        /// Optional RNG seed for deterministic sampling.
        seed: Option<i64>,
    },

    /// `df.sampleBy(col, fractions, seed)` — stratified sampling.
    /// Schema-preserving.
    ///
    /// `col` is the stratum column (resolved by the analyzer against the
    /// input schema). `fractions` is a list of `(stratum literal, fraction)`
    /// pairs — the fraction of rows to keep per stratum value. Strata missing
    /// from `fractions` are dropped entirely (matches Spark: unspecified
    /// fractions are treated as zero).
    SampleBy {
        /// The input relation.
        input: Box<CommonAst>,
        /// The stratum column expression.
        col: Expression,
        /// Per-stratum `(literal, fraction)` pairs.
        fractions: Vec<(Literal, f64)>,
        /// Optional RNG seed.
        seed: Option<i64>,
    },

    /// `LATERAL VIEW [OUTER] generator(arg) table_alias AS col1[, col2, ...]`.
    ///
    /// Spark models this as a correlated unary operator: the generator
    /// expression references columns from the input relation (`e.tags`), and
    /// its output columns are appended (not replaced) to the input schema
    /// under the specified `table_alias`. Lowering folds the `OUTER` flag
    /// into the canonical generator name (`explode_outer`) and splits
    /// `posexplode` into `posexplode_pos`/`posexplode_val` — so `columns`
    /// always carries fully-canonical `FunctionCall` expressions.
    ///
    /// ADR-003 pre-authorizes this variant; composition of existing nodes
    /// fails concretely (see architecture-pass-11 justification).
    LateralView {
        /// The correlated input relation.
        input: Box<CommonAst>,
        /// The table alias (e.g. `t` in `LATERAL VIEW explode(e.tags) t AS tag`).
        table_alias: String,
        /// Per-output-column `(alias, generator FunctionCall)` pairs. Non-empty
        /// invariant; all co-project in a single inner SELECT at emission.
        columns: Vec<(String, Expression)>,
    },

    /// `WITH RECURSIVE name(cols) AS (anchor UNION ALL recursive_term)
    /// SELECT * FROM name` — a recursive CTE whose fixpoint is executed by
    /// DuckDB. The self-reference inside `recursive_term` is an ordinary
    /// `TableScan { table: name }` (not inlined — infinite expansion).
    ///
    /// `union_all = false` is carried from the parser but is an illegal state
    /// for Spark (`UNION_NOT_SUPPORTED_IN_RECURSIVE_CTE`); the analyzer rejects
    /// it before constructing `TypedOp::RecursiveCte` (which drops the field).
    RecursiveCte {
        /// The CTE name (used in the emitted `WITH RECURSIVE` clause and
        /// matches self-reference `TableScan` nodes in the recursive term).
        name: String,
        /// Explicit column-name list from `name(c1, c2, ...)`; empty = inherit
        /// column names from the anchor's output schema.
        column_names: Vec<String>,
        /// `true` for `UNION ALL` (the only legal Spark form); `false` for
        /// bare `UNION` (analyzer-rejected as Spark-emulated error).
        union_all: bool,
        /// The anchor leg — must not self-reference.
        anchor: Box<CommonAst>,
        /// The recursive leg — self-references are `TableScan { table: name }`.
        recursive_term: Box<CommonAst>,
    },

    // ── Join with first-class plan_ids (§2.3) ────────────────────────────
    /// A binary join.
    ///
    /// `left_plan_ids` / `right_plan_ids` carry the set of proto `plan_id`s
    /// that appear on each side of the join. τ's analyzer's analyzer uses these
    /// to disambiguate column references on either side without string
    /// qualifier encoding.
    Join {
        /// The left relation.
        left: Box<CommonAst>,
        /// The right relation.
        right: Box<CommonAst>,
        /// The join type.
        join_type: JoinType,
        /// Optional `ON` condition.
        condition: Option<Expression>,
        /// USING column names (empty when `ON` is used).
        using_columns: Vec<String>,
        /// Whether this is a SQL `NATURAL JOIN`. Invariant: `natural` implies
        /// `condition.is_none() && using_columns.is_empty()` — NATURAL carries
        /// no explicit constraint of its own; the analyzer desugars it into
        /// `using_columns` (or a `Cross`/`TRUE`-condition rewrite when the two
        /// sides share no column names) before it reaches `TypedOp::Join`.
        natural: bool,
        /// Whether the right side is a `LATERAL` derived table (correlated
        /// subquery at the join level). Invariant (analyzer-enforced):
        /// `lateral` implies `!natural && using_columns.is_empty()`.
        /// Orthogonal to `condition` — a lateral join may carry an ON clause
        /// (`JOIN LATERAL (...) t ON cond`).
        lateral: bool,
        /// Plan-ids appearing anywhere under the left side.
        left_plan_ids: Vec<i64>,
        /// Plan-ids appearing anywhere under the right side.
        right_plan_ids: Vec<i64>,
    },
}

/// Child-plan classification shared by [`CommonOp::children`] and
/// [`CommonOp::children_mut`] — ONE exhaustive match (no `_` arm), so adding a
/// `CommonOp` variant fails to compile here until it is classified as a
/// unary-input, Join, SetOp, or leaf operator.
macro_rules! common_op_children {
    ($op:expr, $as_child:ident, $iter:ident) => {
        match $op {
            CommonOp::Project { input, .. }
            | CommonOp::Filter { input, .. }
            | CommonOp::Sort { input, .. }
            | CommonOp::Limit { input, .. }
            | CommonOp::Aggregate { input, .. }
            | CommonOp::WithColumns { input, .. }
            | CommonOp::DropColumns { input, .. }
            | CommonOp::AliasedRelation { input, .. }
            | CommonOp::WithColumnsRenamed { input, .. }
            | CommonOp::ToDf { input, .. }
            | CommonOp::Deduplicate { input, .. }
            | CommonOp::NaFill { input, .. }
            | CommonOp::NaDrop { input, .. }
            | CommonOp::NaReplace { input, .. }
            | CommonOp::Unpivot { input, .. }
            | CommonOp::Pivot { input, .. }
            | CommonOp::Describe { input, .. }
            | CommonOp::Summary { input, .. }
            | CommonOp::FreqItems { input, .. }
            | CommonOp::Crosstab { input, .. }
            | CommonOp::Sample { input, .. }
            | CommonOp::SampleBy { input, .. }
            | CommonOp::LateralView { input, .. } => vec![input.$as_child()],
            CommonOp::RecursiveCte {
                anchor,
                recursive_term,
                ..
            } => vec![anchor.$as_child(), recursive_term.$as_child()],
            CommonOp::Join { left, right, .. } => vec![left.$as_child(), right.$as_child()],
            CommonOp::SetOp { children, .. } => children.$iter().collect(),
            // Leaves: no child *plan* to descend into. `TableFunction` /
            // `Unnest` payloads are expressions, not plans.
            CommonOp::SingleRow
            | CommonOp::TableScan { .. }
            | CommonOp::Values { .. }
            | CommonOp::LocalRelation { .. }
            | CommonOp::FileScan { .. }
            | CommonOp::TableFunction { .. }
            | CommonOp::Unnest { .. } => Vec::new(),
        }
    };
}

impl CommonOp {
    /// The direct child plan nodes of this operator, in tree order: the unary
    /// `input` first, `Join` left then right, `SetOp` children in declared
    /// order. Leaves return an empty vec.
    pub fn children(&self) -> Vec<&CommonAst> {
        common_op_children!(self, as_ref, iter)
    }

    /// Mutable variant of [`Self::children`] — same order, same coverage.
    pub fn children_mut(&mut self) -> Vec<&mut CommonAst> {
        common_op_children!(self, as_mut, iter_mut)
    }
}

/// Where a [`CommonOp::Pivot`]'s grouping (preserved-as-rows) columns come from.
///
/// `Explicit(vec![])` (a legitimately empty DataFrame grouping) is distinct
/// from `Implicit` (derive from the input schema), which a bare `Vec` /
/// `is_empty()` check cannot express.
#[derive(Debug, Clone, PartialEq)]
pub enum PivotGrouping {
    /// Supplied explicitly (DataFrame `groupBy(...).pivot(...)`). May be empty.
    Explicit(Vec<Expression>),
    /// SQL `PIVOT` supplies no grouping list; the analyzer derives it as
    /// `input schema − pivot column − aggregate-referenced columns`.
    Implicit,
}

/// Where a [`CommonOp::Unpivot`]'s id (preserved) columns come from.
///
/// As with [`PivotGrouping`], an explicitly empty id list (DataFrame
/// `df.unpivot(ids, None)`) is distinct from an implicit one (SQL `UNPIVOT`,
/// which lists only value columns).
#[derive(Debug, Clone, PartialEq)]
pub enum UnpivotIds {
    /// Supplied explicitly (DataFrame `df.unpivot(ids, values, ...)`). May be
    /// empty.
    Explicit(Vec<String>),
    /// SQL `UNPIVOT` supplies no id list; the analyzer derives it as
    /// `input schema − value columns`. Requires `values` non-empty.
    Implicit,
}

/// The file formats supported by [`CommonOp::FileScan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FileFormat {
    /// Apache Parquet.
    Parquet,
    /// CSV / delimited text.
    Csv,
    /// JSON / JSON-lines.
    Json,
    /// Apache ORC.
    Orc,
    /// Delta Lake (backed by `delta_scan`).
    Delta,
}

/// The set-operation kind carried by [`CommonOp::SetOp`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SetOpKind {
    /// `UNION` / `UNION ALL` — vertical concatenation.
    Union,
    /// `INTERSECT` — row intersection.
    Intersect,
    /// `EXCEPT` (`MINUS`) — row set difference.
    Except,
}

/// GROUP BY variants for [`CommonOp::Aggregate`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GroupingKind {
    /// Standard `GROUP BY cols`.
    GroupBy,
    /// `GROUP BY ROLLUP(cols)`.
    Rollup,
    /// `GROUP BY CUBE(cols)`.
    Cube,
    /// `GROUP BY GROUPING SETS((cols), (cols), ...)`. The `grouping` list
    /// carries all distinct cols in first-appearance order; the per-set
    /// membership lives in the sibling `grouping_sets` field on
    /// [`CommonOp::Aggregate`]/`TypedOp::Aggregate` (indices into `grouping`)
    /// and is applied at emission time. The SparkSQL front-end populates it;
    /// the DataFrame path leaves it empty and stays a boundary error.
    GroupingSets,
}

/// N7: construct a DataFrame-shaped [`CommonOp::Aggregate`] with Spark's
/// `RelationalGroupedDataset.toDF` layout — the output list is `grouping ++
/// agg_exprs`, built here so `aggregates` IS the complete output list by
/// construction (no per-front-end flag, no fold-at-read-time).
///
/// Empirically verified against Spark 4.1.1: `df.groupBy("dept_id").agg(F.col("dept_id"),
/// F.sum("salary"))` yields schema `[dept_id, dept_id, sum(salary)]` — the
/// grouping key is restated, not deduplicated, when the caller re-selects it
/// inside `.agg(...)`.
///
/// Every DataFrame construction site (`v2_relation_converter::convert_aggregate`
/// / `convert_cov` / `convert_corr` / `convert_approx_quantile`, and
/// `analyzer::crosstab_to_aggregate`) routes through this constructor —
/// `convert_cov`/`convert_corr`/`convert_approx_quantile`/`crosstab_to_aggregate`
/// pass `grouping: vec![]`, an identity fold.
pub fn grouped_aggregate(
    input: CommonAst,
    grouping: Vec<Expression>,
    agg_exprs: Vec<Expression>,
    grouping_kind: GroupingKind,
) -> CommonOp {
    let mut aggregates = grouping.clone();
    aggregates.extend(agg_exprs);
    CommonOp::Aggregate {
        input: Box::new(input),
        grouping,
        aggregates,
        grouping_kind,
        grouping_sets: Vec::new(),
        having: None,
    }
}

/// The join types supported by [`CommonOp::Join`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum JoinType {
    /// `INNER JOIN`.
    Inner,
    /// `LEFT OUTER JOIN`.
    Left,
    /// `RIGHT OUTER JOIN`.
    Right,
    /// `FULL OUTER JOIN`.
    Full,
    /// `CROSS JOIN`.
    Cross,
    /// `LEFT SEMI JOIN`.
    LeftSemi,
    /// `LEFT ANTI JOIN`.
    LeftAnti,
}

#[cfg(test)]
mod tests {
    use super::super::expression::{Literal, LiteralValue};
    use super::*;
    use crate::types::{DataType, StructField};

    #[test]
    fn common_ast_single_row_constructs() {
        let plan = CommonAst::new(CommonOp::SingleRow);
        assert!(matches!(plan.op, CommonOp::SingleRow));
    }

    #[test]
    fn common_op_project_carries_projections() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::Int(42),
            data_type: DataType::Integer,
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![lit.clone()],
        });
        match plan.op {
            CommonOp::Project { projections, .. } => {
                assert_eq!(projections.len(), 1);
                assert_eq!(projections[0], lit);
            }
            _ => panic!("expected Project"),
        }
    }

    #[test]
    fn common_op_join_carries_plan_ids() {
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(CommonAst::new(CommonOp::SingleRow)),
            right: Box::new(CommonAst::new(CommonOp::SingleRow)),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1, 2],
            right_plan_ids: vec![3],
        });
        match plan.op {
            CommonOp::Join {
                left_plan_ids,
                right_plan_ids,
                ..
            } => {
                // Anchor: plan_ids are `Vec<i64>`, not `Vec<String>` (§2.3).
                assert_eq!(left_plan_ids, vec![1i64, 2]);
                assert_eq!(right_plan_ids, vec![3i64]);
            }
            _ => panic!("expected Join"),
        }
    }

    #[test]
    fn common_op_local_relation_carries_expression_literals() {
        // Anti-SQL anchor (§2.1): LocalRelation rows are Expression::Literal,
        // never raw SQL text.
        let schema = StructType::single("x", DataType::Integer);
        let row = vec![Expression::Literal(Literal {
            value: LiteralValue::Int(1),
            data_type: DataType::Integer,
        })];
        let plan = CommonAst::new(CommonOp::LocalRelation {
            schema,
            rows: vec![row],
        });
        match plan.op {
            CommonOp::LocalRelation { rows, .. } => {
                assert_eq!(rows.len(), 1);
                assert!(matches!(rows[0][0], Expression::Literal(_)));
            }
            _ => panic!("expected LocalRelation"),
        }
    }

    #[test]
    fn common_op_setop_construction() {
        // Anchor: SetOp variant + SetOpKind enum land
        let child_a = CommonAst::new(CommonOp::SingleRow);
        let child_b = CommonAst::new(CommonOp::SingleRow);
        let plan = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: false,
            allow_missing_columns: false,
            children: vec![child_a, child_b],
        });
        match plan.op {
            CommonOp::SetOp {
                kind,
                all,
                by_name,
                allow_missing_columns,
                children,
            } => {
                assert_eq!(kind, SetOpKind::Union);
                assert!(all);
                assert!(!by_name);
                assert!(!allow_missing_columns);
                assert_eq!(children.len(), 2);
            }
            _ => panic!("expected SetOp"),
        }
    }

    #[test]
    fn common_op_unpivot_carries_ids_values_and_output_names() {
        // Anchor: Unpivot variant lands with `ids` + `values` as column names,
        // plus explicit variable/value column names for the two new output
        // columns.
        let plan = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
            values: vec!["age".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        match plan.op {
            CommonOp::Unpivot {
                ids,
                values,
                variable_column_name,
                value_column_name,
                ..
            } => {
                assert_eq!(ids, UnpivotIds::Explicit(vec!["id".to_owned()]));
                assert_eq!(values, vec!["age".to_owned(), "salary".to_owned()]);
                assert_eq!(variable_column_name, "metric");
                assert_eq!(value_column_name, "value");
            }
            _ => panic!("expected Unpivot"),
        }
    }

    #[test]
    fn common_op_pivot_carries_grouping_pivot_column_values_and_aggregates() {
        // Anchor (Pass 60): Pivot variant lands with `grouping`,
        // `pivot_column`, `pivot_values` (empty ⇒ implicit / DuckDB-eager),
        // and `aggregates`.
        use super::super::expression::{Literal, LiteralValue, UnresolvedColumn};
        let group = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "dept_id".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let pivot_col = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "active".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let v_true = Expression::Literal(Literal {
            value: LiteralValue::Boolean(true),
            data_type: DataType::Boolean,
        });
        let v_false = Expression::Literal(Literal {
            value: LiteralValue::Boolean(false),
            data_type: DataType::Boolean,
        });
        let agg = Expression::UnresolvedColumn(UnresolvedColumn {
            name: "n".to_owned(),
            qualifier: None,
            plan_id: None,
        });
        let plan = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            grouping: PivotGrouping::Explicit(vec![group.clone()]),
            pivot_column: pivot_col.clone(),
            pivot_values: vec![v_true.clone(), v_false.clone()],
            aggregates: vec![agg.clone()],
        });
        match plan.op {
            CommonOp::Pivot {
                grouping,
                pivot_column,
                pivot_values,
                aggregates,
                ..
            } => {
                assert_eq!(grouping, PivotGrouping::Explicit(vec![group]));
                assert_eq!(pivot_column, pivot_col);
                assert_eq!(pivot_values, vec![v_true, v_false]);
                assert_eq!(aggregates, vec![agg]);
            }
            _ => panic!("expected Pivot"),
        }
    }

    #[test]
    fn common_op_pivot_empty_values_signals_implicit_discovery() {
        // Anchor (Pass 60): empty `pivot_values` ⇒ DuckDB PIVOT eagerly
        // discovers distinct values at runtime (grp-005 semantics).
        use super::super::expression::UnresolvedColumn;
        let plan = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                UnresolvedColumn {
                    name: "active".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
            pivot_column: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            pivot_values: vec![],
            aggregates: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "salary".to_owned(),
                qualifier: None,
                plan_id: None,
            })],
        });
        match plan.op {
            CommonOp::Pivot { pivot_values, .. } => {
                assert!(pivot_values.is_empty());
            }
            _ => panic!("expected Pivot"),
        }
    }

    #[test]
    fn common_op_describe_carries_input_and_cols() {
        let plan = CommonAst::new(CommonOp::Describe {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            cols: vec!["age".to_owned(), "salary".to_owned()],
        });
        match plan.op {
            CommonOp::Describe { cols, .. } => {
                assert_eq!(cols, vec!["age".to_owned(), "salary".to_owned()]);
            }
            _ => panic!("expected Describe"),
        }
    }

    #[test]
    fn common_op_summary_carries_input_and_statistics() {
        let plan = CommonAst::new(CommonOp::Summary {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            statistics: vec!["count".to_owned(), "25%".to_owned()],
        });
        match plan.op {
            CommonOp::Summary { statistics, .. } => {
                assert_eq!(statistics, vec!["count".to_owned(), "25%".to_owned()]);
            }
            _ => panic!("expected Summary"),
        }
    }

    #[test]
    fn common_op_freq_items_carries_input_cols_and_support() {
        let plan = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            cols: vec!["dept_id".to_owned(), "salary".to_owned()],
            support: 0.3,
        });
        match plan.op {
            CommonOp::FreqItems { cols, support, .. } => {
                assert_eq!(cols, vec!["dept_id".to_owned(), "salary".to_owned()]);
                assert!((support - 0.3).abs() < f64::EPSILON);
            }
            _ => panic!("expected FreqItems"),
        }
    }

    #[test]
    fn common_op_crosstab_carries_input_and_two_col_names() {
        let plan = CommonAst::new(CommonOp::Crosstab {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            col1: "dept_id".to_owned(),
            col2: "active".to_owned(),
        });
        match plan.op {
            CommonOp::Crosstab { col1, col2, .. } => {
                assert_eq!(col1, "dept_id");
                assert_eq!(col2, "active");
            }
            _ => panic!("expected Crosstab"),
        }
    }

    #[test]
    fn common_op_sample_carries_bounds_and_flags() {
        // Pass 83 anchor — samp-001: `df.sample(0.5, seed=11)` lowers to
        // `Sample { lower_bound: 0.0, upper_bound: 0.5, with_replacement: false,
        // seed: Some(11) }`.
        let plan = CommonAst::new(CommonOp::Sample {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: false,
            seed: Some(11),
        });
        match plan.op {
            CommonOp::Sample {
                lower_bound,
                upper_bound,
                with_replacement,
                seed,
                ..
            } => {
                assert!((lower_bound - 0.0).abs() < f64::EPSILON);
                assert!((upper_bound - 0.5).abs() < f64::EPSILON);
                assert!(!with_replacement);
                assert_eq!(seed, Some(11));
            }
            _ => panic!("expected CommonOp::Sample"),
        }
    }

    #[test]
    fn common_op_sample_by_carries_col_and_fractions() {
        // Pass 83 anchor — samp-002: `df.sampleBy("dept_id", {10:0.5, 20:0.5,
        // 30:1.0}, seed=11)` lowers with a resolved-later column expression
        // and Vec<(Literal, f64)> fractions.
        use super::super::expression::UnresolvedColumn;
        let dept_lit = |v: i32| Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        };
        let plan = CommonAst::new(CommonOp::SampleBy {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            col: Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            fractions: vec![
                (dept_lit(10), 0.5),
                (dept_lit(20), 0.5),
                (dept_lit(30), 1.0),
            ],
            seed: Some(11),
        });
        match plan.op {
            CommonOp::SampleBy {
                col,
                fractions,
                seed,
                ..
            } => {
                assert!(matches!(col, Expression::UnresolvedColumn(_)));
                assert_eq!(fractions.len(), 3);
                assert!((fractions[2].1 - 1.0).abs() < f64::EPSILON);
                assert_eq!(seed, Some(11));
            }
            _ => panic!("expected CommonOp::SampleBy"),
        }
    }

    #[test]
    fn common_op_file_scan_uses_file_format_enum() {
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let plan = CommonAst::new(CommonOp::FileScan {
            format: FileFormat::Parquet,
            paths: vec!["/tmp/x.parquet".to_owned()],
            schema: Some(schema),
            options: vec![],
        });
        match plan.op {
            CommonOp::FileScan { format, .. } => {
                assert_eq!(format, FileFormat::Parquet);
            }
            _ => panic!("expected FileScan"),
        }
    }

    // ── N7: grouped_aggregate fold ───────────────────────────────────────

    #[test]
    fn grouped_aggregate_folds_grouping_ahead_of_agg_exprs() {
        use super::super::expression::UnresolvedColumn;
        let dept_id = || {
            Expression::UnresolvedColumn(UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            })
        };
        let count_star = Expression::FunctionCall(super::super::expression::FunctionCall {
            name: "count".to_owned(),
            args: vec![],
            distinct: false,
        });
        let op = grouped_aggregate(
            CommonAst::new(CommonOp::SingleRow),
            vec![dept_id()],
            vec![count_star.clone()],
            GroupingKind::GroupBy,
        );
        match op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                ..
            } => {
                // `aggregates` IS `grouping ++ agg_exprs`, by construction.
                assert_eq!(aggregates, vec![dept_id(), count_star]);
                assert_eq!(grouping, vec![dept_id()]);
            }
            other => panic!("expected CommonOp::Aggregate, got {other:?}"),
        }
    }

    #[test]
    fn grouped_aggregate_empty_grouping_is_identity_fold() {
        // The `convert_cov`/`convert_corr`/`convert_approx_quantile`/
        // `crosstab_to_aggregate` shape: `grouping: vec![]` is a no-op fold —
        // `aggregates` is exactly the `agg_exprs` passed in.
        let agg_expr = Expression::FunctionCall(super::super::expression::FunctionCall {
            name: "corr".to_owned(),
            args: vec![],
            distinct: false,
        });
        let op = grouped_aggregate(
            CommonAst::new(CommonOp::SingleRow),
            vec![],
            vec![agg_expr.clone()],
            GroupingKind::GroupBy,
        );
        match op {
            CommonOp::Aggregate {
                grouping,
                aggregates,
                grouping_sets,
                having,
                ..
            } => {
                assert!(grouping.is_empty());
                assert_eq!(aggregates, vec![agg_expr]);
                assert!(grouping_sets.is_empty());
                assert!(having.is_none());
            }
            other => panic!("expected CommonOp::Aggregate, got {other:?}"),
        }
    }
}
