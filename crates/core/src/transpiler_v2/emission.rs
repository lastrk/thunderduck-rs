//! Compiled-dispatch emitter for v2 SQL (ADR-009). Also hosts the
//! external-emit enumeration that INV8 walks.
//!
//! # Design (§Layer 2 of Slice C.1 architecture plan)
//!
//! Slice C.1 lands the dispatch as a **hardcoded `match` statement** over
//! [`crate::transpiler_v2::analyzer::TypedOp`] discriminants: each arm
//! reads pre-analyzed [`crate::transpiler_v2::analyzer::TypedAttr`]s and
//! `Schema` fields and emits SQL via per-op renderer helpers. This is the
//! honest C.1 emission form — no declarative-interpreter substrate ships
//! in this slice, so we do not carry data structures the code does not
//! consume. Slice C.2 introduces the per-function declarative rows and
//! (if the row count justifies it) a proc-macro / `build.rs` surface
//! that turns the operator arms into table lookups too.
//!
//! Emitted SQL is wrapped in [`EmittedSql`], whose only constructor
//! (`emit`) fires the [INV2] tap. [`dispatch`] is the sole caller of
//! `emit`, so INV2 holds by type-system construction.
//!
//! # Scope for Slice C.1
//!
//! The dispatch arms cover: Project, Filter, Sort, Limit, Tail, Distinct,
//! WithColumns, DropColumns, AliasedRelation, TableScan, LocalRelation,
//! RangeRelation, Union{,All}, Intersect{,All}, Except{,All}, and a
//! primitive Aggregate. Scalar-expression rendering (per-function
//! projection casts for Spark parity) is delegated to the legacy
//! [`crate::generator::SqlGenerator::gen_expr`] via [`render_expr`] —
//! that delegation is the deliberate C.1/C.2 seam. Slice C.2 replaces
//! each per-function `gen_expr` fan-out with a declarative row keyed on
//! the function name.

use crate::expression::{Expression, NullOrdering, SortDirection, SortOrder};
use crate::generator::SqlGenerator;
use crate::logical::spark_column_name;
use crate::transpiler_v2::analyzer::{Schema, TypedOp};
use crate::types::DataType;

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

    /// Rendering a scalar expression through the legacy path failed.
    ///
    /// This is the C.2 seam: the delegating call to
    /// [`crate::generator::SqlGenerator::gen_expr`] returned an error.
    /// Once C.2 lands proper per-function declarative rows this variant
    /// is expected to shrink to genuinely-unrepresentable expressions.
    #[error("legacy expression render failed inside emission arm `{op_kind}`: {reason}")]
    LegacyRenderFailed {
        /// The operator arm that owned the failing expression.
        op_kind: &'static str,
        /// Diagnostic message from the legacy generator.
        reason: String,
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
/// **Stub.** Empty today; entries are added as ADR-010 extension targets
/// are wired into the table.
///
/// TODO INV6: populate as ADR-010 extension targets are declared.
pub fn extension_targets() -> &'static [&'static str] {
    &[]
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
            left, right, all, ..
        } => render_union(left, right, *all),
        TypedOp::Intersect {
            left, right, all, ..
        } => render_intersect(left, right, *all),
        TypedOp::Except {
            left, right, all, ..
        } => render_except(left, right, *all),
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

