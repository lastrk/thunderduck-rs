//! Compiled-dispatch emitter for v2 SQL (ADR-009). Also hosts the
//! external-emit enumeration that INV8 walks.
//!
//! # Design (§Layer 2 of Slice C.2 architecture plan)
//!
//! **After Slice C.2**, the emitter is fully self-contained: every scalar
//! expression is rendered by an exhaustive [`render_expr`] match arm, and
//! every function call is rendered by an exhaustive [`render_function_call`]
//! match arm keyed on the lowercased Spark function name. The C.1 seam that
//! delegated scalar rendering to [`crate::generator::SqlGenerator::gen_expr`]
//! is drained; no legacy generator dependency remains here.
//!
//! Emitted SQL is wrapped in [`EmittedSql`], whose only constructor
//! (`emit`) fires the [INV2] tap. [`dispatch`] is the sole caller of
//! `emit`, so INV2 holds by type-system construction.
//!
//! # Scope for Slice C.2 + Slice D Phase 1
//!
//! The dispatch arms cover: Project, Filter, Sort, Limit, Tail, Distinct,
//! WithColumns, DropColumns, AliasedRelation, TableScan, LocalRelation,
//! RangeRelation, Union{,All}, Intersect{,All}, Except{,All}, and a
//! primitive Aggregate. Scalar-expression rendering covers Literal,
//! ColumnReference, UnresolvedColumn, Binary, Unary, Cast, CaseWhen,
//! Alias, Star, RawSql passthrough, and ~40 scalar function shapes
//! hand-copied verbatim from the legacy `FunctionRegistry`.
//!
//! **Slice D Phase 1** adds the ext4 extension-function arms whose
//! DuckDB-side implementation already ships in `thdck_spark_funcs`
//! (ADR-020 pin `ext4`): `spark_hash`, `spark_xxhash64`, `spark_skewness`,
//! and the DECIMAL routes for `spark_sum`, `spark_avg`, `spark_decimal_div`.
//! Native-DuckDB scalars newly wired: `crc32`, `percentile_approx`,
//! `median`, `kurtosis`, `count_if`. Extension arms that require the
//! future `ext5` release (`try_divide`, `try_cast`, `corr`, `covar_samp`,
//! `regr_*`, `try_sum`, `try_avg`) remain surfaced as
//! `EmissionError::UnsupportedFunction` — the caller in `service.rs`
//! treats that as fallback-eligible.
//!
//! Two Slice D dispatch sites are non-obvious and are **not** reachable by
//! grepping [`render_function_call`] alone: `spark_decimal_div` is dispatched
//! from [`render_binary`]'s DECIMAL `/` branch (via `render_spark_decimal_div`),
//! not from a `render_function_call` arm. DECIMAL `sum`/`avg`/`mean` are
//! rewritten to `spark_sum`/`spark_avg` inside [`spark_aggregate_rewrite`],
//! which is invoked by `render_aggregate` before the aggregate expression
//! is rendered — again, not visible in [`render_function_call`] arms.

use crate::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression,
    ColumnReference, Expression, FunctionCall, Literal, LiteralValue, NullOrdering, SortDirection,
    SortOrder, StarExpression, UnaryExpression, UnaryOp, UnresolvedColumn,
};
use crate::logical::spark_column_name;
use crate::transpiler_v2::analyzer::{Schema, TypedOp};
use crate::types::{DataType, TypeMapper};

// ── EmittedSql: single-writer newtype ─────────────────────────────────────────

/// A SQL fragment produced by [`dispatch`]. The only constructor is the
/// module-private [`emit`] function, which fires the [INV2] tap.
///
/// This newtype is what makes the emitter the *single writer* of v2 SQL
/// by construction — every other module that wants SQL text has to go
/// through [`dispatch`] and receive an `EmittedSql`.
#[derive(Debug, Clone)]
#[must_use]
pub struct EmittedSql(String);

impl EmittedSql {
    /// Unwrap into the underlying SQL text. Used at the boundary in
    /// [`crate::transpiler_v2::generate`] where the string leaves the module.
    pub fn into_string(self) -> String {
        self.0
    }

    /// Borrow the underlying SQL text without consuming the wrapper.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Module-private constructor of [`EmittedSql`]. Fires the INV2 emit tap.
///
/// [`dispatch`] is the sole caller of this function; no other path is
/// permitted to build an [`EmittedSql`]. INV2 gains real teeth as a
/// consequence — the tap counts exactly one call per dispatch.
fn emit(sql: String) -> EmittedSql {
    super::fire_emit_tap(sql.as_bytes());
    EmittedSql(sql)
}

// ── EmissionError ─────────────────────────────────────────────────────────────

/// Errors surfaced by the declarative emission table.
///
/// [INV3 §CV.5] requires the emitter be pure over `TypedOp`; every
/// variant here reflects a *dispatch* failure, not an analysis or shape
/// failure (those live in
/// [`crate::transpiler_v2::analyzer::AnalyzerError`]).
#[derive(thiserror::Error, Debug)]
pub enum EmissionError {
    /// The dispatcher had no arm for this typed-op / operand-type
    /// combination. Signals fall-through to the legacy path in the
    /// dispatch wrapper at `service.rs`.
    #[error("no emission arm matched typed-op `{op_kind}` with operand types {operand_types:?}")]
    UnsupportedOp {
        /// Diagnostic name of the typed-op variant that had no match.
        op_kind: &'static str,
        /// Operand types considered when the arm was rejected.
        operand_types: Vec<DataType>,
    },

    /// Child operator failed to dispatch — carries the position (left/right/
    /// input) so a nested failure is diagnosable.
    #[error("child operator `{child}` of `{parent_kind}` failed: {source}")]
    ChildFailed {
        /// Parent operator's diagnostic name.
        parent_kind: &'static str,
        /// Which child slot failed (`"input"`, `"left"`, `"right"`).
        child: &'static str,
        /// Underlying error.
        #[source]
        source: Box<EmissionError>,
    },

    /// A field a dispatch arm referenced was absent from the schema.
    /// Should be unreachable if the analyzer is correct — surface loudly.
    #[error("dispatch arm referenced field `{field}` not present in schema {schema_fields:?}")]
    MissingField {
        /// Field name looked up.
        field: String,
        /// Schema field names that were available.
        schema_fields: Vec<String>,
    },

    /// An `Expression` variant is not implemented by the v2 emitter.
    ///
    /// Slice-D / Slice-F variants (Window, subqueries, complex-type literals,
    /// lambdas, UpdateFields, etc.) surface here. The caller in `service.rs`
    /// treats this as fallback-eligible.
    #[error("emission arm `{op_kind}` does not implement expression variant `{expr_kind}`")]
    UnsupportedExpression {
        /// The operator arm that owned the failing expression.
        op_kind: &'static str,
        /// Diagnostic name of the Expression variant that had no match.
        expr_kind: &'static str,
    },

    /// A `FunctionCall` name has no matching arm in [`render_function_call`].
    ///
    /// Slice-D extensions (`spark_*`, `try_cast`, `try_divide`) and Slice-F
    /// complex-type functions surface here. The caller in `service.rs`
    /// treats this as fallback-eligible.
    #[error("emission arm `{op_kind}` has no rule for function `{name}`")]
    UnsupportedFunction {
        /// The operator arm that owned the failing function call.
        op_kind: &'static str,
        /// The function name whose arm was missing.
        name: String,
    },
}

// ── The EmissionTable + external structural reservations ──────────────────────

/// Handle to the v2 SQL emitter.
///
/// Slice C.1 keeps this as a zero-sized shim over [`dispatch`] so external
/// call sites (e.g. `service.rs`) have a named entry point that mirrors the
/// architecture plan's "single source of truth" wording. When Slice C.2
/// lands the declarative per-function rows, this handle is the natural
/// home for the row-lookup surface.
///
/// [INV3 §CV.5]: [`dispatch_op`]'s SQL-emitting `match` arms are the sole
/// place that maps `TypedOp` → SQL; every caller goes through [`dispatch`].
#[derive(Debug, Default)]
pub struct EmissionTable;

impl EmissionTable {
    /// Dispatch a typed-op through the compiled emitter. See [`dispatch`].
    pub fn dispatch(op: &TypedOp) -> Result<EmittedSql, EmissionError> {
        dispatch(op)
    }
}

// ── ExternalEmit (INV8) ───────────────────────────────────────────────────────

/// A specific external-table emission path, enumerated by [INV8 §CV.5]'s
/// allow-list (see ADR-013).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExternalEmit {
    /// `read_parquet(...)` path-scan.
    ReadParquet,
    /// `iceberg_scan(...)` path-scan.
    IcebergScan,
    /// `delta_scan(...)` path-scan.
    DeltaScan,
    /// `ATTACH … TYPE iceberg` attachment.
    AttachIceberg,
    /// Unity Catalog (`uc_catalog`) attachment.
    UcCatalog,
}

/// Every v2 emit path classified as "external".
///
/// **Stub.** Empty today; the [`super::invariants::inv8_external_access_is_delegated`]
/// test loops over the empty slice and passes vacuously, but the closed
/// set of legal kinds is encoded in [`ExternalEmit`].
///
/// TODO INV8: populate as ADR-013 emit paths land.
pub fn external_emit_paths() -> &'static [ExternalEmit] {
    &[]
}

/// `Extension(name)` targets the dispatch table declares. [INV6 §CV.5]
/// requires every name resolve to a function exported by `thdck_spark_funcs`.
///
/// Populated in Slice D Phase 1 with the ext4-bundled extension functions
/// consumed by [`render_function_call`], [`render_binary`]'s
/// decimal-division branch, and [`spark_aggregate_rewrite`]'s DECIMAL
/// SUM/AVG routing. Further entries land as later Slice-D passes wire
/// ext5 targets (`try_sum`, `try_avg`, `try_cast`, `try_divide`, `corr`,
/// `covar_samp`, `regr_*`).
pub fn extension_targets() -> &'static [&'static str] {
    &[
        "spark_hash",
        "spark_xxhash64",
        "spark_skewness",
        "spark_sum",
        "spark_avg",
        "spark_decimal_div",
    ]
}

// ── dispatch entry point ──────────────────────────────────────────────────────

/// The single dispatch entry point. Wraps [`dispatch_op`]'s `String` result
/// in an [`EmittedSql`] so the emit tap fires exactly once per outermost
/// call.
///
/// [INV3 §CV.5]: [`dispatch_op`]'s `match` arms are the sole place that
/// maps `TypedOp` → SQL. Any escape hatch appears in
/// [`crate::transpiler_v2::C_ESCAPE_HATCHES`].
///
/// [INV2 §CV.5]: the returned `EmittedSql` is constructed via the
/// module-private [`emit`], which fires the emit tap. `dispatch` is the
/// sole caller of `emit`.
pub fn dispatch(op: &TypedOp) -> Result<EmittedSql, EmissionError> {
    let sql = dispatch_op(op)?;
    Ok(emit(sql))
}

/// Internal dispatch that returns a raw `String`. Only [`dispatch`] wraps
/// the outermost result in an [`EmittedSql`] via [`emit`]; recursive
/// child dispatches use this helper to avoid firing the tap once per
/// nested operator.
fn dispatch_op(op: &TypedOp) -> Result<String, EmissionError> {
    match op {
        TypedOp::Project {
            projections,
            input,
            schema,
            ..
        } => render_project(projections, input, schema),
        TypedOp::Filter {
            predicate, input, ..
        } => render_filter(predicate, input),
        TypedOp::Sort {
            input,
            order,
            limit,
            offset,
            ..
        } => render_sort(input, order, limit.as_ref(), offset.as_ref()),
        TypedOp::Limit { input, n, .. } => render_limit(input, n),
        TypedOp::Tail { input, n, .. } => render_tail(input, n),
        TypedOp::Distinct { input, on, .. } => render_distinct(input, on),
        TypedOp::WithColumns { input, columns, .. } => render_with_columns(input, columns),
        TypedOp::DropColumns { input, names, .. } => render_drop_columns(input, names),
        TypedOp::AliasedRelation {
            input,
            alias,
            column_aliases,
            ..
        } => render_aliased_relation(input, alias, column_aliases),
        TypedOp::TableScan { name, .. } => Ok(render_table_scan(name)),
        TypedOp::LocalRelation { schema } => Ok(render_local_relation(schema)),
        TypedOp::RangeRelation {
            start, end, step, ..
        } => Ok(render_range(*start, *end, *step)),
        TypedOp::Union {
            left,
            right,
            all,
            schema,
            ..
        } => render_union(left, right, *all, schema),
        TypedOp::Intersect {
            left,
            right,
            all,
            schema,
        } => render_intersect(left, right, *all, schema),
        TypedOp::Except {
            left,
            right,
            all,
            schema,
        } => render_except(left, right, *all, schema),
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            having,
            ..
        } => render_aggregate(input, grouping, aggregates, having.as_ref()),
        TypedOp::Join { .. } => Err(EmissionError::UnsupportedOp {
            op_kind: "Join",
            operand_types: Vec::new(),
        }),
    }
}

// ── Renderers (one per typed-op arm) ──────────────────────────────────────────

fn render_project(
    projections: &[Expression],
    input: &TypedOp,
    _schema: &Schema,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Project", "input", input)?;
    let child_schema = input.schema();
    let mut parts = Vec::with_capacity(projections.len());
    for expr in projections {
        parts.push(render_projection_slot(expr, child_schema, "Project")?);
    }
    let list = if parts.is_empty() {
        "*".to_string()
    } else {
        parts.join(", ")
    };
    Ok(format!("SELECT {list} FROM ({child_sql})"))
}

fn render_filter(predicate: &Expression, input: &TypedOp) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Filter", "input", input)?;
    let schema = input.schema();
    let pred = render_expr(predicate, schema, "Filter")?;
    Ok(format!("SELECT * FROM ({child_sql}) WHERE {pred}"))
}

fn render_sort(
    input: &TypedOp,
    order: &[SortOrder],
    limit: Option<&Expression>,
    offset: Option<&Expression>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Sort", "input", input)?;
    let schema = input.schema();
    let order_parts: Result<Vec<_>, _> =
        order.iter().map(|s| render_sort_order(s, schema)).collect();
    let order_sql = order_parts?.join(", ");
    let mut sql = format!("SELECT * FROM ({child_sql}) ORDER BY {order_sql}");
    if let Some(lim) = limit {
        let n = render_expr(lim, schema, "Sort")?;
        sql.push_str(&format!(" LIMIT {n}"));
    }
    if let Some(off) = offset {
        let n = render_expr(off, schema, "Sort")?;
        sql.push_str(&format!(" OFFSET {n}"));
    }
    Ok(sql)
}

fn render_sort_order(s: &SortOrder, schema: &Schema) -> Result<String, EmissionError> {
    let expr_sql = render_expr(&s.expr, schema, "Sort")?;
    let dir = match s.direction {
        SortDirection::Asc => "ASC",
        SortDirection::Desc => "DESC",
    };
    let nulls = match s.null_ordering {
        NullOrdering::NullsFirst => "NULLS FIRST",
        NullOrdering::NullsLast => "NULLS LAST",
    };
    Ok(format!("{expr_sql} {dir} {nulls}"))
}

fn render_limit(input: &TypedOp, n: &Expression) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Limit", "input", input)?;
    let schema = input.schema();
    let n_sql = render_expr(n, schema, "Limit")?;
    Ok(format!("SELECT * FROM ({child_sql}) LIMIT {n_sql}"))
}

/// Emit the DuckDB emulation of Spark `TAIL n` — grab the last `n` rows.
///
/// DuckDB has no `TAIL`; we number rows with `ROW_NUMBER()` and keep those
/// whose position exceeds `count(*) - n`. To avoid materialising `child_sql`
/// twice (once for the numbering, once for the COUNT), we wrap the child
/// in a WITH clause so DuckDB can share the subquery plan.
///
/// M6 closure (see Slice C.1 review): the C.1 shape inlined `({child_sql})`
/// twice, forcing DuckDB to re-plan the sub-tree. The CTE shape below
/// materialises it once — a two-line change from the C.1 form.
fn render_tail(input: &TypedOp, n: &Expression) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Tail", "input", input)?;
    let schema = input.schema();
    let n_sql = render_expr(n, schema, "Tail")?;
    Ok(format!(
        "WITH __td_child AS ({child_sql}) \
         SELECT * EXCLUDE (__td_row_num__) FROM \
         (SELECT *, ROW_NUMBER() OVER () AS __td_row_num__ FROM __td_child) \
         WHERE __td_row_num__ > (SELECT COUNT(*) FROM __td_child) - {n_sql}"
    ))
}

fn render_distinct(input: &TypedOp, on: &[Expression]) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Distinct", "input", input)?;
    if on.is_empty() {
        return Ok(format!("SELECT DISTINCT * FROM ({child_sql})"));
    }
    let schema = input.schema();
    let parts: Result<Vec<_>, _> = on
        .iter()
        .map(|e| render_expr(e, schema, "Distinct"))
        .collect();
    let on_sql = parts?.join(", ");
    Ok(format!(
        "SELECT DISTINCT ON ({on_sql}) * FROM ({child_sql})"
    ))
}

fn render_with_columns(
    input: &TypedOp,
    columns: &[(
        String,
        Expression,
        crate::transpiler_v2::analyzer::TypedAttr,
    )],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("WithColumns", "input", input)?;
    let schema = input.schema();
    if columns.is_empty() {
        return Ok(format!("SELECT * FROM ({child_sql})"));
    }
    let mut parts = Vec::with_capacity(columns.len());
    for (name, expr, _attr) in columns {
        let expr_sql = render_expr(expr, schema, "WithColumns")?;
        parts.push(format!("{expr_sql} AS {}", quote_ident(name)));
    }
    let list = parts.join(", ");
    Ok(format!("SELECT *, {list} FROM ({child_sql})"))
}

fn render_drop_columns(input: &TypedOp, names: &[String]) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("DropColumns", "input", input)?;
    if names.is_empty() {
        return Ok(format!("SELECT * FROM ({child_sql})"));
    }
    let excl = names
        .iter()
        .map(|n| quote_ident(n))
        .collect::<Vec<_>>()
        .join(", ");
    Ok(format!("SELECT * EXCLUDE ({excl}) FROM ({child_sql})"))
}

fn render_aliased_relation(
    input: &TypedOp,
    alias: &str,
    column_aliases: &[String],
) -> Result<String, EmissionError> {
    // Mirror legacy `SqlGenerator::gen_aliased_relation`
    // (`crates/core/src/generator/mod.rs:865-878`): emit the child as a
    // subquery with an `AS <alias>` clause, plus positional column-alias
    // parens when `column_aliases` is non-empty. Without the alias,
    // qualified column references (`df.alias('t').select(F.col('t.a'))`)
    // fail at DuckDB execution — the alias is load-bearing, not cosmetic.
    let child_sql = dispatch_child("AliasedRelation", "input", input)?;
    if alias.is_empty() {
        return Ok(format!("SELECT * FROM ({child_sql})"));
    }
    let alias_sql = quote_ident(alias);
    if column_aliases.is_empty() {
        Ok(format!("SELECT * FROM ({child_sql}) AS {alias_sql}"))
    } else {
        let cols = column_aliases
            .iter()
            .map(|c| quote_ident(c))
            .collect::<Vec<_>>()
            .join(", ");
        Ok(format!(
            "SELECT * FROM ({child_sql}) AS {alias_sql}({cols})"
        ))
    }
}

fn render_table_scan(name: &str) -> String {
    quote_ident(name)
}

fn render_local_relation(schema: &Schema) -> String {
    if schema.is_empty() {
        return "SELECT NULL WHERE FALSE".to_string();
    }
    // C.1 emits a well-typed empty relation via NULL casts. Non-empty
    // local relations carrying Arrow bytes are legacy-path only.
    let cols = schema
        .fields
        .iter()
        .map(|f| {
            format!(
                "CAST(NULL AS {}) AS {}",
                crate::types::TypeMapper::to_duckdb(&f.data_type),
                quote_ident(&f.name)
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    format!("SELECT {cols} WHERE FALSE")
}

fn render_range(start: i64, end: i64, step: i64) -> String {
    // Match Spark's `range(start, end, step)` schema (`id: Long NOT NULL`).
    format!("SELECT UNNEST(range({start}, {end}, {step})) AS \"id\"")
}

/// Wrap `child_sql` in a `SELECT` that casts each of its output columns to
/// the widened union-schema type. Returns `None` when every column already
/// declares the widened type (no CAST needed).
///
/// M3 closure (Slice C.2): when the analyzer's `TypedOp::Union.schema`
/// widens some column's type past a child's projection, we must materialise
/// the widened Spark type at emit time — otherwise DuckDB falls back to its
/// own set-op coercion, which can differ from Spark (e.g. int + long →
/// long in Spark but bigint via DuckDB's own promotion).
fn maybe_wrap_widened_child(
    child_schema: &Schema,
    widened: &Schema,
    child_sql: &str,
) -> Option<String> {
    if child_schema.fields.len() != widened.fields.len() {
        // Arity mismatch — the analyzer should have caught this; leave the
        // child untouched and let DuckDB error.
        return None;
    }
    let diverges = child_schema
        .fields
        .iter()
        .zip(widened.fields.iter())
        .any(|(c, w)| c.data_type != w.data_type);
    if !diverges {
        return None;
    }
    let cols = child_schema
        .fields
        .iter()
        .zip(widened.fields.iter())
        .map(|(c, w)| {
            let name = quote_ident(&c.name);
            if c.data_type == w.data_type {
                name
            } else {
                format!(
                    "CAST({name} AS {}) AS {name}",
                    TypeMapper::to_duckdb(&w.data_type)
                )
            }
        })
        .collect::<Vec<_>>()
        .join(", ");
    Some(format!("SELECT {cols} FROM ({child_sql})"))
}

fn render_union(
    left: &TypedOp,
    right: &TypedOp,
    all: bool,
    widened: &Schema,
) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Union", "left", left)?;
    let right_sql = dispatch_child("Union", "right", right)?;
    let left_final =
        maybe_wrap_widened_child(left.schema(), widened, &left_sql).unwrap_or(left_sql);
    let right_final =
        maybe_wrap_widened_child(right.schema(), widened, &right_sql).unwrap_or(right_sql);
    let op = if all { "UNION ALL" } else { "UNION" };
    Ok(format!("({left_final}) {op} ({right_final})"))
}

fn render_intersect(
    left: &TypedOp,
    right: &TypedOp,
    all: bool,
    widened: &Schema,
) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Intersect", "left", left)?;
    let right_sql = dispatch_child("Intersect", "right", right)?;
    let left_final =
        maybe_wrap_widened_child(left.schema(), widened, &left_sql).unwrap_or(left_sql);
    let right_final =
        maybe_wrap_widened_child(right.schema(), widened, &right_sql).unwrap_or(right_sql);
    let op = if all { "INTERSECT ALL" } else { "INTERSECT" };
    Ok(format!("({left_final}) {op} ({right_final})"))
}

fn render_except(
    left: &TypedOp,
    right: &TypedOp,
    all: bool,
    widened: &Schema,
) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Except", "left", left)?;
    let right_sql = dispatch_child("Except", "right", right)?;
    let left_final =
        maybe_wrap_widened_child(left.schema(), widened, &left_sql).unwrap_or(left_sql);
    let right_final =
        maybe_wrap_widened_child(right.schema(), widened, &right_sql).unwrap_or(right_sql);
    let op = if all { "EXCEPT ALL" } else { "EXCEPT" };
    Ok(format!("({left_final}) {op} ({right_final})"))
}

fn render_aggregate(
    input: &TypedOp,
    grouping: &[Expression],
    aggregates: &[crate::transpiler_v2::ast::AggregateCall],
    having: Option<&Expression>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_child("Aggregate", "input", input)?;
    let schema = input.schema();
    let mut select_parts = Vec::with_capacity(grouping.len() + aggregates.len());
    for g in grouping {
        select_parts.push(render_projection_slot(g, schema, "Aggregate")?);
    }
    for agg in aggregates {
        // Order matches legacy `SqlGenerator::gen_aggregate_call`
        // (`crates/core/src/generator/mod.rs:180-199`): render the inner
        // call, inject `DISTINCT` inside the parens if flagged, then
        // wrap in `FILTER (WHERE ...)` if present. `is_distinct` comes
        // from Spark's aggregate protocol (`AggregateExpr.is_distinct`,
        // propagated through the lowering adapter) and must not be
        // dropped — `COUNT(DISTINCT x)` and `COUNT(x)` differ.
        //
        // Slice D Phase 1: DECIMAL-typed SUM/AVG arguments must route
        // through the `spark_sum` / `spark_avg` extension functions to
        // preserve Spark's widened DECIMAL return types. The rewrite
        // both renames the aggregate and picks the outer CAST target.
        let (func_to_render, extra_cast) = match spark_aggregate_rewrite(&agg.func, schema) {
            Some((rewritten, target)) => (rewritten, Some(target)),
            None => (agg.func.clone(), None),
        };
        let mut decorated = render_expr(&func_to_render, schema, "Aggregate")?;
        if agg.is_distinct {
            decorated = inject_distinct(decorated);
        }
        if let Some(filter) = &agg.filter {
            let filter_sql = render_expr(filter, schema, "Aggregate")?;
            decorated = format!("{decorated} FILTER (WHERE {filter_sql})");
        }
        // Spark-parity return-type CAST for aggregates whose Spark return
        // type diverges from DuckDB's default (e.g., SUM of integer types
        // returns HUGEINT in DuckDB but BIGINT in Spark). For DECIMAL
        // SUM/AVG the CAST target is supplied by `spark_aggregate_rewrite`
        // (Slice D Phase 1); everything else routes through the
        // integral-return `spark_aggregate_return_cast` helper.
        let cast_target = extra_cast.or_else(|| spark_aggregate_return_cast(&agg.func, schema));
        if let Some(target) = cast_target {
            decorated = format!("CAST({decorated} AS {})", TypeMapper::to_duckdb(&target));
        }
        let name = match &agg.func {
            Expression::Alias(a) => a.alias.clone(),
            other => spark_column_name(other),
        };
        select_parts.push(format!("{decorated} AS {}", quote_ident(&name)));
    }
    let select_list = if select_parts.is_empty() {
        "*".to_string()
    } else {
        select_parts.join(", ")
    };
    let mut sql = format!("SELECT {select_list} FROM ({child_sql})");
    if !grouping.is_empty() {
        let g_parts: Result<Vec<_>, _> = grouping
            .iter()
            .map(|e| render_expr(e, schema, "Aggregate"))
            .collect();
        sql.push_str(&format!(" GROUP BY {}", g_parts?.join(", ")));
    }
    if let Some(h) = having {
        let h_sql = render_expr(h, schema, "Aggregate")?;
        sql.push_str(&format!(" HAVING {h_sql}"));
    }
    Ok(sql)
}

// ── Helpers ───────────────────────────────────────────────────────────────────

/// Dispatch a child operator, wrapping its error with position info so a
/// nested failure names the parent + child slot.
fn dispatch_child(
    parent_kind: &'static str,
    child: &'static str,
    op: &TypedOp,
) -> Result<String, EmissionError> {
    dispatch_op(op).map_err(|e| EmissionError::ChildFailed {
        parent_kind,
        child,
        source: Box::new(e),
    })
}

/// Render one projection slot as `<expr> AS "<name>"` for named slots, or
/// raw `<expr>` for Star.
fn render_projection_slot(
    expr: &Expression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    match expr {
        Expression::Star(s) => Ok(render_star(s)),
        Expression::Alias(a) => {
            let inner = render_expr(&a.expr, schema, op_kind)?;
            // Apply Spark-parity projection-level CAST wraps (e.g. int/int → DOUBLE)
            // before the `AS <alias>` binding so the alias binds the widened value,
            // matching legacy behavior for aliased expressions.
            let wrapped = match spark_return_cast(&a.expr, schema) {
                Some(dt) => format!("CAST({inner} AS {})", TypeMapper::to_duckdb(&dt)),
                None => inner,
            };
            Ok(format!("{wrapped} AS {}", quote_ident(&a.alias)))
        }
        Expression::ColumnReference(c) if c.qualifier.is_none() => Ok(quote_ident(&c.name)),
        Expression::UnresolvedColumn(u) if u.qualifier.is_none() => Ok(quote_ident(&u.name)),
        other => {
            let inner = render_expr(other, schema, op_kind)?;
            // ADR-015 naming convention: `spark_column_name(expr) AS out`
            // when the surface expression is not itself a bare column ref.
            let name = spark_column_name(other);
            let wrapped = match spark_return_cast(other, schema) {
                Some(dt) => format!("CAST({inner} AS {})", TypeMapper::to_duckdb(&dt)),
                None => inner,
            };
            Ok(format!("{wrapped} AS {}", quote_ident(&name)))
        }
    }
}