fn render_tail(input: &TypedOp, n: &Expression) -> Result<String, EmissionError> {
    // DuckDB has no TAIL — emit an emulation via ROW_NUMBER over the
    // reverse ordering. The legacy generator uses the same shape.  This
    // is an ADR-007 forced transliteration; a future slice may promote
    // it to a B-layer rewrite.
    let child_sql = dispatch_child("Tail", "input", input)?;
    let schema = input.schema();
    let n_sql = render_expr(n, schema, "Tail")?;
    Ok(format!(
        "SELECT * EXCLUDE (__td_row_num__) FROM (SELECT *, ROW_NUMBER() OVER () AS __td_row_num__ FROM ({child_sql})) \
         WHERE __td_row_num__ > (SELECT COUNT(*) FROM ({child_sql})) - {n_sql}"
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

// M3 (rearchitect plan §Layer 3, "Union widening"): the widened schema
// carried by the analyzer's `TypedOp::Union` wins over any child
// projection's declared cast target — per-column re-cast (so DuckDB
// materializes the widened Spark type at emit time) is deferred to
// Slice C.2's projection-cast slot. Today the emitter just concatenates
// the two child subqueries with `UNION ALL` / `UNION`, and downstream
// operators see the analyzer's widened schema unchanged.
//
// TODO Slice C.2: emit an explicit CAST wrapper on each child's SELECT
// list to the widened union column types, so DuckDB does not fall back
// to its own coercion at set-op time.
fn render_union(left: &TypedOp, right: &TypedOp, all: bool) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Union", "left", left)?;
    let right_sql = dispatch_child("Union", "right", right)?;
    let op = if all { "UNION ALL" } else { "UNION" };
    Ok(format!("({left_sql}) {op} ({right_sql})"))
}

fn render_intersect(left: &TypedOp, right: &TypedOp, all: bool) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Intersect", "left", left)?;
    let right_sql = dispatch_child("Intersect", "right", right)?;
    let op = if all { "INTERSECT ALL" } else { "INTERSECT" };
    Ok(format!("({left_sql}) {op} ({right_sql})"))
}

fn render_except(left: &TypedOp, right: &TypedOp, all: bool) -> Result<String, EmissionError> {
    let left_sql = dispatch_child("Except", "left", left)?;
    let right_sql = dispatch_child("Except", "right", right)?;
    let op = if all { "EXCEPT ALL" } else { "EXCEPT" };
    Ok(format!("({left_sql}) {op} ({right_sql})"))
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
        let mut decorated = render_expr(&agg.func, schema, "Aggregate")?;
        if agg.is_distinct {
            decorated = inject_distinct(decorated);
        }
        if let Some(filter) = &agg.filter {
            let filter_sql = render_expr(filter, schema, "Aggregate")?;
            decorated = format!("{decorated} FILTER (WHERE {filter_sql})");
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
        Expression::Star(_) => Ok("*".to_string()),
        Expression::Alias(a) => {
            let inner = render_expr(&a.expr, schema, op_kind)?;
            Ok(format!("{inner} AS {}", quote_ident(&a.alias)))
        }
        Expression::ColumnReference(c) if c.qualifier.is_none() => Ok(quote_ident(&c.name)),
        Expression::UnresolvedColumn(u) if u.qualifier.is_none() => Ok(quote_ident(&u.name)),
        other => {
            let inner = render_expr(other, schema, op_kind)?;
            // ADR-015 naming convention: `spark_column_name(expr) AS out`
            // when the surface expression is not itself a bare column ref.
            let name = spark_column_name(other);
            Ok(format!("{inner} AS {}", quote_ident(&name)))
        }
    }
}

/// Render a scalar expression by delegating to the legacy generator.
///
/// TODO Slice C.2: this delegation is the **deliberate C.1/C.2 seam** —
/// per-function declarative rows keyed on `FunctionCall::name` (with
/// projection-cast slots for Spark parity) will replace the
/// `SqlGenerator::gen_expr` call below. INV3's grep test forbids a
/// module-level import of the runtime function registry; the transitive
/// pull through `SqlGenerator::gen_expr` (which internally routes
/// through the function registry) is what Slice C.2 drains. When C.2
/// lands, the `SqlGenerator` import in this file goes away too.
fn render_expr(
    expr: &Expression,
    schema: &Schema,
    op_kind: &'static str,
) -> Result<String, EmissionError> {
    // Construct a fresh generator seeded with the child schema so type-
    // aware dispatch behaves the same as the legacy `SqlGenerator`.
    // `SqlGenerator::new()` starts empty; `with_schema` is private, so we
    // seed via `set_schema_for_v2` (added in this slice for the shared
    // seam).
    let gen = SqlGenerator::new().with_schema_for_v2(schema.clone());
    gen.gen_expr(expr)
        .map_err(|e| EmissionError::LegacyRenderFailed {
            op_kind,
            reason: e.to_string(),
        })
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
        // the emitted function call as `count(DISTINCT ...)`, not be
        // silently dropped.
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
        // H1 regression — `render_aliased_relation` must not drop the
        // alias or per-column aliases. Legacy shape:
        // `(<child>) AS <alias>(<c1>, <c2>, ...)`.
        use crate::transpiler_v2::analyzer::TypedOp;

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

        // And without column aliases the parens must not appear.
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
        // No column-alias parens: the alias is followed immediately by
        // the end of the emitted fragment, not by `(...`.
        assert!(
            !s.contains("AS \"t\"("),
            "bare aliased relation must not emit a column list, got {s:?}"
        );
    }
}