// ── Scalar expression rendering (INV3 choke point) ────────────────────────────

/// Render a scalar expression to SQL. Exhaustive over [`Expression`] —
/// every Slice-C variant handled inline; Slice-D/F variants surface as
/// [`EmissionError::UnsupportedExpression`] for fallback-eligible failure.
///
/// After Slice C.2, this is the sole scalar-expression choke point: no
/// legacy `SqlGenerator::gen_expr` delegation remains. INV3's grep asserts
/// the absence of any `crate::generator::SqlGenerator` import above.
fn render_expr(
    expr: &Expression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    match expr {
        Expression::Literal(l) => Ok(render_literal(l)),
        Expression::ColumnReference(c) => Ok(render_column_ref(c)),
        Expression::UnresolvedColumn(u) => Ok(render_unresolved_column(u)),
        Expression::Alias(a) => {
            let inner = render_expr(&a.expr, schema, op_kind)?;
            Ok(format!("{inner} AS {}", quote_ident(&a.alias)))
        }
        Expression::Binary(b) => render_binary(b, schema, op_kind),
        Expression::Unary(u) => render_unary(u, schema, op_kind),
        Expression::Cast(c) => render_cast(c, schema, op_kind),
        Expression::CaseWhen(cw) => render_case_when(cw, schema, op_kind),
        Expression::FunctionCall(f) => render_function_call(f, schema, op_kind),
        Expression::Star(s) => Ok(render_star(s)),
        // RawSql passthrough matches legacy behavior (`spark.expr()` escape hatch).
        Expression::RawSql(r) => Ok(r.sql.clone()),
        // Slice-D / Slice-F variants — surface as fallback-eligible errors.
        Expression::Window(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "Window",
        }),
        Expression::InSubquery(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "InSubquery",
        }),
        Expression::ExistsSubquery(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "ExistsSubquery",
        }),
        Expression::ScalarSubquery(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "ScalarSubquery",
        }),
        Expression::Lambda(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "Lambda",
        }),
        Expression::LambdaVariable(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "LambdaVariable",
        }),
        Expression::ArrayLiteral(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "ArrayLiteral",
        }),
        Expression::MapLiteral(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "MapLiteral",
        }),
        Expression::StructLiteral(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "StructLiteral",
        }),
        Expression::Between(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "Between",
        }),
        Expression::InList(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "InList",
        }),
        Expression::Like(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "Like",
        }),
        Expression::Interval(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "Interval",
        }),
        Expression::IsDistinctFrom(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "IsDistinctFrom",
        }),
        Expression::ExtractValue(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "ExtractValue",
        }),
        Expression::RowConstructor(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "RowConstructor",
        }),
        Expression::UpdateFields(_) => Err(EmissionError::UnsupportedExpression {
            op_kind,
            expr_kind: "UpdateFields",
        }),
    }
}

/// Render a typed literal. Mirrors legacy `gen_literal_with_type` /
/// `gen_literal` at `crates/core/src/generator/mod.rs:2235-2293` — the
/// shape is duplicated here as the C.2 contamination barrier per plan §1.
fn render_literal(l: &Literal) -> String {
    // Preserve Spark-inferred DECIMAL precision/scale for decimal literals.
    if let (LiteralValue::Decimal(s), DataType::Decimal { precision, scale }) =
        (&l.value, &l.data_type)
    {
        return format!("{s}::DECIMAL({precision},{scale})");
    }
    match &l.value {
        LiteralValue::Null => "NULL".to_string(),
        LiteralValue::Boolean(b) => if *b { "TRUE" } else { "FALSE" }.to_string(),
        LiteralValue::Byte(n) => format!("CAST({n} AS TINYINT)"),
        LiteralValue::Short(n) => format!("CAST({n} AS SMALLINT)"),
        LiteralValue::Int(n) => n.to_string(),
        LiteralValue::Long(n) => format!("{n}::BIGINT"),
        LiteralValue::Float(f) => {
            if f.is_infinite() {
                if *f > 0.0 {
                    "'Infinity'::FLOAT".to_string()
                } else {
                    "'-Infinity'::FLOAT".to_string()
                }
            } else if f.is_nan() {
                "'NaN'::FLOAT".to_string()
            } else {
                format!("{f}::FLOAT")
            }
        }
        LiteralValue::Double(d) => {
            if d.is_infinite() {
                if *d > 0.0 {
                    "'Infinity'::DOUBLE".to_string()
                } else {
                    "'-Infinity'::DOUBLE".to_string()
                }
            } else if d.is_nan() {
                "'NaN'::DOUBLE".to_string()
            } else {
                format!("{d}::DOUBLE")
            }
        }
        LiteralValue::Decimal(s) => format!("{s}::DECIMAL"),
        LiteralValue::String(s) => format!("'{}'", s.replace('\'', "''")),
        LiteralValue::Date(days) => format!("(DATE '1970-01-01' + INTERVAL {days} DAY)"),
        LiteralValue::Timestamp(micros) => {
            format!("(TIMESTAMPTZ '1970-01-01 00:00:00+00' + INTERVAL {micros} MICROSECOND)")
        }
        LiteralValue::TimestampNtz(micros) => {
            format!("(TIMESTAMP '1970-01-01 00:00:00' + INTERVAL {micros} MICROSECOND)")
        }
        LiteralValue::Binary(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            format!("decode('{hex}')")
        }
    }
}

/// Render a resolved column reference: `"qualifier"."name"` when qualified,
/// `"name"` otherwise.
fn render_column_ref(c: &ColumnReference) -> String {
    match &c.qualifier {
        Some(q) => format!("{}.{}", quote_ident(q), quote_ident(&c.name)),
        None => quote_ident(&c.name),
    }
}

/// Render an unresolved column reference. Mirrors legacy behavior:
/// - Internal plan-id qualifiers (`__plan_id_...__`) are stripped.
/// - Multi-part names like `person.address.city` are split and quoted.
fn render_unresolved_column(u: &UnresolvedColumn) -> String {
    if let Some(q) = &u.qualifier {
        if q.starts_with("__plan_id_") && q.ends_with("__") {
            quote_ident(&u.name)
        } else if u.name.contains('.') {
            let parts: String = u
                .name
                .split('.')
                .map(quote_ident)
                .collect::<Vec<_>>()
                .join(".");
            format!("{}.{}", quote_ident(q), parts)
        } else {
            format!("{}.{}", quote_ident(q), quote_ident(&u.name))
        }
    } else if u.name.contains('.') {
        u.name
            .split('.')
            .map(quote_ident)
            .collect::<Vec<_>>()
            .join(".")
    } else {
        quote_ident(&u.name)
    }
}

fn render_star(s: &StarExpression) -> String {
    match &s.qualifier {
        Some(q) => format!("{}.*", quote_ident(q)),
        None => "*".to_string(),
    }
}

/// Render a binary op. Precedence-aware parenthesisation matches legacy
/// `gen_binary` / `gen_expr_paren` at `generator/mod.rs:1516-1653`.
fn render_binary(
    b: &BinaryExpression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    // Slice D Phase 1: DECIMAL division routes through the
    // `spark_decimal_div` extension so that ROUND_HALF_UP semantics and
    // the widened result-precision/scale match Spark exactly. Mirrors
    // legacy `gen_strict_decimal_div` at `generator/mod.rs:1541-1644`.
    if matches!(b.op, BinaryOp::Div) {
        let lt = b.left.data_type(schema);
        let rt = b.right.data_type(schema);
        if let Some(sql) = render_spark_decimal_div(b, &lt, &rt, schema, op_kind)? {
            return Ok(sql);
        }
    }
    let left = render_expr_paren(&b.left, schema, op_kind, binop_precedence(&b.op))?;
    let right = render_expr_paren(&b.right, schema, op_kind, binop_precedence(&b.op))?;
    let sql = format!("{left} {} {right}", b.op.symbol());
    // DATE ± INTERVAL → DuckDB promotes to TIMESTAMP; cast back to DATE
    // to preserve Spark's DATE return type.
    if matches!(b.op, BinaryOp::Add | BinaryOp::Sub)
        && b.left.data_type(schema) == DataType::Date
        && b.right.data_type(schema).is_interval()
    {
        return Ok(format!("CAST({sql} AS DATE)"));
    }
    Ok(sql)
}

/// Slice D Phase 1: strict-mode decimal division via the `spark_decimal_div`
/// extension. Duplicates the shape of legacy `gen_strict_decimal_div`
/// (`generator/mod.rs:1541-1644`) — Slice C.2's INV3 contamination barrier
/// forbids importing `SqlGenerator`, so the arithmetic is inlined here.
///
/// The three (Decimal, Decimal) / (Decimal, integral) / (integral, Decimal)
/// branches return `Some(sql)`; every other operand-type pair returns
/// `Ok(None)` and lets [`render_binary`] fall through to the plain
/// `left / right` shape.
///
/// Operands are re-rendered via [`render_expr`] (unparenthesised) rather
/// than reusing [`render_expr_paren`] outputs, matching legacy line 1549.
fn render_spark_decimal_div(
    b: &BinaryExpression,
    lt: &DataType,
    rt: &DataType,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<Option<String>, EmissionError> {
    use crate::types::TypeInferenceEngine;
    let left_sql = render_expr(&b.left, schema, op_kind)?;
    let right_sql = render_expr(&b.right, schema, op_kind)?;
    match (lt, rt) {
        (
            DataType::Decimal {
                precision: p1,
                scale: s1,
            },
            DataType::Decimal {
                precision: p2,
                scale: s2,
            },
        ) => {
            // Defensively cast operands to their inferred DECIMAL types;
            // DuckDB may return DOUBLE at runtime (e.g. native AVG inside
            // window functions) even though plan-level inference says
            // DECIMAL. The explicit CASTs guarantee `spark_decimal_div`
            // always receives DECIMAL arguments.
            let left_cast = format!("CAST({left_sql} AS DECIMAL({p1},{s1}))");
            let right_cast = format!("CAST({right_sql} AS DECIMAL({p2},{s2}))");
            let result_type = TypeInferenceEngine::decimal_div_type(*p1, *s1, *p2, *s2);
            if let DataType::Decimal {
                precision: rp,
                scale: rs,
            } = result_type
            {
                Ok(Some(format!(
                    "CAST(spark_decimal_div({left_cast}, {right_cast}) AS DECIMAL({rp},{rs}))"
                )))
            } else {
                Ok(Some(format!(
                    "spark_decimal_div({left_cast}, {right_cast})"
                )))
            }
        }
        (
            DataType::Decimal {
                precision: p1,
                scale: s1,
            },
            i,
        ) if i.is_integral() => {
            if let DataType::Decimal {
                precision: p2,
                scale: s2,
            } = b.right.integral_to_decimal_for_arithmetic(schema)
            {
                let result_type = TypeInferenceEngine::decimal_div_type(*p1, *s1, p2, s2);
                if let DataType::Decimal {
                    precision: rp,
                    scale: rs,
                } = result_type
                {
                    Ok(Some(format!(
                        "CAST(spark_decimal_div({left_sql}, CAST({right_sql} AS DECIMAL({p2},0))) AS DECIMAL({rp},{rs}))"
                    )))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        (
            i,
            DataType::Decimal {
                precision: p2,
                scale: s2,
            },
        ) if i.is_integral() => {
            if let DataType::Decimal {
                precision: p1,
                scale: s1,
            } = b.left.integral_to_decimal_for_arithmetic(schema)
            {
                let result_type = TypeInferenceEngine::decimal_div_type(p1, s1, *p2, *s2);
                if let DataType::Decimal {
                    precision: rp,
                    scale: rs,
                } = result_type
                {
                    Ok(Some(format!(
                        "CAST(spark_decimal_div(CAST({left_sql} AS DECIMAL({p1},0)), {right_sql}) AS DECIMAL({rp},{rs}))"
                    )))
                } else {
                    Ok(None)
                }
            } else {
                Ok(None)
            }
        }
        _ => Ok(None),
    }
}

fn render_expr_paren(
    expr: &Expression,
    schema: &Schema,
    op_kind: &'static str,
    parent_prec: u8,
) -> Result<String, EmissionError> {
    let s = render_expr(expr, schema, op_kind)?;
    if let Expression::Binary(b) = expr {
        if binop_precedence(&b.op) < parent_prec {
            return Ok(format!("({s})"));
        }
    }
    Ok(s)
}

/// Operator precedence — hand-copied from legacy `BinaryOpExt::precedence`
/// (`generator/mod.rs:2696-2715`) so `render_binary` can parenthesise without
/// re-importing the trait.
fn binop_precedence(op: &BinaryOp) -> u8 {
    match op {
        BinaryOp::Or => 1,
        BinaryOp::And => 2,
        BinaryOp::Eq
        | BinaryOp::NotEq
        | BinaryOp::Lt
        | BinaryOp::LtEq
        | BinaryOp::Gt
        | BinaryOp::GtEq => 3,
        BinaryOp::BitwiseOr => 4,
        BinaryOp::BitwiseXor => 5,
        BinaryOp::BitwiseAnd => 6,
        BinaryOp::Add | BinaryOp::Sub | BinaryOp::Concat => 7,
        BinaryOp::Mul | BinaryOp::Div | BinaryOp::Mod => 8,
    }
}

/// Render a unary op. Mirrors legacy `gen_unary` at `generator/mod.rs:1656-1666`.
fn render_unary(
    u: &UnaryExpression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    let operand = render_expr(&u.operand, schema, op_kind)?;
    Ok(match &u.op {
        UnaryOp::Not => format!("NOT ({operand})"),
        UnaryOp::Negate => format!("-({operand})"),
        UnaryOp::IsNull => format!("({operand}) IS NULL"),
        UnaryOp::IsNotNull => format!("({operand}) IS NOT NULL"),
        UnaryOp::IsNaN => format!("isnan({operand})"),
        UnaryOp::IsNotNaN => format!("NOT isnan({operand})"),
    })
}

/// Render a CAST/TRY_CAST. Mirrors legacy `gen_cast` at
/// `generator/mod.rs:1757-1775` — for float → integer targets, wrap with
/// `TRUNC()` so Spark's truncate-toward-zero semantics match DuckDB.
fn render_cast(
    c: &CastExpression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    let expr = render_expr(&c.expr, schema, op_kind)?;
    let ty = TypeMapper::to_duckdb(&c.to_type);
    let is_integer_target = matches!(
        c.to_type,
        DataType::Integer | DataType::Long | DataType::Short | DataType::Byte
    );
    let src_type = c.expr.data_type(schema);
    let is_float_source = matches!(src_type, DataType::Double | DataType::Float);
    if c.try_cast {
        Ok(format!("TRY_CAST({expr} AS {ty})"))
    } else if is_integer_target && is_float_source {
        Ok(format!("CAST(trunc({expr}) AS {ty})"))
    } else {
        Ok(format!("CAST({expr} AS {ty})"))
    }
}

/// Render a CASE WHEN … END expression. Mirrors legacy `gen_case_when` at
/// `generator/mod.rs:1777-1793`.
fn render_case_when(
    cw: &CaseWhenExpression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    let mut s = "CASE".to_string();
    if let Some(base) = &cw.base {
        s.push(' ');
        s.push_str(&render_expr(base, schema, op_kind)?);
    }
    for (cond, result) in &cw.branches {
        let c = render_expr(cond, schema, op_kind)?;
        let r = render_expr(result, schema, op_kind)?;
        s.push_str(&format!(" WHEN {c} THEN {r}"));
    }
    if let Some(else_e) = &cw.else_expr {
        s.push_str(&format!(" ELSE {}", render_expr(else_e, schema, op_kind)?));
    }
    s.push_str(" END");
    Ok(s)
}

/// Render a function call. This is the ~50-arm hand-copied translation
/// table for scalar (non-aggregate, non-window) Spark → DuckDB function
/// shapes. The shapes are duplicated from the legacy `FunctionRegistry`
/// at `crates/core/src/functions/mod.rs` — Slice C.2's contamination
/// barrier per architecture plan §1.
///
/// Unknown functions surface as [`EmissionError::UnsupportedFunction`]
/// which the caller in `service.rs` treats as fallback-eligible.
fn render_function_call(
    f: &FunctionCall,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    // First: SQL-operator pseudo-functions Spark sends as UnresolvedFunction
    // but DuckDB expects as operators. Matches legacy handling in
    // `gen_function_call` (`generator/mod.rs:1671-1716`).
    let lower = f.name.to_ascii_lowercase();
    match lower.as_str() {
        "like" if f.args.len() == 2 => {
            let left = render_expr(&f.args[0], schema, op_kind)?;
            let right = render_expr(&f.args[1], schema, op_kind)?;
            return Ok(format!("{left} LIKE {right}"));
        }
        "ilike" if f.args.len() == 2 => {
            let left = render_expr(&f.args[0], schema, op_kind)?;
            let right = render_expr(&f.args[1], schema, op_kind)?;
            return Ok(format!("{left} ILIKE {right}"));
        }
        "rlike" if f.args.len() == 2 => {
            let left = render_expr(&f.args[0], schema, op_kind)?;
            let right = render_expr(&f.args[1], schema, op_kind)?;
            return Ok(format!("regexp_matches({left}, {right})"));
        }
        "in" if f.args.len() >= 2 => {
            let expr = render_expr(&f.args[0], schema, op_kind)?;
            let list = f.args[1..]
                .iter()
                .map(|a| render_expr(a, schema, op_kind))
                .collect::<Result<Vec<_>, _>>()?
                .join(", ");
            return Ok(format!("{expr} IN ({list})"));
        }
        _ => {}
    }

    // Pre-render args once; each per-function arm reuses them.
    let arg_sqls: Vec<String> = f
        .args
        .iter()
        .map(|a| render_expr(a, schema, op_kind))
        .collect::<Result<Vec<_>, _>>()?;
    let arg_refs: Vec<&str> = arg_sqls.iter().map(|s| s.as_str()).collect();
    let joined = || arg_refs.join(", ");

    let sql: String = match lower.as_str() {
        // ── String direct-mapped ──────────────────────────────────────────
        "upper" => format!("UPPER({})", joined()),
        "lower" => format!("LOWER({})", joined()),
        "length" | "char_length" | "character_length" => format!("LENGTH({})", joined()),
        "bit_length" => format!("BIT_LENGTH({})", joined()),
        "trim" => format!("TRIM({})", joined()),
        "ltrim" => format!("LTRIM({})", joined()),
        "rtrim" => format!("RTRIM({})", joined()),
        "lpad" => format!("LPAD({})", joined()),
        "rpad" => format!("RPAD({})", joined()),
        "repeat" => format!("REPEAT({})", joined()),
        "concat_ws" => format!("CONCAT_WS({})", joined()),
        "replace" => format!("REPLACE({})", joined()),
        "translate" => format!("TRANSLATE({})", joined()),
        "ascii" => format!("ASCII({})", joined()),
        "chr" | "char" => format!("CHR({})", joined()),
        "hex" => format!("HEX({})", joined()),
        "base64" => format!("BASE64({})", joined()),
        "unbase64" => format!("DECODE({})", joined()),
        "left" => format!("LEFT({})", joined()),
        "right" => format!("RIGHT({})", joined()),
        "md5" => format!("MD5({})", joined()),
        "sha" | "sha1" | "sha2" => format!("SHA256({})", arg_refs.first().copied().unwrap_or("")),
        "levenshtein" => format!("LEVENSHTEIN({})", joined()),
        "url_decode" => format!("URL_DECODE({})", joined()),
        "url_encode" => format!("URL_ENCODE({})", joined()),
        "printf" | "format_string" => format!("PRINTF({})", joined()),
        "startswith" => format!("STARTS_WITH({})", joined()),
        "endswith" => format!("ENDS_WITH({})", joined()),

        // ── String custom ────────────────────────────────────────────────
        "substring" | "substr" => format!("SUBSTR({})", joined()),
        "concat" => {
            // Spark propagates NULL; DuckDB CONCAT() treats NULL as ''.
            // Emit `a || b || ...` so NULL propagation matches Spark.
            if arg_refs.is_empty() {
                "''".to_string()
            } else {
                arg_refs.join(" || ")
            }
        }
        "unhex" => {
            let a = arg_refs.first().copied().unwrap_or("");
            format!("FROM_HEX({a})")
        }
        "reverse" => {
            // Polymorphic: LIST_REVERSE for arrays, REVERSE for strings.
            // For unknown/string types, delegate to the `_spark_reverse`
            // session macro (registered at session startup).
            if let Some(a) = f.args.first() {
                match a.data_type(schema) {
                    DataType::Array(_, _) => format!("LIST_REVERSE({})", arg_refs[0]),
                    DataType::String => format!("REVERSE({})", arg_refs[0]),
                    _ => format!("_spark_reverse({})", arg_refs[0]),
                }
            } else {
                "_spark_reverse()".to_string()
            }
        }
        "locate" => match arg_refs.len() {
            0 | 1 => "0".to_string(),
            2 => {
                let sub = arg_refs[0];
                let s = arg_refs[1];
                format!("CASE WHEN {s} IS NULL THEN NULL ELSE INSTR({s}, {sub}) END")
            }
            _ => {
                let sub = arg_refs[0];
                let s = arg_refs[1];
                let p = arg_refs[2];
                format!(
                    "CASE WHEN {s} IS NULL THEN NULL WHEN INSTR(SUBSTR({s}, {p}), {sub}) > 0 \
                     THEN INSTR(SUBSTR({s}, {p}), {sub}) + ({p}) - 1 ELSE 0 END"
                )
            }
        },
        "instr" => match arg_refs.len() {
            3 => {
                let s = arg_refs[0];
                let sub = arg_refs[1];
                let p = arg_refs[2];
                format!(
                    "(CASE WHEN INSTR(SUBSTR({s}, {p}), {sub}) > 0 \
                     THEN INSTR(SUBSTR({s}, {p}), {sub}) + ({p}) - 1 ELSE 0 END)"
                )
            }
            _ => format!("INSTR({})", joined()),
        },
        "regexp_replace" => match arg_refs.len() {
            3 => format!(
                "REGEXP_REPLACE({}, {}, {}, 'g')",
                arg_refs[0], arg_refs[1], arg_refs[2]
            ),
            _ => format!("REGEXP_REPLACE({})", joined()),
        },
        "regexp_extract" => match arg_refs.len() {
            2 => format!("REGEXP_EXTRACT({}, {})", arg_refs[0], arg_refs[1]),
            3 => format!(
                "REGEXP_EXTRACT({}, {}, {})",
                arg_refs[0], arg_refs[1], arg_refs[2]
            ),
            _ => format!("REGEXP_EXTRACT({})", joined()),
        },
        "split" => match arg_refs.len() {
            2 => format!("STR_SPLIT_REGEX({}, {})", arg_refs[0], arg_refs[1]),
            _ => format!("STR_SPLIT_REGEX({})", joined()),
        },
        "overlay" => match arg_refs.len() {
            3 => {
                let (s, r, p) = (arg_refs[0], arg_refs[1], arg_refs[2]);
                format!("LEFT({s}, ({p}) - 1) || ({r}) || SUBSTRING({s}, ({p}) + LENGTH({r}))")
            }
            4 => {
                let (s, r, p, l) = (arg_refs[0], arg_refs[1], arg_refs[2], arg_refs[3]);
                format!("LEFT({s}, ({p}) - 1) || ({r}) || SUBSTRING({s}, ({p}) + ({l}))")
            }
            _ => "NULL".to_string(),
        },
        "format_number" => {
            if arg_refs.len() >= 2 {
                format!(
                    "format('{{:,.' || CAST(({}) AS VARCHAR) || 'f}}', {})",
                    arg_refs[1], arg_refs[0]
                )
            } else {
                format!(
                    "CAST({} AS VARCHAR)",
                    arg_refs.first().copied().unwrap_or("")
                )
            }
        }
        "soundex" => format!("soundex({})", arg_refs.first().copied().unwrap_or("")),
        "initcap" => format!("initcap({})", arg_refs.first().copied().unwrap_or("")),

        // ── Math direct-mapped ───────────────────────────────────────────
        "abs" => format!("ABS({})", joined()),
        "ceil" | "ceiling" => format!("CEIL({})", joined()),
        "floor" => format!("FLOOR({})", joined()),
        "sqrt" => format!("SQRT({})", joined()),
        "cbrt" => format!("CBRT({})", joined()),
        "exp" => format!("EXP({})", joined()),
        "pow" | "power" => format!("POW({})", joined()),
        "ln" => format!("LN({})", joined()),
        "log2" => format!("LOG2({})", joined()),
        "log10" => format!("LOG10({})", joined()),
        "sin" => format!("SIN({})", joined()),
        "cos" => format!("COS({})", joined()),
        "tan" => format!("TAN({})", joined()),
        "asin" => format!("ASIN({})", joined()),
        "acos" => format!("ACOS({})", joined()),
        "atan" => format!("ATAN({})", joined()),
        "atan2" => format!("ATAN2({})", joined()),
        "sinh" => format!("SINH({})", joined()),
        "cosh" => format!("COSH({})", joined()),
        "tanh" => format!("TANH({})", joined()),
        "degrees" => format!("DEGREES({})", joined()),
        "radians" => format!("RADIANS({})", joined()),
        "sign" | "signum" => format!("SIGN({})", joined()),
        "hypot" => format!("HYPOT({})", joined()),
        "greatest" => format!("GREATEST({})", joined()),
        "least" => format!("LEAST({})", joined()),
        "factorial" => format!("FACTORIAL({})", joined()),
        "expm1" => format!("EXPM1({})", joined()),
        "log1p" => format!("LOG1P({})", joined()),
        "round" | "bround" => format!("ROUND({})", joined()),
        "width_bucket" => format!("WIDTH_BUCKET({})", joined()),

        // ── Math custom ──────────────────────────────────────────────────
        "log" => match arg_refs.len() {
            1 => format!("LN({})", arg_refs[0]),
            2 => format!("LOG({}, {})", arg_refs[0], arg_refs[1]),
            _ => "LN(1)".to_string(),
        },
        "pmod" => {
            if arg_refs.len() >= 2 {
                format!(
                    "(({} % {}) + {}) % {}",
                    arg_refs[0], arg_refs[1], arg_refs[1], arg_refs[1]
                )
            } else {
                "0".to_string()
            }
        }
        "mod" => {
            if arg_refs.len() >= 2 {
                format!("({} % {})", arg_refs[0], arg_refs[1])
            } else {
                "0".to_string()
            }
        }
        "shiftleft" => {
            if arg_refs.len() >= 2 {
                format!("({} << {})", arg_refs[0], arg_refs[1])
            } else {
                "0".to_string()
            }
        }
        "shiftright" | "shiftrightunsigned" => {
            if arg_refs.len() >= 2 {
                format!("({} >> {})", arg_refs[0], arg_refs[1])
            } else {
                "0".to_string()
            }
        }
        "bit_count" => format!("BIT_COUNT({})", arg_refs.first().copied().unwrap_or("0")),
        "bit_get" | "getbit" => {
            if arg_refs.len() >= 2 {
                format!("((CAST({} AS BIGINT) >> {}) & 1)", arg_refs[0], arg_refs[1])
            } else {
                "0".to_string()
            }
        }
        "conv" => {
            if arg_refs.len() >= 3 {
                format!("CONV({}, {}, {})", arg_refs[0], arg_refs[1], arg_refs[2])
            } else {
                arg_refs.first().copied().unwrap_or("NULL").to_string()
            }
        }
        "nanvl" => {
            if arg_refs.len() >= 2 {
                format!(
                    "CASE WHEN ISNAN({0}) THEN {1} ELSE {0} END",
                    arg_refs[0], arg_refs[1]
                )
            } else {
                arg_refs.first().copied().unwrap_or("NULL").to_string()
            }
        }

        // ── Date/time direct-mapped ──────────────────────────────────────
        "year" => format!("YEAR({})", joined()),
        "month" => format!("MONTH({})", joined()),
        "day" | "dayofmonth" => format!("DAY({})", joined()),
        "dayofyear" => format!("DAYOFYEAR({})", joined()),
        "weekofyear" => format!("WEEKOFYEAR({})", joined()),
        "quarter" => format!("QUARTER({})", joined()),
        "hour" => format!("HOUR({})", joined()),
        "minute" => format!("MINUTE({})", joined()),
        "second" => format!("SECOND({})", joined()),
        "last_day" => format!("LAST_DAY({})", joined()),
        "date_trunc" | "trunc" => format!("DATE_TRUNC({})", joined()),
        "make_date" => format!("MAKE_DATE({})", joined()),
        "make_timestamp" => format!("MAKE_TIMESTAMP({})", joined()),

        // ── Date/time custom ─────────────────────────────────────────────
        "date_add" => {
            if arg_refs.len() >= 2 {
                format!(
                    "CAST(({} + INTERVAL ({}) DAY) AS DATE)",
                    arg_refs[0], arg_refs[1]
                )
            } else {
                arg_refs.first().copied().unwrap_or("").to_string()
            }
        }
        "date_sub" => {
            if arg_refs.len() >= 2 {
                format!(
                    "CAST(({} - INTERVAL ({}) DAY) AS DATE)",
                    arg_refs[0], arg_refs[1]
                )
            } else {
                arg_refs.first().copied().unwrap_or("").to_string()
            }
        }
        "datediff" => {
            if arg_refs.len() >= 2 {
                format!(
                    "DATE_DIFF('day', CAST({} AS DATE), CAST({} AS DATE))",
                    arg_refs[1], arg_refs[0]
                )
            } else {
                "0".to_string()
            }
        }
        "add_months" => {
            if arg_refs.len() >= 2 {
                format!(
                    "CAST(({} + INTERVAL ({}) MONTH) AS DATE)",
                    arg_refs[0], arg_refs[1]
                )
            } else {
                arg_refs.first().copied().unwrap_or("").to_string()
            }
        }
        "months_between" => {
            if arg_refs.len() >= 2 {
                let d1 = arg_refs[0];
                let d2 = arg_refs[1];
                format!(
                    "((YEAR(CAST({d1} AS DATE)) - YEAR(CAST({d2} AS DATE))) * 12 \
                     + (MONTH(CAST({d1} AS DATE)) - MONTH(CAST({d2} AS DATE))) \
                     + CASE WHEN \
                         DAY(CAST({d1} AS DATE)) = DAY(LAST_DAY(CAST({d1} AS DATE))) \
                         AND DAY(CAST({d2} AS DATE)) = DAY(LAST_DAY(CAST({d2} AS DATE))) \
                       THEN 0.0 \
                       ELSE (DAY(CAST({d1} AS DATE)) - DAY(CAST({d2} AS DATE))) / 31.0 \
                       END)"
                )
            } else {
                "0".to_string()
            }
        }
        "dayofweek" => format!(
            "(DAYOFWEEK({}) + 1)",
            arg_refs.first().copied().unwrap_or("")
        ),
        "weekday" => format!("WEEKDAY({})", arg_refs.first().copied().unwrap_or("")),
        "next_day" => {
            if arg_refs.len() >= 2 {
                format!("CAST(NEXT_DAY({}, {}) AS DATE)", arg_refs[0], arg_refs[1])
            } else {
                arg_refs.first().copied().unwrap_or("NULL").to_string()
            }
        }

        // ── Conditional ──────────────────────────────────────────────────
        "coalesce" | "nvl" => format!("COALESCE({})", joined()),
        "nullif" => {
            if arg_refs.len() >= 2 {
                format!("NULLIF({}, {})", arg_refs[0], arg_refs[1])
            } else {
                "NULL".to_string()
            }
        }
        "ifnull" => format!("IFNULL({})", joined()),
        "nvl2" => {
            if arg_refs.len() >= 3 {
                format!(
                    "CASE WHEN {} IS NOT NULL THEN {} ELSE {} END",
                    arg_refs[0], arg_refs[1], arg_refs[2]
                )
            } else {
                "NULL".to_string()
            }
        }
        "if" => match arg_refs.len() {
            3 => format!(
                "CASE WHEN {} THEN {} ELSE {} END",
                arg_refs[0], arg_refs[1], arg_refs[2]
            ),
            2 => format!("CASE WHEN {} THEN {} END", arg_refs[0], arg_refs[1]),
            _ => "NULL".to_string(),
        },
        "iff" => {
            if arg_refs.len() >= 3 {
                format!("IF({}, {}, {})", arg_refs[0], arg_refs[1], arg_refs[2])
            } else {
                "NULL".to_string()
            }
        }
        "when" => {
            // Variadic pairs of (cond, val), optional trailing ELSE.
            let mut sql = String::from("CASE");
            let mut i = 0;
            while i + 1 < arg_refs.len() {
                sql.push_str(&format!(" WHEN {} THEN {}", arg_refs[i], arg_refs[i + 1]));
                i += 2;
            }
            if i < arg_refs.len() {
                sql.push_str(&format!(" ELSE {}", arg_refs[i]));
            }
            sql.push_str(" END");
            sql
        }

        // ── Aggregate names (permitted inside `render_aggregate` inner
        // ── expression; direct-mapped shape lives here for reuse) ────────
        "sum" => format!("SUM({})", joined()),
        "avg" | "mean" => format!("AVG({})", joined()),
        "min" => format!("MIN({})", joined()),
        "max" => format!("MAX({})", joined()),
        "count" => {
            if arg_sqls.is_empty() || arg_refs[0] == "*" {
                "COUNT(*)".to_string()
            } else {
                format!("COUNT({})", joined())
            }
        }
        "count_distinct" => {
            if arg_refs.len() == 1 {
                format!("COUNT(DISTINCT {})", arg_refs[0])
            } else {
                let fields: String = arg_refs
                    .iter()
                    .enumerate()
                    .map(|(i, a)| format!("'f{i}': {a}"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("COUNT(DISTINCT {{{fields}}})")
            }
        }
        "approx_count_distinct" => format!("APPROX_COUNT_DISTINCT({})", joined()),
        "stddev" | "stddev_samp" | "std" => format!("STDDEV_SAMP({})", joined()),
        "stddev_pop" => format!("STDDEV_POP({})", joined()),
        "variance" | "var_samp" => format!("VAR_SAMP({})", joined()),
        "var_pop" => format!("VAR_POP({})", joined()),
        "first" => format!("FIRST({})", joined()),
        "last" => format!("LAST({})", joined()),
        "bool_and" | "every" => format!("BOOL_AND({})", joined()),
        "bool_or" => format!("BOOL_OR({})", joined()),
        "any_value" => format!("ANY_VALUE({})", joined()),
        // ── Slice D Phase 1: hash / extension aggregates ─────────────────
        // Legacy oracle: `functions/mod.rs:522-525` (hash / xxhash64),
        // `functions/mod.rs:428-430` (skewness). Extension bindings ship
        // in the ext4 release of `thdck_spark_funcs` (see ADR-020).
        "hash" => format!("spark_hash({})", joined()),
        "xxhash64" => format!("spark_xxhash64({})", joined()),
        "skewness" => format!("spark_skewness({})", joined()),
        // `spark_sum` / `spark_avg` are emitted synthetically by
        // `spark_aggregate_rewrite` when a DECIMAL-typed SUM/AVG needs
        // the ext4 extension for Spark-precise widening; render them
        // through here so the aggregate path can dispatch cleanly.
        "spark_sum" => format!("spark_sum({})", joined()),
        "spark_avg" => format!("spark_avg({})", joined()),
        // ── Slice D Phase 1: native-DuckDB scalar/aggregate additions ────
        // Legacy oracle: `functions/mod.rs:1332-1334` (crc32),
        // `functions/mod.rs:460-465` (percentile_approx → approx_quantile),
        // `functions/mod.rs:423-425` (kurtosis → KURTOSIS_POP).
        "crc32" => format!("CRC32({})", arg_refs.first().copied().unwrap_or("")),
        "percentile_approx" => {
            if arg_refs.len() >= 2 {
                // DuckDB's `approx_quantile(x, pct)` overloads all require the
                // quantile position to be `FLOAT` — DuckDB 1.5.1 has no
                // `(X, DOUBLE)` overload, and it does not implicitly downcast
                // a typed DOUBLE (narrowing). Spark's `percentile_approx(col,
                // pct)` accepts a Double `pct` (Python `float` = binary64), so
                // the analyzer types the literal as `DataType::Double` and
                // `render_literal` correctly emits `<v>::DOUBLE`. τ absorbs
                // this DuckDB idiosyncrasy at the emission arm (INV3) by
                // wrapping arg 1 in an explicit CAST to `FLOAT`; arg 0's type
                // is left untouched so the overall return type still matches
                // Spark's `percentile_approx` on the input column type.
                format!(
                    "approx_quantile({}, CAST({} AS FLOAT))",
                    arg_refs[0], arg_refs[1]
                )
            } else {
                "NULL".to_string()
            }
        }
        "median" => format!("MEDIAN({})", joined()),
        // Verify-first arm (plan §5.1): legacy already maps `kurtosis` →
        // `KURTOSIS_POP` (`functions/mod.rs:423-425`) and legacy passes
        // its differential gate, so the native mapping is the empirically
        // vetted shape.
        "kurtosis" => format!("KURTOSIS_POP({})", joined()),
        // Verify-first arm (plan §5.2): legacy has no explicit arm and
        // falls through to pass-through (`functions/mod.rs:66`), emitting
        // `count_if(args)` verbatim; DuckDB accepts this as native
        // `COUNT_IF`. v2 emits the same shape, so parity with the legacy
        // path is preserved by construction.
        "count_if" => format!("COUNT_IF({})", joined()),

        // ── Misc simple ──────────────────────────────────────────────────
        "isnull" => {
            if let Some(a) = arg_refs.first() {
                format!("({a} IS NULL)")
            } else {
                "FALSE".to_string()
            }
        }
        "isnotnull" => {
            if let Some(a) = arg_refs.first() {
                format!("({a} IS NOT NULL)")
            } else {
                "TRUE".to_string()
            }
        }
        "isnan" => format!("ISNAN({})", joined()),

        // ── Spark-parity return-type CAST wrappers (function level) ──────
        // `grouping` returns TINYINT in Spark, INTEGER in DuckDB. Wrap
        // at the function-call site since callers may compose this into
        // larger expressions (matching legacy shape).
        "grouping" => format!("CAST(grouping({}) AS TINYINT)", joined()),
        // `grouping_id` returns BIGINT in Spark, INTEGER in DuckDB.
        "grouping_id" => format!("CAST(grouping_id({}) AS BIGINT)", joined()),

        // Unknown function — fallback-eligible.
        _ => {
            return Err(EmissionError::UnsupportedFunction {
                op_kind,
                name: f.name.clone(),
            });
        }
    };

    // FunctionCall.distinct handles `count(DISTINCT x)`-shape callsites for
    // non-aggregate contexts. AggregateCall.is_distinct (handled in
    // `render_aggregate`) is the aggregate-context flag; these two flags
    // are independent by design.
    let sql = if f.distinct {
        // Multi-column count(distinct) needs struct wrapping per legacy.
        if f.name.eq_ignore_ascii_case("count") && arg_sqls.len() > 1 {
            let fields: String = arg_sqls
                .iter()
                .enumerate()
                .map(|(i, a)| format!("'f{i}': {a}"))
                .collect::<Vec<_>>()
                .join(", ");
            format!("COUNT(DISTINCT {{{fields}}})")
        } else {
            inject_distinct(sql)
        }
    } else {
        sql
    };
    Ok(sql)
}

/// Return the Spark-parity CAST target for a scalar (non-aggregate)
/// expression whose Spark return type diverges from DuckDB's default.
/// Consulted by [`render_projection_slot`] — the CAST is applied at
/// projection level to match legacy shape (see plan §Layer 1).
///
/// C.2 handles the divergences that surface via `Expression::Binary`:
/// integer / integer → DOUBLE (Spark promotes; DuckDB does integer div).
/// Function-level Spark-parity CASTs (`grouping`, `grouping_id`) are
/// applied inside [`render_function_call`] because their CAST-in-place
/// matches legacy shape and composes naturally into larger expressions.
fn spark_return_cast(expr: &Expression, schema: &Schema) -> Option<DataType> {
    match expr {
        Expression::Binary(b) if matches!(b.op, BinaryOp::Div) => {
            let lt = b.left.data_type(schema);
            let rt = b.right.data_type(schema);
            if lt.is_integral() && rt.is_integral() {
                Some(DataType::Double)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// Return the Spark-parity aggregate return-type CAST — the aggregate
/// analogue of [`spark_return_cast`]. Called inside [`render_aggregate`]
/// where `agg.func` is the aggregate call expression.
///
/// Slice C.2 handles the native-DuckDB-return divergences:
/// `SUM(integer)` returns HUGEINT in DuckDB but BIGINT in Spark; and
/// `AVG(integer)` returns DECIMAL in DuckDB but DOUBLE in Spark.
///
/// DECIMAL SUM/AVG rewrites (rename `SUM`/`AVG` → `spark_sum`/`spark_avg`)
/// live in the sibling [`spark_aggregate_rewrite`] helper (Slice D Phase 1)
/// because they must alter both the function name and the outer CAST
/// target — a shape this `Option<DataType>` return cannot express.
fn spark_aggregate_return_cast(func: &Expression, schema: &Schema) -> Option<DataType> {
    // Unwrap Alias so an aliased sum still gets the CAST.
    let inner: &Expression = match func {
        Expression::Alias(a) => &a.expr,
        other => other,
    };
    let call = match inner {
        Expression::FunctionCall(f) => f,
        _ => return None,
    };
    // When the input schema is empty (SQL path without table enrichment),
    // type inference on args is unreliable — skip the rewrite and let
    // DuckDB handle natively (matches legacy `apply_agg_type_casts`).
    if schema.is_empty() {
        return None;
    }
    let name = call.name.to_ascii_lowercase();
    let arg = call.args.first()?;
    let arg_type = arg.data_type(schema);
    match name.as_str() {
        "sum" | "sum_distinct" => match arg_type {
            DataType::Byte | DataType::Short | DataType::Integer | DataType::Long => {
                Some(DataType::Long)
            }
            // DECIMAL routes through `spark_aggregate_rewrite`, not here.
            _ => None,
        },
        "avg" | "mean" => match arg_type {
            DataType::Byte | DataType::Short | DataType::Integer | DataType::Long => {
                Some(DataType::Double)
            }
            // DECIMAL routes through `spark_aggregate_rewrite`, not here.
            _ => None,
        },
        _ => None,
    }
}

/// Slice D Phase 1 — DECIMAL-argument `SUM` / `AVG` route through the
/// `spark_sum` / `spark_avg` extension functions so Spark's widened
/// DECIMAL return types (precision/scale) are preserved exactly.
///
/// Returns `Some((rewritten_expr, outer_cast_target))`:
///   * `rewritten_expr` is the original expression with the inner
///     `FunctionCall.name` rebound to `spark_sum` or `spark_avg` (Alias
///     wrapping is preserved so `spark_column_name` picks up the right
///     output name).
///   * `outer_cast_target` is the widened DECIMAL type
///     [`render_aggregate`] wraps the emitted SQL in via `CAST(... AS ...)`.
///
/// Cast-type formulas duplicate legacy `spark_decimal_agg_type`
/// (`generator/mod.rs:2543-2556`) verbatim — pure arithmetic, no legacy
/// import required (Slice C.2's INV3 contamination barrier).
///
/// Returns `None` for every other aggregate shape (integer args are
/// handled by [`spark_aggregate_return_cast`]; non-decimal / non-integer
/// args need no wrapping).
fn spark_aggregate_rewrite(func: &Expression, schema: &Schema) -> Option<(Expression, DataType)> {
    // Unwrap Alias so we can inspect the underlying FunctionCall while
    // preserving the alias binding on the rebuilt expression.
    let (alias_wrap, call) = match func {
        Expression::Alias(a) => match a.expr.as_ref() {
            Expression::FunctionCall(f) => (Some(a.alias.clone()), f),
            _ => return None,
        },
        Expression::FunctionCall(f) => (None, f),
        _ => return None,
    };
    // Type inference on args is unreliable when the input schema is
    // empty (SQL path without table enrichment). Skip the rewrite and
    // let DuckDB handle natively — matches legacy `apply_agg_type_casts`.
    if schema.is_empty() {
        return None;
    }
    let name = call.name.to_ascii_lowercase();
    let arg = call.args.first()?;
    let (precision, scale) = match arg.data_type(schema) {
        DataType::Decimal { precision, scale } => (precision, scale),
        _ => return None,
    };
    let (new_name, cast_target) = match name.as_str() {
        // Cast-type formulas duplicated from legacy `spark_decimal_agg_type`
        // (`generator/mod.rs:2543-2556`).
        "sum" | "sum_distinct" => {
            let p = ((precision as u16) + 10).min(38) as u8;
            (
                "spark_sum",
                DataType::Decimal {
                    precision: p,
                    scale,
                },
            )
        }
        "avg" | "mean" => {
            let new_p = ((precision as u16) + 4).min(38) as u8;
            let new_s = (scale + 4).min(18).min(new_p);
            (
                "spark_avg",
                DataType::Decimal {
                    precision: new_p,
                    scale: new_s,
                },
            )
        }
        _ => return None,
    };
    // DISTINCT is injected at aggregate level via `agg.is_distinct` in
    // `render_aggregate` (see ~line 642); do NOT propagate `call.distinct`
    // here or we get `spark_sum(DISTINCT DISTINCT x)` for `sum_distinct(decimal)`.
    let rewritten_call = Expression::FunctionCall(FunctionCall {
        name: new_name.to_owned(),
        args: call.args.clone(),
        distinct: false,
    });
    let rewritten = match alias_wrap {
        Some(alias) => Expression::Alias(AliasExpression {
            expr: Box::new(rewritten_call),
            alias,
        }),
        None => rewritten_call,
    };
    Some((rewritten, cast_target))
}

fn quote_ident(name: &str) -> String {
    if !name.contains('"') {
        let mut s = String::with_capacity(name.len() + 2);
        s.push('"');
        s.push_str(name);
        s.push('"');
        s
    } else {
        format!("\"{}\"", name.replace('"', "\"\""))
    }
}

/// Insert `DISTINCT ` immediately after the first `(` in a function-call
/// SQL fragment, turning e.g. `count("x")` into `count(DISTINCT "x")`.
///
/// Mirrors legacy `inject_distinct` in
/// `crates/core/src/generator/mod.rs:2683`. When the fragment has no
/// `(` (a bare column ref, say), the fragment is returned unchanged —
/// the analyzer only sets `AggregateCall.is_distinct` on function-call
/// aggregates, so the missing-paren case is unreachable in practice.
fn inject_distinct(mut s: String) -> String {
    if let Some(pos) = s.find('(') {
        s.insert_str(pos + 1, "DISTINCT ");
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::expression::{AliasExpression, BinaryExpression, BinaryOp, UnresolvedColumn};
    use crate::transpiler_v2::analyzer::{analyze, BaseTypes};
    use crate::transpiler_v2::ast::{CommonAst, CommonOp, Project, TableScan};
    use crate::types::{DataType, StructField, StructType};

    fn base_types() -> BaseTypes {
        let mut m = BaseTypes::new();
        m.insert(
            "nums".to_string(),
            StructType::new(vec![
                StructField::nullable("a", DataType::Integer),
                StructField::nullable("lng", DataType::Long),
            ]),
        );
        m
    }

    fn empty_schema() -> Schema {
        StructType::empty()
    }

    // ── Dispatch-level smoke tests (retained from C.1) ────────────────────

    #[test]
    fn dispatch_project_emits_select_from_child() {
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                projections: vec![Expression::Alias(AliasExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Add,
                        left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "a".to_string(),
                            qualifier: None,
                        })),
                        right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "lng".to_string(),
                            qualifier: None,
                        })),
                    })),
                    alias: "r".to_string(),
                })],
            }),
        };
        let typed = analyze(ast, &base_types()).expect("analyze must succeed");
        let sql = dispatch(&typed.root).expect("dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.starts_with("SELECT "),
            "expected SELECT prefix, got {s:?}"
        );
        assert!(
            s.contains("AS \"r\""),
            "expected `AS \"r\"` alias, got {s:?}"
        );
        assert!(s.contains("FROM ("), "expected FROM subquery, got {s:?}");
    }

    #[test]
    fn dispatch_table_scan_emits_quoted_identifier() {
        let sql = dispatch(&TypedOp::TableScan {
            name: "nums".to_string(),
            schema: StructType::empty(),
        })
        .expect("dispatch table scan must succeed");
        assert_eq!(sql.into_string(), "\"nums\"");
    }

    #[test]
    fn dispatch_range_relation_emits_range_query() {
        let sql = dispatch(&TypedOp::RangeRelation {
            start: 0,
            end: 10,
            step: 1,
            schema: StructType::single("id", DataType::Long),
        })
        .expect("dispatch range must succeed");
        assert!(sql.as_str().contains("range(0, 10, 1)"));
    }

    #[test]
    fn count_distinct_emits_distinct_inside_call() {
        // C2 regression — `AggregateCall.is_distinct` must land inside
        // the emitted function call as `count(DISTINCT ...)`.
        use crate::expression::FunctionCall;
        use crate::transpiler_v2::analyzer::{TypedAttr, TypedOp};
        use crate::transpiler_v2::ast::AggregateCall;

        let agg = TypedOp::Aggregate {
            input: Box::new(TypedOp::TableScan {
                name: "nums".to_string(),
                schema: StructType::new(vec![StructField::nullable("a", DataType::Integer)]),
            }),
            grouping: Vec::new(),
            grouping_types: Vec::new(),
            aggregates: vec![AggregateCall {
                func: Expression::FunctionCall(FunctionCall {
                    name: "count".to_string(),
                    args: vec![Expression::UnresolvedColumn(
                        crate::expression::UnresolvedColumn {
                            name: "a".to_string(),
                            qualifier: None,
                        },
                    )],
                    distinct: false,
                }),
                is_distinct: true,
                filter: None,
            }],
            aggregate_types: vec![TypedAttr {
                data_type: DataType::Long,
                nullable: false,
            }],
            having: None,
            grouping_sets: None,
            schema: StructType::single("count(a)", DataType::Long),
        };
        let sql = dispatch(&agg).expect("aggregate dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.contains("DISTINCT"),
            "COUNT(DISTINCT a) must emit DISTINCT, got {s:?}"
        );
    }

    #[test]
    fn aliased_relation_emits_alias_and_column_list() {
        let ar = TypedOp::AliasedRelation {
            input: Box::new(TypedOp::TableScan {
                name: "nums".to_string(),
                schema: StructType::new(vec![
                    StructField::nullable("a", DataType::Integer),
                    StructField::nullable("lng", DataType::Long),
                ]),
            }),
            alias: "t".to_string(),
            column_aliases: vec!["x".to_string(), "y".to_string()],
            schema: StructType::new(vec![
                StructField::nullable("x", DataType::Integer),
                StructField::nullable("y", DataType::Long),
            ]),
        };
        let sql = dispatch(&ar).expect("aliased-relation dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.contains("AS \"t\""),
            "aliased relation must carry `AS \"t\"`, got {s:?}"
        );
        assert!(
            s.contains("(\"x\", \"y\")"),
            "aliased relation must carry column-alias list, got {s:?}"
        );

        let ar_bare = TypedOp::AliasedRelation {
            input: Box::new(TypedOp::TableScan {
                name: "nums".to_string(),
                schema: StructType::single("a", DataType::Integer),
            }),
            alias: "t".to_string(),
            column_aliases: Vec::new(),
            schema: StructType::single("a", DataType::Integer),
        };
        let sql = dispatch(&ar_bare).expect("bare aliased-relation must dispatch");
        let s = sql.as_str();
        assert!(
            s.contains("AS \"t\""),
            "bare aliased relation must carry alias, got {s:?}"
        );
        assert!(
            !s.contains("AS \"t\"("),
            "bare aliased relation must not emit a column list, got {s:?}"
        );
    }

    // ── Slice C.2: literal / column / unary / cast smoke tests ────────────

    #[test]
    fn render_literal_int_emits_bare_number() {
        assert_eq!(
            render_literal(&Literal {
                value: LiteralValue::Int(42),
                data_type: DataType::Integer
            }),
            "42"
        );
    }

    #[test]
    fn render_literal_long_carries_bigint_annotation() {
        let s = render_literal(&Literal {
            value: LiteralValue::Long(99),
            data_type: DataType::Long,
        });
        assert!(
            s.ends_with("::BIGINT"),
            "long literal must carry ::BIGINT, got {s:?}"
        );
    }

    #[test]
    fn render_literal_decimal_uses_precision_scale() {
        let s = render_literal(&Literal {
            value: LiteralValue::Decimal("1.234".to_string()),
            data_type: DataType::Decimal {
                precision: 10,
                scale: 3,
            },
        });
        assert_eq!(s, "1.234::DECIMAL(10,3)");
    }

    #[test]
    fn render_literal_string_escapes_quotes() {
        let s = render_literal(&Literal {
            value: LiteralValue::String("it's".to_string()),
            data_type: DataType::String,
        });
        assert_eq!(s, "'it''s'");
    }

    #[test]
    fn render_column_ref_qualified_emits_dotted_name() {
        let s = render_column_ref(&ColumnReference {
            name: "col".to_string(),
            qualifier: Some("t".to_string()),
            data_type: DataType::Integer,
            nullable: true,
        });
        assert_eq!(s, "\"t\".\"col\"");
    }

    #[test]
    fn render_unresolved_column_strips_plan_id_qualifier() {
        let s = render_unresolved_column(&UnresolvedColumn {
            name: "col".to_string(),
            qualifier: Some("__plan_id_5__".to_string()),
        });
        assert_eq!(s, "\"col\"");
    }

    #[test]
    fn render_binary_add_matches_symbolic() {
        let sql = render_expr(
            &Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(Literal::int(1)),
                right: Box::new(Literal::int(2)),
            }),
            &empty_schema(),
            "test",
        )
        .expect("render binary");
        assert_eq!(sql, "1 + 2");
    }

    #[test]
    fn render_binary_or_wraps_lower_precedence_child() {
        // (a AND b) OR c should not add extra parens; but (a OR b) AND c
        // should render AND's operand as (a OR b).
        let a_or_b = Expression::Binary(BinaryExpression {
            op: BinaryOp::Or,
            left: Box::new(Literal::boolean(true)),
            right: Box::new(Literal::boolean(false)),
        });
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::And,
            left: Box::new(a_or_b),
            right: Box::new(Literal::boolean(true)),
        });
        let sql = render_expr(&expr, &empty_schema(), "test").expect("render");
        assert!(
            sql.contains("(TRUE OR FALSE)"),
            "OR inside AND must be parenthesised, got {sql:?}"
        );
    }

    #[test]
    fn render_unary_not_wraps_operand() {
        let sql = render_expr(
            &Expression::Unary(UnaryExpression {
                op: UnaryOp::Not,
                operand: Box::new(Literal::boolean(true)),
            }),
            &empty_schema(),
            "test",
        )
        .expect("render unary");
        assert_eq!(sql, "NOT (TRUE)");
    }

    #[test]
    fn render_unary_isnull_uses_postfix_operator() {
        let sql = render_expr(
            &Expression::Unary(UnaryExpression {
                op: UnaryOp::IsNull,
                operand: Box::new(Literal::int(1)),
            }),
            &empty_schema(),
            "test",
        )
        .expect("render unary");
        assert_eq!(sql, "(1) IS NULL");
    }

    #[test]
    fn render_cast_string_to_int_emits_plain_cast() {
        let sql = render_expr(
            &Expression::Cast(CastExpression {
                expr: Box::new(Literal::string("42")),
                to_type: DataType::Integer,
                try_cast: false,
            }),
            &empty_schema(),
            "test",
        )
        .expect("render cast");
        assert_eq!(sql, "CAST('42' AS INTEGER)");
    }

    #[test]
    fn render_cast_double_to_int_wraps_with_trunc() {
        let sql = render_expr(
            &Expression::Cast(CastExpression {
                expr: Box::new(Literal::double(3.7)),
                to_type: DataType::Integer,
                try_cast: false,
            }),
            &empty_schema(),
            "test",
        )
        .expect("render cast");
        assert!(
            sql.contains("trunc"),
            "float→int must wrap with TRUNC, got {sql:?}"
        );
    }

    #[test]
    fn render_cast_try_cast_uses_try_cast_syntax() {
        let sql = render_expr(
            &Expression::Cast(CastExpression {
                expr: Box::new(Literal::string("x")),
                to_type: DataType::Integer,
                try_cast: true,
            }),
            &empty_schema(),
            "test",
        )
        .expect("render cast");
        assert!(
            sql.starts_with("TRY_CAST("),
            "expected TRY_CAST, got {sql:?}"
        );
    }

    #[test]
    fn render_case_when_no_base_emits_case_when_then_end() {
        let sql = render_expr(
            &Expression::CaseWhen(CaseWhenExpression {
                base: None,
                branches: vec![(Literal::boolean(true), Literal::int(1))],
                else_expr: Some(Box::new(Literal::int(0))),
            }),
            &empty_schema(),
            "test",
        )
        .expect("render case");
        assert_eq!(sql, "CASE WHEN TRUE THEN 1 ELSE 0 END");
    }

    // ── Function-call arms — sample coverage across clusters ──────────────

    fn call(name: &str, args: Vec<Expression>) -> Expression {
        Expression::FunctionCall(FunctionCall {
            name: name.to_string(),
            args,
            distinct: false,
        })
    }

    fn s(sql: Result<String, EmissionError>) -> String {
        sql.expect("render must succeed")
    }

    #[test]
    fn fn_upper_lower_length() {
        let sch = empty_schema();
        assert_eq!(
            s(render_expr(
                &call("upper", vec![Literal::string("a")]),
                &sch,
                "t"
            )),
            "UPPER('a')"
        );
        assert_eq!(
            s(render_expr(
                &call("lower", vec![Literal::string("A")]),
                &sch,
                "t"
            )),
            "LOWER('A')"
        );
        assert_eq!(
            s(render_expr(
                &call("length", vec![Literal::string("abc")]),
                &sch,
                "t"
            )),
            "LENGTH('abc')"
        );
        assert_eq!(
            s(render_expr(
                &call("char_length", vec![Literal::string("abc")]),
                &sch,
                "t"
            )),
            "LENGTH('abc')"
        );
    }

    #[test]
    fn fn_concat_uses_pipe_pipe_for_null_propagation() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call("concat", vec![Literal::string("a"), Literal::string("b")]),
            &sch,
            "t",
        ));
        assert_eq!(sql, "'a' || 'b'");
    }

    #[test]
    fn fn_substring_maps_to_substr() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "substring",
                vec![Literal::string("abc"), Literal::int(1), Literal::int(2)],
            ),
            &sch,
            "t",
        ));
        assert_eq!(sql, "SUBSTR('abc', 1, 2)");
    }

    #[test]
    fn fn_regexp_replace_adds_global_flag_for_3_args() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "regexp_replace",
                vec![
                    Literal::string("aab"),
                    Literal::string("a"),
                    Literal::string("x"),
                ],
            ),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("'g'"),
            "3-arg regexp_replace must add 'g' flag, got {sql:?}"
        );
    }

    #[test]
    fn fn_date_add_wraps_in_cast_as_date() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "date_add",
                vec![Literal::string("2024-01-01"), Literal::int(5)],
            ),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("INTERVAL"),
            "date_add must use INTERVAL, got {sql:?}"
        );
        assert!(
            sql.contains("AS DATE"),
            "date_add must CAST AS DATE, got {sql:?}"
        );
    }

    #[test]
    fn fn_datediff_swaps_argument_order() {
        let sch = empty_schema();
        // Spark: datediff(end, start). Legacy emits DATE_DIFF('day', start, end).
        let sql = s(render_expr(
            &call(
                "datediff",
                vec![Literal::string("2024-01-10"), Literal::string("2024-01-01")],
            ),
            &sch,
            "t",
        ));
        // Should be DATE_DIFF('day', start=2024-01-01, end=2024-01-10)
        assert!(
            sql.starts_with("DATE_DIFF('day',"),
            "datediff shape mismatch, got {sql:?}"
        );
    }

    #[test]
    fn fn_coalesce_nvl_alias() {
        let sch = empty_schema();
        assert_eq!(
            s(render_expr(
                &call("coalesce", vec![Literal::int(1), Literal::int(2)]),
                &sch,
                "t"
            )),
            "COALESCE(1, 2)"
        );
        assert_eq!(
            s(render_expr(
                &call("nvl", vec![Literal::int(1), Literal::int(2)]),
                &sch,
                "t"
            )),
            "COALESCE(1, 2)"
        );
    }

    #[test]
    fn fn_when_variadic_builds_case_expr() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "when",
                vec![
                    Literal::boolean(true),
                    Literal::int(1),
                    Literal::boolean(false),
                    Literal::int(2),
                    Literal::int(0),
                ],
            ),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("WHEN TRUE THEN 1"),
            "expected WHEN clause, got {sql:?}"
        );
        assert!(
            sql.contains("WHEN FALSE THEN 2"),
            "expected second WHEN, got {sql:?}"
        );
        assert!(sql.contains("ELSE 0"), "expected ELSE, got {sql:?}");
    }

    #[test]
    fn fn_nvl2_emits_case_when() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "nvl2",
                vec![Literal::int(1), Literal::string("x"), Literal::string("y")],
            ),
            &sch,
            "t",
        ));
        assert!(
            sql.starts_with("CASE WHEN"),
            "nvl2 must emit CASE WHEN, got {sql:?}"
        );
        assert!(
            sql.contains("IS NOT NULL"),
            "nvl2 must check IS NOT NULL, got {sql:?}"
        );
    }

    #[test]
    fn fn_grouping_wraps_in_tinyint_cast() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call("grouping", vec![Literal::int(1)]),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("AS TINYINT"),
            "grouping must cast to TINYINT, got {sql:?}"
        );
    }

    #[test]
    fn fn_grouping_id_wraps_in_bigint_cast() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call("grouping_id", vec![Literal::int(1)]),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("AS BIGINT"),
            "grouping_id must cast to BIGINT, got {sql:?}"
        );
    }

    #[test]
    fn fn_abs_direct_math() {
        let sch = empty_schema();
        assert_eq!(
            s(render_expr(&call("abs", vec![Literal::int(-3)]), &sch, "t")),
            "ABS(-3)"
        );
    }

    #[test]
    fn fn_log_one_arg_maps_to_ln() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call("log", vec![Literal::double(2.71)]),
            &sch,
            "t",
        ));
        assert!(
            sql.starts_with("LN("),
            "1-arg log must map to LN, got {sql:?}"
        );
    }

    #[test]
    fn fn_pmod_matches_positive_modulo_formula() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call("pmod", vec![Literal::int(-3), Literal::int(5)]),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("% 5"),
            "pmod formula must contain modulo, got {sql:?}"
        );
    }

    #[test]
    fn fn_year_direct_dt() {
        let sch = empty_schema();
        assert_eq!(
            s(render_expr(
                &call("year", vec![Literal::string("2024-01-01")]),
                &sch,
                "t"
            )),
            "YEAR('2024-01-01')"
        );
    }

    #[test]
    fn fn_dayofweek_shifted_by_one() {
        let sch = empty_schema();
        // Spark returns 1=Sun..7=Sat, DuckDB is 0=Sun..6=Sat — add 1.
        let sql = s(render_expr(
            &call("dayofweek", vec![Literal::string("2024-01-01")]),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("+ 1"),
            "dayofweek must add 1 to DuckDB result, got {sql:?}"
        );
    }

    #[test]
    fn fn_unknown_returns_fallback_error() {
        let sch = empty_schema();
        let err = render_expr(&call("no_such_fn", vec![]), &sch, "test")
            .expect_err("unknown function must fail");
        assert!(matches!(err, EmissionError::UnsupportedFunction { .. }));
    }

    #[test]
    fn unsupported_expression_variant_returns_fallback_error() {
        // Window is Slice D territory — verify the emitter refuses it
        // rather than silently mis-emitting.
        use crate::expression::WindowFunction;
        let expr = Expression::Window(WindowFunction {
            func: Box::new(call("row_number", vec![])),
            partition_by: vec![],
            order_by: vec![],
            frame: None,
        });
        let err = render_expr(&expr, &empty_schema(), "Project")
            .expect_err("Window must not render in C.2");
        assert!(matches!(err, EmissionError::UnsupportedExpression { .. }));
    }

    // ── Spark-parity CAST tests ────────────────────────────────────────────

    #[test]
    fn int_div_int_promoted_to_double_at_projection_slot() {
        let sch = StructType::new(vec![
            StructField::nullable("a", DataType::Integer),
            StructField::nullable("b", DataType::Integer),
        ]);
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(Expression::ColumnReference(ColumnReference {
                name: "a".to_string(),
                qualifier: None,
                data_type: DataType::Integer,
                nullable: true,
            })),
            right: Box::new(Expression::ColumnReference(ColumnReference {
                name: "b".to_string(),
                qualifier: None,
                data_type: DataType::Integer,
                nullable: true,
            })),
        });
        let sql = render_projection_slot(&expr, &sch, "Project").expect("render projection");
        assert!(
            sql.contains("AS DOUBLE"),
            "int/int must CAST AS DOUBLE, got {sql:?}"
        );
    }

    #[test]
    fn qualified_star_in_projection_slot_preserves_qualifier() {
        // M1 regression: `render_projection_slot` must delegate to
        // `render_star` for `Expression::Star`, otherwise a qualified
        // `Star { qualifier: Some("t") }` collapses to bare `*`.
        use crate::expression::StarExpression;
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                projections: vec![Expression::Star(StarExpression {
                    qualifier: Some("t".to_string()),
                })],
            }),
        };
        let typed = analyze(ast, &base_types()).expect("analyze must succeed");
        let sql = dispatch(&typed.root).expect("dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.contains("\"t\".*"),
            "qualified Star must emit \"t\".* in SQL, got {s:?}"
        );
        assert!(
            !s.contains("SELECT * FROM"),
            "qualified Star must not collapse to bare *, got {s:?}"
        );
    }

    #[test]
    fn alias_of_int_div_gets_double_cast() {
        // M4 regression: `render_projection_slot`'s Alias arm must consult
        // `spark_return_cast` so an aliased `Div(Long, Long)` gets wrapped
        // in `CAST(... AS DOUBLE) AS "r"` instead of losing the projection-
        // level DOUBLE promotion.
        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(TableScan {
                    name: "nums".to_string(),
                    schema: StructType::empty(),
                })),
                projections: vec![Expression::Alias(AliasExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Div,
                        left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "lng".to_string(),
                            qualifier: None,
                        })),
                        right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "lng".to_string(),
                            qualifier: None,
                        })),
                    })),
                    alias: "r".to_string(),
                })],
            }),
        };
        let typed = analyze(ast, &base_types()).expect("analyze must succeed");
        let sql = dispatch(&typed.root).expect("dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.contains("CAST("),
            "alias of Long/Long Div must emit CAST(...), got {s:?}"
        );
        assert!(
            s.contains("AS DOUBLE"),
            "alias of Long/Long Div must emit AS DOUBLE, got {s:?}"
        );
        assert!(
            s.contains("AS \"r\""),
            "alias binding must still emit AS \"r\", got {s:?}"
        );
    }

    /// C.3-4 regression: `Div(Decimal, Decimal)` on a typed AST must route
    /// through `render_spark_decimal_div` so the emitted SQL contains a call
    /// to the `spark_decimal_div` extension. Locks in the current correct
    /// routing behavior so a future refactor of `render_binary`'s Div arm
    /// cannot silently regress DECIMAL/DECIMAL division back to a naked
    /// `d1 / d2` (which would violate Spark's ROUND_HALF_UP semantics).
    ///
    /// Uses `Decimal(5, 2) / Decimal(3, 1)` per the C.3-4 prompt.
    #[test]
    fn decimal_div_decimal_routes_through_spark_decimal_div() {
        let nums_schema = StructType::new(vec![
            StructField::nullable(
                "a",
                DataType::Decimal {
                    precision: 5,
                    scale: 2,
                },
            ),
            StructField::nullable(
                "b",
                DataType::Decimal {
                    precision: 3,
                    scale: 1,
                },
            ),
        ]);
        let mut bt = BaseTypes::new();
        bt.insert("nums".to_string(), nums_schema.clone());

        let ast = CommonAst {
            root: CommonOp::Project(Project {
                input: Box::new(CommonOp::TableScan(TableScan {
                    name: "nums".to_string(),
                    schema: nums_schema,
                })),
                projections: vec![Expression::Alias(AliasExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Div,
                        left: Box::new(Expression::ColumnReference(ColumnReference {
                            name: "a".to_string(),
                            qualifier: None,
                            data_type: DataType::Decimal {
                                precision: 5,
                                scale: 2,
                            },
                            nullable: true,
                        })),
                        right: Box::new(Expression::ColumnReference(ColumnReference {
                            name: "b".to_string(),
                            qualifier: None,
                            data_type: DataType::Decimal {
                                precision: 3,
                                scale: 1,
                            },
                            nullable: true,
                        })),
                    })),
                    alias: "r".to_string(),
                })],
            }),
        };
        let typed = analyze(ast, &bt).expect("analyze must succeed");
        let sql = dispatch(&typed.root).expect("dispatch must succeed");
        let s = sql.as_str();
        assert!(
            s.contains("spark_decimal_div("),
            "Decimal/Decimal Div must route through spark_decimal_div, got {s:?}"
        );
        assert!(
            s.contains("AS \"r\""),
            "alias binding must still emit AS \"r\", got {s:?}"
        );
    }

    /// C.3-5 regression: `SUM(Decimal(9, 2))` on a typed AST must route
    /// through `spark_aggregate_rewrite` so the emitted aggregate becomes
    /// `spark_sum(...)` (extension function) and `render_aggregate` wraps
    /// it in the widened outer CAST target `DECIMAL(19, 2)` — precision
    /// `min(9 + 10, 38) = 19`, scale unchanged at `2`, per the legacy
    /// `spark_decimal_agg_type` formula duplicated in
    /// [`spark_aggregate_rewrite`].
    ///
    /// This behaviour was introduced by Slice D Phase 1 (the
    /// `spark_aggregate_rewrite` helper) together with the C.3-4
    /// `Decimal128` LocalRelation payload fix. Before that combination
    /// landed, corpus case `agg-007`
    /// (`emp.agg(F.sum("bonus").alias("sum_bonus"))`, `bonus: Decimal(9,2)`)
    /// was red on v2. The test locks the invariant so a future refactor
    /// of `render_aggregate` or `spark_aggregate_rewrite` cannot silently
    /// regress DECIMAL SUM routing.
    #[test]
    fn sum_of_decimal_routes_through_spark_sum() {
        use crate::expression::FunctionCall;
        use crate::transpiler_v2::analyzer::{TypedAttr, TypedOp};
        use crate::transpiler_v2::ast::AggregateCall;

        let bonus_type = DataType::Decimal {
            precision: 9,
            scale: 2,
        };
        let widened_type = DataType::Decimal {
            precision: 19,
            scale: 2,
        };
        let agg = TypedOp::Aggregate {
            input: Box::new(TypedOp::TableScan {
                name: "emp".to_string(),
                schema: StructType::new(vec![StructField::nullable("bonus", bonus_type.clone())]),
            }),
            grouping: Vec::new(),
            grouping_types: Vec::new(),
            aggregates: vec![AggregateCall {
                func: Expression::FunctionCall(FunctionCall {
                    name: "sum".to_string(),
                    args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "bonus".to_string(),
                        qualifier: None,
                    })],
                    distinct: false,
                }),
                is_distinct: false,
                filter: None,
            }],
            aggregate_types: vec![TypedAttr {
                data_type: widened_type.clone(),
                nullable: true,
            }],
            having: None,
            grouping_sets: None,
            schema: StructType::single("sum(bonus)", widened_type),
        };
        let sql = dispatch(&agg).expect("aggregate dispatch");
        let s = sql.as_str();
        assert!(
            s.contains("spark_sum("),
            "SUM(Decimal) must route through spark_sum(...), got {s:?}"
        );
        // Note: `TypeMapper::to_duckdb` renders DECIMAL without a space
        // after the comma (`DECIMAL(p,s)`), so the outer CAST target
        // appears as `AS DECIMAL(19,2)` even though the ADR-level spec
        // writes it with a space.
        assert!(
            s.contains("AS DECIMAL(19,2)"),
            "SUM(Decimal(9,2)) must wrap in outer CAST AS DECIMAL(19,2), got {s:?}"
        );
    }

    /// C.3-5 companion: `AVG(Decimal(9, 2))` must route through
    /// `spark_avg(...)` and wrap in `CAST(... AS DECIMAL(13, 6))` —
    /// precision `min(9 + 4, 38) = 13`, scale `min(min(2 + 4, 18), 13) = 6`
    /// per the legacy `spark_decimal_agg_type` formula duplicated in
    /// [`spark_aggregate_rewrite`].
    #[test]
    fn avg_of_decimal_routes_through_spark_avg() {
        use crate::expression::FunctionCall;
        use crate::transpiler_v2::analyzer::{TypedAttr, TypedOp};
        use crate::transpiler_v2::ast::AggregateCall;

        let bonus_type = DataType::Decimal {
            precision: 9,
            scale: 2,
        };
        let widened_type = DataType::Decimal {
            precision: 13,
            scale: 6,
        };
        let agg = TypedOp::Aggregate {
            input: Box::new(TypedOp::TableScan {
                name: "emp".to_string(),
                schema: StructType::new(vec![StructField::nullable("bonus", bonus_type.clone())]),
            }),
            grouping: Vec::new(),
            grouping_types: Vec::new(),
            aggregates: vec![AggregateCall {
                func: Expression::FunctionCall(FunctionCall {
                    name: "avg".to_string(),
                    args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "bonus".to_string(),
                        qualifier: None,
                    })],
                    distinct: false,
                }),
                is_distinct: false,
                filter: None,
            }],
            aggregate_types: vec![TypedAttr {
                data_type: widened_type.clone(),
                nullable: true,
            }],
            having: None,
            grouping_sets: None,
            schema: StructType::single("avg(bonus)", widened_type),
        };
        let sql = dispatch(&agg).expect("aggregate dispatch");
        let s = sql.as_str();
        assert!(
            s.contains("spark_avg("),
            "AVG(Decimal) must route through spark_avg(...), got {s:?}"
        );
        assert!(
            s.contains("AS DECIMAL(13,6)"),
            "AVG(Decimal(9,2)) must wrap in outer CAST AS DECIMAL(13,6), got {s:?}"
        );
    }

    /// C.3-6b regression: `percentile_approx(col, quantile)` must wrap the
    /// second argument in `CAST(... AS FLOAT)` at the emission arm. DuckDB
    /// 1.5.1 only exposes `approx_quantile(X, FLOAT)` overloads — no
    /// `(X, DOUBLE)` variant — and does not implicitly downcast a typed
    /// DOUBLE literal to FLOAT (narrowing). Spark serialises Python `float`
    /// as `Double`, so the analyzer types the quantile as `DataType::Double`
    /// and `render_literal` emits `0.5::DOUBLE`. Without the arm-level CAST
    /// wrap, corpus case `agg-013`
    /// (`emp.agg(F.percentile_approx("salary", 0.5).alias("p50"))`,
    /// `salary: Double nullable`) failed with a DuckDB binder error. The
    /// test locks the invariant so a future refactor of the
    /// `percentile_approx` arm cannot silently regress it.
    #[test]
    fn percentile_approx_wraps_quantile_arg_in_cast_as_float() {
        use crate::expression::FunctionCall;
        use crate::transpiler_v2::analyzer::{TypedAttr, TypedOp};
        use crate::transpiler_v2::ast::AggregateCall;

        let agg = TypedOp::Aggregate {
            input: Box::new(TypedOp::TableScan {
                name: "emp".to_string(),
                schema: StructType::new(vec![StructField::nullable("salary", DataType::Double)]),
            }),
            grouping: Vec::new(),
            grouping_types: Vec::new(),
            aggregates: vec![AggregateCall {
                func: Expression::FunctionCall(FunctionCall {
                    name: "percentile_approx".to_string(),
                    args: vec![
                        Expression::UnresolvedColumn(UnresolvedColumn {
                            name: "salary".to_string(),
                            qualifier: None,
                        }),
                        Literal::double(0.5),
                    ],
                    distinct: false,
                }),
                is_distinct: false,
                filter: None,
            }],
            aggregate_types: vec![TypedAttr {
                data_type: DataType::Double,
                nullable: true,
            }],
            having: None,
            grouping_sets: None,
            schema: StructType::single("percentile_approx(salary, 0.5)", DataType::Double),
        };
        let sql = dispatch(&agg).expect("aggregate dispatch");
        let s = sql.as_str();
        assert!(
            s.contains("approx_quantile("),
            "percentile_approx must route through approx_quantile(...), got {s:?}"
        );
        assert!(
            s.contains("CAST(0.5::DOUBLE AS FLOAT)"),
            "percentile_approx arg 1 must be wrapped in CAST(... AS FLOAT) \
             to satisfy DuckDB's approx_quantile(X, FLOAT) overload, got {s:?}"
        );
    }

    #[test]
    fn agg_sum_of_integer_wraps_in_bigint_cast() {
        // Verify `render_aggregate` wraps SUM(int) in CAST AS BIGINT.
        use crate::expression::FunctionCall;
        use crate::transpiler_v2::analyzer::{TypedAttr, TypedOp};
        use crate::transpiler_v2::ast::AggregateCall;

        let agg = TypedOp::Aggregate {
            input: Box::new(TypedOp::TableScan {
                name: "nums".to_string(),
                schema: StructType::new(vec![StructField::nullable("a", DataType::Integer)]),
            }),
            grouping: Vec::new(),
            grouping_types: Vec::new(),
            aggregates: vec![AggregateCall {
                func: Expression::FunctionCall(FunctionCall {
                    name: "sum".to_string(),
                    args: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "a".to_string(),
                        qualifier: None,
                    })],
                    distinct: false,
                }),
                is_distinct: false,
                filter: None,
            }],
            aggregate_types: vec![TypedAttr {
                data_type: DataType::Long,
                nullable: true,
            }],
            having: None,
            grouping_sets: None,
            schema: StructType::single("sum(a)", DataType::Long),
        };
        let sql = dispatch(&agg).expect("aggregate dispatch");
        let s = sql.as_str();
        assert!(
            s.contains("AS BIGINT"),
            "SUM(integer) must CAST AS BIGINT, got {s:?}"
        );
    }

    // ── Union per-column CAST (M3 closure) ─────────────────────────────────

    #[test]
    fn union_widens_child_columns_with_cast() {
        // Child schemas: left has "x: Integer", right has "x: Long".
        // Widened schema: "x: Long". Emit must wrap the Integer child in
        // CAST(x AS BIGINT) so DuckDB materialises the widened type at
        // set-op time.
        use crate::transpiler_v2::analyzer::TypedOp;
        let left = TypedOp::TableScan {
            name: "L".to_string(),
            schema: StructType::single("x", DataType::Integer),
        };
        let right = TypedOp::TableScan {
            name: "R".to_string(),
            schema: StructType::single("x", DataType::Long),
        };
        let widened = StructType::single("x", DataType::Long);
        let u = TypedOp::Union {
            left: Box::new(left),
            right: Box::new(right),
            all: true,
            by_name: false,
            schema: widened,
        };
        let sql = dispatch(&u).expect("union dispatch");
        let s = sql.as_str();
        // Left must be wrapped in CAST(x AS BIGINT); right must not.
        assert!(
            s.contains("CAST(\"x\" AS BIGINT)"),
            "left child must CAST AS BIGINT to widen to right's type, got {s:?}"
        );
    }

    #[test]
    fn union_matching_schemas_needs_no_cast_wrapper() {
        // Same types on both sides — no CAST wrapper, plain UNION ALL.
        use crate::transpiler_v2::analyzer::TypedOp;
        let left = TypedOp::TableScan {
            name: "L".to_string(),
            schema: StructType::single("x", DataType::Integer),
        };
        let right = TypedOp::TableScan {
            name: "R".to_string(),
            schema: StructType::single("x", DataType::Integer),
        };
        let widened = StructType::single("x", DataType::Integer);
        let u = TypedOp::Union {
            left: Box::new(left),
            right: Box::new(right),
            all: false,
            by_name: false,
            schema: widened,
        };
        let sql = dispatch(&u).expect("union dispatch");
        let s = sql.as_str();
        assert!(
            !s.contains("CAST("),
            "matching schemas must not emit CAST, got {s:?}"
        );
    }

    // ── M6: render_tail CTE ────────────────────────────────────────────────

    #[test]
    fn tail_uses_with_clause_to_share_child_once() {
        use crate::transpiler_v2::analyzer::TypedOp;
        let t = TypedOp::Tail {
            input: Box::new(TypedOp::TableScan {
                name: "nums".to_string(),
                schema: StructType::single("a", DataType::Integer),
            }),
            n: Literal::int(3),
            schema: StructType::single("a", DataType::Integer),
        };
        let sql = dispatch(&t).expect("tail dispatch");
        let s = sql.as_str();
        assert!(
            s.starts_with("WITH __td_child AS "),
            "tail must open with WITH clause, got {s:?}"
        );
        assert!(
            s.contains("__td_child"),
            "tail must reference the CTE, got {s:?}"
        );
        // Verify the child SQL appears exactly once (in the CTE binding).
        let child_occurrences = s.matches("\"nums\"").count();
        assert_eq!(
            child_occurrences, 1,
            "tail must reference child only once (via CTE), got {s:?}"
        );
    }

    // ── C.3-1 regression: sha2 arg-strip ───────────────────────────────────

    /// Regression for hash-002: Spark `sha2(col, 256)` must translate to
    /// DuckDB `SHA256(col)` — the bit-length is fixed by the arm and the
    /// second argument must not be forwarded. Prior to the fix, the arm
    /// emitted `SHA256(col, 256)`, which DuckDB rejects as a Binder Error
    /// (`sha256(VARCHAR, INTEGER_LITERAL)` has no matching candidate).
    #[test]
    fn sha2_with_bit_length_strips_extra_args() {
        let sch = empty_schema();
        let sql = s(render_expr(
            &call(
                "sha2",
                vec![
                    Expression::UnresolvedColumn(UnresolvedColumn {
                        name: "name".to_string(),
                        qualifier: None,
                    }),
                    Literal::int(256),
                ],
            ),
            &sch,
            "t",
        ));
        assert!(
            sql.contains("SHA256("),
            "sha2 must route through SHA256(...), got {sql:?}"
        );
        // Bit-length arg must be stripped: the emitted SQL must not carry the
        // literal `256` as an argument value. Note that the function name
        // itself contains `256`, so we assert on the argument list rather than
        // the raw string.
        assert!(
            !sql.contains(", 256"),
            "sha2 arm must strip the bit-length arg, got {sql:?}"
        );
        assert!(
            !sql.contains(", "),
            "sha2 must be a single-arg call after strip, got {sql:?}"
        );
        assert_eq!(
            sql, "SHA256(\"name\")",
            "sha2 must emit exactly one arg to SHA256, got {sql:?}"
        );
    }
}
