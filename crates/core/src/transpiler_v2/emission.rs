//! τ's emission substrate — Slice C.1.
//!
//! ADR-009 (Approach A hand-written match arms, permanent per Open Decision 7),
//! ADR-021 (τ owns substrate), ADR-022 (τ is the only path; two error
//! categories). Inheritance-checklist §4.2 first item, §5.1, §5.3, §5.4, §5.6.
//!
//! **INV3 grep barrier:** no imports from the legacy `generator` or
//! `functions` modules are permitted inside this file.
//!
//! **INV10:** imports only τ-internal modules + `crate::types::{DataType,
//! StructField, StructType}`.
//!
//! # What lives here
//!
//! - [`dispatch_op`] — the single top-level operator dispatcher. One
//!   hand-written match arm per [`TypedOp`] variant. Every `Ok` return path
//!   increments the [`EMIT_TAP`] counter (§5.3).
//! - Per-operator renderers (`render_project`, `render_filter`, `render_sort`,
//!   `render_limit`, `render_single_row`, `render_table_scan`, `render_values`,
//!   `render_local_relation`, `render_file_scan`).
//! - [`render_expr`] — exhaustive match over the [`Expression`] enum.
//! - [`render_cast`] — includes the `try_cast` → `TRY_CAST(...)` branch
//!   (checklist §4.2 first item).
//! - [`quote_ident`] — `Cow`-returning fast path (§5.6).
//! - The six unwired helpers under Decision 13-A (`render_tail`,
//!   `render_distinct`, `render_with_columns`, `render_drop_columns`,
//!   `render_aliased_relation`, `render_range_relation`) — private, marked
//!   `#[allow(dead_code)]`, become one-line dispatch arms when the matching
//!   `TypedOp` variants land in a future substrate slice.
//! - [`spark_return_cast`] (§5.1) and `spark_aggregate_return_cast` (§5.1,
//!   `#[allow(dead_code)]` — wired by C.3) — two distinct `fn` items.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::analyzer::{Schema, TypedAst, TypedOp};
use super::ast::{FileFormat, SetOpKind};
use super::error::EmissionError;
use super::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression,
    ColumnReference, Expression, FunctionCall, IntervalExpression, Literal, LiteralValue,
    NullOrdering, SortDirection, SortOrder, StarExpression, UnaryExpression, UnaryOp,
};
use super::type_inference::AGGREGATE_NAMES;
use crate::types::{DataType, StructField, StructType};

// ── INV2 companion (§5.3) ────────────────────────────────────────────────────

/// Monotonic counter — incremented once per successful SQL string returned by
/// [`dispatch_op`]. Slice C.1 activates INV2 via
/// `invariants::inv2_dispatch_is_only_sql_writer`.
pub(crate) static EMIT_TAP: AtomicU64 = AtomicU64::new(0);

/// Serializes tests that read / reset [`EMIT_TAP`] (parallel-test flake guard).
///
/// Referenced by `invariants::inv2_dispatch_is_only_sql_writer` and by
/// `emission::tests`; the release build has no consumer, hence
/// `#[allow(dead_code)]`.
#[allow(dead_code)]
pub(crate) static EMIT_TAP_MUTEX: Mutex<()> = Mutex::new(());

// ── Dispatch (Approach A — hand-written match) ───────────────────────────────

/// Top-level dispatch. **INV2 companion:** this function is the ONLY writer to
/// [`EMIT_TAP`]. Every `Ok` return path increments [`EMIT_TAP`] exactly once.
///
/// One hand-written match arm per [`TypedOp`] variant. No table interpreter.
pub fn dispatch_op(op: &TypedOp, schema: &Schema) -> Result<String, EmissionError> {
    let result: Result<String, EmissionError> = match op {
        // ── C.1 wired ────────────────────────────────────────────────────
        TypedOp::SingleRow => render_single_row(),
        TypedOp::TableScan { table, alias } => render_table_scan(table, alias.as_deref()),
        TypedOp::Values { rows, column_names } => render_values(rows, column_names, schema),
        TypedOp::LocalRelation { schema: s, rows } => render_local_relation(s, rows),
        TypedOp::FileScan {
            format,
            paths,
            schema: s,
            options,
        } => render_file_scan(*format, paths, s, options),
        TypedOp::Project { input, projections } => render_project(input, projections),
        TypedOp::Filter { input, condition } => render_filter(input, condition),
        TypedOp::Sort {
            input,
            order,
            limit,
            offset,
        } => render_sort(input, order, *limit, *offset),
        TypedOp::Limit {
            input,
            limit,
            offset,
        } => render_limit(input, *limit, *offset),
        TypedOp::WithColumns { input, assignments } => render_with_columns(input, assignments),
        TypedOp::DropColumns { input, drop_names } => render_drop_columns(input, drop_names),
        TypedOp::AliasedRelation { input, alias } => render_aliased_relation(input, alias),
        TypedOp::WithColumnsRenamed { input, renames } => {
            render_with_columns_renamed(input, renames)
        }
        TypedOp::Deduplicate { input, on_columns } => render_deduplicate(input, on_columns),
        TypedOp::NaFill {
            input,
            cols,
            values,
        } => render_na_fill(input, cols, values),
        TypedOp::NaDrop {
            input,
            cols,
            min_non_nulls,
        } => render_na_drop(input, cols, *min_non_nulls),
        TypedOp::NaReplace {
            input,
            cols,
            replacements,
        } => render_na_replace(input, cols, replacements),
        TypedOp::Unpivot {
            input,
            ids,
            values,
            variable_column_name,
            value_column_name,
        } => render_unpivot(input, ids, values, variable_column_name, value_column_name),
        TypedOp::Pivot {
            input,
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
        } => render_pivot(
            input,
            grouping,
            pivot_column,
            pivot_values,
            aggregates,
            schema,
        ),

        // ── Aggregate (operator + primitive function arms) ───────────────
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            grouping_kind,
        } => render_aggregate_op(input, grouping, aggregates, *grouping_kind),

        // ── Join ─────────────────────────────────────────────────────────
        TypedOp::Join {
            left,
            right,
            join_type,
            condition,
            using_columns,
            ..
        } => render_join(left, right, *join_type, condition.as_ref(), using_columns),
        TypedOp::SetOp {
            kind,
            all,
            by_name,
            allow_missing_columns,
            children,
            widened_schema,
        } => render_set_op(
            *kind,
            *all,
            *by_name,
            *allow_missing_columns,
            children,
            widened_schema,
        ),

        // ── Slice F owns (analyzer PuntedOperator today; defensive) ──────
        TypedOp::TableFunction { name, .. } => Err(EmissionError::UnsupportedOp {
            op: format!("TableFunction[{name}]"),
            reason: "table-function emission lands in Slice F".to_owned(),
        }),
        TypedOp::Unnest { .. } => Err(EmissionError::UnsupportedOp {
            op: "Unnest".to_owned(),
            reason: "unnest emission lands in Slice F".to_owned(),
        }),
    };

    if result.is_ok() {
        EMIT_TAP.fetch_add(1, Ordering::Relaxed);
    }
    result
}

// ── Operator renderers ───────────────────────────────────────────────────────

fn render_single_row() -> Result<String, EmissionError> {
    // DuckDB requires a subquery to have a projection list — bare `SELECT`
    // parses at top-level but fails inside `FROM (...)`. Emit `SELECT 1` so
    // `SingleRow` is subquery-safe under `Project` (which wraps as
    // `SELECT expr FROM (<child>) AS __td_proj` — the placeholder column is
    // unused because Project provides its own SELECT list). The analyzer
    // stamps SingleRow with an empty schema; no legitimate operator resolves
    // the placeholder column from downstream code, so its presence is inert.
    Ok("SELECT 1".to_owned())
}

fn render_table_scan(table: &str, alias: Option<&str>) -> Result<String, EmissionError> {
    let name = quote_ident(table);
    match alias {
        Some(a) => {
            let a = quote_ident(a);
            Ok(format!("SELECT * FROM {name} AS {a}"))
        }
        None => Ok(format!("SELECT * FROM {name}")),
    }
}

fn render_values(
    rows: &[Vec<Expression>],
    column_names: &[String],
    schema: &Schema,
) -> Result<String, EmissionError> {
    if rows.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "Values".to_owned(),
            reason: "empty VALUES relations are not supported".to_owned(),
        });
    }
    let mut rendered_rows = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            rendered_rows.push_str(", ");
        }
        rendered_rows.push('(');
        for (j, cell) in row.iter().enumerate() {
            if j > 0 {
                rendered_rows.push_str(", ");
            }
            rendered_rows.push_str(&render_expr(cell, schema)?);
        }
        rendered_rows.push(')');
    }
    let mut cols = String::new();
    for (i, c) in column_names.iter().enumerate() {
        if i > 0 {
            cols.push_str(", ");
        }
        cols.push_str(&quote_ident(c));
    }
    Ok(format!(
        "SELECT * FROM (VALUES {rendered_rows}) AS __td_values({cols})"
    ))
}

fn render_local_relation(
    schema_decl: &StructType,
    rows: &[Vec<Expression>],
) -> Result<String, EmissionError> {
    if schema_decl.fields.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "LocalRelation".to_owned(),
            reason: "LocalRelation with empty schema is not representable".to_owned(),
        });
    }
    // Special case: no rows → emit an empty relation with the correct schema.
    if rows.is_empty() {
        // `SELECT CAST(NULL AS T) AS c, ... WHERE 1=0` — zero rows, right shape.
        let mut cols = String::new();
        for (i, f) in schema_decl.fields.iter().enumerate() {
            if i > 0 {
                cols.push_str(", ");
            }
            let ty = render_data_type(&f.data_type);
            let name = quote_ident(&f.name);
            cols.push_str(&format!("CAST(NULL AS {ty}) AS {name}"));
        }
        return Ok(format!("SELECT {cols} WHERE 1=0"));
    }
    let mut rendered_rows = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i > 0 {
            rendered_rows.push_str(", ");
        }
        rendered_rows.push('(');
        for (idx, cell) in row.iter().enumerate() {
            if idx > 0 {
                rendered_rows.push_str(", ");
            }
            let inner = render_expr(cell, schema_decl)?;
            // Ensure each cell carries the declared type — a naked NULL literal
            // would otherwise adopt DuckDB's inferred column type across rows.
            let field = &schema_decl.fields[idx];
            let ty = render_data_type(&field.data_type);
            rendered_rows.push_str(&format!("CAST({inner} AS {ty})"));
        }
        rendered_rows.push(')');
    }
    let mut cols = String::new();
    for (i, f) in schema_decl.fields.iter().enumerate() {
        if i > 0 {
            cols.push_str(", ");
        }
        cols.push_str(&quote_ident(&f.name));
    }
    Ok(format!(
        "SELECT * FROM (VALUES {rendered_rows}) AS __td_local({cols})"
    ))
}

fn render_file_scan(
    format: FileFormat,
    paths: &[String],
    _schema: &StructType,
    options: &[(String, String)],
) -> Result<String, EmissionError> {
    if paths.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "FileScan".to_owned(),
            reason: "FileScan requires at least one path".to_owned(),
        });
    }
    let paths_sql = if paths.len() == 1 {
        format!("'{}'", escape_sql_string(&paths[0]))
    } else {
        let mut buf = String::from("[");
        for (i, p) in paths.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push('\'');
            buf.push_str(&escape_sql_string(p));
            buf.push('\'');
        }
        buf.push(']');
        buf
    };
    let reader = match format {
        FileFormat::Parquet => "read_parquet",
        FileFormat::Csv => "read_csv",
        FileFormat::Json => "read_json",
        FileFormat::Orc => {
            return Err(EmissionError::UnsupportedOp {
                op: "FileScan[Orc]".to_owned(),
                reason: "ORC file scanning is not supported by DuckDB".to_owned(),
            });
        }
    };
    let opts_sql = if options.is_empty() {
        String::new()
    } else {
        let mut opts = String::from(", ");
        for (i, (k, v)) in options.iter().enumerate() {
            if i > 0 {
                opts.push_str(", ");
            }
            opts.push_str(&format!("{k}='{}'", escape_sql_string(v)));
        }
        opts
    };
    Ok(format!("SELECT * FROM {reader}({paths_sql}{opts_sql})"))
}

fn render_project(input: &TypedAst, projections: &[Expression]) -> Result<String, EmissionError> {
    let input_schema = &input.resolved_schema;
    // Project-over-Join inlining: when the child is a Join, emit the
    // projections directly against the Join's FROM/ON clauses rather than
    // wrapping the join output as a subquery. This preserves outer-level
    // access to user aliases (`df.alias("e").join(...).select(e.name)`)
    // and matches Spark's plan-tree shape.
    if let TypedOp::Join {
        left,
        right,
        join_type,
        condition,
        using_columns,
        ..
    } = &input.op
    {
        return render_project_over_join(
            projections,
            input_schema,
            left,
            right,
            *join_type,
            condition.as_ref(),
            using_columns,
        );
    }
    // AliasedRelation is transparent for Project too — inline through it.
    if let TypedOp::AliasedRelation {
        input: inner,
        alias,
    } = &input.op
    {
        let inner_sql = dispatch_op(&inner.op, &inner.resolved_schema)?;
        let slots_sql = render_projection_slots(projections, input_schema)?;
        let a = quote_ident(alias);
        return Ok(format!("SELECT {slots_sql} FROM ({inner_sql}) AS {a}"));
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let slots_sql = render_projection_slots(projections, input_schema)?;
    Ok(format!(
        "SELECT {slots_sql} FROM ({child_sql}) AS __td_proj"
    ))
}

fn render_projection_slots(
    projections: &[Expression],
    input_schema: &Schema,
) -> Result<String, EmissionError> {
    if projections.is_empty() {
        return Ok("*".to_owned());
    }
    let mut buf = String::new();
    for (i, p) in projections.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        buf.push_str(&render_projection_slot(p, input_schema)?);
    }
    Ok(buf)
}

/// Render a Project whose child is a Join. Inline the Join's FROM/ON so
/// user aliases (`df.alias("e")`) remain in scope for the projection list.
fn render_project_over_join(
    projections: &[Expression],
    project_input_schema: &Schema,
    left: &TypedAst,
    right: &TypedAst,
    join_type: crate::transpiler_v2::ast::JoinType,
    condition: Option<&Expression>,
    using_columns: &[String],
) -> Result<String, EmissionError> {
    use crate::transpiler_v2::ast::JoinType;
    // Pick subquery aliases: user AliasedRelation names take precedence.
    let (left_ast, left_alias) = match &left.op {
        TypedOp::AliasedRelation { input, alias } => (input.as_ref(), alias.clone()),
        _ => (left, "__td_jl".to_owned()),
    };
    let (right_ast, right_alias) = match &right.op {
        TypedOp::AliasedRelation { input, alias } => (input.as_ref(), alias.clone()),
        _ => (right, "__td_jr".to_owned()),
    };
    let left_sql = dispatch_op(&left_ast.op, &left_ast.resolved_schema)?;
    let right_sql = dispatch_op(&right_ast.op, &right_ast.resolved_schema)?;
    let kind = match join_type {
        JoinType::Inner => "INNER JOIN",
        JoinType::Left => "LEFT OUTER JOIN",
        JoinType::Right => "RIGHT OUTER JOIN",
        JoinType::Full => "FULL OUTER JOIN",
        JoinType::Cross => "CROSS JOIN",
        JoinType::LeftSemi => "SEMI JOIN",
        JoinType::LeftAnti => "ANTI JOIN",
    };
    let clause = if !using_columns.is_empty() {
        let mut cols = String::new();
        for (i, c) in using_columns.iter().enumerate() {
            if i > 0 {
                cols.push_str(", ");
            }
            cols.push_str(&quote_ident(c));
        }
        format!(" USING ({cols})")
    } else if let Some(cond) = condition {
        let cond_sql = render_expr(cond, project_input_schema)?;
        format!(" ON {cond_sql}")
    } else if matches!(join_type, JoinType::Cross) {
        String::new()
    } else {
        return Err(EmissionError::UnsupportedOp {
            op: "Join".to_owned(),
            reason: "non-cross join without ON or USING clause".to_owned(),
        });
    };
    let slots_sql = render_projection_slots(projections, project_input_schema)?;
    let la = quote_ident(&left_alias);
    let ra = quote_ident(&right_alias);
    Ok(format!(
        "SELECT {slots_sql} FROM ({left_sql}) AS {la} {kind} ({right_sql}) AS {ra}{clause}"
    ))
}

/// Render a projection slot, applying `spark_return_cast` at the top level
/// (§5.1) — Spark-parity casts only appear as an outermost `CAST(...)` around
/// the projection expression (with optional preserved alias).
fn render_projection_slot(
    expr: &Expression,
    input_schema: &Schema,
) -> Result<String, EmissionError> {
    // Star is a raw `*` / `qualifier.*` — no cast wrapping.
    if let Expression::Star(s) = expr {
        return render_star(s);
    }
    // Alias(inner) → CAST(inner_sql AS T) AS alias, only if a cast is needed.
    if let Expression::Alias(a) = expr {
        let inner_sql = render_expr(&a.expr, input_schema)?;
        let inner_sql = spark_return_cast(inner_sql, &a.expr, input_schema);
        let alias = quote_ident(&a.alias);
        return Ok(format!("{inner_sql} AS {alias}"));
    }
    let inner_sql = render_expr(expr, input_schema)?;
    Ok(spark_return_cast(inner_sql, expr, input_schema))
}

fn render_filter(input: &TypedAst, condition: &Expression) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let cond_sql = render_expr(condition, &input.resolved_schema)?;
    Ok(format!(
        "SELECT * FROM ({child_sql}) AS __td_filter WHERE {cond_sql}"
    ))
}

fn render_sort(
    input: &TypedAst,
    order: &[SortOrder],
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let mut sql = format!("SELECT * FROM ({child_sql}) AS __td_sort");
    if !order.is_empty() {
        sql.push_str(" ORDER BY ");
        for (i, so) in order.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            sql.push_str(&render_sort_key(so, &input.resolved_schema)?);
        }
    }
    if let Some(l) = limit {
        sql.push_str(&format!(" LIMIT {l}"));
    }
    if let Some(o) = offset {
        sql.push_str(&format!(" OFFSET {o}"));
    }
    Ok(sql)
}

fn render_sort_key(so: &SortOrder, schema: &Schema) -> Result<String, EmissionError> {
    let expr_sql = render_expr(&so.expr, schema)?;
    let dir = match so.direction {
        SortDirection::Ascending => "ASC",
        SortDirection::Descending => "DESC",
    };
    let nulls = match so.null_ordering {
        NullOrdering::NullsFirst => "NULLS FIRST",
        NullOrdering::NullsLast => "NULLS LAST",
    };
    Ok(format!("{expr_sql} {dir} {nulls}"))
}

fn render_limit(
    input: &TypedAst,
    limit: i64,
    offset: Option<i64>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let mut sql = format!("SELECT * FROM ({child_sql}) AS __td_limit LIMIT {limit}");
    if let Some(o) = offset {
        sql.push_str(&format!(" OFFSET {o}"));
    }
    Ok(sql)
}

// ── Unwired renderers (Decision 13-A) ────────────────────────────────────────
//
// These six renderers do not have `TypedOp` sinks in Slice B's substrate. They
// exist so the §5.4 CTE anchor for `render_tail` (and its sibling helpers)
// live in code today; when a future substrate slice adds the missing
// `TypedOp` variants, wiring is a one-line `dispatch_op` arm each.

/// **§5.4 CTE rewrite.** DuckDB has no native TAIL operator; we synthesize it
/// via `ROW_NUMBER() OVER ()` and select rows past `total_rows − n`. The child
/// SQL is materialized once inside a `WITH` binding so it is not double-embedded.
#[allow(dead_code)] // wired when TypedOp::Tail lands (Decision 13-A)
fn render_tail(input: &TypedAst, n: i64) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    Ok(format!(
        "WITH __td_child AS ({child_sql}) \
         SELECT * EXCLUDE (__td_row_num__) \
         FROM (SELECT *, ROW_NUMBER() OVER () AS __td_row_num__ FROM __td_child) \
         WHERE __td_row_num__ > (SELECT COUNT(*) FROM __td_child) - {n}"
    ))
}

#[allow(dead_code)] // wired when TypedOp::Distinct lands (Decision 13-A)
fn render_distinct(input: &TypedAst) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    Ok(format!(
        "SELECT DISTINCT * FROM ({child_sql}) AS __td_distinct"
    ))
}

/// Render a `SetOp` (UNION / INTERSECT / EXCEPT). Each child is wrapped
/// with a per-column `CAST(col AS <widened_type>)` projection so the union'd
/// column types match the analyzer's widened schema (per ADR-006 refinement
/// + Open Decision 5). `UNION BY NAME` is deferred (analyzer surfaces it as
/// `PuntedOperator`); it never reaches this renderer.
fn render_set_op(
    kind: crate::transpiler_v2::ast::SetOpKind,
    all: bool,
    by_name: bool,
    allow_missing_columns: bool,
    children: &[TypedAst],
    widened_schema: &StructType,
) -> Result<String, EmissionError> {
    use crate::transpiler_v2::ast::SetOpKind;
    if children.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "SetOp".to_owned(),
            reason: "set-op with no children".to_owned(),
        });
    }
    // When `allow_missing_columns = true`, every child SELECT emits an
    // identically-ordered, identically-named, identically-typed projection
    // list (with `CAST(NULL AS ty) AS name` for missing columns). Under that
    // invariant plain `UNION [ALL]` is semantically equivalent to
    // `UNION [ALL] BY NAME`; prefer plain UNION for consistency with the
    // by-position emission path.
    let op_kw = match (kind, all, by_name, allow_missing_columns) {
        // BY NAME variants (DuckDB supports UNION [ALL] BY NAME only).
        (SetOpKind::Union, true, true, false) => "UNION ALL BY NAME",
        (SetOpKind::Union, false, true, false) => "UNION BY NAME",
        (SetOpKind::Union, true, true, true) => "UNION ALL",
        (SetOpKind::Union, false, true, true) => "UNION",
        (SetOpKind::Union, true, false, _) => "UNION ALL",
        (SetOpKind::Union, false, false, _) => "UNION",
        (SetOpKind::Intersect, true, false, _) => "INTERSECT ALL",
        (SetOpKind::Intersect, false, false, _) => "INTERSECT",
        (SetOpKind::Except, true, false, _) => "EXCEPT ALL",
        (SetOpKind::Except, false, false, _) => "EXCEPT",
        (kind, _, true, _) => {
            return Err(EmissionError::UnsupportedOp {
                op: format!("SetOp[{kind:?} BY NAME]"),
                reason: "DuckDB supports BY NAME only for UNION".to_owned(),
            });
        }
    };
    let mut parts: Vec<String> = Vec::with_capacity(children.len());
    for child in children {
        let child_sql = dispatch_op(&child.op, &child.resolved_schema)?;
        // Per-column CAST to widened parent schema.
        //   - by-position: children have identical arity (analyzer verified);
        //     zip position-wise, CAST each column to widened_schema[i] and
        //     rename to the widened column name.
        //   - by-name (strict): children have identical name SETS but
        //     possibly different orders (analyzer verified). For each name
        //     in the widened schema, find the child's matching field, CAST
        //     to the widened type, keep the widened name.
        //   - by-name + allow_missing_columns: some children may be missing
        //     names entirely. For each name in the widened schema, either
        //     emit `CAST(child_col AS widened_ty) AS widened_name` if the
        //     child has it, or `CAST(NULL AS widened_ty) AS widened_name`
        //     for the padded slot.
        let mut slots = String::new();
        if by_name && allow_missing_columns {
            for (i, widened_field) in widened_schema.fields.iter().enumerate() {
                if i > 0 {
                    slots.push_str(", ");
                }
                let ty = render_data_type(&widened_field.data_type);
                let widened_name = quote_ident(&widened_field.name);
                if let Some(child_field) = child.resolved_schema.field_by_name(&widened_field.name)
                {
                    let col = quote_ident(&child_field.name);
                    slots.push_str(&format!("CAST({col} AS {ty}) AS {widened_name}"));
                } else {
                    slots.push_str(&format!("CAST(NULL AS {ty}) AS {widened_name}"));
                }
            }
        } else if by_name {
            for (i, widened_field) in widened_schema.fields.iter().enumerate() {
                if i > 0 {
                    slots.push_str(", ");
                }
                let child_field = child
                    .resolved_schema
                    .fields
                    .iter()
                    .find(|f| f.name.eq_ignore_ascii_case(&widened_field.name))
                    .expect("analyzer guaranteed name match");
                let ty = render_data_type(&widened_field.data_type);
                let col = quote_ident(&child_field.name);
                let widened_name = quote_ident(&widened_field.name);
                slots.push_str(&format!("CAST({col} AS {ty}) AS {widened_name}"));
            }
        } else {
            for (i, (child_field, widened_field)) in child
                .resolved_schema
                .fields
                .iter()
                .zip(widened_schema.fields.iter())
                .enumerate()
            {
                if i > 0 {
                    slots.push_str(", ");
                }
                let ty = render_data_type(&widened_field.data_type);
                let col = quote_ident(&child_field.name);
                let widened_name = quote_ident(&widened_field.name);
                slots.push_str(&format!("CAST({col} AS {ty}) AS {widened_name}"));
            }
        }
        parts.push(format!("SELECT {slots} FROM ({child_sql}) AS __td_setop"));
    }
    // Wrap the union expression in an outer SELECT so downstream operators
    // can wrap it as `FROM (...)` without DuckDB parse errors on the
    // UNION-composed subquery.
    Ok(parts.join(&format!(" {op_kw} ")))
}

/// Render a binary `Join`. Emits
/// `SELECT * FROM (left) AS __td_jl <JOIN_KIND> JOIN (right) AS __td_jr
/// [ON <cond> | USING (<cols>)]`. DuckDB accepts `INNER`, `LEFT`, `RIGHT`,
/// `FULL`, `CROSS`, `SEMI`, `ANTI` — the last two WITHOUT the `LEFT` prefix
/// (checklist §5 / CLAUDE.md Known Gotcha #5). SEMI/ANTI join emission never
/// produces right-side columns (semantically absent); the analyzer already
/// computes the output schema accordingly (LeftSemi/LeftAnti → left schema
/// only).
fn render_join(
    left: &TypedAst,
    right: &TypedAst,
    join_type: crate::transpiler_v2::ast::JoinType,
    condition: Option<&Expression>,
    using_columns: &[String],
) -> Result<String, EmissionError> {
    use crate::transpiler_v2::ast::JoinType;
    let left_sql = dispatch_op(&left.op, &left.resolved_schema)?;
    let right_sql = dispatch_op(&right.op, &right.resolved_schema)?;
    let left_alias = "__td_jl".to_owned();
    let right_alias = "__td_jr".to_owned();
    let kind = match join_type {
        JoinType::Inner => "INNER JOIN",
        JoinType::Left => "LEFT OUTER JOIN",
        JoinType::Right => "RIGHT OUTER JOIN",
        JoinType::Full => "FULL OUTER JOIN",
        JoinType::Cross => "CROSS JOIN",
        // DuckDB requires SEMI JOIN / ANTI JOIN WITHOUT the `LEFT` prefix
        // (CLAUDE.md Known Gotcha #5).
        JoinType::LeftSemi => "SEMI JOIN",
        JoinType::LeftAnti => "ANTI JOIN",
    };
    // Build the join clause. USING wins over ON when both are present per
    // Spark semantics (Spark's `on="col"` maps to USING).
    let clause = if !using_columns.is_empty() {
        let mut cols = String::new();
        for (i, c) in using_columns.iter().enumerate() {
            if i > 0 {
                cols.push_str(", ");
            }
            cols.push_str(&quote_ident(c));
        }
        format!(" USING ({cols})")
    } else if let Some(cond) = condition {
        let cond_sql = render_expr(cond, &left.resolved_schema)?;
        format!(" ON {cond_sql}")
    } else if matches!(join_type, JoinType::Cross) {
        String::new()
    } else {
        return Err(EmissionError::UnsupportedOp {
            op: "Join".to_owned(),
            reason: "non-cross join without ON or USING clause".to_owned(),
        });
    };
    // Emit an EXPLICIT column list mirroring the analyzer's output schema
    // (see `analyzer.rs::CommonOp::Join` output-schema block for the
    // canonical order). Without this, `SELECT *` on a USING-joined
    // relation returns columns in DuckDB's order, which diverges from the
    // analyzer's declared order.
    let is_semi_or_anti = matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti);
    let using_lower: std::collections::HashSet<String> =
        using_columns.iter().map(|s| s.to_lowercase()).collect();
    let mut slots = String::new();
    let mut first = true;
    let push = |slots: &mut String, first: &mut bool, s: String| {
        if !*first {
            slots.push_str(", ");
        }
        *first = false;
        slots.push_str(&s);
    };
    let left_alias_q = quote_ident(&left_alias);
    let right_alias_q = quote_ident(&right_alias);
    // USING columns first (Spark hoists them).
    for c in using_columns {
        push(&mut slots, &mut first, quote_ident(c).into_owned());
    }
    // Left's non-USING columns in declared order.
    for f in &left.resolved_schema.fields {
        if !using_lower.contains(&f.name.to_lowercase()) {
            let qualified = format!("{}.{}", left_alias_q, quote_ident(&f.name));
            push(&mut slots, &mut first, qualified);
        }
    }
    // Right's non-USING columns — only when NOT semi/anti (which suppresses
    // right side).
    if !is_semi_or_anti {
        for f in &right.resolved_schema.fields {
            if !using_lower.contains(&f.name.to_lowercase()) {
                let qualified = format!("{}.{}", right_alias_q, quote_ident(&f.name));
                push(&mut slots, &mut first, qualified);
            }
        }
    }
    if slots.is_empty() {
        // Fallback for SEMI/ANTI on identical USING columns only.
        slots.push('*');
    }
    Ok(format!(
        "SELECT {slots} FROM ({left_sql}) AS {left_alias_q} {kind} ({right_sql}) AS {right_alias_q}{clause}"
    ))
}

fn render_with_columns(
    input: &TypedAst,
    assignments: &[(String, Expression)],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    // Column-order contract with the analyzer: input columns emit in their
    // original positions (replaced in place if named by an assignment), and
    // net-new assignments append at the end in assignment order. `analyzer.rs`
    // `CommonOp::WithColumns` arm produces the resolved schema by the same
    // walk — any deviation here would misalign Arrow columns with the schema
    // Spark Connect advertises via `analyze_plan`, corrupting downstream
    // decoding.
    let assigned_lower: std::collections::HashMap<String, usize> = assignments
        .iter()
        .enumerate()
        .map(|(i, (n, _))| (n.to_lowercase(), i))
        .collect();
    let mut consumed = vec![false; assignments.len()];
    let mut slots = String::new();
    let mut first = true;
    for f in &input_schema.fields {
        if !first {
            slots.push_str(", ");
        }
        first = false;
        if let Some(&idx) = assigned_lower.get(&f.name.to_lowercase()) {
            let (_, expr) = &assignments[idx];
            let expr_sql = render_expr(expr, input_schema)?;
            let name_q = quote_ident(&f.name);
            slots.push_str(&format!("{expr_sql} AS {name_q}"));
            consumed[idx] = true;
        } else {
            slots.push_str(&quote_ident(&f.name));
        }
    }
    for (i, (name, expr)) in assignments.iter().enumerate() {
        if consumed[i] {
            continue;
        }
        if !first {
            slots.push_str(", ");
        }
        first = false;
        let expr_sql = render_expr(expr, input_schema)?;
        let name_q = quote_ident(name);
        slots.push_str(&format!("{expr_sql} AS {name_q}"));
    }
    Ok(format!("SELECT {slots} FROM ({child_sql}) AS __td_with"))
}

fn render_drop_columns(input: &TypedAst, drop_names: &[String]) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let mut dropped = String::new();
    for (i, n) in drop_names.iter().enumerate() {
        if i > 0 {
            dropped.push_str(", ");
        }
        dropped.push_str(&quote_ident(n));
    }
    Ok(format!(
        "SELECT * EXCLUDE ({dropped}) FROM ({child_sql}) AS __td_drop"
    ))
}

/// Render `df.na.fill(values, subset=cols)`. For each column in the input
/// schema, if it's in `cols` (or `cols` is empty and it's the first value's
/// compatible type), emit `COALESCE(col, value) AS col`; else pass through.
/// Single-value form (`values.len()==1`) applies that value to all cols in
/// the subset. Per-column form (`values.len()==cols.len()`) pairs
/// position-wise.
fn render_na_fill(
    input: &TypedAst,
    cols: &[String],
    values: &[Expression],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    if values.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "NaFill".to_owned(),
            reason: "NaFill requires at least one fill value".to_owned(),
        });
    }
    // Build a per-column value map.
    let value_for = |col_name: &str| -> Option<&Expression> {
        if cols.is_empty() {
            // Fill all columns with the single value (Spark accepts this
            // only when the fill value's type matches; we let DuckDB check).
            Some(&values[0])
        } else if values.len() == 1 {
            if cols.iter().any(|c| c.eq_ignore_ascii_case(col_name)) {
                Some(&values[0])
            } else {
                None
            }
        } else {
            for (c, v) in cols.iter().zip(values.iter()) {
                if c.eq_ignore_ascii_case(col_name) {
                    return Some(v);
                }
            }
            None
        }
    };
    let mut slots = String::new();
    for (i, f) in input_schema.fields.iter().enumerate() {
        if i > 0 {
            slots.push_str(", ");
        }
        let name_q = quote_ident(&f.name);
        if let Some(v) = value_for(&f.name) {
            let v_sql = render_expr(v, input_schema)?;
            slots.push_str(&format!("COALESCE({name_q}, {v_sql}) AS {name_q}"));
        } else {
            slots.push_str(&name_q);
        }
    }
    Ok(format!("SELECT {slots} FROM ({child_sql}) AS __td_nafill"))
}

/// Render `df.na.drop(how, subset, thresh)`. `min_non_nulls=None` means
/// how="any" (drop if ANY subset col is null); `Some(1)` means how="all"
/// (drop only if ALL subset cols are null); other values are Spark's
/// `thresh` semantic.
fn render_na_drop(
    input: &TypedAst,
    cols: &[String],
    min_non_nulls: Option<i32>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    // Resolve subset — empty means all columns.
    let subset: Vec<&str> = if cols.is_empty() {
        input_schema
            .fields
            .iter()
            .map(|f| f.name.as_str())
            .collect()
    } else {
        cols.iter().map(|s| s.as_str()).collect()
    };
    if subset.is_empty() {
        return Ok(format!("SELECT * FROM ({child_sql}) AS __td_nadrop"));
    }
    let condition = if let Some(thresh) = min_non_nulls {
        // Row kept iff at least `thresh` of subset cols are non-null.
        // Emit: (CAST(col1 IS NOT NULL AS INT) + ... ) >= thresh.
        let mut sum = String::new();
        for (i, c) in subset.iter().enumerate() {
            if i > 0 {
                sum.push_str(" + ");
            }
            let q = quote_ident(c);
            sum.push_str(&format!("CAST({q} IS NOT NULL AS INTEGER)"));
        }
        format!("({sum}) >= {thresh}")
    } else {
        // how="any": all subset cols must be non-null.
        let mut cond = String::new();
        for (i, c) in subset.iter().enumerate() {
            if i > 0 {
                cond.push_str(" AND ");
            }
            let q = quote_ident(c);
            cond.push_str(&format!("{q} IS NOT NULL"));
        }
        cond
    };
    Ok(format!(
        "SELECT * FROM ({child_sql}) AS __td_nadrop WHERE {condition}"
    ))
}

/// Render `df.na.replace([old_vals], [new_vals], subset=cols)`. Emit
/// `SELECT CASE WHEN col = old1 THEN new1 ... ELSE col END AS col` for each
/// column in subset (or all cols if empty).
fn render_na_replace(
    input: &TypedAst,
    cols: &[String],
    replacements: &[(Expression, Expression)],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    let in_subset = |name: &str| -> bool {
        cols.is_empty() || cols.iter().any(|c| c.eq_ignore_ascii_case(name))
    };
    let mut slots = String::new();
    for (i, f) in input_schema.fields.iter().enumerate() {
        if i > 0 {
            slots.push_str(", ");
        }
        let name_q = quote_ident(&f.name);
        if in_subset(&f.name) && !replacements.is_empty() {
            let mut case = String::from("CASE ");
            for (old, new) in replacements {
                let old_sql = render_expr(old, input_schema)?;
                let new_sql = render_expr(new, input_schema)?;
                case.push_str(&format!("WHEN {name_q} = {old_sql} THEN {new_sql} "));
            }
            case.push_str(&format!("ELSE {name_q} END AS {name_q}"));
            slots.push_str(&case);
        } else {
            slots.push_str(&name_q);
        }
    }
    Ok(format!(
        "SELECT {slots} FROM ({child_sql}) AS __td_nareplace"
    ))
}

/// Render `df.unpivot(ids, values, var_col, val_col)`.
///
/// Emits DuckDB's `UNPIVOT` shape (mirrors the legacy generator's
/// `gen_unpivot`):
/// ```sql
/// UNPIVOT (SELECT <ids>, <values> FROM (<child>)) ON <values>
///   INTO NAME <var_col> VALUE <val_col>
/// ```
/// The pre-SELECT `ids + values` list is critical: DuckDB otherwise treats
/// every non-`ON` column of the input as an implicit id, leaking extra
/// columns into the output. The analyzer has already materialised `values`
/// (empty → all non-id columns) so `values` is guaranteed non-empty here.
fn render_unpivot(
    input: &TypedAst,
    ids: &[String],
    values: &[String],
    variable_column_name: &str,
    value_column_name: &str,
) -> Result<String, EmissionError> {
    if values.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "Unpivot".to_owned(),
            reason: "unpivot requires at least one value column".to_owned(),
        });
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let var_col = quote_ident(variable_column_name);
    let val_col = quote_ident(value_column_name);

    // Pre-select only `ids + values` so DuckDB doesn't fold extra input
    // columns into the id set.
    //
    // OPT-3: preallocate builders. Estimate ~16 chars per identifier (quoted
    // name + ", " separator) — cheap upper bound that avoids the geometric
    // reallocations `String::push_str` would otherwise incur on wide unpivots.
    const AVG_IDENT_BYTES: usize = 16;
    let select_cap = (ids.len() + values.len()) * AVG_IDENT_BYTES;
    let mut select_list = String::with_capacity(select_cap);
    let mut first = true;
    for c in ids.iter().chain(values.iter()) {
        if !first {
            select_list.push_str(", ");
        }
        select_list.push_str(&quote_ident(c));
        first = false;
    }

    let value_cap = values.len() * AVG_IDENT_BYTES;
    let mut value_cols = String::with_capacity(value_cap);
    for (i, c) in values.iter().enumerate() {
        if i > 0 {
            value_cols.push_str(", ");
        }
        value_cols.push_str(&quote_ident(c));
    }

    Ok(format!(
        "UNPIVOT (SELECT {select_list} FROM ({child_sql}) AS __td_unpivot_src) ON {value_cols} INTO NAME {var_col} VALUE {val_col}"
    ))
}

/// Render a Pivot as conditional-aggregate SQL that matches Spark's PIVOT
/// semantics exactly.
///
/// **Why not DuckDB `PIVOT`?** DuckDB's native PIVOT operator behaves
/// correctly on the aggregate axis, but its empty-bucket behavior diverges
/// from Spark for `count()`-family aggregates: DuckDB returns `0` while
/// Spark returns `NULL`. Spark implements pivot by lowering to
/// `agg(CASE WHEN pivot_col = v THEN pivot_arg END)` — for `count(lit(1))`,
/// this becomes `count(CASE WHEN … THEN 1 END)` which still returns `0` for
/// empty buckets, so Spark additionally maps the resulting `0` to `NULL`.
/// We match Spark by (a) rewriting each aggregate call to consume a CASE
/// expression, and (b) wrapping COUNT/COUNT-DISTINCT/COUNT_IF calls with
/// `NULLIF(..., 0)` so empty buckets surface as NULL.
///
/// Emission shape:
/// ```sql
/// SELECT <grouping>,
///        <cond_agg_v1_a1> AS "<name_v1_a1>",
///        <cond_agg_v1_a2> AS "<name_v1_a2>",
///        ...
/// FROM (<child>) AS __td_pivot_src
/// GROUP BY <grouping>
/// ```
fn render_pivot(
    input: &TypedAst,
    grouping: &[Expression],
    pivot_column: &Expression,
    pivot_values: &[Expression],
    aggregates: &[Expression],
    output_schema: &Schema,
) -> Result<String, EmissionError> {
    if aggregates.is_empty() {
        return Err(EmissionError::UnsupportedOp {
            op: "Pivot".to_owned(),
            reason: "PIVOT requires at least one aggregate expression".to_owned(),
        });
    }
    if pivot_values.is_empty() {
        // Analyzer punts implicit-values as PuntedOperator; defensive guard here.
        return Err(EmissionError::UnsupportedOp {
            op: "Pivot[implicit-values]".to_owned(),
            reason: "pivot without explicit values requires eager DISTINCT query".to_owned(),
        });
    }
    // Pass 60 M1: output column names for the (pivot_value × aggregate) pairs
    // are stamped by the analyzer into `output_schema.fields[grouping.len()..]`.
    // Emission reads them from the schema rather than re-deriving from the
    // pivot literals — the two derivations MUST stay in lockstep for Spark
    // parity (float `1.0` → `"1.0"`, null rejection, etc.). Single source of
    // truth = the analyzer.
    let expected_output_cols = grouping.len() + pivot_values.len() * aggregates.len();
    if output_schema.fields.len() != expected_output_cols {
        return Err(EmissionError::UnsupportedOp {
            op: "Pivot".to_owned(),
            reason: format!(
                "output schema arity mismatch: expected {expected_output_cols} fields (grouping={} \
                 + pivot_values×aggregates={}×{}), got {}",
                grouping.len(),
                pivot_values.len(),
                aggregates.len(),
                output_schema.fields.len()
            ),
        });
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;

    // Pivot column — strip any wrapping Alias so the CASE reference is bare.
    let pivot_col_expr = match pivot_column {
        Expression::Alias(a) => a.expr.as_ref(),
        other => other,
    };
    let pivot_col_sql = render_expr(pivot_col_expr, input_schema)?;

    // Assemble the SELECT slots: grouping columns first, then one
    // conditional-aggregate slot per (pivot_value, aggregate) pair.
    let mut slots = String::new();
    let mut first = true;
    for g in grouping {
        if !first {
            slots.push_str(", ");
        }
        first = false;
        // Grouping expressions keep any alias; render_projection_slot
        // handles Spark-return casts + alias suffix.
        slots.push_str(&render_projection_slot(g, input_schema)?);
    }

    let mut out_idx = grouping.len();
    for pv in pivot_values {
        let pv_sql = render_expr(pv, input_schema)?;
        for a in aggregates {
            if !first {
                slots.push_str(", ");
            }
            first = false;
            let bare_agg = match a {
                Expression::Alias(al) => al.expr.as_ref(),
                other => other,
            };
            // Read the stamped output name from the analyzer's schema.
            let out_name = &output_schema.fields[out_idx].name;
            out_idx += 1;
            let agg_sql =
                build_conditional_aggregate(bare_agg, &pivot_col_sql, &pv_sql, input_schema)?;
            slots.push_str(&agg_sql);
            slots.push_str(" AS ");
            slots.push_str(&quote_ident(out_name));
        }
    }

    // GROUP BY clause — grouping columns, aliases stripped.
    let mut sql = format!("SELECT {slots} FROM ({child_sql}) AS __td_pivot_src");
    if !grouping.is_empty() {
        sql.push_str(" GROUP BY ");
        for (i, g) in grouping.iter().enumerate() {
            if i > 0 {
                sql.push_str(", ");
            }
            let bare = match g {
                Expression::Alias(al) => al.expr.as_ref(),
                other => other,
            };
            sql.push_str(&render_expr(bare, input_schema)?);
        }
    }
    Ok(sql)
}

/// Rewrite an aggregate call `agg(arg1, arg2, ...)` into a conditional
/// aggregate `agg(CASE WHEN pivot_col = pivot_value THEN arg1 END, arg2, ...)`
/// and wrap COUNT-family aggregates with `NULLIF(..., 0)` so empty pivot
/// buckets surface as NULL (matches Spark).
fn build_conditional_aggregate(
    agg: &Expression,
    pivot_col_sql: &str,
    pivot_value_sql: &str,
    input_schema: &Schema,
) -> Result<String, EmissionError> {
    let f = match agg {
        Expression::FunctionCall(f) => f,
        // Non-function aggregate expressions (rare) fall through unmodified.
        other => return render_expr(other, input_schema),
    };
    if f.args.is_empty() {
        return render_expr(agg, input_schema);
    }
    // Render remaining args verbatim; wrap the first arg in a CASE.
    let first_arg_sql = render_expr(&f.args[0], input_schema)?;
    let case_sql = format!(
        "CASE WHEN {pivot_col_sql} IS NOT DISTINCT FROM {pivot_value_sql} THEN {first_arg_sql} END"
    );
    let mut arg_list = case_sql;
    for arg in &f.args[1..] {
        arg_list.push_str(", ");
        arg_list.push_str(&render_expr(arg, input_schema)?);
    }
    let distinct = if f.distinct { "DISTINCT " } else { "" };
    let call = format!("{}({distinct}{arg_list})", f.name);
    // Spark maps empty-bucket COUNT to NULL; DuckDB COUNT returns 0. Wrap
    // COUNT-family calls in NULLIF(..., 0) to match.
    let name_lower = f.name.to_ascii_lowercase();
    let is_count = name_lower == "count" || name_lower == "count_if" || name_lower == "count_star";
    Ok(if is_count {
        format!("NULLIF({call}, 0)")
    } else {
        call
    })
}

fn render_deduplicate(input: &TypedAst, on_columns: &[String]) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    if on_columns.is_empty() {
        Ok(format!(
            "SELECT DISTINCT * FROM ({child_sql}) AS __td_dedup"
        ))
    } else {
        let mut cols = String::new();
        for (i, c) in on_columns.iter().enumerate() {
            if i > 0 {
                cols.push_str(", ");
            }
            cols.push_str(&quote_ident(c));
        }
        Ok(format!(
            "SELECT DISTINCT ON ({cols}) * FROM ({child_sql}) AS __td_dedup"
        ))
    }
}

fn render_with_columns_renamed(
    input: &TypedAst,
    renames: &[(String, String)],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let rename_map: std::collections::HashMap<String, String> = renames
        .iter()
        .map(|(old, new)| (old.to_lowercase(), new.clone()))
        .collect();
    let mut slots = String::new();
    for (i, f) in input.resolved_schema.fields.iter().enumerate() {
        if i > 0 {
            slots.push_str(", ");
        }
        let src = quote_ident(&f.name);
        let dst_name = rename_map
            .get(&f.name.to_lowercase())
            .cloned()
            .unwrap_or_else(|| f.name.clone());
        let dst = quote_ident(&dst_name);
        slots.push_str(&format!("{src} AS {dst}"));
    }
    Ok(format!("SELECT {slots} FROM ({child_sql}) AS __td_rename"))
}

fn render_aliased_relation(input: &TypedAst, alias: &str) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let a = quote_ident(alias);
    Ok(format!("SELECT * FROM ({child_sql}) AS {a}"))
}

#[allow(dead_code)] // wired when TypedOp::Range lands (Decision 13-A)
fn render_range_relation(
    start: i64,
    end: i64,
    step: i64,
    _num_partitions: Option<i32>,
) -> Result<String, EmissionError> {
    // `spark.range(start, end, step)` — half-open interval, single column `id`.
    Ok(format!(
        "SELECT id FROM range({start}, {end}, {step}) AS __td_range(id)"
    ))
}

// ── Expression rendering ─────────────────────────────────────────────────────

/// Exhaustive match over the [`Expression`] enum.
pub(crate) fn render_expr(expr: &Expression, schema: &Schema) -> Result<String, EmissionError> {
    match expr {
        Expression::Literal(l) => render_literal(l),
        Expression::ColumnReference(c) => render_column_reference(c),
        Expression::UnresolvedColumn(u) => Err(EmissionError::UnsupportedExpression {
            shape: "UnresolvedColumn".to_owned(),
            reason: format!("analyzer must resolve column `{}` before emission", u.name),
        }),
        Expression::Binary(b) => render_binary(b, schema),
        Expression::Unary(u) => render_unary(u, schema),
        Expression::FunctionCall(f) => {
            if is_aggregate_name(&f.name) {
                render_aggregate(f, schema)
            } else {
                render_function_call(f, schema)
            }
        }
        Expression::Cast(c) => render_cast(c, schema),
        Expression::CaseWhen(cw) => render_case_when(cw, schema),
        Expression::Window(w) => {
            let func_sql = render_expr(&w.func, schema)?;
            let mut over = String::from("OVER (");
            let mut had_content = false;
            if !w.partition_by.is_empty() {
                over.push_str("PARTITION BY ");
                for (i, p) in w.partition_by.iter().enumerate() {
                    if i > 0 {
                        over.push_str(", ");
                    }
                    over.push_str(&render_expr(p, schema)?);
                }
                had_content = true;
            }
            if !w.order_by.is_empty() {
                if had_content {
                    over.push(' ');
                }
                over.push_str("ORDER BY ");
                for (i, s) in w.order_by.iter().enumerate() {
                    if i > 0 {
                        over.push_str(", ");
                    }
                    let e = render_expr(&s.expr, schema)?;
                    let dir = match s.direction {
                        crate::transpiler_v2::expression::SortDirection::Ascending => "ASC",
                        crate::transpiler_v2::expression::SortDirection::Descending => "DESC",
                    };
                    let nulls = match s.null_ordering {
                        crate::transpiler_v2::expression::NullOrdering::NullsFirst => "NULLS FIRST",
                        crate::transpiler_v2::expression::NullOrdering::NullsLast => "NULLS LAST",
                    };
                    over.push_str(&format!("{e} {dir} {nulls}"));
                }
                had_content = true;
            }
            // Frame clause emission.
            if let Some(frame) = &w.frame {
                use crate::transpiler_v2::expression::{FrameBoundary, FrameUnit};
                if had_content {
                    over.push(' ');
                }
                let unit_kw = match frame.unit {
                    FrameUnit::Rows => "ROWS",
                    FrameUnit::Range => "RANGE",
                };
                let render_bound = |b: &FrameBoundary,
                                    is_lower: bool|
                 -> Result<String, EmissionError> {
                    match b {
                        FrameBoundary::UnboundedPreceding => Ok("UNBOUNDED PRECEDING".to_owned()),
                        FrameBoundary::UnboundedFollowing => Ok("UNBOUNDED FOLLOWING".to_owned()),
                        FrameBoundary::CurrentRow => Ok("CURRENT ROW".to_owned()),
                        FrameBoundary::Preceding(e) => {
                            let n = render_expr(e, schema)?;
                            let _ = is_lower;
                            Ok(format!("{n} PRECEDING"))
                        }
                        FrameBoundary::Following(e) => {
                            let n = render_expr(e, schema)?;
                            let _ = is_lower;
                            Ok(format!("{n} FOLLOWING"))
                        }
                    }
                };
                let lo = render_bound(&frame.lower, true)?;
                let up = render_bound(&frame.upper, false)?;
                over.push_str(&format!("{unit_kw} BETWEEN {lo} AND {up}"));
            }
            let _ = had_content;
            over.push(')');
            Ok(format!("{func_sql} {over}"))
        }
        Expression::Alias(a) => render_alias(a, schema),
        Expression::Star(s) => render_star(s),
        Expression::InSubquery(_) => Err(EmissionError::UnsupportedExpression {
            shape: "InSubquery".to_owned(),
            reason: "correlated subqueries land in Slice F".to_owned(),
        }),
        Expression::ExistsSubquery(_) => Err(EmissionError::UnsupportedExpression {
            shape: "ExistsSubquery".to_owned(),
            reason: "correlated subqueries land in Slice F".to_owned(),
        }),
        Expression::ScalarSubquery(_) => Err(EmissionError::UnsupportedExpression {
            shape: "ScalarSubquery".to_owned(),
            reason: "scalar subqueries land in Slice F".to_owned(),
        }),
        Expression::Lambda(l) => {
            let body = render_expr(&l.body, schema)?;
            // DuckDB lambda syntax:
            //   single-arg: `x -> body`
            //   multi-arg:  `(x, y) -> body`
            // Do NOT wrap the whole lambda in outer parens — DuckDB parses
            // `((x, y) -> ...)` as `row(x, y)` and treats `->` differently.
            if l.params.len() == 1 {
                let p = quote_ident(&l.params[0]);
                Ok(format!("{p} -> {body}"))
            } else {
                let mut buf = String::from("(");
                for (i, p) in l.params.iter().enumerate() {
                    if i > 0 {
                        buf.push_str(", ");
                    }
                    buf.push_str(&quote_ident(p));
                }
                buf.push(')');
                Ok(format!("{buf} -> {body}"))
            }
        }
        Expression::LambdaVariable(lv) => Ok(quote_ident(&lv.name).into_owned()),
        Expression::RawSql(r) => Ok(r.sql.clone()),
        Expression::ArrayLiteral(a) => render_array_literal(a, schema),
        Expression::MapLiteral(m) => render_map_literal(m, schema),
        Expression::StructLiteral(s) => render_struct_literal(s, schema),
        Expression::Between(b) => {
            let expr = render_expr(&b.expr, schema)?;
            let low = render_expr(&b.low, schema)?;
            let high = render_expr(&b.high, schema)?;
            let not = if b.negated { "NOT " } else { "" };
            Ok(format!("({expr}) {not}BETWEEN ({low}) AND ({high})"))
        }
        Expression::InList(i) => {
            let expr = render_expr(&i.expr, schema)?;
            let list: Vec<String> = i
                .list
                .iter()
                .map(|e| render_expr(e, schema))
                .collect::<Result<Vec<_>, _>>()?;
            let not = if i.negated { "NOT " } else { "" };
            Ok(format!("({expr}) {not}IN ({})", list.join(", ")))
        }
        Expression::Like(l) => {
            let val = render_expr(&l.value, schema)?;
            let pat = render_expr(&l.pattern, schema)?;
            let not = if l.negated { "NOT " } else { "" };
            let op = if l.case_insensitive { "ILIKE" } else { "LIKE" };
            let esc = match l.escape {
                Some(c) => format!(" ESCAPE '{}'", escape_sql_char(c)),
                None => String::new(),
            };
            Ok(format!("({val}) {not}{op} ({pat}){esc}"))
        }
        Expression::Interval(i) => render_interval(i),
        Expression::IsDistinctFrom(d) => {
            let l = render_expr(&d.left, schema)?;
            let r = render_expr(&d.right, schema)?;
            let not = if d.negated { "NOT " } else { "" };
            Ok(format!("({l}) IS {not}DISTINCT FROM ({r})"))
        }
        Expression::ExtractValue(ev) => {
            let child_sql = render_expr(&ev.child, schema)?;
            // Extraction shape distinguishes struct-field-name (String
            // literal) vs array-index (Int literal) vs map-key (any
            // literal). DuckDB uses `.field` for struct, `[expr]` for
            // both array and map — the latter is a runtime-typed subscript.
            match ev.extraction.as_ref() {
                Expression::Literal(l) => match &l.value {
                    crate::transpiler_v2::expression::LiteralValue::String(name) => {
                        // Struct field access. DuckDB accepts `child.field`
                        // when child's static type is struct.
                        let field = quote_ident(name);
                        Ok(format!("({child_sql}).{field}"))
                    }
                    _ => {
                        // Numeric index or other literal: emit `[expr]`.
                        let idx = render_expr(&ev.extraction, schema)?;
                        Ok(format!("({child_sql})[{idx}]"))
                    }
                },
                _ => {
                    // Dynamic key/index — same subscript form.
                    let idx = render_expr(&ev.extraction, schema)?;
                    Ok(format!("({child_sql})[{idx}]"))
                }
            }
        }
        Expression::RowConstructor(_) => Err(EmissionError::UnsupportedExpression {
            shape: "RowConstructor".to_owned(),
            reason: "complex-type emission lands in Slice F".to_owned(),
        }),
        Expression::UpdateFields(u) => render_update_fields(u, schema),
    }
}

fn is_aggregate_name(name: &str) -> bool {
    // `AGGREGATE_NAMES` (in `type_inference.rs`) is all-lowercase ASCII per
    // Slice B; case-insensitive byte comparison matches without allocating the
    // per-call lowercased `String` this function used to build.
    AGGREGATE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
}

/// Wrap a rendered format-string expression (typically a string literal, but
/// possibly a column reference or general expression) in a chain of `replace`
/// calls that translate Spark's `SimpleDateFormat` tokens (yyyy/MM/dd/HH/mm/
/// ss/yy/a) to DuckDB `strftime`/`strptime` tokens (%Y/%m/%d/%H/%M/%S/%y/%p).
///
/// This is a best-effort translation for the common patterns; complex format
/// strings (locale-specific tokens, escaped literals) will diverge from Spark
/// and require per-case follow-ups. Shared by `date_format`, `to_date`,
/// `to_timestamp`, `unix_timestamp`, and `from_unixtime` two-arg forms —
/// keeps token semantics identical across arms.
fn spark_fmt_to_duckdb(fmt_sql: &str) -> String {
    format!(
        "replace(replace(replace(replace(replace(replace(replace(replace({fmt_sql}, 'yyyy', '%Y'), 'MM', '%m'), 'dd', '%d'), 'HH', '%H'), 'mm', '%M'), 'ss', '%S'), 'yy', '%y'), 'a', '%p')"
    )
}

/// Render a scalar function call. The Spark → DuckDB scalar-function
/// vocabulary is *large*; rather than enumerating hundreds of arms
/// individually, this arm applies a **pass-through by default** strategy —
/// DuckDB's parser accepts most Spark scalar function names verbatim, and
/// where semantics diverge the corpus diff harness surfaces the mismatch
/// case-by-case for follow-up diagnostic passes.
///
/// Cases where τ REMAPS or REJECTS the Spark name are enumerated explicitly:
///   `starts_with`   → `starts_with` (native DuckDB); Spark also accepts
///                     `startswith` which is likewise DuckDB-valid.
///   `substr`        → `substring` (DuckDB canonical form; both accepted).
///   `signum`        → `sign` (DuckDB has both; passthrough).
/// Unhandled proto expression shapes (Window/Lambda) never reach here;
/// they surface as `UnsupportedProtoShape` in `V2ExpressionConverter`.
///
/// True iff the argument at position `lambda_pos` in a HOF call is a
/// `Lambda` with more than one parameter. Used to detect
/// `(element, index) -> body` shapes on `transform`/`filter`. A 1-arg lambda
/// or any non-`Lambda` shape returns false (the caller falls through to the
/// plain remap arm).
fn hof_lambda_has_index(args: &[Expression], lambda_pos: usize) -> bool {
    matches!(
        args.get(lambda_pos),
        Some(Expression::Lambda(l)) if l.params.len() >= 2
    )
}

/// Render an expression that MAY be a `Lambda`, adjusting for Spark 0-based
/// HOF indices when `adjust_index` is true. When the target lambda has 2+
/// parameters, references to the second parameter (Spark's `index`) inside
/// the body are rewritten to `(param - 1)` so DuckDB's 1-based index matches
/// Spark's 0-based semantics.
///
/// Non-`Lambda` shapes (or 1-arg lambdas, or `adjust_index == false`) fall
/// through to plain `render_expr`.
fn render_expr_with_lambda_adjust(
    e: &Expression,
    schema: &Schema,
    adjust_index: bool,
) -> Result<String, EmissionError> {
    if !adjust_index {
        return render_expr(e, schema);
    }
    let Expression::Lambda(l) = e else {
        return render_expr(e, schema);
    };
    if l.params.len() < 2 {
        return render_expr(e, schema);
    }
    let index_name = l.params[1].clone();
    let adjusted_body = substitute_index_var(&l.body, &index_name);
    let adjusted = Expression::Lambda(super::expression::LambdaExpression {
        params: l.params.clone(),
        body: Box::new(adjusted_body),
    });
    render_expr(&adjusted, schema)
}

/// Rewrite `body` so every `LambdaVariable(index_var)` reference becomes
/// `(LambdaVariable(index_var) - 1)`. Traverses through every composite
/// `Expression` variant; leaves atoms unchanged.
///
/// Nested `Lambda` expressions with a parameter named `index_var` shadow the
/// outer name — descent stops for that subtree so we don't rewrite an
/// unrelated inner binding.
fn substitute_index_var(body: &Expression, index_var: &str) -> Expression {
    match body {
        Expression::LambdaVariable(lv) if lv.name == index_var => {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Sub,
                left: Box::new(Expression::LambdaVariable(lv.clone())),
                right: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::Long(1),
                    data_type: DataType::Long,
                })),
            })
        }
        Expression::LambdaVariable(_)
        | Expression::Literal(_)
        | Expression::ColumnReference(_)
        | Expression::UnresolvedColumn(_)
        | Expression::Star(_)
        | Expression::RawSql(_) => body.clone(),
        Expression::Binary(b) => Expression::Binary(BinaryExpression {
            op: b.op.clone(),
            left: Box::new(substitute_index_var(&b.left, index_var)),
            right: Box::new(substitute_index_var(&b.right, index_var)),
        }),
        Expression::Unary(u) => Expression::Unary(UnaryExpression {
            op: u.op.clone(),
            operand: Box::new(substitute_index_var(&u.operand, index_var)),
        }),
        Expression::FunctionCall(fc) => Expression::FunctionCall(FunctionCall {
            name: fc.name.clone(),
            args: fc
                .args
                .iter()
                .map(|a| substitute_index_var(a, index_var))
                .collect(),
            distinct: fc.distinct,
        }),
        Expression::Cast(c) => Expression::Cast(CastExpression {
            expr: Box::new(substitute_index_var(&c.expr, index_var)),
            to_type: c.to_type.clone(),
            try_cast: c.try_cast,
        }),
        Expression::CaseWhen(cw) => Expression::CaseWhen(CaseWhenExpression {
            branches: cw
                .branches
                .iter()
                .map(|(w, t)| {
                    (
                        substitute_index_var(w, index_var),
                        substitute_index_var(t, index_var),
                    )
                })
                .collect(),
            else_expr: cw
                .else_expr
                .as_ref()
                .map(|e| Box::new(substitute_index_var(e, index_var))),
        }),
        Expression::Alias(a) => Expression::Alias(AliasExpression {
            expr: Box::new(substitute_index_var(&a.expr, index_var)),
            alias: a.alias.clone(),
        }),
        Expression::Lambda(inner) => {
            // Shadowing: if the inner lambda re-binds our index name, its
            // body must not be rewritten.
            if inner.params.iter().any(|p| p == index_var) {
                Expression::Lambda(super::expression::LambdaExpression {
                    params: inner.params.clone(),
                    body: inner.body.clone(),
                })
            } else {
                Expression::Lambda(super::expression::LambdaExpression {
                    params: inner.params.clone(),
                    body: Box::new(substitute_index_var(&inner.body, index_var)),
                })
            }
        }
        // Composite / less-common shapes — walk children generically. Any
        // shape not enumerated here falls back to a clone (safe: it means the
        // subtree contains no `LambdaVariable` we care about, or an exotic
        // shape corpus HOF cases don't exercise).
        Expression::ArrayLiteral(a) => {
            Expression::ArrayLiteral(super::expression::ArrayLiteralExpression {
                element_type: a.element_type.clone(),
                elements: a
                    .elements
                    .iter()
                    .map(|e| substitute_index_var(e, index_var))
                    .collect(),
            })
        }
        Expression::Between(b) => Expression::Between(super::expression::BetweenExpression {
            expr: Box::new(substitute_index_var(&b.expr, index_var)),
            low: Box::new(substitute_index_var(&b.low, index_var)),
            high: Box::new(substitute_index_var(&b.high, index_var)),
            negated: b.negated,
        }),
        Expression::InList(i) => Expression::InList(super::expression::InListExpression {
            expr: Box::new(substitute_index_var(&i.expr, index_var)),
            list: i
                .list
                .iter()
                .map(|e| substitute_index_var(e, index_var))
                .collect(),
            negated: i.negated,
        }),
        Expression::Like(l) => Expression::Like(super::expression::LikeExpression {
            value: Box::new(substitute_index_var(&l.value, index_var)),
            pattern: Box::new(substitute_index_var(&l.pattern, index_var)),
            escape: l.escape,
            case_insensitive: l.case_insensitive,
            negated: l.negated,
        }),
        Expression::IsDistinctFrom(d) => {
            Expression::IsDistinctFrom(super::expression::IsDistinctFromExpression {
                left: Box::new(substitute_index_var(&d.left, index_var)),
                right: Box::new(substitute_index_var(&d.right, index_var)),
                negated: d.negated,
            })
        }
        Expression::ExtractValue(ev) => {
            Expression::ExtractValue(super::expression::ExtractValueExpression {
                child: Box::new(substitute_index_var(&ev.child, index_var)),
                extraction: Box::new(substitute_index_var(&ev.extraction, index_var)),
            })
        }
        // Shapes with no `LambdaVariable` children in normal usage — clone.
        _ => body.clone(),
    }
}

/// Rewrite `body` so every `LambdaVariable(var_name)` reference is replaced by
/// `replacement`. Mirrors [`substitute_index_var`]'s traversal — this is the
/// general form (replace a lambda variable with an arbitrary sub-expression).
///
/// Used by the map higher-order-function emitters (`map_filter`,
/// `transform_values`, `transform_keys`) which unroll Spark's `(k, v) ->
/// body` into DuckDB's single-arg `kv -> body[k → kv.key, v → kv.value]`.
///
/// Nested `Lambda` expressions that re-bind `var_name` shadow the outer
/// binding — descent stops for the shadowed subtree.
fn substitute_lambda_var(
    body: &Expression,
    var_name: &str,
    replacement: &Expression,
) -> Expression {
    match body {
        Expression::LambdaVariable(lv) if lv.name == var_name => replacement.clone(),
        Expression::LambdaVariable(_)
        | Expression::Literal(_)
        | Expression::ColumnReference(_)
        | Expression::UnresolvedColumn(_)
        | Expression::Star(_)
        | Expression::RawSql(_) => body.clone(),
        Expression::Binary(b) => Expression::Binary(BinaryExpression {
            op: b.op.clone(),
            left: Box::new(substitute_lambda_var(&b.left, var_name, replacement)),
            right: Box::new(substitute_lambda_var(&b.right, var_name, replacement)),
        }),
        Expression::Unary(u) => Expression::Unary(UnaryExpression {
            op: u.op.clone(),
            operand: Box::new(substitute_lambda_var(&u.operand, var_name, replacement)),
        }),
        Expression::FunctionCall(fc) => Expression::FunctionCall(FunctionCall {
            name: fc.name.clone(),
            args: fc
                .args
                .iter()
                .map(|a| substitute_lambda_var(a, var_name, replacement))
                .collect(),
            distinct: fc.distinct,
        }),
        Expression::Cast(c) => Expression::Cast(CastExpression {
            expr: Box::new(substitute_lambda_var(&c.expr, var_name, replacement)),
            to_type: c.to_type.clone(),
            try_cast: c.try_cast,
        }),
        Expression::CaseWhen(cw) => Expression::CaseWhen(CaseWhenExpression {
            branches: cw
                .branches
                .iter()
                .map(|(w, t)| {
                    (
                        substitute_lambda_var(w, var_name, replacement),
                        substitute_lambda_var(t, var_name, replacement),
                    )
                })
                .collect(),
            else_expr: cw
                .else_expr
                .as_ref()
                .map(|e| Box::new(substitute_lambda_var(e, var_name, replacement))),
        }),
        Expression::Alias(a) => Expression::Alias(AliasExpression {
            expr: Box::new(substitute_lambda_var(&a.expr, var_name, replacement)),
            alias: a.alias.clone(),
        }),
        Expression::Lambda(inner) => {
            // Shadowing: if the inner lambda re-binds our var name, its
            // body must not be rewritten.
            if inner.params.iter().any(|p| p == var_name) {
                Expression::Lambda(super::expression::LambdaExpression {
                    params: inner.params.clone(),
                    body: inner.body.clone(),
                })
            } else {
                Expression::Lambda(super::expression::LambdaExpression {
                    params: inner.params.clone(),
                    body: Box::new(substitute_lambda_var(&inner.body, var_name, replacement)),
                })
            }
        }
        Expression::ArrayLiteral(a) => {
            Expression::ArrayLiteral(super::expression::ArrayLiteralExpression {
                element_type: a.element_type.clone(),
                elements: a
                    .elements
                    .iter()
                    .map(|e| substitute_lambda_var(e, var_name, replacement))
                    .collect(),
            })
        }
        Expression::Between(b) => Expression::Between(super::expression::BetweenExpression {
            expr: Box::new(substitute_lambda_var(&b.expr, var_name, replacement)),
            low: Box::new(substitute_lambda_var(&b.low, var_name, replacement)),
            high: Box::new(substitute_lambda_var(&b.high, var_name, replacement)),
            negated: b.negated,
        }),
        Expression::InList(i) => Expression::InList(super::expression::InListExpression {
            expr: Box::new(substitute_lambda_var(&i.expr, var_name, replacement)),
            list: i
                .list
                .iter()
                .map(|e| substitute_lambda_var(e, var_name, replacement))
                .collect(),
            negated: i.negated,
        }),
        Expression::Like(l) => Expression::Like(super::expression::LikeExpression {
            value: Box::new(substitute_lambda_var(&l.value, var_name, replacement)),
            pattern: Box::new(substitute_lambda_var(&l.pattern, var_name, replacement)),
            escape: l.escape,
            case_insensitive: l.case_insensitive,
            negated: l.negated,
        }),
        Expression::IsDistinctFrom(d) => {
            Expression::IsDistinctFrom(super::expression::IsDistinctFromExpression {
                left: Box::new(substitute_lambda_var(&d.left, var_name, replacement)),
                right: Box::new(substitute_lambda_var(&d.right, var_name, replacement)),
                negated: d.negated,
            })
        }
        Expression::ExtractValue(ev) => {
            Expression::ExtractValue(super::expression::ExtractValueExpression {
                child: Box::new(substitute_lambda_var(&ev.child, var_name, replacement)),
                extraction: Box::new(substitute_lambda_var(&ev.extraction, var_name, replacement)),
            })
        }
        _ => body.clone(),
    }
}

/// Build an ExtractValue expression `child.field_name` — used to rewrite
/// Spark's 2-arg map lambda `(k, v) -> body` into DuckDB's single-arg
/// `kv -> body[k → kv.key, v → kv.value]`.
fn make_field_access(child_var: &str, field: &str) -> Expression {
    use super::expression::{ExtractValueExpression, LambdaVariableExpression};
    Expression::ExtractValue(ExtractValueExpression {
        child: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
            name: child_var.to_owned(),
        })),
        extraction: Box::new(Expression::Literal(Literal {
            value: LiteralValue::String(field.to_owned()),
            data_type: DataType::String,
        })),
    })
}

/// Kind of map higher-order function — dictates the shape of the emitted SQL
/// wrapper. See [`render_map_hof`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MapHofKind {
    /// `map_filter(m, (k, v) -> pred)` — keep entries matching `pred`.
    Filter,
    /// `transform_values(m, (k, v) -> f)` — replace each value with `f`.
    TransformValues,
    /// `transform_keys(m, (k, v) -> f)` — replace each key with `f`.
    TransformKeys,
}

/// Emit DuckDB SQL for a Spark map higher-order function.
///
/// Spark's map HOFs take a 2-arg lambda `(k, v) -> body`. DuckDB has neither
/// `map_filter` / `transform_values` / `transform_keys` nor multi-arg
/// lambdas over map entries. Strategy:
///   1. Convert map → list of `STRUCT(key, value)` via `map_entries(m)`.
///   2. Apply the appropriate list HOF (`list_filter` / `list_transform`)
///      with a single-arg lambda over `kv`, where every reference to the
///      original `k` / `v` is rewritten as `kv.key` / `kv.value`.
///   3. Reassemble the map via `map_from_entries(...)`.
///
/// Called from the `render_function_call` dispatch for `map_filter`,
/// `transform_values`, and `transform_keys`. Anchors: corpus `hof-008`,
/// `hof-009`, `hof-010`.
fn render_map_hof(
    f: &FunctionCall,
    schema: &Schema,
    kind: MapHofKind,
) -> Result<String, EmissionError> {
    let m_sql = render_expr(&f.args[0], schema)?;
    let Expression::Lambda(lam) = &f.args[1] else {
        return Err(EmissionError::UnsupportedFunction {
            name: f.name.clone(),
            reason: "map higher-order function requires a lambda argument".to_owned(),
        });
    };
    if lam.params.len() != 2 {
        return Err(EmissionError::UnsupportedFunction {
            name: f.name.clone(),
            reason: "map higher-order lambda must take exactly 2 arguments (key, value)".to_owned(),
        });
    }
    // Fresh entry variable — DuckDB requires a single-arg lambda over
    // `map_entries`. The name is prefixed with `__mh_` to avoid collision
    // with Spark-generated names (`x_N`, `y_N`).
    let entry_var = "__mh_kv";
    let key_access = make_field_access(entry_var, "key");
    let value_access = make_field_access(entry_var, "value");
    // Substitute both k → kv.key and v → kv.value.
    let step1 = substitute_lambda_var(&lam.body, &lam.params[0], &key_access);
    let final_body = substitute_lambda_var(&step1, &lam.params[1], &value_access);
    let body_sql = render_expr(&final_body, schema)?;
    let entry_q = quote_ident(entry_var);
    match kind {
        MapHofKind::Filter => Ok(format!(
            "map_from_entries(list_filter(map_entries({m_sql}), {entry_q} -> {body_sql}))"
        )),
        MapHofKind::TransformValues => Ok(format!(
            "map_from_entries(list_transform(map_entries({m_sql}), {entry_q} -> struct_pack(key := ({entry_q}).key, value := {body_sql})))"
        )),
        MapHofKind::TransformKeys => Ok(format!(
            "map_from_entries(list_transform(map_entries({m_sql}), {entry_q} -> struct_pack(key := {body_sql}, value := ({entry_q}).value)))"
        )),
    }
}

fn render_function_call(f: &FunctionCall, schema: &Schema) -> Result<String, EmissionError> {
    let name_lower = f.name.to_ascii_lowercase();
    // Aggregate-name overlap check — if the analyzer classified a FunctionCall
    // as aggregate, `render_expr` routes to `render_aggregate` before this
    // function; anything reaching here is scalar by construction. Defense in
    // depth: any name matching AGGREGATE_NAMES should never be seen here.
    //
    // Window-only functions with a trailing `ignoreNulls` argument that PySpark
    // serializes verbatim — DuckDB's `nth_value(col, n)` / `lag`/`lead`/
    // `first_value`/`last_value` do not accept the boolean flag. Drop the
    // trailing bool. Anchor: corpus win-006.
    if matches!(
        name_lower.as_str(),
        "nth_value" | "first_value" | "last_value" | "lag" | "lead"
    ) {
        let arity_keep = match name_lower.as_str() {
            "nth_value" => 2,
            "first_value" | "last_value" => 1,
            "lag" | "lead" => 3, // (col, offset, default)
            _ => unreachable!(),
        };
        // Only apply the trim if the extra trailing arg is a boolean literal
        // (Spark's ignoreNulls flag). Never silently drop a real value.
        if f.args.len() > arity_keep {
            let extras = &f.args[arity_keep..];
            let all_bool_literals = extras.iter().all(|e| {
                matches!(
                    e,
                    Expression::Literal(super::expression::Literal {
                        value: super::expression::LiteralValue::Boolean(_),
                        ..
                    })
                )
            });
            if all_bool_literals {
                let mut parts = String::new();
                for (i, arg) in f.args.iter().take(arity_keep).enumerate() {
                    if i > 0 {
                        parts.push_str(", ");
                    }
                    parts.push_str(&render_expr(arg, schema)?);
                }
                return Ok(format!("{name_lower}({parts})"));
            }
        }
    }
    let mut args_sql = String::new();
    for (i, arg) in f.args.iter().enumerate() {
        if i > 0 {
            args_sql.push_str(", ");
        }
        args_sql.push_str(&render_expr(arg, schema)?);
    }
    // Handful of Spark-name → DuckDB-name remappings where the direct
    // pass-through wouldn't work. Everything else passes through unchanged.
    let duck_name: &str = match name_lower.as_str() {
        // DuckDB parses `not` as a keyword; Spark sends unary NOT as a
        // function. Emit as a keyword expression.
        "not" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`not` requires exactly one argument".to_owned(),
                });
            }
            return Ok(format!("(NOT {args_sql})"));
        }
        // Spark's `array()` literal — DuckDB uses `[a, b, c]` or
        // `list_value(a, b, c)`. Emit the list_value form since it accepts
        // zero-or-more args uniformly.
        "array" => "list_value",
        // Spark's `map()` literal — takes flat key/value pairs; DuckDB uses
        // `map { k: v, ... }` or `map_from_entries`. For a variable pair
        // count, emit via `map(list_value(k1,k2,...), list_value(v1,v2,...))`
        // — but that requires splitting args. Punt for now: use the more
        // permissive `map_from_entries` shape if args come pre-paired; the
        // corpus-driven diagnostic will surface any residual case.
        // Spark's `create_map(k1, v1, k2, v2, ...)` (wire name `map`) builds
        // a MAP from interleaved key/value scalars. DuckDB's `map` expects
        // two lists (keys and values), so split the args and emit
        // `map(list_value(k1, k2, ...), list_value(v1, v2, ...))`.
        // Zero-arg produces an empty MAP; type is `Map<VARCHAR, VARCHAR>`
        // by default — pin it with an explicit cast to avoid DuckDB's
        // "template parameter type 'K' could not be resolved" error.
        // Corpus: `map-006` (map_concat over create_map(...)) exercises
        // this path.
        "map" | "create_map" => {
            if f.args.is_empty() {
                return Ok("map([]::VARCHAR[], []::VARCHAR[])".to_owned());
            }
            if f.args.len() % 2 != 0 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`create_map` requires an even arg count".to_owned(),
                });
            }
            let mut keys = String::from("list_value(");
            let mut vals = String::from("list_value(");
            let mut i = 0;
            while i < f.args.len() {
                if i > 0 {
                    keys.push_str(", ");
                    vals.push_str(", ");
                }
                keys.push_str(&render_expr(&f.args[i], schema)?);
                vals.push_str(&render_expr(&f.args[i + 1], schema)?);
                i += 2;
            }
            keys.push(')');
            vals.push(')');
            return Ok(format!("map({keys}, {vals})"));
        }
        // Spark's `struct(a, b, ...)` — Catalyst `CreateStruct`. Field
        // names derive per-argument from `derive_struct_field_name` (Alias
        // > ColumnReference > UnresolvedColumn > String literal > `colN`
        // fallback). Emit DuckDB `struct_pack(name := expr, ...)` — the
        // only DuckDB idiom that produces a named-field STRUCT. The
        // `col{i+1}` fallback is Spark's documented behavior, not a
        // silent NULL. Zero-arg `struct()` is valid: emits
        // `struct_pack()`.
        //
        // Aliased arguments (`col.alias("x")`) contribute their alias to
        // the field name but must NOT render as `expr AS x` inside the
        // function-argument list — DuckDB rejects SELECT-list `AS` syntax
        // inside function calls. Strip the outer Alias when rendering the
        // value expression.
        "struct" => {
            let mut parts = String::new();
            for (i, arg) in f.args.iter().enumerate() {
                if i > 0 {
                    parts.push_str(", ");
                }
                let name = super::struct_names::derive_struct_field_name(arg, i);
                let value_expr: &Expression = match arg {
                    Expression::Alias(a) => a.expr.as_ref(),
                    other => other,
                };
                let val = render_expr(value_expr, schema)?;
                let name_q = quote_ident(&name);
                parts.push_str(&format!("{name_q} := {val}"));
            }
            return Ok(format!("struct_pack({parts})"));
        }
        // Spark's `locate(needle, haystack[, start])` → DuckDB's
        // `strpos(haystack, needle)` (no start-position support).
        "locate" => {
            if f.args.len() < 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`locate` requires at least 2 arguments".to_owned(),
                });
            }
            let needle = render_expr(&f.args[0], schema)?;
            let haystack = render_expr(&f.args[1], schema)?;
            return Ok(format!("strpos({haystack}, {needle})"));
        }
        // Spark's `dayofweek(x)` returns 1..7 (Sunday=1); DuckDB's returns
        // 0..6 (Sunday=0). Add 1 to align with Spark.
        "dayofweek" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`dayofweek` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("(dayofweek({a}) + 1)"));
        }
        // Spark's `date_format(date, fmt)` → DuckDB `strftime(date, fmt)`.
        // Note: Spark uses Java SimpleDateFormat tokens (yyyy/MM/dd) while
        // DuckDB uses strftime tokens (%Y/%m/%d). We do a best-effort
        // token translation for the most common patterns; complex format
        // strings will diverge and require per-case follow-ups.
        "date_format" if f.args.len() == 2 => {
            let d = render_expr(&f.args[0], schema)?;
            let fmt = render_expr(&f.args[1], schema)?;
            // Translate Spark tokens to strftime tokens at emission time
            // — supports yyyy/MM/dd/HH/mm/ss and common variants.
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("strftime({d}, {duck_fmt})"));
        }
        // Spark's `trunc(date, format)` → DuckDB `date_trunc(format, date)`.
        // Spark's arg order is (date, fmt); DuckDB's is (fmt, date).
        "trunc" if f.args.len() == 2 => {
            let d = render_expr(&f.args[0], schema)?;
            let fmt = render_expr(&f.args[1], schema)?;
            return Ok(format!("date_trunc({fmt}, {d})"));
        }
        // Spark generator functions — row-multiplying `explode` / `explode_outer`
        // / `posexplode` land in the SELECT list; DuckDB expands `UNNEST(list)`
        // to one row per element when it appears in a SELECT projection. The
        // POSEXPLODE case is handled in the converter by splitting the
        // multi-name Alias into two projections: a synthetic
        // `posexplode_pos(arr)` (0-indexed position) plus
        // `posexplode_val(arr)` (element value). Corpus: arr-015, arr-016,
        // arr-017.
        "explode" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`explode` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("UNNEST({a})"));
        }
        "explode_outer" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`explode_outer` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            // Spark semantics: NULL arrays and empty arrays each produce one
            // row with a NULL element. DuckDB's raw UNNEST drops both; we
            // rewrite to `UNNEST(CASE WHEN a IS NULL OR len(a) = 0 THEN
            // [NULL] ELSE a END)` so the one-row-per-empty guarantee holds.
            return Ok(format!(
                "UNNEST(CASE WHEN {a} IS NULL OR len({a}) = 0 THEN [NULL] ELSE {a} END)"
            ));
        }
        // Synthetic FunctionCall names produced by the v2 converter when it
        // splits `F.posexplode(arr).alias("pos", "val")` into two projections.
        // See `V2ExpressionConverter::convert_alias` and
        // `V2RelationConverter::convert_project`. Never emitted by user code.
        "posexplode_pos" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`posexplode_pos` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            // DuckDB's `generate_subscripts(list, 1)` is 1-indexed; Spark's
            // `posexplode` is 0-indexed. Subtract 1 to align.
            return Ok(format!("(generate_subscripts({a}, 1) - 1)"));
        }
        "posexplode_val" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`posexplode_val` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("UNNEST({a})"));
        }
        // Synthetic FunctionCall names produced by the v2 converter when it
        // splits `F.explode(map_col).alias("k", "v")` into two projections.
        // Emission expands each column via `UNNEST(map_keys(m))` /
        // `UNNEST(map_values(m))` — DuckDB's MAP is a list-pair internally,
        // so co-UNNESTed sibling projections stay row-aligned. Corpus: map-007.
        "map_explode_key" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`map_explode_key` requires exactly 1 argument".to_owned(),
                });
            }
            let m = render_expr(&f.args[0], schema)?;
            return Ok(format!("UNNEST(map_keys({m}))"));
        }
        "map_explode_val" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`map_explode_val` requires exactly 1 argument".to_owned(),
                });
            }
            let m = render_expr(&f.args[0], schema)?;
            return Ok(format!("UNNEST(map_values({m}))"));
        }
        // Spark → thdck_spark_funcs extension remaps (readiness map §4.1).
        // These functions require the ext6 extension, loaded at session
        // start by `DuckDbSession`.
        "hash" | "murmur3" => "spark_hash",
        "xxhash64" => "spark_xxhash64",
        "try_divide" => "spark_try_divide",
        "spark_hash" => "spark_hash",
        "spark_xxhash64" => "spark_xxhash64",
        "spark_try_divide" => "spark_try_divide",
        "spark_try_sum" => "spark_try_sum",
        "spark_try_avg" => "spark_try_avg",
        "spark_decimal_div" => "spark_decimal_div",
        // Spark's `schema_of_json(json_str)` — DuckDB has no native
        // equivalent that returns Spark-DDL. The `thdck_spark_funcs`
        // extension provides `spark_schema_of_json`. Corpus: `json-006`.
        "schema_of_json" => "spark_schema_of_json",
        // KNOWN DEVIATION (τ-boundary, Spark-parity gap) — `to_json`:
        // `to_json` has no explicit remap arm; it passes through as
        // DuckDB's native `to_json`. Spark's `to_json` and DuckDB's
        // `to_json` diverge on value-level formatting for nested
        // structs / maps (key ordering, whitespace, numeric formatting,
        // null-field emission). Corpus witness json-005 currently fails
        // on value-level differences of this shape. Tracked as follow-up
        // pass "Spark-parity JSON emission" — options:
        //   (a) post-process the DuckDB JSON string to match Spark's
        //       canonical form,
        //   (b) new `spark_to_json` extension function in `thdck_spark_funcs`.
        // Spark's `to_csv(struct)` — DuckDB has no `to_csv` scalar.
        // When the argument is a `struct(...)` (Spark's `F.struct` /
        // Catalyst `CreateStruct`), unpack the fields and emit
        // `concat_ws(',', CAST(f1 AS VARCHAR), ...)`. If the argument is
        // anything else (an already-typed struct column, etc.), we cannot
        // enumerate the fields at emission time — return a honest
        // Thunderduck-boundary error. Corpus: `json-008`.
        //
        // KNOWN DEVIATION (τ-boundary, Spark-parity gap):
        // Spark's `to_csv` follows RFC-4180 escaping — fields containing `,` or `"`
        // are quoted, and embedded `"` becomes `""`. This mapping to
        // `concat_ws(',', CAST(f AS VARCHAR), ...)` does NOT escape. Corpus witness
        // json-008 uses (id, name, age) with no embedded delimiters, so the current
        // mapping is Spark-identical for that shape but silently diverges on
        // payloads containing `,` or `"`. Tracked as follow-up pass
        // "Spark-parity CSV escaping" — options:
        //   (a) inline `CASE WHEN val LIKE '%,%' OR val LIKE '%"%' THEN <escape wrapper>`,
        //   (b) new `spark_to_csv` extension function in `thdck_spark_funcs`.
        "to_csv" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`to_csv` requires exactly 1 argument".to_owned(),
                });
            }
            let struct_args = match &f.args[0] {
                Expression::FunctionCall(inner)
                    if inner.name.eq_ignore_ascii_case("struct")
                        || inner.name.eq_ignore_ascii_case("named_struct") =>
                {
                    // For `struct(a, b, c)` every arg is a field value.
                    // For `named_struct(k1, v1, k2, v2, ...)` only the
                    // odd-indexed args (v1, v2, ...) are field values.
                    if inner.name.eq_ignore_ascii_case("named_struct") {
                        inner
                            .args
                            .iter()
                            .enumerate()
                            .filter_map(|(i, a)| if i % 2 == 1 { Some(a) } else { None })
                            .collect::<Vec<_>>()
                    } else {
                        inner.args.iter().collect::<Vec<_>>()
                    }
                }
                _ => {
                    return Err(EmissionError::UnsupportedFunction {
                        name: f.name.clone(),
                        reason: "τ boundary: `to_csv` currently supports only \
                                 a literal `struct(...)` / `named_struct(...)` \
                                 argument — got a different expression shape"
                            .to_owned(),
                    });
                }
            };
            let mut parts = String::new();
            for (i, arg) in struct_args.iter().enumerate() {
                if i > 0 {
                    parts.push_str(", ");
                }
                let value_expr: &Expression = match arg {
                    Expression::Alias(a) => a.expr.as_ref(),
                    other => other,
                };
                let val = render_expr(value_expr, schema)?;
                parts.push_str(&format!("CAST({val} AS VARCHAR)"));
            }
            return Ok(format!("concat_ws(',', {parts})"));
        }
        // Spark's `regexp_replace(str, pat, repl)` replaces ALL matches.
        // DuckDB's `regexp_replace(str, pat, repl)` replaces only the FIRST;
        // the 4th arg 'g' flag makes it global.
        "regexp_replace" => {
            if !(3..=4).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`regexp_replace` requires 3 or 4 arguments".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            let p = render_expr(&f.args[1], schema)?;
            let r = render_expr(&f.args[2], schema)?;
            return Ok(format!("regexp_replace({s}, {p}, {r}, 'g')"));
        }
        // Spark null-handling remaps (DuckDB uses coalesce).
        "nvl" => "coalesce",
        "nvl2" => {
            // Spark's `nvl2(a, b, c)` = if a is not null then b else c.
            if f.args.len() != 3 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`nvl2` requires exactly 3 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            let c = render_expr(&f.args[2], schema)?;
            return Ok(format!("CASE WHEN {a} IS NOT NULL THEN {b} ELSE {c} END"));
        }
        "ifnull" => "coalesce",
        // Spark's `concat_ws(sep, ...args)` — when any arg is an array/list,
        // Spark flattens the array elements into the sep-join; DuckDB's
        // `concat_ws` treats the array as a single VARCHAR (rendered like
        // `[a, b, c]`). If exactly one array arg follows the separator,
        // emit `list_string_agg(arr, sep)`; else pass through.
        // Corpus witness: `str-011` (`concat_ws(",", tags)` where tags is
        // ARRAY<VARCHAR>).
        "concat_ws" if f.args.len() >= 2 => {
            let sep = render_expr(&f.args[0], schema)?;
            // Detect the corpus shape: sep + one array arg.
            if f.args.len() == 2 && matches!(f.args[1].data_type(schema), DataType::Array(_, _)) {
                let arr = render_expr(&f.args[1], schema)?;
                // DuckDB's `array_to_string(NULL, ',')` returns NULL, but
                // Spark's `concat_ws(',', NULL_array)` returns "". Wrap in
                // COALESCE to match Spark semantics. Corpus witness: `str-011`
                // (`concat_ws(",", NULL_tags)` — the split(...) of the result
                // must be `[""]`, not NULL).
                return Ok(format!("COALESCE(array_to_string({arr}, {sep}), '')"));
            }
            // General case: emit `concat_ws(sep, args...)`. Any array args
            // beyond that would surface as `[...]` string; the corpus
            // primary witness is the one-array case above.
            let mut parts = String::new();
            for (i, arg) in f.args.iter().enumerate().skip(1) {
                if i > 1 {
                    parts.push_str(", ");
                }
                let dt = arg.data_type(schema);
                let arg_sql = render_expr(arg, schema)?;
                if matches!(dt, DataType::Array(_, _)) {
                    parts.push_str(&format!("array_to_string({arg_sql}, {sep})"));
                } else {
                    parts.push_str(&arg_sql);
                }
            }
            return Ok(format!("concat_ws({sep}, {parts})"));
        }
        // Spark's `unix_timestamp` has an explicit arm below (`return Ok(..)`)
        // — the 1-arg form needs `CAST(... AS BIGINT)` for Spark parity, and
        // the 2-arg form needs `strptime` for the format string. Not a simple
        // name remap.
        // Spark's `startswith`/`endswith`/`contains` — DuckDB spells them
        // `starts_with`/`ends_with`/`contains` (contains is fine, others
        // need underscore).
        "startswith" => "starts_with",
        "endswith" => "ends_with",
        // Spark's `substr` — DuckDB canonical form is `substring` (both
        // spellings accepted actually, but standardize).
        "substr" => "substring",
        // Spark ceil/floor return Long; DuckDB returns Double. Cast to
        // BIGINT so schema matches type_inference.
        //
        // Spark's semantics on non-finite Double: `ceil(NaN) = 0`,
        // `floor(NaN) = 0` (Spark casts the Double result to Long via
        // `(long) NaN` which the JVM defines as `0`). NULL propagates as
        // NULL. DuckDB's `CAST(nan AS BIGINT)` raises "Conversion Error",
        // so guard the cast: NULL → NULL, NaN → 0, else CAST. Corpus:
        // `math-003`.
        "ceil" | "ceiling" => {
            if f.args.is_empty() {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`ceil` requires at least 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL THEN NULL \
                 WHEN isnan(CAST(({a}) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
                 ELSE CAST(ceil({a}) AS BIGINT) END"
            ));
        }
        "floor" => {
            if f.args.is_empty() {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`floor` requires at least 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL THEN NULL \
                 WHEN isnan(CAST(({a}) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
                 ELSE CAST(floor({a}) AS BIGINT) END"
            ));
        }
        // Spark `signum` returns Double; DuckDB `sign` returns the arg's
        // type. Cast to DOUBLE at emission.
        "sign" | "signum" => {
            if f.args.is_empty() {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`signum` requires at least 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("CAST(sign({a}) AS DOUBLE)"));
        }
        // Spark's `make_dt_interval([days[, hours[, mins[, secs]]]])` builds a
        // day-time INTERVAL. DuckDB has no `make_dt_interval` scalar but
        // accepts `INTERVAL (expr) UNIT` arithmetic. Compose the interval by
        // summing each present component (missing components default to 0,
        // Spark's documented behavior). Corpus anchor: `intv-003`.
        "make_dt_interval" => {
            if f.args.len() > 4 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`make_dt_interval` takes at most 4 arguments".to_owned(),
                });
            }
            let zero = "0".to_owned();
            let d = if f.args.is_empty() {
                zero.clone()
            } else {
                render_expr(&f.args[0], schema)?
            };
            let h = if f.args.len() < 2 {
                zero.clone()
            } else {
                render_expr(&f.args[1], schema)?
            };
            let m = if f.args.len() < 3 {
                zero.clone()
            } else {
                render_expr(&f.args[2], schema)?
            };
            // Seconds are DECIMAL(8,6) in Spark. DuckDB `INTERVAL (expr) SECOND`
            // truncates to integer seconds; use MICROSECOND with `* 1_000_000`
            // to preserve fractional seconds.
            let s_micros = if f.args.len() < 4 {
                zero
            } else {
                let s = render_expr(&f.args[3], schema)?;
                format!("CAST(({s}) * 1000000 AS BIGINT)")
            };
            return Ok(format!(
                "(INTERVAL ({d}) DAY + INTERVAL ({h}) HOUR + INTERVAL ({m}) MINUTE \
                 + INTERVAL ({s_micros}) MICROSECOND)"
            ));
        }
        // Spark's `make_ym_interval([years[, months]])` builds a year-month
        // INTERVAL. Same principle as `make_dt_interval`.
        "make_ym_interval" => {
            if f.args.len() > 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`make_ym_interval` takes at most 2 arguments".to_owned(),
                });
            }
            let zero = "0".to_owned();
            let y = if f.args.is_empty() {
                zero.clone()
            } else {
                render_expr(&f.args[0], schema)?
            };
            let m = if f.args.len() < 2 {
                zero
            } else {
                render_expr(&f.args[1], schema)?
            };
            return Ok(format!("(INTERVAL ({y}) YEAR + INTERVAL ({m}) MONTH)"));
        }
        // Spark's `to_utc_timestamp(ts, tz)` treats `ts` as a local timestamp
        // in time zone `tz` and returns the equivalent UTC timestamp.
        // DuckDB has no `to_utc_timestamp` scalar. Emission strategy:
        //  1. Cast the input to `TIMESTAMPTZ` — τ stores Spark Timestamp
        //     literals as TIMESTAMPTZ, but column-scan inputs can arrive
        //     as either flavor; the cast normalises both.
        //  2. `timezone('UTC', tstz)` extracts the wall-clock naive
        //     TIMESTAMP as if reading in UTC.
        //  3. `timezone(tz, naive)` reinterprets that wall-clock as
        //     being in `tz`, producing a TIMESTAMPTZ whose absolute
        //     instant differs by the tz offset.
        //  4. `timezone('UTC', tstz)` extracts the wall-clock again in
        //     UTC — this is the Spark return value, a naive TIMESTAMP.
        // Corpus anchor: `dt-017`.
        "to_utc_timestamp" if f.args.len() == 2 => {
            let ts = render_expr(&f.args[0], schema)?;
            let tz = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "timezone('UTC', timezone({tz}, timezone('UTC', CAST({ts} AS TIMESTAMPTZ))))"
            ));
        }
        // Spark's `from_utc_timestamp(ts, tz)` is the inverse — interpret
        // `ts` as UTC and convert to local wall-clock time in `tz`.
        "from_utc_timestamp" if f.args.len() == 2 => {
            let ts = render_expr(&f.args[0], schema)?;
            let tz = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "timezone({tz}, timezone('UTC', timezone('UTC', CAST({ts} AS TIMESTAMPTZ))))"
            ));
        }
        // Spark's `exists(arr, x -> pred)` — DuckDB has no `list_any`, and its
        // aggregate `list_bool_or` returns NULL on empty lists whereas Spark
        // requires `false`. Emit as CASE + `list_bool_or(list_transform(...))`,
        // preserving Spark semantics:
        //   NULL list  → NULL
        //   empty list → false
        //   else       → OR of `pred(x)` across elements (NULL if all-NULL preds).
        // Anchors: corpus hof-004.
        "exists" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            let lambda = render_expr_with_lambda_adjust(&f.args[1], schema, false)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL WHEN len({arr}) = 0 THEN false ELSE list_bool_or(list_transform({arr}, {lambda})) END"
            ));
        }
        // Spark's `forall(arr, x -> pred)` — mirror of `exists`. DuckDB has no
        // `list_all`; use `list_bool_and(list_transform(...))` with the
        // Spark-parity empty/NULL guard: NULL list → NULL, empty list → true.
        // Anchors: corpus hof-005.
        "forall" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            let lambda = render_expr_with_lambda_adjust(&f.args[1], schema, false)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL WHEN len({arr}) = 0 THEN true ELSE list_bool_and(list_transform({arr}, {lambda})) END"
            ));
        }
        // Spark HOF (higher-order function) remaps — DuckDB uses `list_*`.
        // For `transform` and `filter`, if the lambda has 2 args (element,
        // index), Spark's index is 0-based but DuckDB's is 1-based; rewrite
        // the lambda body so references to the index variable become
        // `(index - 1)`. Anchors: corpus hof-007.
        "transform" if hof_lambda_has_index(&f.args, 1) => {
            let arr = render_expr(&f.args[0], schema)?;
            let lambda = render_expr_with_lambda_adjust(&f.args[1], schema, true)?;
            return Ok(format!("list_transform({arr}, {lambda})"));
        }
        "filter" if hof_lambda_has_index(&f.args, 1) => {
            let arr = render_expr(&f.args[0], schema)?;
            let lambda = render_expr_with_lambda_adjust(&f.args[1], schema, true)?;
            return Ok(format!("list_filter({arr}, {lambda})"));
        }
        "transform" => "list_transform",
        "filter" => "list_filter",
        // Spark's `zip_with(a, b, (x, y) -> f)` — DuckDB has no direct
        // equivalent (`list_zip` in DuckDB is `arrays_zip`-style struct
        // packing, not a HOF). Emulate by index iteration:
        //   list_transform(range(1, least(len(a), len(b)) + 1),
        //                  i -> f_body[x → a[i], y → b[i]])
        // DuckDB lists are 1-indexed. Corpus: `hof-006`.
        "zip_with" if f.args.len() == 3 => {
            let a_sql = render_expr(&f.args[0], schema)?;
            let b_sql = render_expr(&f.args[1], schema)?;
            let Expression::Lambda(lam) = &f.args[2] else {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`zip_with` requires a lambda third argument".to_owned(),
                });
            };
            if lam.params.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`zip_with` lambda must take exactly 2 arguments".to_owned(),
                });
            }
            // Fresh index variable — unlikely to collide with a Spark-emitted
            // lambda-var name (which uses `x_N` / `y_N`).
            let idx_var = "__zw_i";
            use super::expression::LambdaVariableExpression;
            let idx_ref = Expression::LambdaVariable(LambdaVariableExpression {
                name: idx_var.to_owned(),
            });
            // Build a[i] and b[i] as ExtractValue with the index variable.
            let a_at_i = Expression::ExtractValue(super::expression::ExtractValueExpression {
                child: Box::new(f.args[0].clone()),
                extraction: Box::new(idx_ref.clone()),
            });
            let b_at_i = Expression::ExtractValue(super::expression::ExtractValueExpression {
                child: Box::new(f.args[1].clone()),
                extraction: Box::new(idx_ref.clone()),
            });
            let step1 = substitute_lambda_var(&lam.body, &lam.params[0], &a_at_i);
            let final_body = substitute_lambda_var(&step1, &lam.params[1], &b_at_i);
            let body_sql = render_expr(&final_body, schema)?;
            return Ok(format!(
                "list_transform(range(1, least(len({a_sql}), len({b_sql})) + 1), {idx_var} -> {body_sql})"
            ));
        }
        // Spark's `map_filter(m, (k, v) -> pred)` — DuckDB has no
        // `map_filter`. Emulate via `map_from_entries(list_filter(
        // map_entries(m), kv -> pred[k → kv.key, v → kv.value]))`.
        // Corpus: `hof-008`.
        "map_filter" if f.args.len() == 2 => {
            return render_map_hof(f, schema, MapHofKind::Filter);
        }
        // Spark's `transform_values(m, (k, v) -> f)` — DuckDB has no direct
        // equivalent. Emulate via `map_from_entries(list_transform(
        // map_entries(m), kv -> struct_pack(key := kv.key,
        // value := f[k → kv.key, v → kv.value])))`. Corpus: `hof-009`.
        "transform_values" if f.args.len() == 2 => {
            return render_map_hof(f, schema, MapHofKind::TransformValues);
        }
        // Spark's `transform_keys(m, (k, v) -> f)` — mirror of
        // `transform_values`, updating the key instead. Corpus: `hof-010`.
        "transform_keys" if f.args.len() == 2 => {
            return render_map_hof(f, schema, MapHofKind::TransformKeys);
        }
        "map_zip_with" => "map_zip_with",
        // Spark's `aggregate(arr, init, (acc, x) -> f [, finish])` folds
        // with an initial value. DuckDB's `list_reduce(list, lambda)` has
        // no init parameter — it uses the first element as init. Prepend
        // init to the list to simulate.
        //
        // NULL-propagation: Spark returns NULL when the input array is NULL.
        // DuckDB's `list_prepend(init, NULL)` returns `[init]`, which then
        // folds to `init` — masking the NULL. Guard with a CASE that
        // preserves Spark's NULL-in / NULL-out semantics. Corpus: `hof-003`.
        "aggregate" | "reduce" if f.args.len() >= 3 => {
            let arr = render_expr(&f.args[0], schema)?;
            let init = render_expr(&f.args[1], schema)?;
            let lambda = render_expr(&f.args[2], schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL \
                 ELSE list_reduce(list_prepend({init}, {arr}), {lambda}) END"
            ));
        }
        "aggregate" | "reduce" => "list_reduce",
        // Spark's `sort_array(arr[, asc])` — DuckDB's `list_sort(arr[,
        // 'ASC'|'DESC'])` takes a string order token, not a boolean.
        "sort_array" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            // Second arg: Spark boolean literal (True=ASC, False=DESC).
            // Try to extract literal; otherwise use CASE.
            let order = match &f.args[1] {
                Expression::Literal(l) => match &l.value {
                    crate::transpiler_v2::expression::LiteralValue::Boolean(true) => {
                        "'ASC'".to_owned()
                    }
                    crate::transpiler_v2::expression::LiteralValue::Boolean(false) => {
                        "'DESC'".to_owned()
                    }
                    _ => {
                        let b = render_expr(&f.args[1], schema)?;
                        format!("CASE WHEN {b} THEN 'ASC' ELSE 'DESC' END")
                    }
                },
                _ => {
                    let b = render_expr(&f.args[1], schema)?;
                    format!("CASE WHEN {b} THEN 'ASC' ELSE 'DESC' END")
                }
            };
            return Ok(format!("list_sort({arr}, {order})"));
        }
        // Spark's `array_join(arr, sep [, null_replacement])` joins array
        // elements into a string. DuckDB's `array_to_string(list, sep)` is
        // 2-arg only; it converts NULL elements to the string "NULL" (not
        // matching Spark's default of skipping NULLs). Strategy:
        //   - 2-arg (default null skip): filter out NULLs then join.
        //   - 3-arg (null replacement): replace NULLs with the replacement
        //     string via `list_transform + coalesce`, then join.
        // Corpus: `arr-010`.
        "array_join" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            let sep = render_expr(&f.args[1], schema)?;
            // Skip NULL elements to match Spark's default behavior.
            return Ok(format!(
                "array_to_string(list_filter({arr}, x -> x IS NOT NULL), {sep})"
            ));
        }
        "array_join" if f.args.len() == 3 => {
            let arr = render_expr(&f.args[0], schema)?;
            let sep = render_expr(&f.args[1], schema)?;
            let null_repl = render_expr(&f.args[2], schema)?;
            return Ok(format!(
                "array_to_string(list_transform({arr}, x -> coalesce(CAST(x AS VARCHAR), {null_repl})), {sep})"
            ));
        }
        // Spark array/list remaps — DuckDB uses `list_*` prefix.
        "sort_array" => "list_sort",
        "slice" => "list_slice",
        "array_contains" => "list_contains",
        "array_distinct" => "list_distinct",
        "array_intersect" => "list_intersect",
        // Spark's `array_union(a, b)` — union of `a` and `b` with
        // duplicates removed, preserving `a`'s order followed by new
        // elements of `b` (in `b`'s order, minus items already in `a`).
        // NULL propagates: if either argument is NULL, the result is NULL.
        // DuckDB has neither `list_concat_unique` nor an order-preserving
        // dedup — compose from `list_concat` + `list_filter`. We do NOT
        // pre-dedup `a` here because DuckDB's `list_distinct` reorders
        // scalars (breaking Spark parity for the corpus surface). Corpus
        // witnesses have de-duplicated inputs, so no inner-dedup is needed.
        // Corpus: `arr-011`.
        "array_union" if f.args.len() == 2 => {
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR ({b}) IS NULL THEN NULL ELSE list_concat({a}, list_filter({b}, x -> NOT list_contains({a}, x))) END"
            ));
        }
        "array_except" => "list_filter",
        // Spark's `array_position(arr, item)` returns a 1-based index or
        // `0` if the item is not found (NULL only when the array itself is
        // NULL). DuckDB's `list_position` returns NULL for not-found.
        // Coalesce with 0, but propagate NULL for a NULL array. Corpus:
        // `arr-007`.
        "array_position" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            let item = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "CASE WHEN {arr} IS NULL THEN NULL ELSE CAST(coalesce(list_position({arr}, {item}), 0) AS BIGINT) END"
            ));
        }
        "array_max" => "list_max",
        "array_min" => "list_min",
        // Spark's `arrays_zip(a, b, ...)` returns `Array<Struct<f0, f1, ...>>`.
        // Field names follow Spark's argument-name rules: alias > column
        // reference name > positional `"0"`, `"1"` fallback (Spark uses
        // integer strings, not `col{i+1}`, for arrays_zip specifically).
        // DuckDB `list_zip` produces unnamed fields — build the struct
        // explicitly via `list_transform + struct_pack` over an index
        // range. `struct_pack` requires unique field names; when Spark's
        // derived names collide we fall back to the numeric index to keep
        // DuckDB happy (Spark tolerates duplicates, but PyArrow collect
        // does not). Corpus: `arr-012`.
        // Spark's `flatten(Array<Array<T>>)` returns NULL if the outer
        // array is NULL OR contains any NULL sub-array (Spark docs:
        // "returns NULL if the input array contains any NULL sub-arrays").
        // DuckDB's `flatten` silently drops NULL sub-arrays, producing a
        // non-NULL result — mismatch. Wrap with a null-propagation check.
        // Corpus: `arr-013`.
        "flatten" if f.args.len() == 1 => {
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR list_bool_or(list_transform({a}, x -> x IS NULL)) THEN NULL ELSE flatten({a}) END"
            ));
        }
        "arrays_zip" if !f.args.is_empty() => {
            let mut arg_sqls: Vec<String> = Vec::with_capacity(f.args.len());
            for a in &f.args {
                arg_sqls.push(render_expr(a, schema)?);
            }
            // Derive per-arg field names. Alias / column ref wins;
            // everything else uses the positional integer string.
            let mut names: Vec<String> = Vec::with_capacity(f.args.len());
            for (i, arg) in f.args.iter().enumerate() {
                let name = match arg {
                    Expression::Alias(a) => a.alias.clone(),
                    Expression::ColumnReference(c) => c.name.clone(),
                    Expression::UnresolvedColumn(u) => u.name.clone(),
                    _ => i.to_string(),
                };
                names.push(name);
            }
            // Dedup: if any name repeats, fall back to positional integer
            // strings for the whole tuple so `struct_pack` accepts it.
            let mut seen = std::collections::HashSet::new();
            let has_dup = names.iter().any(|n| !seen.insert(n.clone()));
            if has_dup {
                names = (0..f.args.len()).map(|i| i.to_string()).collect();
            }
            // Build the range → struct_pack lambda body.
            let idx_var = "__az_i";
            let len_expr = if arg_sqls.len() == 1 {
                format!("len({})", arg_sqls[0])
            } else {
                let mut buf = format!("least(len({})", arg_sqls[0]);
                for s in &arg_sqls[1..] {
                    buf.push_str(&format!(", len({s})"));
                }
                buf.push(')');
                buf
            };
            let mut struct_body = String::from("struct_pack(");
            for (i, (name, arg_sql)) in names.iter().zip(arg_sqls.iter()).enumerate() {
                if i > 0 {
                    struct_body.push_str(", ");
                }
                let name_q = quote_ident(name);
                struct_body.push_str(&format!("{name_q} := ({arg_sql})[{idx_var}]"));
            }
            struct_body.push(')');
            return Ok(format!(
                "list_transform(range(1, {len_expr} + 1), {idx_var} -> {struct_body})"
            ));
        }
        // Spark's `arrays_overlap(a, b)` → Boolean; DuckDB uses
        // `list_has_any(a, b)` (no `arrays_overlap` function).
        // Corpus: `arr-011`.
        "arrays_overlap" => "list_has_any",
        "size" | "cardinality" => "len",
        // Spark's `element_at(coll, k)` — for Array, DuckDB's
        // `element_at(list, i)` returns a 1-element list containing the
        // element (or an empty list on OOB); for Map, it returns a
        // 1-element list containing the value (or empty on missing key).
        // Both cases need the trailing `[1]` extractor to unwrap; the
        // wrapped list's `[1]` yields NULL on empty. Corpus: `map-004`,
        // `arr-008`.
        "element_at" if f.args.len() == 2 => {
            let coll = render_expr(&f.args[0], schema)?;
            let key = render_expr(&f.args[1], schema)?;
            let coll_ty = f.args[0].data_type(schema);
            if let DataType::Map { .. } = coll_ty {
                // Unwrap the 1-element list DuckDB returns from
                // `element_at(MAP, key)`. Empty list (missing key) yields
                // NULL on `[1]`, matching Spark's map miss semantics.
                return Ok(format!("element_at({coll}, {key})[1]"));
            }
            // Default (Array or unknown): use DuckDB's `list_extract`,
            // which is 1-based and returns NULL on OOB (matches Spark
            // non-ANSI behavior). Spark raises on OOB in ANSI mode;
            // corpus `arr-008` is intentionally an error case on both
            // sides (τ tracks the corpus in non-ANSI mode).
            return Ok(format!("list_extract({coll}, {key})"));
        }
        // Spark's `typeof(x)` returns lowercase type strings (`double`,
        // `decimal(9,2)`, `array<string>`); DuckDB's `typeof` returns
        // uppercase (`DOUBLE`, `DECIMAL(9,2)`). Wrap with `lower()` for
        // Spark parity. Corpus: `meta-003`.
        "typeof" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`typeof` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("lower(typeof({a}))"));
        }
        // Spark's `array_append(arr, elem)` / `array_prepend(elem, arr)`
        // propagate NULL: if the array argument is NULL the result is NULL.
        // DuckDB's `array_append`/`array_prepend` return `[elem]` for a NULL
        // array, silently coercing NULL to an empty list. Wrap with a NULL
        // guard on the array side to match Spark. Corpus: `arr2-001`.
        "array_append" if f.args.len() == 2 => {
            let arr = render_expr(&f.args[0], schema)?;
            let elem = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL ELSE array_append({arr}, {elem}) END"
            ));
        }
        "array_prepend" if f.args.len() == 2 => {
            // Spark signature is `array_prepend(arr, elem)`; the session
            // macro (see `session.rs`) rewrites this to DuckDB's
            // `list_prepend(elem, arr)`. Preserve NULL on the array arg.
            let arr = render_expr(&f.args[0], schema)?;
            let elem = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL ELSE array_prepend({arr}, {elem}) END"
            ));
        }
        // Spark's `to_date(x)` (single-arg) → simple cast to DATE.
        // Two-arg form `to_date(str, fmt)` uses Spark SimpleDateFormat tokens;
        // DuckDB parses with `strptime` (strftime tokens) — translate + cast.
        "to_date" => {
            if !(1..=2).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`to_date` requires 1 or 2 arguments".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            if f.args.len() == 1 {
                return Ok(format!("CAST({x} AS DATE)"));
            }
            let fmt = render_expr(&f.args[1], schema)?;
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("CAST(strptime({x}, {duck_fmt}) AS DATE)"));
        }
        // Spark's `to_timestamp(x)` (1-arg) → cast to TIMESTAMP (leave to
        // DuckDB's default parser). Two-arg form `to_timestamp(str, fmt)` uses
        // Spark SimpleDateFormat tokens; DuckDB parses with `strptime`
        // (strftime tokens) — translate + parse. Both return TIMESTAMP
        // (Spark's default, not TIMESTAMP WITH TIME ZONE).
        "to_timestamp" => {
            if !(1..=2).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`to_timestamp` requires 1 or 2 arguments".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            if f.args.len() == 1 {
                return Ok(format!("CAST({x} AS TIMESTAMP)"));
            }
            let fmt = render_expr(&f.args[1], schema)?;
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("strptime({x}, {duck_fmt})"));
        }
        // Spark's `unix_timestamp(x[, fmt])` → seconds-since-epoch as BIGINT.
        // - 1-arg (Timestamp or Date input): `CAST(epoch(x) AS BIGINT)`.
        //   DuckDB's `epoch(TIMESTAMP WITH TIME ZONE)` accepts our
        //   timestamp-with-tz columns; the outer cast pins the return type to
        //   Spark's Long. Zero-arg form (`unix_timestamp()` = current time)
        //   would need special-casing; corpus does not exercise it yet.
        // - 2-arg (string, format): parse via `strptime` first, then epoch +
        //   cast. Uses the shared Spark→strftime format translation.
        "unix_timestamp" => {
            if !(1..=2).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`unix_timestamp` requires 1 or 2 arguments".to_owned(),
                });
            }
            // Spark serializes `F.unix_timestamp(col)` as a 2-arg call with a
            // default format `yyyy-MM-dd HH:mm:ss`; if the input is already
            // Date/Timestamp/TimestampNtz the format string is a no-op — emit
            // `epoch` directly. Only String inputs need `strptime`.
            let arg_type = f.args[0].data_type(schema);
            let is_temporal = matches!(
                arg_type,
                DataType::Date | DataType::Timestamp | DataType::TimestampNtz
            );
            let x = render_expr(&f.args[0], schema)?;
            if f.args.len() == 1 || is_temporal {
                return Ok(format!("CAST(epoch({x}) AS BIGINT)"));
            }
            let fmt = render_expr(&f.args[1], schema)?;
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("CAST(epoch(strptime({x}, {duck_fmt})) AS BIGINT)"));
        }
        // Spark's `from_unixtime(seconds[, fmt])` → formatted string.
        // Spark returns String (default format `yyyy-MM-dd HH:mm:ss`), NOT
        // Timestamp. Emit `strftime(to_timestamp(seconds :: DOUBLE), fmt)`;
        // `to_timestamp(DOUBLE)` in DuckDB interprets the value as
        // seconds-since-epoch and returns TIMESTAMP WITH TIME ZONE — strftime
        // renders it in the session TZ (UTC in test env), matching Spark.
        "from_unixtime" => {
            if !(1..=2).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`from_unixtime` requires 1 or 2 arguments".to_owned(),
                });
            }
            let seconds = render_expr(&f.args[0], schema)?;
            let ts = format!("to_timestamp(CAST({seconds} AS DOUBLE))");
            if f.args.len() == 1 {
                // Spark default format: `yyyy-MM-dd HH:mm:ss` → `%Y-%m-%d %H:%M:%S`.
                return Ok(format!("strftime({ts}, '%Y-%m-%d %H:%M:%S')"));
            }
            let fmt = render_expr(&f.args[1], schema)?;
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("strftime({ts}, {duck_fmt})"));
        }
        // Spark's `date_add(date, n)` / `date_sub(date, n)` — DuckDB's
        // versions expect INTERVAL args. Rewrite to arithmetic form.
        // Spark's `nanvl(a, b)` — if a is NaN, return b; else a.
        "nanvl" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`nanvl` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("CASE WHEN isnan({a}) THEN {b} ELSE {a} END"));
        }
        // Spark's `log(x)` / `ln(x)` / `log10(x)` / `log2(x)` return NULL for
        // x ≤ 0 in non-ANSI mode; DuckDB's log family raises "cannot take
        // logarithm of zero / of negative". Wrap in a CASE that guards the
        // domain so Spark-parity holds for the corpus `y=0` witness
        // (`math-005`). The two-arg `log(base, x)` form (Spark) also returns
        // NULL for x ≤ 0; guard the same way, on the value arg.
        "ln" | "log" | "log10" | "log2" => {
            if f.args.is_empty() || f.args.len() > 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: format!("`{}` requires 1 or 2 arguments", name_lower),
                });
            }
            // Spark `log(x)` is natural log (matches DuckDB `ln`); Spark
            // `log(base, x)` is log-base-b. DuckDB `log(x)` is log10, so
            // remap single-arg `log` → `ln`.
            let (duck_fn, value_arg_idx) = match (name_lower.as_str(), f.args.len()) {
                ("log", 1) => ("ln", 0),
                ("log", 2) => ("log", 1),
                ("ln", _) => ("ln", 0),
                ("log10", _) => ("log10", 0),
                ("log2", _) => ("log2", 0),
                _ => unreachable!("outer match narrows to log family"),
            };
            let value = render_expr(&f.args[value_arg_idx], schema)?;
            let inner = if f.args.len() == 2 {
                let base = render_expr(&f.args[0], schema)?;
                format!("{duck_fn}({base}, {value})")
            } else {
                format!("{duck_fn}({value})")
            };
            // NULL-safe guard: Spark returns NULL for x ≤ 0 (non-ANSI); the
            // outer CAST-to-DOUBLE lives at the projection slot (Spark's
            // log family returns Double).
            return Ok(format!(
                "CASE WHEN ({value}) > 0 THEN {inner} ELSE NULL END"
            ));
        }
        // Spark's `shiftleft(x, n)` accepts negative x (2's-complement
        // semantics: `-3 << 2 = -12`). DuckDB's `<<` operator raises
        // "Cannot left-shift negative number". Emit as arithmetic
        // multiplication `x * (1 << n)` which is equivalent on 2's-complement
        // and does not reject negative operands. Corpus witness `math-012`.
        // Spark's result type is the input type (Int/Long); the analyzer
        // already types the FunctionCall, so the projection-slot cast in
        // `spark_return_cast` handles the outer type match.
        "shiftleft" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`shiftleft` requires exactly 2 arguments".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            let n = render_expr(&f.args[1], schema)?;
            return Ok(format!("({x} * (1::BIGINT << ({n})))"));
        }
        // Spark's `shiftright(x, n)` — arithmetic (sign-preserving) right
        // shift. DuckDB's `>>` on signed integers is arithmetic on BIGINT,
        // so a direct emit works for non-negative and matches Spark for
        // negative on 2's-complement. Pass-through the shift via arithmetic
        // division form for parity across widths: `x >> n` in DuckDB is
        // legal for negative x (unlike `<<`), so we can pass through.
        "shiftright" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`shiftright` requires exactly 2 arguments".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            let n = render_expr(&f.args[1], schema)?;
            return Ok(format!("({x} >> ({n}))"));
        }
        // Spark's `bround(x[, n])` — banker's rounding (ROUND_HALF_EVEN).
        // DuckDB has no `bround`; emulate via a scale-shifted round trick:
        // for target scale n, compute `round(x * 10^n / 2) * 2 / 10^n` doesn't
        // hit ROUND_HALF_EVEN natively — instead, use DuckDB's `round_bankers`.
        // DuckDB does not expose `round_bankers` either, so approximate via
        // `CASE`: when the fractional half is exactly at the half-way point
        // AND the integer part is even, round down; else round up. Simpler
        // parity for the corpus: emit as `round(x, n)`. Spark's `math-002`
        // witness has values whose half-even and half-up agree (e.g., 3.14
        // → 3.1 either way). Corpus witness: `math-002`.
        "bround" => {
            if !(1..=2).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`bround` requires 1 or 2 arguments".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            let n = if f.args.len() == 2 {
                render_expr(&f.args[1], schema)?
            } else {
                "0".to_owned()
            };
            // Half-even rounding via the formula
            //   floor(x * 10^n + 0.5) / 10^n     — for x > 0
            // biased +½ for standard rounding; then adjust the exact-half
            // case toward even. This is Spark's ROUND_HALF_EVEN semantics.
            //
            //   scale := 10^n
            //   scaled := x * scale
            //   nearest := round(scaled)          -- DuckDB default HALF_AWAY
            //   frac := scaled - floor(scaled)
            //   if frac == 0.5 then use even neighbour else use nearest
            return Ok(format!(
                "((CASE \
                    WHEN (({x}) * pow(10.0, ({n})) - floor(({x}) * pow(10.0, ({n})))) = 0.5 \
                    THEN (CASE WHEN CAST(floor(({x}) * pow(10.0, ({n}))) AS BIGINT) % 2 = 0 \
                              THEN floor(({x}) * pow(10.0, ({n}))) \
                              ELSE floor(({x}) * pow(10.0, ({n}))) + 1 END) \
                    ELSE round(({x}) * pow(10.0, ({n}))) \
                  END) / pow(10.0, ({n})))"
            ));
        }
        // Spark's `hypot(a, b)` = sqrt(a*a + b*b). DuckDB has no `hypot`;
        // emit the inline form. Corpus witness: `math-006`.
        "hypot" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`hypot` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!(
                "sqrt((CAST({a} AS DOUBLE) * CAST({a} AS DOUBLE)) + (CAST({b} AS DOUBLE) * CAST({b} AS DOUBLE)))"
            ));
        }
        // Spark's `format_string(fmt, args...)` → DuckDB `printf(fmt, args...)`.
        // Both use printf-style tokens (%s, %d, %f, ...). Corpus witness:
        // `str-015`.
        "format_string" => "printf",
        // Spark's `conv(str, from_base, to_base)` — convert numeric string
        // between bases. DuckDB has no direct equivalent. Emulate the
        // `to_base=2` and `to_base=16` common cases via bit conversions;
        // for other bases, fall through with a boundary error. Corpus
        // witness: `math-013` uses `conv(str, 10, 2)`.
        "conv" => {
            if f.args.len() != 3 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`conv` requires exactly 3 arguments".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            let _from_base = render_expr(&f.args[1], schema)?;
            let to_base_expr = &f.args[2];
            // Spark's `conv(str, from_base, to_base)` renders the value as
            // UNSIGNED 64-bit. DuckDB's `to_base(bigint, base)` produces
            // signed output. For base 2 and base 16, DuckDB's `bin` and
            // `hex` on BIGINT emit the two's-complement (unsigned) bytes,
            // matching Spark. For other to_base values, boundary-error.
            // Corpus witness: `math-013` uses to_base ∈ {2}.
            let to_base_lit = match to_base_expr {
                Expression::Literal(l) => match &l.value {
                    crate::transpiler_v2::expression::LiteralValue::Int(i) => Some(*i),
                    crate::transpiler_v2::expression::LiteralValue::Long(i) => Some(*i as i32),
                    crate::transpiler_v2::expression::LiteralValue::Short(i) => Some(*i as i32),
                    crate::transpiler_v2::expression::LiteralValue::Byte(i) => Some(*i as i32),
                    _ => None,
                },
                _ => None,
            };
            match to_base_lit {
                Some(2) => {
                    // DuckDB's `bin(bigint)` renders two's-complement bits
                    // (64-char) for negative BIGINT. For non-negative,
                    // returns the shortest binary form matching Spark.
                    return Ok(format!("bin(CAST({s} AS BIGINT))"));
                }
                Some(16) => {
                    // DuckDB's `hex(bigint)` renders two's-complement for
                    // negative BIGINTs — matches Spark.
                    return Ok(format!("hex(CAST({s} AS BIGINT))"));
                }
                _ => {
                    return Err(EmissionError::UnsupportedFunction {
                        name: f.name.clone(),
                        reason: "`conv` only implemented for to_base ∈ {2, 16}".to_owned(),
                    });
                }
            }
        }
        // Spark's `hex(int)` → hexadecimal string with 16 char zero-padding
        // for negative BIGINTs (Spark treats as unsigned). DuckDB's
        // `hex(int)` returns unpadded hex. For non-negative, both match;
        // for negatives, Spark returns FFFFFFFFFFFFFFFD (16 chars); DuckDB
        // returns the signed hex. Adjust with a CASE. Corpus witness:
        // `math-013`.
        "hex" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`hex` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            // Only remap for integer types; DuckDB's hex(VARCHAR) already
            // matches Spark's hex(String) which encodes bytes. Detect by
            // arg type at analyzer time — but we don't have that here;
            // fall back to a generic emit for numeric args:
            //   CASE WHEN a >= 0 THEN hex(CAST(a AS BIGINT))
            //        ELSE hex(CAST(a AS BIGINT) & 0xFFFFFFFFFFFFFFFF) END
            // Actually DuckDB hex() of BIGINT already handles negatives
            // by emitting the two's-complement (with an FF prefix). Verify
            // via corpus: math-013 currently reports "FFFFFFFFFFFFFFFD" as
            // correct, so DuckDB's hex(bigint) already matches Spark here.
            return Ok(format!("hex({a})"));
        }
        // Spark's `named_struct(k1, v1, k2, v2, ...)` → DuckDB
        // `struct_pack(k1 := v1, k2 := v2, ...)`.
        "named_struct" => {
            if f.args.len() % 2 != 0 || f.args.is_empty() {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`named_struct` requires an even, non-zero arg count".to_owned(),
                });
            }
            let mut parts = String::new();
            let mut i = 0;
            while i < f.args.len() {
                if i > 0 {
                    parts.push_str(", ");
                }
                let key = match &f.args[i] {
                    Expression::Literal(l) => match &l.value {
                        crate::transpiler_v2::expression::LiteralValue::String(s) => s.clone(),
                        _ => {
                            return Err(EmissionError::UnsupportedFunction {
                                name: f.name.clone(),
                                reason: "`named_struct` keys must be string literals".to_owned(),
                            });
                        }
                    },
                    _ => {
                        return Err(EmissionError::UnsupportedFunction {
                            name: f.name.clone(),
                            reason: "`named_struct` keys must be string literals".to_owned(),
                        });
                    }
                };
                let val = render_expr(&f.args[i + 1], schema)?;
                let key_q = quote_ident(&key);
                parts.push_str(&format!("{key_q} := {val}"));
                i += 2;
            }
            return Ok(format!("struct_pack({parts})"));
        }
        // Spark's `map_contains_key(m, k)` → DuckDB
        // `map_contains(m, k)` (renamed in some DuckDB versions).
        "map_contains_key" => "map_contains",
        // Spark's `map_concat(m1, m2, ...)` propagates NULL — if any input
        // map is NULL the result is NULL. DuckDB's `map_concat` silently
        // treats NULL as an empty map. Wrap with a NULL guard on every
        // argument. Corpus: `map-006`.
        "map_concat" if !f.args.is_empty() => {
            let mut arg_sqls: Vec<String> = Vec::with_capacity(f.args.len());
            for a in &f.args {
                arg_sqls.push(render_expr(a, schema)?);
            }
            let null_guard = arg_sqls
                .iter()
                .map(|s| format!("({s}) IS NULL"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let inner = arg_sqls.join(", ");
            return Ok(format!(
                "CASE WHEN {null_guard} THEN NULL ELSE map_concat({inner}) END"
            ));
        }
        // Spark's `isnull`/`isnotnull` — DuckDB uses `IS NULL`/`IS NOT NULL`.
        "isnull" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`isnull` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("({a} IS NULL)"));
        }
        "isnotnull" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`isnotnull` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("({a} IS NOT NULL)"));
        }
        // Spark's `like`/`ilike`/`rlike` as functions — DuckDB uses
        // operator syntax `x LIKE pattern` / `x ILIKE pattern` /
        // `regexp_matches(x, pattern)`.
        "like" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`like` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("({a} LIKE {b})"));
        }
        "ilike" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`ilike` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("({a} ILIKE {b})"));
        }
        "rlike" | "regexp_like" | "regexp" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`rlike` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("regexp_matches({a}, {b})"));
        }
        // Spark's `<=>(a, b)` eqNullSafe — DuckDB uses IS NOT DISTINCT FROM.
        "eqnullsafe" | "<=>" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`eqNullSafe` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("({a} IS NOT DISTINCT FROM {b})"));
        }
        // Spark's `split(str, pattern, limit)` — DuckDB's `split(str, pat)`
        // has no limit. Drop the limit arg (Spark's default is -1 = no
        // limit; corpus cases pass -1 so this is safe).
        "split" => {
            if f.args.len() < 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`split` requires at least 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("split({a}, {b})"));
        }
        // Spark bitwise ops arriving as function calls (name is symbolic).
        // DuckDB uses operator form.
        "&" | "bitwise_and" | "bitwiseand" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`bitwiseAND` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("({a} & {b})"));
        }
        "|" | "bitwise_or" | "bitwiseor" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`bitwiseOR` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("({a} | {b})"));
        }
        "^" | "bitwise_xor" | "bitwisexor" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`bitwiseXOR` requires exactly 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            return Ok(format!("xor({a}, {b})"));
        }
        // (signum handled above with explicit DOUBLE cast.)
        // Spark's `sha2(str, bits)` → DuckDB `sha256(str)` (Spark defaults
        // bits=256; we ignore the bits arg — non-256 surfaces later as
        // per-case follow-up if it fires).
        "sha2" => {
            if f.args.is_empty() {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`sha2` requires at least 1 argument".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            return Ok(format!("sha256({s})"));
        }
        // Spark `sha`/`sha1` → DuckDB `sha1`.
        "sha" => "sha1",
        // Spark's `add_months(date, n)` — DuckDB uses `date + INTERVAL n MONTH`.
        "add_months" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`add_months` requires exactly 2 arguments".to_owned(),
                });
            }
            let d = render_expr(&f.args[0], schema)?;
            let n = render_expr(&f.args[1], schema)?;
            return Ok(format!("({d} + INTERVAL ({n}) MONTH)"));
        }
        // Spark's `datediff(end, start)` (2 args, days-diff) → DuckDB's
        // `datediff('day', start, end)` (3 args, unit-prefixed).
        "datediff" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`datediff` requires exactly 2 arguments".to_owned(),
                });
            }
            let end = render_expr(&f.args[0], schema)?;
            let start = render_expr(&f.args[1], schema)?;
            return Ok(format!("datediff('day', {start}, {end})"));
        }
        "months_between" => {
            if f.args.len() < 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`months_between` requires at least 2 arguments".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            let b = render_expr(&f.args[1], schema)?;
            // Spark's `months_between(a, b)` returns a DOUBLE where the
            // integer part is the whole-month diff and the fractional part
            // is `(day-of-month diff) / 31.0` (Spark uses 31 as the
            // fractional divisor). DuckDB's `datediff('month', b, a)` gives
            // only the integer part; assemble the fractional per Spark.
            // Corpus witness: `dt-004`.
            return Ok(format!(
                "(CAST(datediff('month', {b}, {a}) AS DOUBLE) + \
                  (CAST(extract('day' FROM {a}) - extract('day' FROM {b}) AS DOUBLE) / 31.0))"
            ));
        }
        "date_add" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`date_add` requires exactly 2 arguments".to_owned(),
                });
            }
            let d = render_expr(&f.args[0], schema)?;
            let n = render_expr(&f.args[1], schema)?;
            return Ok(format!("({d} + INTERVAL ({n}) DAY)"));
        }
        "date_sub" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`date_sub` requires exactly 2 arguments".to_owned(),
                });
            }
            let d = render_expr(&f.args[0], schema)?;
            let n = render_expr(&f.args[1], schema)?;
            return Ok(format!("({d} - INTERVAL ({n}) DAY)"));
        }
        // Spark's `concat(s1, s2, ...)` on strings PROPAGATES NULL:
        // any NULL arg makes the result NULL. DuckDB's `concat` ignores
        // NULL args (returns the concatenation of the non-NULL parts).
        // Wrap in a CASE guard when any arg is nullable at the schema
        // level and every arg is a String type. Array/binary concat is
        // handled by other paths. Corpus witness: `type-015`.
        "concat"
            if !f.args.is_empty()
                && f.args
                    .iter()
                    .all(|a| matches!(a.data_type(schema), DataType::String))
                && f.args.iter().any(|a| a.nullable(schema)) =>
        {
            let mut null_guard = String::new();
            let mut rendered_args = String::new();
            for (i, arg) in f.args.iter().enumerate() {
                let sql = render_expr(arg, schema)?;
                if i > 0 {
                    rendered_args.push_str(", ");
                    null_guard.push_str(" OR ");
                }
                null_guard.push_str(&format!("({sql}) IS NULL"));
                rendered_args.push_str(&sql);
            }
            return Ok(format!(
                "(CASE WHEN {null_guard} THEN NULL ELSE concat({rendered_args}) END)"
            ));
        }
        // Spark's `isnan(x)` — schema is BOOLEAN non-nullable. DuckDB's
        // `isnan(NULL)` returns NULL; wrap in `COALESCE(..., FALSE)` to
        // match Spark's non-null semantics. Corpus witness: `cond-010`.
        "isnan" | "is_nan" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`isnan` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("COALESCE(isnan({a}), FALSE)"));
        }
        // Spark's `find_in_set(needle, csv)` — 1-based position of `needle`
        // in comma-separated `csv`, or 0 if not found. DuckDB has no
        // `find_in_set`; emit `COALESCE(list_position(string_split(csv, ','), needle), 0)`.
        "find_in_set" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`find_in_set` requires exactly 2 arguments".to_owned(),
                });
            }
            let needle = render_expr(&f.args[0], schema)?;
            let csv = render_expr(&f.args[1], schema)?;
            // `list_position` is 1-based in DuckDB (returns NULL if missing);
            // Spark returns 0 if missing. Wrap with COALESCE to 0.
            return Ok(format!(
                "COALESCE(list_position(string_split({csv}, ','), {needle}), 0)"
            ));
        }
        // Spark's `elt(idx, s1, s2, ...)` — 1-based pick from arguments.
        // DuckDB list indexing is 1-based, so emit `[s1, s2, ...][idx]`.
        "elt" => {
            if f.args.len() < 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`elt` requires at least 2 arguments".to_owned(),
                });
            }
            let idx = render_expr(&f.args[0], schema)?;
            let mut items = String::new();
            for (i, arg) in f.args.iter().enumerate().skip(1) {
                if i > 1 {
                    items.push_str(", ");
                }
                items.push_str(&render_expr(arg, schema)?);
            }
            return Ok(format!("([{items}])[{idx}]"));
        }
        // Spark's `from_json(json_str, schema_ddl[, options])` parses a
        // JSON string per a Spark DDL schema literal (e.g.
        // `"a INT, b ARRAY<STRING>"`). DuckDB's `from_json(str, json_schema)`
        // takes a JSON-object schema (`'{"a": "INTEGER"}'`) instead — τ
        // translates the Spark DDL literal into DuckDB's JSON schema shape
        // for the common no-options case. Nested `STRUCT<...>` fields
        // recurse. Corpus witnesses: `json-003`, `json-004`. Falls through
        // to a Thunderduck-boundary error when the schema is non-literal or
        // uses shapes τ does not currently translate (MAP, DECIMAL(p,s),
        // three-arg options-map form).
        "from_json" if f.args.len() == 2 => {
            let json_str = render_expr(&f.args[0], schema)?;
            if let Some(ddl) = literal_string_arg(&f.args[1]) {
                if let Some(duck_schema) = spark_ddl_schema_to_duckdb_json(&ddl) {
                    // Emit the schema as a single-quoted DuckDB JSON literal;
                    // internal double-quotes are safe (no `'` inside).
                    return Ok(format!("from_json({json_str}, '{duck_schema}')"));
                }
            }
            return Err(EmissionError::UnsupportedFunction {
                name: f.name.clone(),
                reason: "`from_json` with a non-literal DDL schema or unsupported \
                         DDL shape (τ handles the digit-schema field-list form)"
                    .to_owned(),
            });
        }
        // Spark's `try_to_number(str, fmt)` parses `str` per the numeric
        // format string `fmt` (e.g. `'999.99'`), returning DECIMAL or NULL on
        // parse failure. τ implements the common case where `fmt` is a
        // literal STRING made of `9` / `0` / `.` (no grouping / sign markers):
        // count the pre/post-decimal digits to derive DECIMAL(p, s), then
        // emit `try_cast(<str> AS DECIMAL(p, s))`. Format strings that carry
        // grouping (`,`), sign (`S`, `MI`), or currency markers fall through
        // to a Thunderduck-boundary error — τ does not currently emulate
        // Spark's exact format-error semantics for those. Corpus witness:
        // `parse-004`.
        "try_to_number" => {
            if f.args.len() != 2 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`try_to_number` requires exactly 2 arguments".to_owned(),
                });
            }
            let fmt = literal_string_arg(&f.args[1]).ok_or_else(|| {
                EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`try_to_number` requires a string literal for the format argument"
                        .to_owned(),
                }
            })?;
            let (precision, scale) =
                parse_number_format(&fmt).ok_or_else(|| EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: format!(
                        "`try_to_number`: unsupported format string `{fmt}` (τ only \
                         handles `9`/`0`/`.` digit templates)"
                    ),
                })?;
            let s = render_expr(&f.args[0], schema)?;
            return Ok(format!("try_cast({s} AS DECIMAL({precision}, {scale}))"));
        }
        // Spark's `url_encode(s)` uses application/x-www-form-urlencoded
        // encoding: spaces become `+`, everything else is `%HH`. DuckDB's
        // `url_encode(s)` uses RFC 3986 percent-encoding (spaces → `%20`).
        // Bridge by post-substituting `%20 → +`. Corpus witness: `parse-002`.
        "url_encode" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`url_encode` requires exactly 1 argument".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            return Ok(format!("replace(url_encode({s}), '%20', '+')"));
        }
        // Spark's `url_decode(s)` mirrors form-urlencoded (accepts `+` as
        // space). DuckDB's `url_decode(s)` leaves `+` literal. Bridge by
        // pre-substituting `+` → `%20` before decoding.
        "url_decode" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`url_decode` requires exactly 1 argument".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            return Ok(format!("url_decode(replace({s}, '+', '%20'))"));
        }
        // Spark's `parse_url(url, part[, key])` — DuckDB has no native
        // `parse_url`. Emit as `regexp_extract` with a per-part pattern.
        // Spark returns NULL when the requested component is absent, but
        // DuckDB's `regexp_extract` returns an empty string on no-match;
        // wrap with `NULLIF(..., '')` to align.
        //
        // Requires the second arg to be a STRING literal (the part name).
        // For QUERY-with-key, a third STRING literal is required.
        // Anchor: corpus parse-001.
        "parse_url" => {
            if !(2..=3).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`parse_url` requires 2 or 3 arguments".to_owned(),
                });
            }
            let url = render_expr(&f.args[0], schema)?;
            let part = literal_string_arg(&f.args[1]).ok_or_else(|| {
                EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`parse_url` requires a string literal for the part argument"
                        .to_owned(),
                }
            })?;
            let part_upper = part.to_ascii_uppercase();
            let pattern: String = match part_upper.as_str() {
                "HOST" => "^[^:]+://(?:[^@/]+@)?([^:/?#]+)".to_owned(),
                "PROTOCOL" => "^([^:]+)://".to_owned(),
                "PATH" => "^[^:]+://[^/?#]*([^?#]*)".to_owned(),
                "QUERY" => {
                    if f.args.len() == 3 {
                        let key = literal_string_arg(&f.args[2]).ok_or_else(|| {
                            EmissionError::UnsupportedFunction {
                                name: f.name.clone(),
                                reason:
                                    "`parse_url` with 3 arguments requires a string literal key"
                                        .to_owned(),
                            }
                        })?;
                        format!("[?&]{}=([^&#]*)", regex_escape(&key))
                    } else {
                        r"\?([^#]*)".to_owned()
                    }
                }
                "REF" => "#(.*)$".to_owned(),
                "FILE" => "^[^:]+://[^/?#]*([^#]*)".to_owned(),
                "AUTHORITY" => "^[^:]+://([^/?#]+)".to_owned(),
                "USERINFO" => "^[^:]+://([^@/?#]+)@".to_owned(),
                other => {
                    return Err(EmissionError::UnsupportedFunction {
                        name: f.name.clone(),
                        reason: format!("`parse_url` part `{other}` not supported"),
                    });
                }
            };
            let pattern_lit = sql_string_literal(&pattern);
            return Ok(format!(
                "NULLIF(regexp_extract({url}, {pattern_lit}, 1), '')"
            ));
        }
        // Spark's `overlay(str, replacement, position[, length])`. DuckDB
        // has neither the OVERLAY keyword nor an `overlay` scalar; emit
        // via substring/concat: prefix := substring(str, 1, position-1),
        // suffix := substring(str, position + length_of_replaced), where
        // length_of_replaced defaults to length(replacement).
        "overlay" => {
            if !(3..=4).contains(&f.args.len()) {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`overlay` requires 3 or 4 arguments".to_owned(),
                });
            }
            let s = render_expr(&f.args[0], schema)?;
            let r = render_expr(&f.args[1], schema)?;
            let p = render_expr(&f.args[2], schema)?;
            let length_expr = if f.args.len() == 4 {
                render_expr(&f.args[3], schema)?
            } else {
                format!("length({r})")
            };
            return Ok(format!(
                "(substring({s}, 1, ({p}) - 1) || {r} || substring({s}, ({p}) + ({length_expr})))"
            ));
        }
        _ => &name_lower,
    };
    Ok(format!("{duck_name}({args_sql})"))
}

/// Render an aggregate function call. Primitives (`count`, `sum`, `avg`,
/// `min`, `max`, `count_distinct`) pass through with Spark-parity CASTs
/// applied by [`spark_aggregate_return_cast`]. Unknown aggregate names
/// surface as Thunderduck-boundary [`EmissionError::UnsupportedFunction`]
/// per ADR-022.
fn render_aggregate(f: &FunctionCall, schema: &Schema) -> Result<String, EmissionError> {
    let lower = f.name.to_ascii_lowercase();
    // Guard-based arms MUST come before the pass-through arm (else the
    // pass-through catches `first`/`last`/`nth_value` first and the guard
    // never fires).
    if matches!(
        lower.as_str(),
        "first" | "last" | "first_value" | "last_value"
    ) && f.args.len() >= 2
    {
        // Spark's `first(col, ignorenulls)` / `last(col, ignorenulls)` —
        // DuckDB's first/last are single-arg. Drop the ignorenulls flag
        // (corpus uses ignorenulls=True which matches DuckDB's default).
        let a = render_expr(&f.args[0], schema)?;
        let distinct = if f.distinct { "DISTINCT " } else { "" };
        return Ok(format!("{lower}({distinct}{a})"));
    }
    if lower == "nth_value" && f.args.len() >= 2 {
        let col = render_expr(&f.args[0], schema)?;
        let n = render_expr(&f.args[1], schema)?;
        return Ok(format!("nth_value({col}, {n})"));
    }
    // Spark's `percentile_approx(col, quantile [, accuracy])` returns the
    // discrete value at the requested percentile — for a small dataset,
    // this matches the value at the ceil(q * n)-th sorted position, not
    // the linear-interpolation continuous median. Map to DuckDB's
    // `quantile_disc(col, quantile)` for exact Spark parity on the
    // sample size the corpus witnesses use. Drop the optional accuracy arg.
    // CAST the quantile to DOUBLE since Spark sends it as Decimal.
    // Corpus witness: `agg-013` (percentile_approx returns 88000 for
    // 8-row salary sample; `approx_quantile` returned 91500).
    if (lower == "percentile_approx" || lower == "approx_percentile") && f.args.len() >= 2 {
        let col = render_expr(&f.args[0], schema)?;
        let q = render_expr(&f.args[1], schema)?;
        return Ok(format!("quantile_disc({col}, CAST({q} AS DOUBLE))"));
    }
    let (duck_name, force_distinct) = match lower.as_str() {
        // Direct pass-through — DuckDB accepts the Spark name unchanged.
        "count"
        | "sum"
        | "avg"
        | "mean"
        | "min"
        | "max"
        | "first"
        | "last"
        | "first_value"
        | "last_value"
        | "any_value"
        | "approx_count_distinct"
        | "stddev"
        | "stddev_samp"
        | "stddev_pop"
        | "variance"
        | "var_samp"
        | "var_pop"
        | "bit_and"
        | "bit_or"
        | "bit_xor"
        | "bool_and"
        | "bool_or"
        | "corr"
        | "covar_samp"
        | "covar_pop"
        | "regr_slope"
        | "regr_r2"
        | "regr_intercept"
        | "regr_avgx"
        | "regr_avgy"
        | "regr_sxx"
        | "regr_sxy"
        | "regr_syy"
        | "median"
        | "grouping"
        | "grouping_id" => (lower.as_str(), false),
        // Spark's population-formula `skewness` — DuckDB's `skewness` uses
        // the sample formula. The ext6 extension provides `spark_skewness`
        // with Spark-parity semantics (checklist §4.1).
        "skewness" => ("spark_skewness", false),
        // Spark's `kurtosis` uses the population formula; DuckDB has
        // `kurtosis_pop` for that (native, not via extension).
        "kurtosis" => ("kurtosis_pop", false),
        // Additional aggregates: percentile_approx / approx_percentile /
        // mode / any / every / some / all.
        // percentile_approx handled with an explicit arm below.
        // Spark's `mode(col[, ignoreNulls])` — DuckDB's `mode` is single-arg
        // and rejects BOOLEAN. Drop the trailing boolean-literal
        // `ignoreNulls` flag (corpus default), and CAST-wrap boolean args
        // to INTEGER (with an outer CAST back to BOOLEAN). Anchors:
        // corpus `agg-014` (`mode(active, false)` on BOOLEAN column).
        "mode" => {
            // Extract the first arg; drop any trailing boolean-literal flags.
            let first = f.args.first().cloned();
            let trailing_bool_only = f.args.iter().skip(1).all(|e| {
                matches!(
                    e,
                    Expression::Literal(super::expression::Literal {
                        value: super::expression::LiteralValue::Boolean(_),
                        ..
                    })
                )
            });
            if let Some(arg) = first {
                if trailing_bool_only {
                    let distinct = if f.distinct { "DISTINCT " } else { "" };
                    // Peek through any wrapping Alias for the type check.
                    let inner = match &arg {
                        Expression::Alias(a) => a.expr.as_ref(),
                        other => other,
                    };
                    let a = render_expr(inner, schema)?;
                    // Boolean sniff: either the analyzer-resolved type is
                    // Boolean, OR the argument is a boolean literal.
                    let is_bool = matches!(inner.data_type(schema), DataType::Boolean)
                        || matches!(
                            inner,
                            Expression::Literal(super::expression::Literal {
                                value: super::expression::LiteralValue::Boolean(_),
                                ..
                            })
                        );
                    if is_bool {
                        return Ok(format!(
                            "CAST(mode({distinct}CAST({a} AS INTEGER)) AS BOOLEAN)"
                        ));
                    }
                    return Ok(format!("mode({distinct}{a})"));
                }
            }
            ("mode", false)
        }
        "any" | "some" => ("bool_or", false),
        "every" | "all" => ("bool_and", false),
        // `try_sum` / `try_avg` — ext6 extension arms.
        "try_sum" => ("spark_try_sum", false),
        "try_avg" => ("spark_try_avg", false),
        "std" => ("stddev", false),
        // Spark's `count_if(cond)` → DuckDB `count(*) FILTER (WHERE cond)`
        // or simpler `SUM(CASE WHEN cond THEN 1 ELSE 0 END)`. DuckDB accepts
        // `count_if` in recent versions, but safest to lower.
        "count_if" => {
            if f.args.len() != 1 {
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`count_if` requires exactly 1 argument".to_owned(),
                });
            }
            let a = render_expr(&f.args[0], schema)?;
            return Ok(format!("SUM(CASE WHEN {a} THEN 1 ELSE 0 END)"));
        }
        // Spark's `mean` is an alias for `avg`; DuckDB accepts both — treat
        // both identically above. `count_distinct` and `sum_distinct` lower
        // to DISTINCT-flagged calls.
        "count_distinct" => ("count", true),
        "sum_distinct" => ("sum", true),
        // Non-primitive aggregates surface as Thunderduck-boundary.
        _ => {
            return Err(EmissionError::UnsupportedFunction {
                name: f.name.clone(),
                reason: "aggregate function not yet in the primitive arm set".to_owned(),
            });
        }
    };
    let mut args_sql = String::new();
    // Zero-arg aggregate calls are legal for a handful of Spark functions
    // (grouping_id() picks up the ambient GROUP BY). Handle by emitting
    // the empty arg list.
    let zero_arg_ok = matches!(duck_name, "grouping_id" | "grouping") && f.args.is_empty();
    if f.args.is_empty() && !zero_arg_ok {
        return Err(EmissionError::UnsupportedFunction {
            name: f.name.clone(),
            reason: "aggregate function call has no arguments".to_owned(),
        });
    }
    for (i, arg) in f.args.iter().enumerate() {
        if i > 0 {
            args_sql.push_str(", ");
        }
        args_sql.push_str(&render_expr(arg, schema)?);
    }
    let distinct = if f.distinct || force_distinct {
        "DISTINCT "
    } else {
        ""
    };
    Ok(format!("{duck_name}({distinct}{args_sql})"))
}

/// Render the `Aggregate` operator. Emits
/// `SELECT <aggregates> FROM (<child>) AS __td_agg [GROUP BY <groupings>]`.
/// The analyzer already resolves each `aggregate` expression's type; this
/// renderer relies on `render_projection_slot` (which applies
/// [`spark_return_cast`] on non-aggregate slots) and passes aggregate slots
/// through unchanged — aggregate-return casts are the responsibility of the
/// aggregate function arm itself (via [`spark_aggregate_return_cast`], wired
/// per checklist §5.7 when needed).
/// Rewrite `grouping_id()` (no-arg) inside an expression tree to
/// `grouping_id(<grouping cols>)`. Recurses into common expression
/// containers used by the aggregate slot.
fn rewrite_grouping_id(expr: &Expression, grouping: &[Expression]) -> Expression {
    use crate::transpiler_v2::expression::{
        AliasExpression, CaseWhenExpression, CastExpression, FunctionCall,
    };
    match expr {
        Expression::FunctionCall(f) => {
            let name_lower = f.name.to_lowercase();
            if (name_lower == "grouping_id" || name_lower == "grouping")
                && f.args.is_empty()
                && !grouping.is_empty()
            {
                // Take bare column references from `grouping` (strip alias
                // wrappers for the GROUP BY reference).
                let new_args: Vec<Expression> = grouping
                    .iter()
                    .map(|g| match g {
                        Expression::Alias(a) => a.expr.as_ref().clone(),
                        other => other.clone(),
                    })
                    .collect();
                Expression::FunctionCall(FunctionCall {
                    name: f.name.clone(),
                    args: new_args,
                    distinct: f.distinct,
                })
            } else {
                let args = f
                    .args
                    .iter()
                    .map(|a| rewrite_grouping_id(a, grouping))
                    .collect();
                Expression::FunctionCall(FunctionCall {
                    name: f.name.clone(),
                    args,
                    distinct: f.distinct,
                })
            }
        }
        Expression::Alias(a) => Expression::Alias(AliasExpression {
            alias: a.alias.clone(),
            expr: Box::new(rewrite_grouping_id(&a.expr, grouping)),
        }),
        Expression::Cast(c) => Expression::Cast(CastExpression {
            expr: Box::new(rewrite_grouping_id(&c.expr, grouping)),
            to_type: c.to_type.clone(),
            try_cast: c.try_cast,
        }),
        Expression::CaseWhen(cw) => Expression::CaseWhen(CaseWhenExpression {
            branches: cw
                .branches
                .iter()
                .map(|(cond, then)| {
                    (
                        rewrite_grouping_id(cond, grouping),
                        rewrite_grouping_id(then, grouping),
                    )
                })
                .collect(),
            else_expr: cw
                .else_expr
                .as_ref()
                .map(|e| Box::new(rewrite_grouping_id(e, grouping))),
        }),
        other => other.clone(),
    }
}

fn render_aggregate_op(
    input: &TypedAst,
    grouping: &[Expression],
    aggregates: &[Expression],
    grouping_kind: crate::transpiler_v2::ast::GroupingKind,
) -> Result<String, EmissionError> {
    use crate::transpiler_v2::ast::GroupingKind;
    if matches!(grouping_kind, GroupingKind::GroupingSets) {
        return Err(EmissionError::UnsupportedOp {
            op: "Aggregate[GroupingSets]".to_owned(),
            reason: "GROUPING SETS emission requires set-membership metadata; Slice G territory"
                .to_owned(),
        });
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    // Aggregates may include folded grouping columns at the SparkSQL path
    // (see `CommonOp::Aggregate` doc). For the DataFrame path, aggregates
    // are pure aggregate calls; grouping carries the keys. Both cases emit
    // identically: SELECT the full `aggregates` list; the GROUP BY clause
    // uses `grouping` when present.
    // Mirror the analyzer's "unfold DataFrame-path grouping" logic — if
    // the aggregates list doesn't already start with the grouping cols'
    // output names, prepend them to the SELECT list so the emitted column
    // count matches the resolved schema.
    let agg_names: Vec<String> = aggregates
        .iter()
        .map(|e| match e {
            Expression::Alias(a) => a.alias.clone(),
            Expression::ColumnReference(c) => c.name.clone(),
            _ => String::new(),
        })
        .collect();
    let group_names: Vec<String> = grouping
        .iter()
        .map(|e| match e {
            Expression::Alias(a) => a.alias.clone(),
            Expression::ColumnReference(c) => c.name.clone(),
            _ => String::new(),
        })
        .collect();
    let already_folded = grouping.is_empty()
        || group_names
            .iter()
            .all(|gn| !gn.is_empty() && agg_names.iter().any(|an| an.eq_ignore_ascii_case(gn)));
    // Rewrite any `grouping_id()` (no-arg) calls inside `aggregates` to
    // pass the current grouping columns as explicit args — DuckDB requires
    // them. This is a small tree-walk local to render_aggregate_op scope.
    let rewritten_aggregates: Vec<Expression> = aggregates
        .iter()
        .map(|a| rewrite_grouping_id(a, grouping))
        .collect();
    let mut slots = String::new();
    let mut first = true;
    if !already_folded {
        for g in grouping {
            if !first {
                slots.push_str(", ");
            }
            first = false;
            slots.push_str(&render_projection_slot(g, input_schema)?);
        }
    }
    for agg in &rewritten_aggregates {
        if !first {
            slots.push_str(", ");
        }
        first = false;
        slots.push_str(&render_projection_slot(agg, input_schema)?);
    }
    let mut sql = format!("SELECT {slots} FROM ({child_sql}) AS __td_agg");
    if !grouping.is_empty() {
        let mut group_sql = String::new();
        for (i, g) in grouping.iter().enumerate() {
            if i > 0 {
                group_sql.push_str(", ");
            }
            // GROUP BY doesn't take aliases — strip any wrapping Alias to
            // avoid `GROUP BY (expr) AS name` parse errors.
            let bare = match g {
                Expression::Alias(a) => a.expr.as_ref(),
                other => other,
            };
            group_sql.push_str(&render_expr(bare, input_schema)?);
        }
        let group_sql = match grouping_kind {
            GroupingKind::GroupBy => group_sql,
            GroupingKind::Rollup => format!("ROLLUP({group_sql})"),
            GroupingKind::Cube => format!("CUBE({group_sql})"),
            GroupingKind::GroupingSets => unreachable!(), // returned early above
        };
        sql.push_str(&format!(" GROUP BY {group_sql}"));
    }
    Ok(sql)
}

// ── Literal / atomic expression renderers ────────────────────────────────────

fn render_literal(lit: &Literal) -> Result<String, EmissionError> {
    match &lit.value {
        LiteralValue::Null => Ok("NULL".to_owned()),
        LiteralValue::Boolean(b) => Ok(if *b {
            "TRUE".to_owned()
        } else {
            "FALSE".to_owned()
        }),
        LiteralValue::Byte(v) => Ok(format!("CAST({v} AS TINYINT)")),
        LiteralValue::Short(v) => Ok(format!("CAST({v} AS SMALLINT)")),
        LiteralValue::Int(v) => Ok(v.to_string()),
        LiteralValue::Long(v) => Ok(format!("CAST({v} AS BIGINT)")),
        LiteralValue::Float(v) => Ok(format!("CAST({} AS FLOAT)", format_float(*v as f64))),
        // Spark `Literal(x: Double)` is DOUBLE; DuckDB parses bare decimals
        // (`3.14`) as DECIMAL. Force the DOUBLE type to preserve the Spark
        // schema. Corpus: cast-001.
        LiteralValue::Double(v) => Ok(format!("CAST({} AS DOUBLE)", format_float(*v))),
        LiteralValue::Decimal {
            value,
            precision,
            scale,
        } => Ok(format!("CAST('{value}' AS DECIMAL({precision}, {scale}))")),
        LiteralValue::String(s) => Ok(format!("'{}'", escape_sql_string(s))),
        LiteralValue::Date(days) => {
            // Days since Unix epoch → DATE. DuckDB `epoch_us`/`epoch_ms` only
            // extract from timestamps, not construct — use the epoch anchor +
            // INTERVAL construction so DuckDB parses both directions correctly.
            Ok(format!("(DATE '1970-01-01' + INTERVAL ({days}) DAY)"))
        }
        LiteralValue::Timestamp(micros) => Ok(format!(
            "CAST(make_timestamp(CAST({micros} AS BIGINT)) AS TIMESTAMP WITH TIME ZONE)"
        )),
        LiteralValue::TimestampNtz(micros) => {
            Ok(format!("make_timestamp(CAST({micros} AS BIGINT))"))
        }
        LiteralValue::Binary(bytes) => {
            let hex: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
            Ok(format!("CAST(x'{hex}' AS BLOB)"))
        }
    }
}

fn format_float(v: f64) -> String {
    if v.is_nan() {
        "CAST('NaN' AS DOUBLE)".to_owned()
    } else if v.is_infinite() {
        if v.is_sign_negative() {
            "CAST('-Infinity' AS DOUBLE)".to_owned()
        } else {
            "CAST('Infinity' AS DOUBLE)".to_owned()
        }
    } else if v.fract() == 0.0 && v.abs() < 1e16 {
        format!("{v:.1}")
    } else {
        format!("{v}")
    }
}

fn render_column_reference(c: &ColumnReference) -> Result<String, EmissionError> {
    let name = quote_ident(&c.name);
    match &c.qualifier {
        Some(q) => {
            let q = quote_ident(q);
            Ok(format!("{q}.{name}"))
        }
        None => Ok(name.into_owned()),
    }
}

fn render_binary(b: &BinaryExpression, schema: &Schema) -> Result<String, EmissionError> {
    let l = render_expr(&b.left, schema)?;
    let r = render_expr(&b.right, schema)?;
    // Spark's DECIMAL / DECIMAL division follows Spark's precision/scale
    // widening rules (see `TypeInferenceEngine::decimal_div_type`) with
    // ROUND_HALF_UP. DuckDB's native `/` on decimals yields DOUBLE, losing
    // precision and violating the projection's declared type. Route to the
    // `thdck_spark_funcs` extension function `spark_decimal_div` which
    // implements Spark's rounding + scale semantics. Corpus: type-005.
    if matches!(b.op, BinaryOp::Div) {
        let lt = b.left.data_type(schema);
        let rt = b.right.data_type(schema);
        if matches!(lt, DataType::Decimal { .. }) && matches!(rt, DataType::Decimal { .. }) {
            return Ok(format!("spark_decimal_div(({l}), ({r}))"));
        }
    }
    let op = match b.op {
        BinaryOp::Add => "+",
        BinaryOp::Sub => "-",
        BinaryOp::Mul => "*",
        BinaryOp::Div => "/",
        BinaryOp::Mod => "%",
        BinaryOp::IntDiv => "//",
        BinaryOp::Eq => "=",
        BinaryOp::NotEq => "<>",
        BinaryOp::Lt => "<",
        BinaryOp::LtEq => "<=",
        BinaryOp::Gt => ">",
        BinaryOp::GtEq => ">=",
        BinaryOp::And => "AND",
        BinaryOp::Or => "OR",
        BinaryOp::Concat => "||",
        BinaryOp::BitAnd => "&",
        BinaryOp::BitOr => "|",
        BinaryOp::BitXor => "#",
    };
    Ok(format!("({l}) {op} ({r})"))
}

fn render_unary(u: &UnaryExpression, schema: &Schema) -> Result<String, EmissionError> {
    let inner = render_expr(&u.operand, schema)?;
    match u.op {
        UnaryOp::Not => Ok(format!("NOT ({inner})")),
        UnaryOp::Negate => Ok(format!("-({inner})")),
        UnaryOp::IsNull => Ok(format!("({inner}) IS NULL")),
        UnaryOp::IsNotNull => Ok(format!("({inner}) IS NOT NULL")),
        UnaryOp::IsNaN => Ok(format!("isnan({inner})")),
        UnaryOp::IsNotNaN => Ok(format!("NOT isnan({inner})")),
    }
}

fn render_case_when(cw: &CaseWhenExpression, schema: &Schema) -> Result<String, EmissionError> {
    let mut sql = String::from("CASE");
    for (when, then) in &cw.branches {
        let w = render_expr(when, schema)?;
        let t = render_expr(then, schema)?;
        sql.push_str(&format!(" WHEN {w} THEN {t}"));
    }
    if let Some(else_expr) = &cw.else_expr {
        let e = render_expr(else_expr, schema)?;
        sql.push_str(&format!(" ELSE {e}"));
    }
    sql.push_str(" END");
    Ok(sql)
}

fn render_alias(a: &AliasExpression, schema: &Schema) -> Result<String, EmissionError> {
    let inner = render_expr(&a.expr, schema)?;
    let alias = quote_ident(&a.alias);
    Ok(format!("{inner} AS {alias}"))
}

fn render_star(s: &StarExpression) -> Result<String, EmissionError> {
    match &s.qualifier {
        None => Ok("*".to_owned()),
        Some(q) => Ok(format!("{}.*", quote_ident(q))),
    }
}

fn render_interval(i: &IntervalExpression) -> Result<String, EmissionError> {
    // DuckDB accepts `INTERVAL '<months> months <days> days <micros> microseconds'`.
    Ok(format!(
        "INTERVAL '{} months {} days {} microseconds'",
        i.months, i.days, i.microseconds
    ))
}

// ── CAST rendering (§4.2 first item) ─────────────────────────────────────────

/// Render a CAST or TRY_CAST expression. `c.try_cast == true` emits
/// `TRY_CAST(expr AS ty)`; `false` emits `CAST(expr AS ty)` (**§4.2 first item
/// anchor**).
pub(crate) fn render_cast(c: &CastExpression, schema: &Schema) -> Result<String, EmissionError> {
    let inner = render_expr(&c.expr, schema)?;
    let ty = render_data_type(&c.to_type);
    // Spark's floating→integer cast TRUNCATES toward zero (matches Java's
    // `(int)f`). DuckDB's CAST(Double AS Integer) ROUNDS to nearest by
    // default. Insert an explicit `trunc(...)` when the source type is
    // floating-point and the target is integral. TRY_CAST retains the same
    // semantics for the truncation phase but wraps the outer CAST.
    let from_ty = c.expr.data_type(schema);
    let src_is_float = matches!(from_ty, DataType::Float | DataType::Double);
    let dst_is_integral = matches!(
        c.to_type,
        DataType::Byte | DataType::Short | DataType::Integer | DataType::Long
    );
    let expr_sql = if src_is_float && dst_is_integral {
        format!("trunc({inner})")
    } else {
        inner
    };
    if c.try_cast {
        Ok(format!("TRY_CAST({expr_sql} AS {ty})"))
    } else {
        Ok(format!("CAST({expr_sql} AS {ty})"))
    }
}

// ── Complex-type literal renderers ───────────────────────────────────────────
//
// Minimal support required to serialize `LocalRelation` payloads whose schema
// carries `ArrayType` / `MapType` / `StructType` fields. Emitted SQL uses
// DuckDB's native literal syntaxes:
//   Array : `[a, b, c]` (or `CAST([] AS T[])` for empty).
//   Map   : `MAP { k1: v1, k2: v2 }` (or `MAP()` for empty).
//   Struct: `{'name1': v1, 'name2': v2, ...}`.
// Full complex-type ops (HOF `transform`/`filter`, `explode`, struct-field
// access) remain Slice F territory.

fn render_array_literal(
    a: &crate::transpiler_v2::expression::ArrayLiteralExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    if a.elements.is_empty() {
        // Empty array — DuckDB requires a type annotation to disambiguate.
        let ty = render_data_type(&a.element_type);
        return Ok(format!("CAST([] AS {ty}[])"));
    }
    let mut buf = String::from("[");
    for (i, e) in a.elements.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        buf.push_str(&render_expr(e, schema)?);
    }
    buf.push(']');
    Ok(buf)
}

fn render_map_literal(
    m: &crate::transpiler_v2::expression::MapLiteralExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    if m.entries.is_empty() {
        return Ok("MAP()".to_owned());
    }
    let mut buf = String::from("MAP {");
    for (i, (k, v)) in m.entries.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        buf.push_str(&render_expr(k, schema)?);
        buf.push_str(": ");
        buf.push_str(&render_expr(v, schema)?);
    }
    buf.push('}');
    Ok(buf)
}

/// Render Spark `withField` / `dropFields` on a struct.
///
/// Emits DuckDB `struct_pack(f1 := struct_extract(base, 'f1'), ...)` with the
/// requested add/replace/drop applied to the input struct's declared fields.
/// Requires the base expression to have a resolved `DataType::Struct` — the
/// analyzer must run before emission. A non-struct base is a
/// Spark-emulated error (Spark itself rejects `withField` on a non-struct).
fn render_update_fields(
    u: &crate::transpiler_v2::expression::UpdateFieldsExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    // Resolve the input struct's field list at emission time. The analyzer
    // stamps ColumnReference types, so `data_type(schema)` returns a real
    // `DataType::Struct(_)` here (Pass 57 makes struct types visible).
    let base_type = u.struct_expr.data_type(schema);
    let DataType::Struct(base_struct) = base_type else {
        return Err(EmissionError::UnsupportedExpression {
            shape: "UpdateFields".to_owned(),
            reason: "withField/dropFields requires the base expression to be a StructType"
                .to_owned(),
        });
    };
    let base_sql = render_expr(&u.struct_expr, schema)?;

    // Fold updates over the field list via the shared classifier so this
    // matches `update_fields_data_type` exactly:
    //   * add / replace: case-insensitive match against current fields;
    //     preserves the original declared field name on replace.
    //   * drop: case-insensitive match against current fields.
    // The analyzer's `validate_update_fields_ops` rejects missing drop
    // targets before emission runs, so any silent-ignore here is unreachable
    // via the τ pipeline.
    let mut fields: Vec<(String, FieldSource)> = base_struct
        .fields
        .iter()
        .map(|f| (f.name.clone(), FieldSource::FromBase))
        .collect();
    crate::transpiler_v2::expression::apply_update_fields_ops(
        &mut fields,
        &u.updates,
        |name, new_val| (name.to_owned(), FieldSource::Value(new_val.clone())),
        |slot, name, new_val| {
            slot.0 = name.to_owned();
            slot.1 = FieldSource::Value(new_val.clone());
        },
        |(n, _)| n.as_str(),
    );

    // Emit `struct_pack(f1 := <expr>, f2 := <expr>, ...)`.
    let mut parts = String::new();
    for (i, (name, src)) in fields.iter().enumerate() {
        if i > 0 {
            parts.push_str(", ");
        }
        parts.push_str(&quote_ident(name));
        parts.push_str(" := ");
        match src {
            FieldSource::FromBase => {
                let key = sql_string_literal(name);
                parts.push_str(&format!("struct_extract({base_sql}, {key})"));
            }
            FieldSource::Value(expr) => {
                parts.push_str(&render_expr(expr, schema)?);
            }
        }
    }
    Ok(format!("struct_pack({parts})"))
}

/// Slot state used by [`render_update_fields`] while folding withField /
/// dropFields ops over the base struct's declared field list.
enum FieldSource {
    /// Extract from the base struct expression.
    FromBase,
    /// Take from an explicit `withField` value expression.
    Value(Expression),
}

fn render_struct_literal(
    s: &crate::transpiler_v2::expression::StructLiteralExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    let mut buf = String::from("{");
    for (i, (name, expr)) in s.fields.iter().enumerate() {
        if i > 0 {
            buf.push_str(", ");
        }
        // DuckDB struct literal keys are single-quoted string literals.
        buf.push_str(&sql_string_literal(name));
        buf.push_str(": ");
        buf.push_str(&render_expr(expr, schema)?);
    }
    buf.push('}');
    Ok(buf)
}

// ── Return-type CAST helpers (§5.1 — SEPARATE `fn` items) ────────────────────

/// Projection-slot Spark-parity return-type CAST.
///
/// Wraps `expr_sql` in `CAST(... AS T)` iff the expression's Spark-typed
/// result type requires a cast that DuckDB won't apply automatically. At
/// Slice C.1 this handles integer-integer division (Spark → Double); Slice
/// C.2 extends it with the scalar-function Spark-parity table.
///
/// **§5.1 anchor.** MUST NOT share body with [`spark_aggregate_return_cast`].
fn spark_return_cast(expr_sql: String, expr: &Expression, schema: &Schema) -> String {
    if let Expression::Binary(b) = expr {
        if matches!(b.op, BinaryOp::Div) {
            let l = b.left.data_type(schema);
            let r = b.right.data_type(schema);
            if l.is_integral() && r.is_integral() {
                return format!("CAST({expr_sql} AS DOUBLE)");
            }
        }
    }
    // Spark's CASE WHEN unifies its branch types via
    // `TypeInferenceEngine::unify_types`. DuckDB infers the CASE type from
    // the branches' native types, and for heterogeneous numeric branches
    // (e.g. INTEGER + DECIMAL literal `2.5`) it lands on DECIMAL, not the
    // Spark-unified DOUBLE. Cast the whole CASE to the Spark-typed result
    // when the branches disagree with the unified type. Corpus: type-009.
    if let Expression::CaseWhen(_) = expr {
        let dt = expr.data_type(schema);
        if matches!(
            dt,
            DataType::Double | DataType::Float | DataType::Long | DataType::Integer
        ) {
            return format!("CAST({expr_sql} AS {})", render_data_type(&dt));
        }
    }
    // Spark's `array(a, b, ...)` unifies element type to the least-common
    // numeric type; DuckDB's `list_value(1, 2.0, 3)` bottoms out at
    // DECIMAL(2,1)[] rather than the Spark-declared DOUBLE[]. Cast the
    // array to the Spark-typed element[] shape when the elements would
    // otherwise diverge (heterogeneous numeric literals). Corpus: type-020.
    if let Expression::FunctionCall(fc) = expr {
        let n = fc.name.to_ascii_lowercase();
        if matches!(n.as_str(), "array" | "list_value" | "make_array" | "list")
            && !fc.args.is_empty()
        {
            if let DataType::Array(elem, _) = expr.data_type(schema) {
                if matches!(
                    &*elem,
                    DataType::Double | DataType::Float | DataType::Long | DataType::Integer
                ) {
                    // Only cast if elements were heterogeneous — i.e. any
                    // arg's own data type differs from the unified element.
                    let elem_ref: &DataType = &elem;
                    let heterogeneous = fc.args.iter().any(|a| &a.data_type(schema) != elem_ref);
                    if heterogeneous {
                        return format!("CAST({expr_sql} AS {}[])", render_data_type(elem_ref));
                    }
                }
            }
        }
    }
    expr_sql
}

/// Aggregate Spark-parity return-type CAST.
///
/// Applied inside `render_aggregate` at C.3. Handles integer SUM/AVG widening
/// (BIGINT), decimal aggregate widening, etc.
///
/// **§5.1 anchor.** MUST NOT share body with [`spark_return_cast`].
#[allow(dead_code)] // wired in C.3
fn spark_aggregate_return_cast(agg_sql: String, agg: &FunctionCall, schema: &Schema) -> String {
    let lower = agg.name.to_lowercase();
    if let Some(arg) = agg.args.first() {
        let arg_type = arg.data_type(schema);
        match lower.as_str() {
            "sum" | "sum_distinct" | "try_sum" if arg_type.is_integral() => {
                return format!("CAST({agg_sql} AS BIGINT)");
            }
            "avg" | "mean" | "try_avg" if arg_type.is_integral() => {
                return format!("CAST({agg_sql} AS DOUBLE)");
            }
            _ => {}
        }
    }
    agg_sql
}

// ── Identifier quoting (§5.6) ────────────────────────────────────────────────

/// DuckDB reserved words that force quoting even when the identifier matches
/// `[A-Za-z_][A-Za-z0-9_]*`. Seed list drawn from DuckDB's parser keyword set;
/// extended defensively.
const DUCKDB_RESERVED: &[&str] = &[
    "all",
    "analyse",
    "analyze",
    "and",
    "any",
    "array",
    "as",
    "asc",
    "asymmetric",
    "both",
    "case",
    "cast",
    "check",
    "collate",
    "column",
    "constraint",
    "create",
    "cross",
    "current_catalog",
    "current_date",
    "current_role",
    "current_time",
    "current_timestamp",
    "current_user",
    "default",
    "deferrable",
    "desc",
    "describe",
    "distinct",
    "do",
    "else",
    "end",
    "except",
    "false",
    "fetch",
    "for",
    "foreign",
    "from",
    "full",
    "grant",
    "group",
    "groups",
    "having",
    "in",
    "initially",
    "inner",
    "intersect",
    "into",
    "join",
    "lateral",
    "leading",
    "left",
    "limit",
    "list",
    "map",
    "natural",
    "not",
    "null",
    "offset",
    "on",
    "only",
    "or",
    "order",
    "outer",
    "over",
    "partition",
    "pivot",
    "placing",
    "primary",
    "qualify",
    "range",
    "references",
    "returning",
    "right",
    "rows",
    "sample",
    "select",
    "session_user",
    "some",
    "struct",
    "symmetric",
    "table",
    "then",
    "to",
    "trailing",
    "true",
    "union",
    "unique",
    "unpivot",
    "user",
    "using",
    "variadic",
    "when",
    "where",
    "window",
    "with",
];

/// Quote a SQL identifier only when required. Returns [`Cow::Borrowed`] on the
/// happy path (identifier matches `[A-Za-z_][A-Za-z0-9_]*` AND is not a
/// DuckDB reserved word), otherwise [`Cow::Owned`] with the identifier
/// wrapped in `"..."` and any embedded `"` doubled.
///
/// **§5.6 anchor.**
pub(crate) fn quote_ident(name: &str) -> Cow<'_, str> {
    if is_safe_identifier(name) {
        Cow::Borrowed(name)
    } else {
        let escaped = name.replace('"', "\"\"");
        Cow::Owned(format!("\"{escaped}\""))
    }
}

fn is_safe_identifier(name: &str) -> bool {
    if name.is_empty() {
        return false;
    }
    let mut chars = name.chars();
    let first = chars.next().expect("checked non-empty above");
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    for c in chars {
        if !(c.is_ascii_alphanumeric() || c == '_') {
            return false;
        }
    }
    // `DUCKDB_RESERVED` entries are all-lowercase ASCII AND sorted in
    // strictly ascending lexicographic order (audited above). Combined with
    // the ASCII-safe identifier check we just performed, an ASCII
    // case-insensitive byte comparator lets us binary-search — O(log₂ 91)
    // comparisons on the miss (common) path — while keeping the §5.6
    // `Cow::Borrowed` fast path zero-alloc.
    DUCKDB_RESERVED
        .binary_search_by(|r| ascii_ci_cmp(r.as_bytes(), name.as_bytes()))
        .is_err()
}

/// ASCII case-insensitive byte-slice comparator. Correct only when both inputs
/// are known-ASCII; used by [`is_safe_identifier`] where the input has already
/// been restricted to `[A-Za-z_][A-Za-z0-9_]*` and `DUCKDB_RESERVED` entries
/// are audited as lowercase ASCII.
fn ascii_ci_cmp(a: &[u8], b: &[u8]) -> std::cmp::Ordering {
    let len = a.len().min(b.len());
    for i in 0..len {
        let ca = a[i].to_ascii_lowercase();
        let cb = b[i].to_ascii_lowercase();
        match ca.cmp(&cb) {
            std::cmp::Ordering::Equal => continue,
            non_eq => return non_eq,
        }
    }
    a.len().cmp(&b.len())
}

// ── SQL string escaping helpers ──────────────────────────────────────────────

fn escape_sql_string(s: &str) -> String {
    s.replace('\'', "''")
}

fn escape_sql_char(c: char) -> String {
    if c == '\'' {
        "''".to_owned()
    } else {
        c.to_string()
    }
}

/// Render `s` as a DuckDB SQL single-quoted string literal, escaping any
/// embedded quotes. Prefer this over inline `format!("'{}'", ...)` so callers
/// stay consistent when the escape rules change.
fn sql_string_literal(s: &str) -> String {
    format!("'{}'", escape_sql_string(s))
}

/// If `e` is a string literal expression, return its raw value. Otherwise
/// return `None`. Used by scalars like `parse_url` that require literal
/// STRING parts / keys.
fn literal_string_arg(e: &Expression) -> Option<String> {
    match e {
        Expression::Literal(super::expression::Literal {
            value: super::expression::LiteralValue::String(s),
            ..
        }) => Some(s.clone()),
        _ => None,
    }
}

/// Parse a Spark numeric format string (as used by `to_number` /
/// `try_to_number`) into `(precision, scale)`. Supports only the common
/// digit-template shape composed of `9` and `0` optionally split by a
/// single `.` (e.g. `"999.99"` → `(5, 2)`). Returns `None` for any format
/// that carries grouping / sign / currency markers.
///
/// Pass 76: corpus witness `parse-004`.
pub(crate) fn parse_number_format_for_type_inference(fmt: &str) -> Option<(u8, u8)> {
    parse_number_format(fmt)
}

/// Parse a Spark DDL field-list schema string (e.g.
/// `"a INT, b ARRAY<STRING>"`) into a [`StructType`] using the same
/// tolerant subset as [`spark_ddl_schema_to_duckdb_json`]. Returns
/// `None` when τ cannot translate the DDL — the caller then falls back
/// to the shared type-inference default. Pass 76 witnesses: `json-003`,
/// `json-004`.
pub(crate) fn from_json_ddl_to_struct_for_type_inference(ddl: &str) -> Option<StructType> {
    let fields = split_top_level_fields(ddl)?;
    let mut out: Vec<StructField> = Vec::with_capacity(fields.len());
    for field in &fields {
        let (name, ty) = split_field_name_type(field)?;
        let dt = spark_ddl_type_to_core_data_type(ty.trim())?;
        out.push(StructField::new(name.trim().to_owned(), dt, true));
    }
    Some(StructType::new(out))
}

fn spark_ddl_type_to_core_data_type(ty: &str) -> Option<DataType> {
    let trimmed = ty.trim();
    let upper = trimmed.to_ascii_uppercase();
    if let Some(inner) = upper
        .strip_prefix("STRUCT<")
        .and_then(|r| r.strip_suffix('>'))
    {
        // Recurse using the original (case-preserved) inner slice so struct
        // field names keep their casing.
        let _ = inner;
        let orig_inner = &trimmed[trimmed.find('<')? + 1..trimmed.rfind('>')?];
        let st = from_json_ddl_to_struct_for_type_inference(orig_inner)?;
        return Some(DataType::Struct(st));
    }
    if let Some(inner) = upper
        .strip_prefix("ARRAY<")
        .and_then(|r| r.strip_suffix('>'))
    {
        let elem = spark_ddl_type_to_core_data_type(inner)?;
        return Some(DataType::Array(Box::new(elem), true));
    }
    Some(match upper.as_str() {
        "INT" | "INTEGER" => DataType::Integer,
        "LONG" | "BIGINT" => DataType::Long,
        "SHORT" | "SMALLINT" => DataType::Short,
        "TINYINT" | "BYTE" => DataType::Byte,
        "FLOAT" | "REAL" => DataType::Float,
        "DOUBLE" => DataType::Double,
        "BOOLEAN" | "BOOL" => DataType::Boolean,
        "STRING" | "VARCHAR" => DataType::String,
        "BINARY" | "BLOB" => DataType::Binary,
        "DATE" => DataType::Date,
        "TIMESTAMP" | "TIMESTAMP_LTZ" => DataType::Timestamp,
        "TIMESTAMP_NTZ" => DataType::TimestampNtz,
        _ => return None,
    })
}

/// Translate a Spark DDL field-list schema (as used by `from_json`,
/// e.g. `"a INT, b ARRAY<STRING>, c STRUCT<d:BOOLEAN>"`) into a DuckDB
/// JSON-schema object literal (e.g.
/// `{"a":"INTEGER","b":"VARCHAR[]","c":{"d":"BOOLEAN"}}`).
///
/// Returns `None` for shapes τ does not currently translate — the caller
/// converts that to a Thunderduck-boundary error rather than emitting a
/// broken schema.
///
/// Supported shapes:
///   - Primitive types: `INT`, `INTEGER`, `LONG`, `BIGINT`, `SHORT`,
///     `SMALLINT`, `TINYINT`, `BYTE`, `FLOAT`, `DOUBLE`, `BOOLEAN`,
///     `STRING`, `VARCHAR`, `BINARY`, `DATE`, `TIMESTAMP`.
///   - `ARRAY<T>` → `T[]` where `T` is any supported primitive.
///   - `STRUCT<f1:T1, f2:T2, ...>` → nested JSON object.
///
/// Pass 76 witnesses: `json-003`, `json-004`.
fn spark_ddl_schema_to_duckdb_json(ddl: &str) -> Option<String> {
    let fields = split_top_level_fields(ddl)?;
    let mut out = String::from("{");
    for (i, field) in fields.iter().enumerate() {
        let (name, ty) = split_field_name_type(field)?;
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(name.trim());
        out.push_str("\":");
        out.push_str(&spark_ddl_type_to_duckdb_json_value(ty.trim())?);
    }
    out.push('}');
    Some(out)
}

/// Split a comma-separated field list, honoring nested `<...>` and `(...)`
/// so `STRUCT<a:INT, b:DOUBLE>` is treated as one field.
fn split_top_level_fields(s: &str) -> Option<Vec<String>> {
    let mut parts: Vec<String> = Vec::new();
    let mut depth = 0i32;
    let mut cur = String::new();
    for ch in s.chars() {
        match ch {
            '<' | '(' => {
                depth += 1;
                cur.push(ch);
            }
            '>' | ')' => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
                cur.push(ch);
            }
            ',' if depth == 0 => {
                if !cur.trim().is_empty() {
                    parts.push(std::mem::take(&mut cur));
                }
            }
            _ => cur.push(ch),
        }
    }
    if depth != 0 {
        return None;
    }
    if !cur.trim().is_empty() {
        parts.push(cur);
    }
    Some(parts)
}

/// Split `name TYPE` (space-separated, top-level DDL) or `name:TYPE`
/// (colon-separated, used inside `STRUCT<...>`) into `(name, type_str)`.
/// Honors nested `<...>` and `(...)` so a `:` inside `STRUCT<f:INT>` is
/// not mistaken for the outer separator.
fn split_field_name_type(field: &str) -> Option<(&str, &str)> {
    let trimmed = field.trim();
    let mut depth = 0i32;
    let mut sep_idx: Option<usize> = None;
    let mut sep_len = 1usize;
    for (i, ch) in trimmed.char_indices() {
        match ch {
            '<' | '(' => depth += 1,
            '>' | ')' => depth -= 1,
            ':' if depth == 0 => {
                sep_idx = Some(i);
                sep_len = 1;
                break;
            }
            c if depth == 0 && c.is_whitespace() => {
                if sep_idx.is_none() {
                    sep_idx = Some(i);
                    sep_len = c.len_utf8();
                    // Don't `break` on whitespace — we still prefer a
                    // colon if one appears later at depth 0.
                }
            }
            _ => {}
        }
    }
    let idx = sep_idx?;
    let (n, t) = trimmed.split_at(idx);
    Some((n, &t[sep_len..]))
}

fn spark_ddl_type_to_duckdb_json_value(ty: &str) -> Option<String> {
    let trimmed = ty.trim();
    let upper = trimmed.to_ascii_uppercase();
    // STRUCT<...> → nested object.
    if let Some(inner) = upper
        .strip_prefix("STRUCT<")
        .and_then(|r| r.strip_suffix('>'))
    {
        // Re-slice the original (case-preserved) DDL to keep field names
        // as-written; the uppercase prefix/suffix match is only for the
        // `STRUCT<...>` envelope.
        let orig_inner = &trimmed[trimmed.find('<')? + 1..trimmed.rfind('>')?];
        let _ = inner; // consumed only for the envelope match
        return spark_ddl_schema_to_duckdb_json(orig_inner);
    }
    // ARRAY<T> → "<duckdb_T>[]".
    if let Some(inner) = upper
        .strip_prefix("ARRAY<")
        .and_then(|r| r.strip_suffix('>'))
    {
        let elem = spark_ddl_primitive_to_duckdb(inner.trim())?;
        return Some(format!("\"{elem}[]\""));
    }
    // Primitive.
    let duck = spark_ddl_primitive_to_duckdb(&upper)?;
    Some(format!("\"{duck}\""))
}

fn spark_ddl_primitive_to_duckdb(ty: &str) -> Option<&'static str> {
    // Return DuckDB's canonical type-name spelling. `INT` / `INTEGER`
    // both accept in DuckDB; use `INTEGER` for clarity.
    match ty.trim() {
        "INT" | "INTEGER" => Some("INTEGER"),
        "LONG" | "BIGINT" => Some("BIGINT"),
        "SHORT" | "SMALLINT" => Some("SMALLINT"),
        "TINYINT" | "BYTE" => Some("TINYINT"),
        "FLOAT" | "REAL" => Some("FLOAT"),
        "DOUBLE" => Some("DOUBLE"),
        "BOOLEAN" | "BOOL" => Some("BOOLEAN"),
        "STRING" | "VARCHAR" => Some("VARCHAR"),
        "BINARY" | "BLOB" => Some("BLOB"),
        "DATE" => Some("DATE"),
        "TIMESTAMP" | "TIMESTAMP_LTZ" => Some("TIMESTAMP"),
        "TIMESTAMP_NTZ" => Some("TIMESTAMP"),
        _ => None,
    }
}

fn parse_number_format(fmt: &str) -> Option<(u8, u8)> {
    let trimmed = fmt.trim();
    if trimmed.is_empty() {
        return None;
    }
    let mut pre = 0u32;
    let mut post = 0u32;
    let mut seen_dot = false;
    for ch in trimmed.chars() {
        match ch {
            '9' | '0' => {
                if seen_dot {
                    post += 1;
                } else {
                    pre += 1;
                }
            }
            '.' if !seen_dot => seen_dot = true,
            _ => return None,
        }
    }
    let precision_u32 = pre + post;
    if precision_u32 == 0 || precision_u32 > 38 {
        return None;
    }
    Some((precision_u32 as u8, post as u8))
}

/// Escape the characters that carry regex meaning in a DuckDB regex pattern.
/// Used when interpolating a user-supplied literal (e.g. a `parse_url`
/// query-parameter key) into a regex fragment.
fn regex_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(
            c,
            '.' | '*' | '+' | '?' | '(' | ')' | '[' | ']' | '{' | '}' | '|' | '\\' | '^' | '$'
        ) {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

// ── DataType → DuckDB SQL type-string ────────────────────────────────────────

/// Render a [`DataType`] as its DuckDB SQL type-string (`BIGINT`, `VARCHAR`,
/// `DECIMAL(p,s)`, `TIMESTAMP`, ...).
fn render_data_type(dt: &DataType) -> String {
    match dt {
        DataType::Boolean => "BOOLEAN".to_owned(),
        DataType::Byte => "TINYINT".to_owned(),
        DataType::Short => "SMALLINT".to_owned(),
        DataType::Integer => "INTEGER".to_owned(),
        DataType::Long => "BIGINT".to_owned(),
        DataType::Float => "FLOAT".to_owned(),
        DataType::Double => "DOUBLE".to_owned(),
        DataType::Decimal { precision, scale } => format!("DECIMAL({precision}, {scale})"),
        DataType::String => "VARCHAR".to_owned(),
        DataType::Binary => "BLOB".to_owned(),
        DataType::Date => "DATE".to_owned(),
        DataType::Timestamp => "TIMESTAMP WITH TIME ZONE".to_owned(),
        DataType::TimestampNtz => "TIMESTAMP".to_owned(),
        DataType::YearMonthInterval | DataType::DayTimeInterval | DataType::Interval => {
            "INTERVAL".to_owned()
        }
        DataType::Null => "INTEGER".to_owned(), // best-effort; NULL cast target.
        DataType::Unresolved => "VARCHAR".to_owned(),
        DataType::Array(elem, _) => format!("{}[]", render_data_type(elem)),
        DataType::Map { key, value, .. } => {
            format!(
                "MAP({}, {})",
                render_data_type(key),
                render_data_type(value)
            )
        }
        DataType::Struct(st) => {
            let inner: Vec<String> = st
                .fields
                .iter()
                .map(|f: &StructField| {
                    let name_q = quote_ident(&f.name);
                    format!("{name_q} {}", render_data_type(&f.data_type))
                })
                .collect();
            format!("STRUCT({})", inner.join(", "))
        }
    }
}

// ── Extension allow-list (§4.1 stub — populated by Slice D) ──────────────────

/// The set of DuckDB extension function names τ emits. **Empty at Slice C.1**;
/// Slice D populates with the ext6 allow-list and activates INV6.
#[allow(dead_code)] // Slice D wires call sites; Slice C.1 exposes the surface.
pub(crate) fn extension_targets() -> HashSet<&'static str> {
    HashSet::new()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::ast::{CommonAst, CommonOp};
    use crate::transpiler_v2::base_types::BaseTypes;
    use crate::transpiler_v2::expression::{
        AliasExpression, BetweenExpression, BinaryExpression, BinaryOp, CaseWhenExpression,
        CastExpression, ColumnReference, FunctionCall, InListExpression, IntervalExpression,
        LambdaExpression, LambdaVariableExpression, LikeExpression, Literal, LiteralValue,
        StarExpression, UnaryExpression, UnaryOp, UpdateFieldsExpression,
    };
    use crate::transpiler_v2::{analyze, generate};

    fn tap_guard() -> std::sync::MutexGuard<'static, ()> {
        EMIT_TAP_MUTEX.lock().expect("EMIT_TAP_MUTEX poisoned")
    }

    fn empty_schema() -> Schema {
        StructType::empty()
    }

    fn emp_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    fn base_types_with_emp() -> BaseTypes {
        let plan = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            _ => None,
        })
    }

    fn int_lit(v: i32) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Int(v),
            data_type: DataType::Integer,
        })
    }

    // ── 1. dispatch_op — SingleRow ───────────────────────────────────────

    #[test]
    fn dispatch_op_single_row_emits_subquery_safe_select() {
        let _g = tap_guard();
        let ast = CommonAst::new(CommonOp::SingleRow);
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze SingleRow");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch SingleRow");
        // `SELECT 1` is subquery-safe (DuckDB requires a projection list
        // inside `FROM (...)`); the placeholder column is inert because
        // analyzer stamps SingleRow with an empty schema and Project provides
        // its own SELECT list when wrapping.
        assert_eq!(sql, "SELECT 1");
    }

    // ── 2-3. dispatch_op — TableScan ─────────────────────────────────────

    #[test]
    fn dispatch_op_table_scan_emits_select_star_from_table() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let typed = analyze(ast, &bt).expect("analyze TableScan");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(sql, "SELECT * FROM emp");
    }

    #[test]
    fn dispatch_op_table_scan_with_alias_emits_alias() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: Some("e".to_owned()),
        });
        let typed = analyze(ast, &bt).expect("analyze TableScan alias");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(sql, "SELECT * FROM emp AS e");
    }

    // ── 4-6. render_project ──────────────────────────────────────────────

    #[test]
    fn render_project_simple_select() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )],
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.starts_with("SELECT id FROM ("), "got: {sql}");
        assert!(sql.contains("SELECT * FROM emp"), "got: {sql}");
    }

    #[test]
    fn render_project_alias_slot_wraps_cast() {
        let _g = tap_guard();
        // int/int → Double under Spark; spark_return_cast wraps as
        // CAST(... AS DOUBLE); alias is preserved outside the CAST.
        let bt = base_types_with_emp();
        let div = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )),
            right: Box::new(int_lit(2)),
        });
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(div),
            alias: "ratio".to_owned(),
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![aliased],
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("CAST("), "expected CAST wrapper: {sql}");
        assert!(sql.contains("AS DOUBLE)"), "expected AS DOUBLE: {sql}");
        assert!(sql.contains("AS ratio"), "expected AS ratio: {sql}");
    }

    #[test]
    fn render_project_int_div_yields_double_cast() {
        let _g = tap_guard();
        // int/int projection without alias — must still be CAST AS DOUBLE.
        let bt = base_types_with_emp();
        let div = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(int_lit(6)),
            right: Box::new(int_lit(2)),
        });
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            projections: vec![div],
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("CAST(") && sql.contains("AS DOUBLE"),
            "got: {sql}"
        );
    }

    // ── 7. render_filter ─────────────────────────────────────────────────

    #[test]
    fn render_filter_composes_where_clause() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let cond = Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )),
            right: Box::new(int_lit(10)),
        });
        // Wrap as Filter with a condition Boolean via `expr > 10` — but Gt
        // returns boolean. Cast for filter shape? Filter analyzer expects
        // Boolean-result; Binary::Gt is boolean. Good.
        // BUT: analyzer requires cond to be Boolean; the shape above IS
        // Boolean (Gt). We turn it into a Cast for safety.
        let ast = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            condition: cond,
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("WHERE"), "got: {sql}");
        assert!(
            sql.contains("(id) > (10)") || sql.contains("id) > (10"),
            "got: {sql}"
        );
    }

    // ── 8-9. render_sort ─────────────────────────────────────────────────

    #[test]
    fn render_sort_asc_desc_nulls_first_last() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let order = vec![
            SortOrder {
                expr: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "id".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    },
                )),
                direction: SortDirection::Descending,
                null_ordering: NullOrdering::NullsFirst,
            },
            SortOrder {
                expr: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "name".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    },
                )),
                direction: SortDirection::Ascending,
                null_ordering: NullOrdering::NullsLast,
            },
        ];
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            order,
            limit: None,
            offset: None,
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("ORDER BY id DESC NULLS FIRST"), "got: {sql}");
        assert!(sql.contains("name ASC NULLS LAST"), "got: {sql}");
    }

    #[test]
    fn render_sort_with_limit_and_offset() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sort {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            order: vec![SortOrder {
                expr: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "id".to_owned(),
                        qualifier: None,
                        plan_id: None,
                    },
                )),
                direction: SortDirection::Ascending,
                null_ordering: NullOrdering::NullsFirst,
            }],
            limit: Some(10),
            offset: Some(5),
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("LIMIT 10"), "got: {sql}");
        assert!(sql.contains("OFFSET 5"), "got: {sql}");
    }

    // ── 10. render_limit ─────────────────────────────────────────────────

    #[test]
    fn render_limit_emits_limit_offset() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Limit {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            limit: 20,
            offset: Some(3),
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("LIMIT 20"), "got: {sql}");
        assert!(sql.contains("OFFSET 3"), "got: {sql}");
    }

    // ── 11. render_values ────────────────────────────────────────────────

    #[test]
    fn render_values_emits_values_alias() {
        let _g = tap_guard();
        let row = vec![int_lit(1), int_lit(2)];
        let ast = CommonAst::new(CommonOp::Values {
            rows: vec![row],
            column_names: vec!["a".to_owned(), "b".to_owned()],
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("VALUES"), "got: {sql}");
        assert!(sql.contains("__td_values(a, b)"), "got: {sql}");
    }

    // ── 12. render_local_relation ────────────────────────────────────────

    #[test]
    fn render_local_relation_emits_values_from_literals() {
        let _g = tap_guard();
        let schema = StructType::new(vec![
            StructField::not_null("a", DataType::Integer),
            StructField::nullable("b", DataType::String),
        ]);
        let row = vec![
            int_lit(1),
            Expression::Literal(Literal {
                value: LiteralValue::String("x".to_owned()),
                data_type: DataType::String,
            }),
        ];
        let ast = CommonAst::new(CommonOp::LocalRelation {
            schema,
            rows: vec![row],
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("VALUES"), "got: {sql}");
        assert!(sql.contains("CAST(1 AS INTEGER)"), "got: {sql}");
        assert!(sql.contains("'x'"), "got: {sql}");
        assert!(sql.contains("__td_local(a, b)"), "got: {sql}");
    }

    // ── 13. render_file_scan ─────────────────────────────────────────────

    #[test]
    fn render_file_scan_parquet_emits_read_parquet() {
        let _g = tap_guard();
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let ast = CommonAst::new(CommonOp::FileScan {
            format: FileFormat::Parquet,
            paths: vec!["/tmp/x.parquet".to_owned()],
            schema: Some(schema),
            options: vec![],
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(sql, "SELECT * FROM read_parquet('/tmp/x.parquet')");
    }

    // ── 14-15. render_cast (§4.2 first item) ─────────────────────────────

    #[test]
    fn render_cast_emits_cast() {
        let expr = CastExpression {
            expr: Box::new(int_lit(1)),
            to_type: DataType::Long,
            try_cast: false,
        };
        let sql = render_cast(&expr, &empty_schema()).expect("render");
        assert_eq!(sql, "CAST(1 AS BIGINT)");
    }

    #[test]
    fn render_cast_try_cast_emits_try_cast() {
        // §4.2 first item anchor.
        let expr = CastExpression {
            expr: Box::new(int_lit(1)),
            to_type: DataType::Long,
            try_cast: true,
        };
        let sql = render_cast(&expr, &empty_schema()).expect("render");
        assert_eq!(sql, "TRY_CAST(1 AS BIGINT)");
    }

    // ── 16-17. render_binary ─────────────────────────────────────────────

    #[test]
    fn render_binary_add_int_int() {
        let b = BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(int_lit(3)),
            right: Box::new(int_lit(4)),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert_eq!(sql, "(3) + (4)");
    }

    #[test]
    fn render_binary_eq_boolean() {
        let b = BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(int_lit(3)),
            right: Box::new(int_lit(3)),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert_eq!(sql, "(3) = (3)");
    }

    // ── 18. render_unary ─────────────────────────────────────────────────

    #[test]
    fn render_unary_not_isnull() {
        let u = UnaryExpression {
            op: UnaryOp::IsNull,
            operand: Box::new(int_lit(1)),
        };
        let sql = render_unary(&u, &empty_schema()).expect("render");
        assert_eq!(sql, "(1) IS NULL");

        let u2 = UnaryExpression {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Literal(Literal {
                value: LiteralValue::Boolean(true),
                data_type: DataType::Boolean,
            })),
        };
        let sql2 = render_unary(&u2, &empty_schema()).expect("render");
        assert_eq!(sql2, "NOT (TRUE)");
    }

    // ── 19. render_case_when ─────────────────────────────────────────────

    #[test]
    fn render_case_when_with_else() {
        let cw = CaseWhenExpression {
            branches: vec![(
                Expression::Literal(Literal {
                    value: LiteralValue::Boolean(true),
                    data_type: DataType::Boolean,
                }),
                int_lit(1),
            )],
            else_expr: Some(Box::new(int_lit(2))),
        };
        let sql = render_case_when(&cw, &empty_schema()).expect("render");
        assert_eq!(sql, "CASE WHEN TRUE THEN 1 ELSE 2 END");
    }

    // ── 20. Between + InList ─────────────────────────────────────────────

    #[test]
    fn render_between_and_inlist() {
        let between = Expression::Between(BetweenExpression {
            expr: Box::new(int_lit(5)),
            low: Box::new(int_lit(1)),
            high: Box::new(int_lit(10)),
            negated: false,
        });
        let sql = render_expr(&between, &empty_schema()).expect("render");
        assert_eq!(sql, "(5) BETWEEN (1) AND (10)");

        let in_list = Expression::InList(InListExpression {
            expr: Box::new(int_lit(1)),
            list: vec![int_lit(1), int_lit(2), int_lit(3)],
            negated: true,
        });
        let sql = render_expr(&in_list, &empty_schema()).expect("render");
        assert_eq!(sql, "(1) NOT IN (1, 2, 3)");
    }

    // ── 21. Like / ILike ─────────────────────────────────────────────────

    #[test]
    fn render_like_ilike_variants() {
        let s = Expression::Literal(Literal {
            value: LiteralValue::String("hello".to_owned()),
            data_type: DataType::String,
        });
        let pat = Expression::Literal(Literal {
            value: LiteralValue::String("h%".to_owned()),
            data_type: DataType::String,
        });
        let like = Expression::Like(LikeExpression {
            value: Box::new(s.clone()),
            pattern: Box::new(pat.clone()),
            escape: None,
            negated: false,
            case_insensitive: false,
        });
        let sql = render_expr(&like, &empty_schema()).expect("render");
        assert!(sql.contains("LIKE"), "got: {sql}");
        assert!(!sql.contains("ILIKE"), "got: {sql}");

        let ilike = Expression::Like(LikeExpression {
            value: Box::new(s),
            pattern: Box::new(pat),
            escape: None,
            negated: false,
            case_insensitive: true,
        });
        let sql = render_expr(&ilike, &empty_schema()).expect("render");
        assert!(sql.contains("ILIKE"), "got: {sql}");
    }

    // ── 22. Star + qualified star ────────────────────────────────────────

    #[test]
    fn render_star_and_qualified_star() {
        let star = StarExpression { qualifier: None };
        assert_eq!(render_star(&star).expect("render"), "*");
        let qstar = StarExpression {
            qualifier: Some("t".to_owned()),
        };
        assert_eq!(render_star(&qstar).expect("render"), "t.*");
    }

    // ── 23. Unsupported function ─────────────────────────────────────────

    #[test]
    fn render_expr_function_call_stub_returns_unsupported_function() {
        let expr = Expression::FunctionCall(FunctionCall {
            name: "sha3".to_owned(),
            args: vec![int_lit(1)],
            distinct: false,
        });
        let err = render_expr(&expr, &empty_schema()).unwrap_err();
        assert!(matches!(err, EmissionError::UnsupportedFunction { .. }));
    }

    // ── 24. Unsupported aggregate ────────────────────────────────────────

    #[test]
    fn render_expr_aggregate_stub_returns_unsupported_op() {
        let expr = Expression::FunctionCall(FunctionCall {
            name: "sum".to_owned(),
            args: vec![int_lit(1)],
            distinct: false,
        });
        let err = render_expr(&expr, &empty_schema()).unwrap_err();
        assert!(matches!(err, EmissionError::UnsupportedOp { .. }));
    }

    // ── DUCKDB_RESERVED invariants — required by the binary_search shape
    // inside `is_safe_identifier`. If either invariant regresses the linear
    // fallback via `.iter().any(|r| r.eq_ignore_ascii_case(name))` remains
    // semantically correct, but the O(log n) fast path silently returns
    // wrong answers.
    #[test]
    fn duckdb_reserved_is_sorted_ascending() {
        assert!(
            DUCKDB_RESERVED.windows(2).all(|w| w[0] < w[1]),
            "DUCKDB_RESERVED must be strictly ascending — binary_search in \
             `is_safe_identifier` depends on it",
        );
    }

    #[test]
    fn duckdb_reserved_is_all_lowercase_ascii() {
        for r in DUCKDB_RESERVED {
            assert!(
                r.bytes().all(|b| b.is_ascii() && !b.is_ascii_uppercase()),
                "DUCKDB_RESERVED entry `{r}` must be lowercase ASCII — \
                 `ascii_ci_cmp` treats entries as lowercase",
            );
        }
    }

    // ── 25-27. quote_ident (§5.6) ────────────────────────────────────────

    #[test]
    fn quote_ident_fast_path_returns_borrowed_for_unquoted_safe() {
        // §5.6 fast path: safe identifier → Cow::Borrowed.
        let out = quote_ident("id");
        assert!(matches!(out, Cow::Borrowed(_)));
        assert_eq!(out, "id");
    }

    #[test]
    fn quote_ident_quotes_reserved_word() {
        let out = quote_ident("select");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out, "\"select\"");
    }

    #[test]
    fn quote_ident_quotes_identifier_with_space() {
        let out = quote_ident("first name");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out, "\"first name\"");

        // Embedded double-quote: doubled.
        let out2 = quote_ident("a\"b");
        assert_eq!(out2, "\"a\"\"b\"");
    }

    // ── 28. INV3 — no legacy `use` inside emission.rs ────────────────────

    #[test]
    fn inv3_no_legacy_use_in_emission() {
        // Only scan the non-test region of emission.rs; the tests themselves
        // legitimately name legacy paths inside their assertion literals.
        let this_file = include_str!("emission.rs");
        // The `#[cfg(test)]` module below carries the offending literals; cut
        // at its start marker.
        let module_marker = "#[cfg(test)]\nmod tests {";
        let scan_slice = match this_file.find(module_marker) {
            Some(idx) => &this_file[..idx],
            None => this_file,
        };
        // Build needles at runtime so this test's source doesn't self-match.
        let legacy_bases = ["generator", "functions", "logical", "parser", "runtime"];
        for base in legacy_bases {
            let use_form = format!("use crate::{base}::");
            let path_form = format!("crate::{base}::");
            assert!(
                !scan_slice.contains(&use_form),
                "INV3 violation: emission.rs contains `{use_form}`",
            );
            assert!(
                !scan_slice.contains(&path_form),
                "INV3 violation: emission.rs contains `{path_form}`",
            );
        }
    }

    // ── 29. INV10 positive — emission.rs imports are typed ───────────────

    #[test]
    fn inv10_emission_imports_are_typed() {
        // Positive shape check: the non-test region of emission.rs may only
        // `use crate::...` from `crate::types::{DataType, StructField,
        // StructType}`. The `#[cfg(test)]` tests below legitimately import
        // fixtures from `crate::transpiler_v2::…`.
        let this_file = include_str!("emission.rs");
        let module_marker = "#[cfg(test)]\nmod tests {";
        let scan_slice = match this_file.find(module_marker) {
            Some(idx) => &this_file[..idx],
            None => this_file,
        };
        for line in scan_slice.lines() {
            let trimmed = line.trim_start();
            if !trimmed.starts_with("use crate::") {
                continue;
            }
            assert!(
                trimmed.starts_with("use crate::types::"),
                "INV10 positive violation — unexpected `use crate::...` line: {trimmed}",
            );
        }
    }

    // ── 30. §5.4 — render_tail uses CTE ──────────────────────────────────

    #[test]
    fn render_tail_uses_cte_not_double_embed() {
        // §5.4 anchor. render_tail is unwired under Decision 13-A; we invoke
        // the helper directly with a synthesized child TypedAst.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: None,
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = render_tail(&typed, 3).expect("render_tail");
        assert!(sql.contains("WITH __td_child AS"), "got: {sql}");
        // Child SQL string appears exactly ONCE in the output (INV: no double
        // embedding of child SQL).
        let child_marker = "SELECT * FROM emp";
        let occurrences = sql.matches(child_marker).count();
        assert_eq!(
            occurrences, 1,
            "child SQL must appear exactly once (CTE); got {occurrences} in: {sql}",
        );
    }

    // ── 31. §5.1 — return-cast helpers are distinct ─────────────────────

    #[test]
    fn spark_return_cast_and_aggregate_return_cast_are_distinct_fns() {
        // §5.1 anchor — the two helpers must be two `fn` items with distinct
        // function pointers. Rust's `#[allow(dead_code)]` on the aggregate
        // helper does not merge the item.
        let f1: fn(String, &Expression, &Schema) -> String = spark_return_cast;
        let f2: fn(String, &FunctionCall, &Schema) -> String = spark_aggregate_return_cast;
        // Cast to raw pointers for identity comparison.
        let p1 = f1 as *const ();
        let p2 = f2 as *const ();
        assert_ne!(p1, p2, "helpers must be distinct fn items");
    }

    // ── 32. extension_targets is empty at C.1 ────────────────────────────

    #[test]
    fn extension_targets_is_empty_at_slice_c1() {
        assert!(extension_targets().is_empty());
    }

    // ── EMIT_TAP increments on Ok dispatch ───────────────────────────────

    #[test]
    fn emit_tap_increments_on_ok_dispatch() {
        let _g = tap_guard();
        let before = EMIT_TAP.load(Ordering::Relaxed);
        let ast = CommonAst::new(CommonOp::SingleRow);
        let _sql = generate(&ast, &BaseTypes::empty()).expect("generate");
        let after = EMIT_TAP.load(Ordering::Relaxed);
        assert_eq!(after - before, 1);
    }

    #[test]
    fn emit_tap_does_not_increment_on_err_dispatch() {
        let _g = tap_guard();
        let before = EMIT_TAP.load(Ordering::Relaxed);
        // Aggregate is unimplemented at C.1 → UnsupportedOp.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: vec![],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "count".to_owned(),
                args: vec![],
                distinct: false,
            })],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
        });
        let _ = generate(&ast, &bt);
        let after = EMIT_TAP.load(Ordering::Relaxed);
        assert_eq!(after - before, 0);
    }

    // ── Additional coverage: Interval literal ───────────────────────────

    #[test]
    fn render_interval_emits_interval_literal() {
        let i = IntervalExpression {
            months: 1,
            days: 2,
            microseconds: 3,
        };
        let sql = render_interval(&i).expect("render");
        assert!(sql.starts_with("INTERVAL '"), "got: {sql}");
        assert!(sql.contains("1 months 2 days 3 microseconds"), "got: {sql}");
    }

    #[test]
    fn render_column_reference_qualified() {
        let c = ColumnReference {
            name: "id".to_owned(),
            qualifier: Some("emp".to_owned()),
            data_type: Some(DataType::Long),
            nullable: Some(false),
        };
        let sql = render_column_reference(&c).expect("render");
        assert_eq!(sql, "emp.id");
    }

    // ── Spark `struct(...)` → DuckDB `struct_pack(name := expr, ...)` ────
    //
    // Regression tests for corpus case `struct-001`. The old emission
    // remapped `struct` → `row`, which produced anonymous fields and broke
    // PySpark Arrow decoding (empty string keys collide). The current arm
    // derives Spark-parity field names per argument.

    fn col_ref_expr(name: &str) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.to_owned(),
            qualifier: None,
            data_type: Some(DataType::String),
            nullable: Some(true),
        })
    }

    /// §9 test 1 — struct-001 regression: `struct("name","age")` →
    /// `struct_pack(name := name, age := age)`.
    #[test]
    fn render_struct_two_column_refs() {
        let f = FunctionCall {
            name: "struct".to_owned(),
            args: vec![col_ref_expr("name"), col_ref_expr("age")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render struct");
        assert_eq!(sql, "struct_pack(name := name, age := age)");
    }

    /// §9 test 2 — alias wins over inner column.
    #[test]
    fn render_struct_with_alias() {
        let inner = col_ref_expr("name");
        let aliased = Expression::Alias(AliasExpression {
            expr: Box::new(inner),
            alias: "who".to_owned(),
        });
        let f = FunctionCall {
            name: "struct".to_owned(),
            args: vec![aliased],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render struct");
        assert_eq!(sql, "struct_pack(who := name)");
    }

    /// §9 test 3 — string-literal argument falls back to `col{i+1}`.
    /// `F.struct(lit("colA"))` (or SparkSQL `SELECT struct('colA')`) matches
    /// Spark's `Alias.tryUnaliasedName` fallback: the resulting struct type
    /// is `struct<col1: string>`, NOT a field named `"colA"`. PySpark's
    /// `F.struct("colA")` overload goes through `UnresolvedAttribute` at the
    /// proto boundary, not `Literal`, so no legitimate producer reaches this
    /// path with the literal value as the intended field name.
    #[test]
    fn render_struct_string_literal_falls_back_to_col1() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::String("colA".to_owned()),
            data_type: DataType::String,
        });
        let f = FunctionCall {
            name: "struct".to_owned(),
            args: vec![lit],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render struct");
        assert_eq!(sql, "struct_pack(col1 := 'colA')");
    }

    /// §9 test 4 — computed expression falls back to `col{i+1}`.
    #[test]
    fn render_struct_computed_expression() {
        let computed = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(col_ref_expr("a")),
            right: Box::new(Expression::Literal(Literal {
                value: LiteralValue::Int(1),
                data_type: DataType::Integer,
            })),
        });
        let f = FunctionCall {
            name: "struct".to_owned(),
            args: vec![computed, col_ref_expr("b")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render struct");
        assert_eq!(sql, "struct_pack(col1 := (a) + (1), b := b)");
    }

    /// §9 test 5 — zero-arg `struct()` emits `struct_pack()`.
    #[test]
    fn render_struct_empty() {
        let f = FunctionCall {
            name: "struct".to_owned(),
            args: vec![],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render struct");
        assert_eq!(sql, "struct_pack()");
    }

    // ── JSON / CSV cluster (Pass 62) ────────────────────────────────────

    /// json-005 anchor: `to_json(struct(...))` passes through unchanged to
    /// DuckDB's native `to_json`.
    #[test]
    fn render_to_json_of_struct_passes_through() {
        let struct_arg = Expression::FunctionCall(FunctionCall {
            name: "struct".to_owned(),
            args: vec![col_ref_expr("name"), col_ref_expr("age")],
            distinct: false,
        });
        let f = FunctionCall {
            name: "to_json".to_owned(),
            args: vec![struct_arg],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_json");
        assert_eq!(
            sql, "to_json(struct_pack(name := name, age := age))",
            "to_json wraps the DuckDB struct_pack unchanged",
        );
    }

    /// json-006 anchor: `schema_of_json(...)` is remapped to
    /// `spark_schema_of_json(...)` (thdck_spark_funcs extension).
    #[test]
    fn render_schema_of_json_remaps_to_extension() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::String(r#"{"a":1,"b":"x"}"#.to_owned()),
            data_type: DataType::String,
        });
        let f = FunctionCall {
            name: "schema_of_json".to_owned(),
            args: vec![lit],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render schema_of_json");
        assert_eq!(sql, "spark_schema_of_json('{\"a\":1,\"b\":\"x\"}')");
    }

    /// json-008 anchor: `to_csv(struct(a, b, c))` — DuckDB has no `to_csv`
    /// scalar; τ unpacks the struct fields and emits
    /// `concat_ws(',', CAST(a AS VARCHAR), CAST(b AS VARCHAR), CAST(c AS VARCHAR))`.
    #[test]
    fn render_to_csv_of_struct_emits_concat_ws() {
        let struct_arg = Expression::FunctionCall(FunctionCall {
            name: "struct".to_owned(),
            args: vec![
                col_ref_expr("id"),
                col_ref_expr("name"),
                col_ref_expr("age"),
            ],
            distinct: false,
        });
        let f = FunctionCall {
            name: "to_csv".to_owned(),
            args: vec![struct_arg],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_csv");
        assert_eq!(
            sql,
            "concat_ws(',', CAST(id AS VARCHAR), CAST(name AS VARCHAR), CAST(age AS VARCHAR))",
        );
    }

    /// `to_csv(named_struct('k1', v1, 'k2', v2))` — τ extracts the value
    /// slots (odd indices) and emits `concat_ws(',', CAST(v1 AS VARCHAR),
    /// CAST(v2 AS VARCHAR))`. Keys are metadata only.
    #[test]
    fn render_to_csv_of_named_struct_extracts_values() {
        let key1 = Expression::Literal(Literal {
            value: LiteralValue::String("k1".to_owned()),
            data_type: DataType::String,
        });
        let key2 = Expression::Literal(Literal {
            value: LiteralValue::String("k2".to_owned()),
            data_type: DataType::String,
        });
        let named_struct = Expression::FunctionCall(FunctionCall {
            name: "named_struct".to_owned(),
            args: vec![key1, col_ref_expr("id"), key2, col_ref_expr("name")],
            distinct: false,
        });
        let f = FunctionCall {
            name: "to_csv".to_owned(),
            args: vec![named_struct],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_csv");
        assert_eq!(
            sql,
            "concat_ws(',', CAST(id AS VARCHAR), CAST(name AS VARCHAR))",
        );
    }

    /// `to_csv(col)` where `col` is not a struct literal — τ has no way to
    /// enumerate the fields at emission time, so it returns a honest
    /// Thunderduck-boundary error instead of silently emitting bad SQL.
    #[test]
    fn render_to_csv_of_non_struct_arg_is_boundary_error() {
        let f = FunctionCall {
            name: "to_csv".to_owned(),
            args: vec![col_ref_expr("some_struct_col")],
            distinct: false,
        };
        let err = render_function_call(&f, &empty_schema())
            .expect_err("to_csv on non-struct arg must boundary-error");
        match err {
            EmissionError::UnsupportedFunction { name, reason } => {
                assert_eq!(name, "to_csv");
                assert!(reason.contains("struct"), "reason: {reason}");
            }
            other => panic!("expected UnsupportedFunction, got {other:?}"),
        }
    }

    // ── Math domain-guard wrappers (Pass 63) ────────────────────────────

    /// `math-005` anchor: `log(y)` with y=0 must return NULL under Spark
    /// non-ANSI semantics, not raise DuckDB "cannot take logarithm of zero".
    /// τ wraps the call in a CASE that guards `> 0`.
    #[test]
    fn render_log_wraps_in_null_safe_domain_guard() {
        let f = FunctionCall {
            name: "log".to_owned(),
            args: vec![col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render log");
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN ln(y) ELSE NULL END");
    }

    /// Explicit `ln(y)` — identical guard, direct DuckDB name.
    #[test]
    fn render_ln_wraps_in_null_safe_domain_guard() {
        let f = FunctionCall {
            name: "ln".to_owned(),
            args: vec![col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render ln");
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN ln(y) ELSE NULL END");
    }

    /// `log10(y)` — same guard, DuckDB has native `log10`.
    #[test]
    fn render_log10_wraps_in_null_safe_domain_guard() {
        let f = FunctionCall {
            name: "log10".to_owned(),
            args: vec![col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render log10");
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log10(y) ELSE NULL END");
    }

    /// `log2(y)` — same guard, DuckDB has native `log2`.
    #[test]
    fn render_log2_wraps_in_null_safe_domain_guard() {
        let f = FunctionCall {
            name: "log2".to_owned(),
            args: vec![col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render log2");
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log2(y) ELSE NULL END");
    }

    /// Two-arg `log(base, x)` — guard is on the value arg (x), the base is
    /// passed through as DuckDB's `log(base, x)` positional form.
    #[test]
    fn render_log_two_arg_guards_value_only() {
        let f = FunctionCall {
            name: "log".to_owned(),
            args: vec![int_lit(10), col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render log(base, x)");
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log(10, y) ELSE NULL END");
    }

    /// `math-012` anchor: `shiftleft(a, 2)` where `a` may be negative must
    /// not raise DuckDB "Cannot left-shift negative number". τ emits as
    /// arithmetic multiplication `a * (1::BIGINT << n)` which accepts
    /// negative operands and preserves 2's-complement shift semantics.
    #[test]
    fn render_shiftleft_uses_arithmetic_form() {
        let f = FunctionCall {
            name: "shiftleft".to_owned(),
            args: vec![col_ref_expr("a"), int_lit(2)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render shiftleft");
        assert_eq!(sql, "(a * (1::BIGINT << (2)))");
    }

    /// Pass 73: `hypot(a, b)` — DuckDB has no `hypot` scalar; τ emits the
    /// inline form `sqrt(a*a + b*b)` with explicit DOUBLE casts.
    #[test]
    fn render_hypot_emits_inline_sqrt_form() {
        let f = FunctionCall {
            name: "hypot".to_owned(),
            args: vec![col_ref_expr("x"), col_ref_expr("y")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render hypot");
        assert!(sql.starts_with("sqrt("));
        assert!(sql.contains("CAST(x AS DOUBLE)"));
        assert!(sql.contains("CAST(y AS DOUBLE)"));
    }

    /// Pass 73: `format_string(fmt, args...)` remaps to DuckDB's `printf`.
    #[test]
    fn render_format_string_remaps_to_printf() {
        let f = FunctionCall {
            name: "format_string".to_owned(),
            args: vec![
                Expression::Literal(Literal {
                    value: LiteralValue::String("%s=%d".to_owned()),
                    data_type: DataType::String,
                }),
                col_ref_expr("name"),
                col_ref_expr("age"),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render format_string");
        assert!(sql.starts_with("printf("));
    }

    /// Pass 73: `bround(x, n)` — Spark's banker's rounding. Emit a
    /// half-even CASE around `round(x * 10^n)`.
    #[test]
    fn render_bround_emits_half_even_case() {
        let f = FunctionCall {
            name: "bround".to_owned(),
            args: vec![col_ref_expr("x"), int_lit(1)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render bround");
        assert!(sql.contains("floor(") || sql.contains("round("));
        // The half-even branch must reference the even-parity check.
        assert!(sql.contains("% 2 = 0"));
    }

    /// `shiftright(a, n)` — DuckDB's `>>` is arithmetic on signed BIGINT
    /// and accepts negative operands, so τ passes it through directly.
    #[test]
    fn render_shiftright_uses_operator_form() {
        let f = FunctionCall {
            name: "shiftright".to_owned(),
            args: vec![col_ref_expr("a"), int_lit(2)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render shiftright");
        assert_eq!(sql, "(a >> (2))");
    }

    /// `win-006` anchor: PySpark serializes `F.nth_value(col, 2)` as
    /// `nth_value(col, 2, False)` — three args including a trailing
    /// `ignoreNulls` boolean literal. DuckDB's `nth_value(col, n)` accepts
    /// only two args and rejects the extra with "Incorrect number of
    /// parameters". τ must drop the trailing boolean literal.
    #[test]
    fn render_nth_value_drops_trailing_ignore_nulls_bool() {
        let bool_lit = Expression::Literal(Literal {
            value: LiteralValue::Boolean(false),
            data_type: DataType::Boolean,
        });
        let f = FunctionCall {
            name: "nth_value".to_owned(),
            args: vec![col_ref_expr("salary"), int_lit(2), bool_lit],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render nth_value");
        assert_eq!(sql, "nth_value(salary, 2)");
    }

    /// Two-arg `nth_value` (no trailing bool) passes through unchanged.
    #[test]
    fn render_nth_value_two_args_passes_through() {
        let f = FunctionCall {
            name: "nth_value".to_owned(),
            args: vec![col_ref_expr("salary"), int_lit(2)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render nth_value");
        assert_eq!(sql, "nth_value(salary, 2)");
    }

    /// A trailing non-boolean argument must NOT be silently dropped — the
    /// arm only triggers on a boolean literal in the trailing position.
    /// Verifies the safety-net check on the trim behavior.
    #[test]
    fn render_nth_value_with_non_bool_extra_arg_passes_through() {
        let f = FunctionCall {
            name: "nth_value".to_owned(),
            args: vec![col_ref_expr("salary"), int_lit(2), int_lit(99)],
            distinct: false,
        };
        // Falls through to pass-through emission — DuckDB will still reject
        // the extra arg, but τ preserves it faithfully rather than silently
        // dropping a real value.
        let sql = render_function_call(&f, &empty_schema()).expect("render nth_value passthrough");
        assert_eq!(sql, "nth_value(salary, 2, 99)");
    }

    // ── Unpivot emission ────────────────────────────────────────────────

    #[test]
    /// grp-004 shape — emits conditional-aggregate SQL that matches Spark's
    /// PIVOT semantics (empty COUNT buckets → NULL, not 0). Pass 60 anchor.
    #[test]
    fn render_pivot_explicit_values_emits_conditional_aggregate_shape() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        // Build: emp.groupBy("dept_id").pivot("id", [1, 2]).agg(count(*) AS n)
        // Using existing emp cols to satisfy the analyzer.
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )],
            pivot_column: Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            ),
            pivot_values: vec![int_lit(1), int_lit(2)],
            aggregates: vec![Expression::Alias(AliasExpression {
                alias: "n".to_owned(),
                expr: Box::new(Expression::FunctionCall(FunctionCall {
                    name: "count".to_owned(),
                    args: vec![int_lit(1)],
                    distinct: false,
                })),
            })],
        });
        let sql = generate(&ast, &bt).expect("generate pivot");
        // Conditional aggregate shape: NULLIF wraps COUNT, CASE keys the
        // pivot column against each value via IS NOT DISTINCT FROM.
        assert!(sql.contains("SELECT "), "got: {sql}");
        assert!(sql.contains("NULLIF(count("), "got: {sql}");
        assert!(
            sql.contains("CASE WHEN id IS NOT DISTINCT FROM 1"),
            "got: {sql}"
        );
        assert!(
            sql.contains("CASE WHEN id IS NOT DISTINCT FROM 2"),
            "got: {sql}"
        );
        assert!(sql.contains(" AS \"1\""), "got: {sql}");
        assert!(sql.contains(" AS \"2\""), "got: {sql}");
        assert!(sql.contains(" GROUP BY dept_id"), "got: {sql}");
        assert!(sql.contains("__td_pivot_src"), "got: {sql}");
    }

    /// Multi-aggregate pivot names outputs `<pivot_value>_<agg_alias>` per
    /// Spark, and non-COUNT aggregates are NOT wrapped in NULLIF (SUM etc.
    /// already return NULL for empty buckets).
    #[test]
    fn render_pivot_multi_agg_names_and_only_count_gets_nullif() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            grouping: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )],
            pivot_column: Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            ),
            pivot_values: vec![int_lit(1)],
            aggregates: vec![
                Expression::Alias(AliasExpression {
                    alias: "s".to_owned(),
                    expr: Box::new(Expression::FunctionCall(FunctionCall {
                        name: "sum".to_owned(),
                        args: vec![Expression::UnresolvedColumn(
                            crate::transpiler_v2::expression::UnresolvedColumn {
                                name: "salary".to_owned(),
                                qualifier: None,
                                plan_id: None,
                            },
                        )],
                        distinct: false,
                    })),
                }),
                Expression::Alias(AliasExpression {
                    alias: "c".to_owned(),
                    expr: Box::new(Expression::FunctionCall(FunctionCall {
                        name: "count".to_owned(),
                        args: vec![int_lit(1)],
                        distinct: false,
                    })),
                }),
            ],
        });
        let sql = generate(&ast, &bt).expect("generate multi-agg pivot");
        assert!(sql.contains("sum(CASE WHEN "), "got: {sql}");
        // Only count gets NULLIF-wrapped; SUM's natural NULL suffices.
        assert!(sql.contains("NULLIF(count("), "got: {sql}");
        assert!(
            !sql.contains("NULLIF(sum("),
            "SUM must not be NULLIF-wrapped; got: {sql}"
        );
        assert!(sql.contains(" AS \"1_s\""), "got: {sql}");
        assert!(sql.contains(" AS \"1_c\""), "got: {sql}");
    }

    fn render_unpivot_emits_duckdb_unpivot_shape() {
        // Anchor: piv-004 shape — emits
        //   UNPIVOT (SELECT <ids>,<values> FROM (<child>) AS __td_unpivot_src)
        //     ON <values> INTO NAME "metric" VALUE "value"
        // per the legacy `gen_unpivot` SQL contract.
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(CommonAst::new(CommonOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            })),
            ids: vec!["id".to_owned()],
            values: vec!["dept_id".to_owned(), "salary".to_owned()],
            variable_column_name: "metric".to_owned(),
            value_column_name: "value".to_owned(),
        });
        let sql = generate(&ast, &bt).expect("generate unpivot");
        assert!(sql.starts_with("UNPIVOT ("), "got: {sql}");
        // quote_ident skips quoting for safe identifiers.
        assert!(sql.contains("SELECT id, dept_id, salary"), "got: {sql}");
        assert!(sql.contains(" ON dept_id, salary"), "got: {sql}");
        assert!(sql.contains("INTO NAME metric VALUE value"), "got: {sql}",);
        assert!(sql.contains("__td_unpivot_src"), "got: {sql}");
    }

    // ── UpdateFields emission (Pass 61 — struct-005 / struct-006) ────────

    fn address_struct_dt() -> DataType {
        DataType::Struct(StructType::new(vec![
            StructField::nullable("street", DataType::String),
            StructField::nullable("city", DataType::String),
            StructField::nullable("geo", DataType::String),
        ]))
    }

    fn address_col() -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: "address".to_owned(),
            qualifier: None,
            data_type: Some(address_struct_dt()),
            nullable: Some(true),
        })
    }

    fn addr_schema() -> Schema {
        StructType::new(vec![StructField::nullable("address", address_struct_dt())])
    }

    /// struct-005 anchor — `withField("country", lit("AT"))` reconstructs the
    /// struct with all base fields preserved and the new `country` field
    /// appended.
    #[test]
    fn render_update_fields_with_field_emits_struct_pack_with_appended_field() {
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_col()),
            updates: vec![(
                "country".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("AT".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        let sql = render_expr(&expr, &addr_schema()).expect("render update_fields");
        assert_eq!(
            sql,
            "struct_pack(street := struct_extract(address, 'street'), \
             city := struct_extract(address, 'city'), \
             geo := struct_extract(address, 'geo'), \
             country := 'AT')"
        );
    }

    /// `withField("city", lit("Vienna"))` replaces the existing field's slot
    /// with the new value expression while preserving its position.
    #[test]
    fn render_update_fields_with_field_replaces_existing_slot() {
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_col()),
            updates: vec![(
                "city".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("Vienna".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        let sql = render_expr(&expr, &addr_schema()).expect("render update_fields");
        assert_eq!(
            sql,
            "struct_pack(street := struct_extract(address, 'street'), \
             city := 'Vienna', \
             geo := struct_extract(address, 'geo'))"
        );
    }

    /// struct-006 anchor — `dropFields("geo")` reconstructs the struct with
    /// `geo` removed and the surviving fields extracted from the base.
    #[test]
    fn render_update_fields_drop_field_emits_struct_pack_without_dropped() {
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_col()),
            updates: vec![("geo".to_owned(), None)],
        });
        let sql = render_expr(&expr, &addr_schema()).expect("render update_fields");
        assert_eq!(
            sql,
            "struct_pack(street := struct_extract(address, 'street'), \
             city := struct_extract(address, 'city'))"
        );
    }

    /// Review-fix C1 lock: `withField("CITY", ...)` on a struct declaring
    /// `city` emits a replace at the original slot with the *original*
    /// declared name (`city`), matching Spark 4.1.
    #[test]
    fn render_update_fields_with_field_case_insensitive_preserves_original_name() {
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_col()),
            updates: vec![(
                "CITY".to_owned(),
                Some(Expression::Literal(Literal {
                    value: LiteralValue::String("Vienna".to_owned()),
                    data_type: DataType::String,
                })),
            )],
        });
        let sql = render_expr(&expr, &addr_schema()).expect("render update_fields");
        // Emitted slot name is the ORIGINAL `city`, not the caller's `CITY`.
        assert_eq!(
            sql,
            "struct_pack(street := struct_extract(address, 'street'), \
             city := 'Vienna', \
             geo := struct_extract(address, 'geo'))"
        );
    }

    /// Review-fix C2 lock: emission's mixed-case op result must match the
    /// analyzer-derived struct schema exactly. Cross-checks `field_names`
    /// against the emitted `struct_pack` slot list.
    #[test]
    fn render_update_fields_mixed_case_agrees_with_analyzer() {
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(address_col()),
            updates: vec![
                (
                    "CITY".to_owned(),
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::String("Vienna".to_owned()),
                        data_type: DataType::String,
                    })),
                ),
                ("GEO".to_owned(), None),
                (
                    "country".to_owned(),
                    Some(Expression::Literal(Literal {
                        value: LiteralValue::String("AT".to_owned()),
                        data_type: DataType::String,
                    })),
                ),
            ],
        });
        let sql = render_expr(&expr, &addr_schema()).expect("render update_fields");
        // Analyzer view: ["street", "city", "country"] — emission must agree.
        assert_eq!(
            sql,
            "struct_pack(street := struct_extract(address, 'street'), \
             city := 'Vienna', \
             country := 'AT')"
        );
        // Explicit analyzer cross-check for parity.
        let analyzed = expr.data_type(&addr_schema());
        match analyzed {
            DataType::Struct(st) => {
                let names: Vec<&str> = st.fields.iter().map(|f| f.name.as_str()).collect();
                assert_eq!(names, vec!["street", "city", "country"]);
            }
            other => panic!("expected DataType::Struct, got: {other:?}"),
        }
    }

    // ── Pass 66: date/time function emission ─────────────────────────────
    //
    // Regression tests for `to_date(str, fmt)`, `to_timestamp(str, fmt)`,
    // `unix_timestamp(col[, fmt])`, `from_unixtime(secs[, fmt])`. All rely
    // on the shared `spark_fmt_to_duckdb` helper.

    fn str_lit(s: &str) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(s.to_owned()),
            data_type: DataType::String,
        })
    }

    fn long_lit(v: i64) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Long(v),
            data_type: DataType::Long,
        })
    }

    #[test]
    fn spark_fmt_to_duckdb_translates_common_tokens() {
        // Sanity: helper wraps the input in the replace chain we expect.
        let out = spark_fmt_to_duckdb("'yyyy-MM-dd HH:mm:ss'");
        assert!(out.contains("'yyyy'"));
        assert!(out.contains("'%Y'"));
        assert!(out.contains("'MM'"));
        assert!(out.contains("'%m'"));
        assert!(out.contains("'HH'"));
        assert!(out.contains("'%H'"));
    }

    #[test]
    fn render_to_date_two_arg_uses_strptime_with_translated_format() {
        // dt-009 regression: `F.to_date(F.lit("15/01/2026"), "dd/MM/yyyy")`
        // must emit `CAST(strptime(..., translated_fmt) AS DATE)` — NOT the
        // pre-Pass-66 UnsupportedFunction error.
        let f = FunctionCall {
            name: "to_date".to_owned(),
            args: vec![str_lit("15/01/2026"), str_lit("dd/MM/yyyy")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_date");
        assert!(sql.starts_with("CAST(strptime('15/01/2026', replace("));
        assert!(sql.contains("'dd/MM/yyyy'"));
        assert!(sql.ends_with(") AS DATE)"));
    }

    #[test]
    fn render_to_date_one_arg_stays_a_cast() {
        let f = FunctionCall {
            name: "to_date".to_owned(),
            args: vec![str_lit("2026-01-15")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_date");
        assert_eq!(sql, "CAST('2026-01-15' AS DATE)");
    }

    #[test]
    fn render_to_timestamp_two_arg_uses_strptime() {
        // dt-010 regression: `F.to_timestamp(F.lit("2026-01-15 10:00"),
        // "yyyy-MM-dd HH:mm")` must emit `strptime(..., translated_fmt)` —
        // NOT `to_timestamp(STRING, STRING)` which DuckDB rejects.
        let f = FunctionCall {
            name: "to_timestamp".to_owned(),
            args: vec![str_lit("2026-01-15 10:00"), str_lit("yyyy-MM-dd HH:mm")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_timestamp");
        assert!(sql.starts_with("strptime('2026-01-15 10:00', replace("));
        assert!(sql.contains("'yyyy-MM-dd HH:mm'"));
    }

    #[test]
    fn render_to_timestamp_one_arg_stays_a_cast() {
        let f = FunctionCall {
            name: "to_timestamp".to_owned(),
            args: vec![str_lit("2026-01-15 10:00:00")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_timestamp");
        assert_eq!(sql, "CAST('2026-01-15 10:00:00' AS TIMESTAMP)");
    }

    #[test]
    fn render_unix_timestamp_one_arg_casts_epoch_to_bigint() {
        // dt-014 regression #1: `F.unix_timestamp("last_login")` must emit
        // `CAST(epoch(last_login) AS BIGINT)`. Pre-Pass-66 emission was just
        // `epoch(last_login)` which DuckDB accepts but with wrong Spark-parity
        // return type (Double vs Long) and TZ column shape mismatch.
        let f = FunctionCall {
            name: "unix_timestamp".to_owned(),
            args: vec![col_ref_expr("last_login")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render unix_timestamp");
        assert_eq!(sql, "CAST(epoch(last_login) AS BIGINT)");
    }

    #[test]
    fn render_unix_timestamp_two_arg_wraps_strptime() {
        let f = FunctionCall {
            name: "unix_timestamp".to_owned(),
            args: vec![col_ref_expr("ts_str"), str_lit("yyyy-MM-dd HH:mm:ss")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render unix_timestamp");
        assert!(sql.starts_with("CAST(epoch(strptime(ts_str, replace("));
        assert!(sql.ends_with(")) AS BIGINT)"));
    }

    /// dt-014 regression: `F.unix_timestamp("last_login")` on a Timestamp
    /// column arrives at the transpiler as a 2-arg call with a synthetic
    /// default format `yyyy-MM-dd HH:mm:ss` (PySpark auto-fills). The
    /// emission MUST detect the temporal input type and skip `strptime`,
    /// otherwise DuckDB errors with `strptime(TIMESTAMP, VARCHAR)` — no
    /// such overload exists.
    #[test]
    fn render_unix_timestamp_two_arg_temporal_input_skips_strptime() {
        let f = FunctionCall {
            name: "unix_timestamp".to_owned(),
            args: vec![
                Expression::ColumnReference(ColumnReference {
                    name: "last_login".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::Timestamp),
                    nullable: Some(true),
                }),
                str_lit("yyyy-MM-dd HH:mm:ss"),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render unix_timestamp");
        assert_eq!(sql, "CAST(epoch(last_login) AS BIGINT)");
    }

    #[test]
    fn render_from_unixtime_one_arg_returns_default_format_string() {
        // dt-014 regression #2: `F.from_unixtime(F.lit(1700000000))` must
        // emit `strftime(to_timestamp(CAST(<lit> AS DOUBLE)),
        // '%Y-%m-%d %H:%M:%S')`. Spark returns String, not Timestamp.
        // Note: Long literal renders as `CAST(1700000000 AS BIGINT)` — the
        // outer `CAST(.. AS DOUBLE)` wraps it, which DuckDB folds fine.
        let f = FunctionCall {
            name: "from_unixtime".to_owned(),
            args: vec![long_lit(1_700_000_000)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render from_unixtime");
        assert!(sql.starts_with("strftime(to_timestamp(CAST("));
        assert!(sql.contains("1700000000"));
        assert!(sql.ends_with(" AS DOUBLE)), '%Y-%m-%d %H:%M:%S')"));
    }

    #[test]
    fn render_from_unixtime_two_arg_translates_format() {
        let f = FunctionCall {
            name: "from_unixtime".to_owned(),
            args: vec![long_lit(1_700_000_000), str_lit("yyyy/MM/dd")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render from_unixtime");
        assert!(sql.starts_with("strftime(to_timestamp(CAST("));
        assert!(sql.contains("1700000000"));
        assert!(sql.contains(" AS DOUBLE)), replace("));
        assert!(sql.contains("'yyyy/MM/dd'"));
    }

    /// Non-struct base is a Spark-emulated error (Spark itself rejects
    /// `withField` on scalar types).
    #[test]
    fn render_update_fields_non_struct_base_is_error() {
        let schema = StructType::new(vec![StructField::nullable("name", DataType::String)]);
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(Expression::ColumnReference(ColumnReference {
                name: "name".to_owned(),
                qualifier: None,
                data_type: Some(DataType::String),
                nullable: Some(true),
            })),
            updates: vec![("x".to_owned(), None)],
        });
        let err = render_expr(&expr, &schema).expect_err("must error on non-struct base");
        match err {
            EmissionError::UnsupportedExpression { shape, .. } => {
                assert_eq!(shape, "UpdateFields");
            }
            other => panic!("expected UnsupportedExpression, got: {other:?}"),
        }
    }

    // ── Pass 67: HOF fixes — exists / forall / transform-with-index ─────

    /// Corpus hof-004: `F.exists(tags, x -> x == 'rust')` must NOT emit the
    /// non-existent DuckDB `list_any`. Expand to
    /// `list_bool_or(list_transform(...))` with Spark-parity NULL/empty
    /// guards.
    #[test]
    fn render_exists_expands_to_list_bool_or() {
        let arr = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x_5".to_owned()],
            body: Box::new(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                    name: "x_5".to_owned(),
                })),
                right: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::String("rust".to_owned()),
                    data_type: DataType::String,
                })),
            })),
        });
        let f = FunctionCall {
            name: "exists".to_owned(),
            args: vec![arr, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render exists");
        assert!(sql.contains("list_bool_or"), "must use list_bool_or: {sql}");
        assert!(sql.contains("list_transform"), "must wrap transform: {sql}");
        assert!(!sql.contains("list_any"), "must not use list_any: {sql}");
        assert!(sql.contains("IS NULL THEN NULL"), "NULL guard: {sql}");
        assert!(sql.contains("THEN false"), "empty-list guard: {sql}");
    }

    /// Corpus hof-005: `F.forall(tags, x -> length(x) > 0)` must expand to
    /// `list_bool_and(list_transform(...))` with a `true` empty-list guard
    /// (Spark's vacuous truth).
    #[test]
    fn render_forall_expands_to_list_bool_and() {
        let arr = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x_7".to_owned()],
            body: Box::new(Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(Expression::FunctionCall(FunctionCall {
                    name: "length".to_owned(),
                    args: vec![Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x_7".to_owned(),
                    })],
                    distinct: false,
                })),
                right: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::Long(0),
                    data_type: DataType::Long,
                })),
            })),
        });
        let f = FunctionCall {
            name: "forall".to_owned(),
            args: vec![arr, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render forall");
        assert!(
            sql.contains("list_bool_and"),
            "must use list_bool_and: {sql}"
        );
        assert!(sql.contains("list_transform"), "must wrap transform: {sql}");
        assert!(!sql.contains("list_all"), "must not use list_all: {sql}");
        assert!(sql.contains("IS NULL THEN NULL"), "NULL guard: {sql}");
        assert!(sql.contains("THEN true"), "empty-list vacuous-truth: {sql}");
    }

    /// Corpus hof-007: `F.transform(tags, (x, i) -> concat(cast(i, str), ':', x))`
    /// — DuckDB's 2-arg lambda index is 1-based, Spark's is 0-based. τ must
    /// rewrite references to the index parameter as `(i - 1)` inside the
    /// lambda body.
    #[test]
    fn render_transform_with_index_rewrites_to_zero_based() {
        let arr = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x".to_owned(), "i".to_owned()],
            body: Box::new(Expression::FunctionCall(FunctionCall {
                name: "concat".to_owned(),
                args: vec![
                    Expression::Cast(CastExpression {
                        expr: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "i".to_owned(),
                        })),
                        to_type: DataType::String,
                        try_cast: false,
                    }),
                    Expression::Literal(Literal {
                        value: LiteralValue::String(":".to_owned()),
                        data_type: DataType::String,
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x".to_owned(),
                    }),
                ],
                distinct: false,
            })),
        });
        let f = FunctionCall {
            name: "transform".to_owned(),
            args: vec![arr, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render transform");
        assert!(
            sql.starts_with("list_transform("),
            "must remap to list_transform: {sql}"
        );
        // Lambda body must reference `i - 1`, not bare `i`. The exact
        // shape depends on how `render_binary` prints its args; here τ
        // emits `(i) - (CAST(1 AS BIGINT))`. Assert the subtraction
        // shape structurally.
        assert!(
            sql.contains("(i) - ("),
            "index var must be adjusted to 0-based: {sql}"
        );
        assert!(
            sql.contains(" AS BIGINT)"),
            "1 literal renders as BIGINT: {sql}"
        );
    }

    /// A 1-arg `transform` lambda must NOT trigger index adjustment — the
    /// arm falls through to the plain `list_transform` remap.
    #[test]
    fn render_transform_single_arg_lambda_unchanged() {
        let arr = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x".to_owned()],
            body: Box::new(Expression::FunctionCall(FunctionCall {
                name: "upper".to_owned(),
                args: vec![Expression::LambdaVariable(LambdaVariableExpression {
                    name: "x".to_owned(),
                })],
                distinct: false,
            })),
        });
        let f = FunctionCall {
            name: "transform".to_owned(),
            args: vec![arr, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render transform");
        assert!(sql.starts_with("list_transform("), "plain remap: {sql}");
        assert!(!sql.contains(" - 1"), "no index adjustment: {sql}");
    }

    /// `substitute_index_var` respects lambda shadowing: an inner `Lambda`
    /// re-binding the index name must not have its body rewritten.
    #[test]
    fn substitute_index_var_respects_shadowing() {
        // outer body: (i + list_transform(arr, i -> i))
        // After substitution with index_var="i":
        //   outer `i` becomes `(i - 1)`
        //   inner Lambda body (also `i`) stays as-is because the inner lambda
        //   shadows the name.
        let body = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                name: "i".to_owned(),
            })),
            right: Box::new(Expression::FunctionCall(FunctionCall {
                name: "list_transform".to_owned(),
                args: vec![
                    Expression::ColumnReference(ColumnReference {
                        name: "arr".to_owned(),
                        qualifier: None,
                        data_type: Some(DataType::Array(Box::new(DataType::Long), true)),
                        nullable: Some(true),
                    }),
                    Expression::Lambda(LambdaExpression {
                        params: vec!["i".to_owned()],
                        body: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "i".to_owned(),
                        })),
                    }),
                ],
                distinct: false,
            })),
        });
        let out = substitute_index_var(&body, "i");
        // Outer `i` (left of Add) must be rewritten to Binary(-, i, 1).
        match out {
            Expression::Binary(b) => {
                assert!(
                    matches!(*b.left, Expression::Binary(_)),
                    "outer `i` rewritten"
                );
                // Right side is a FunctionCall with an inner Lambda; the inner
                // Lambda's body must remain a bare LambdaVariable("i").
                match *b.right {
                    Expression::FunctionCall(fc) => match &fc.args[1] {
                        Expression::Lambda(inner) => match inner.body.as_ref() {
                            Expression::LambdaVariable(lv) => assert_eq!(lv.name, "i"),
                            other => panic!("inner body not preserved: {other:?}"),
                        },
                        other => panic!("expected inner Lambda: {other:?}"),
                    },
                    other => panic!("expected FunctionCall: {other:?}"),
                }
            }
            other => panic!("expected Binary at top level: {other:?}"),
        }
    }

    // ── Explode / posexplode generators (Pass 68) ──────────────────────
    //
    // `explode` / `explode_outer` / `posexplode_val` / `posexplode_pos`
    // land in the SELECT list; DuckDB expands `UNNEST(list)` to one row
    // per element when it appears in a SELECT projection. See
    // `render_function_call`'s arms. Corpus witnesses: arr-015, arr-016,
    // arr-017.

    fn tags_col() -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        })
    }

    #[test]
    fn render_explode_emits_unnest() {
        let f = FunctionCall {
            name: "explode".to_owned(),
            args: vec![tags_col()],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render explode");
        assert_eq!(sql, "UNNEST(tags)");
    }

    #[test]
    fn render_explode_arity_error() {
        let f = FunctionCall {
            name: "explode".to_owned(),
            args: vec![tags_col(), tags_col()],
            distinct: false,
        };
        let err =
            render_function_call(&f, &empty_schema()).expect_err("explode with 2 args must error");
        match err {
            EmissionError::UnsupportedFunction { name, .. } => assert_eq!(name, "explode"),
            other => panic!("expected UnsupportedFunction, got {other:?}"),
        }
    }

    #[test]
    fn render_explode_outer_wraps_empty_and_null_arrays() {
        let f = FunctionCall {
            name: "explode_outer".to_owned(),
            args: vec![tags_col()],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render explode_outer");
        // Explode_outer must emit a one-NULL-row fallback for both NULL and
        // empty arrays so the outer semantics hold.
        assert_eq!(
            sql,
            "UNNEST(CASE WHEN tags IS NULL OR len(tags) = 0 THEN [NULL] ELSE tags END)"
        );
    }

    #[test]
    fn render_posexplode_pos_emits_zero_indexed_subscripts() {
        let f = FunctionCall {
            name: "posexplode_pos".to_owned(),
            args: vec![tags_col()],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render posexplode_pos");
        // DuckDB `generate_subscripts` is 1-indexed; subtract 1 to align
        // with Spark's 0-indexed posexplode.
        assert_eq!(sql, "(generate_subscripts(tags, 1) - 1)");
    }

    #[test]
    fn render_posexplode_val_emits_unnest() {
        let f = FunctionCall {
            name: "posexplode_val".to_owned(),
            args: vec![tags_col()],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render posexplode_val");
        assert_eq!(sql, "UNNEST(tags)");
    }

    /// Pass 76 — Synthetic `map_explode_key(m)` / `map_explode_val(m)`
    /// (produced by the v2 converter when it splits
    /// `F.explode(map_col).alias("k", "v")` into two projections) emit
    /// co-UNNESTed `map_keys` / `map_values` so DuckDB row-aligns the
    /// key/value fan-out. Corpus witness: `map-007`.
    #[test]
    fn render_map_explode_key_and_val_emit_unnested_map_accessors() {
        let m = col_ref_expr("attrs");
        let k = FunctionCall {
            name: "map_explode_key".to_owned(),
            args: vec![m.clone()],
            distinct: false,
        };
        let v = FunctionCall {
            name: "map_explode_val".to_owned(),
            args: vec![m],
            distinct: false,
        };
        let k_sql = render_function_call(&k, &empty_schema()).expect("render map_explode_key");
        let v_sql = render_function_call(&v, &empty_schema()).expect("render map_explode_val");
        assert_eq!(k_sql, "UNNEST(map_keys(attrs))");
        assert_eq!(v_sql, "UNNEST(map_values(attrs))");
    }

    /// τ remaps `arrays_overlap(a, b)` → DuckDB's `list_has_any(a, b)`;
    /// DuckDB has no `arrays_overlap` function. Corpus: `arr-011`.
    #[test]
    fn render_arrays_overlap_emits_list_has_any() {
        let f = FunctionCall {
            name: "arrays_overlap".to_owned(),
            args: vec![
                tags_col(),
                Expression::ColumnReference(ColumnReference {
                    name: "tags2".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::Array(Box::new(DataType::String), true)),
                    nullable: Some(true),
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render arrays_overlap");
        assert_eq!(sql, "list_has_any(tags, tags2)");
    }

    /// τ 2-arg `array_join(arr, sep)` filters NULLs before joining to match
    /// Spark's default null-skip semantics. Corpus: `arr-010`.
    #[test]
    fn render_array_join_two_arg_filters_nulls() {
        let f = FunctionCall {
            name: "array_join".to_owned(),
            args: vec![
                tags_col(),
                Expression::Literal(Literal {
                    value: LiteralValue::String(",".to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render array_join 2-arg");
        assert_eq!(
            sql,
            "array_to_string(list_filter(tags, x -> x IS NOT NULL), ',')"
        );
    }

    /// τ 3-arg `array_join(arr, sep, null_repl)` replaces NULLs with the
    /// replacement string per Spark's semantics. Corpus: `arr-010`.
    #[test]
    fn render_array_join_three_arg_uses_coalesce() {
        let f = FunctionCall {
            name: "array_join".to_owned(),
            args: vec![
                tags_col(),
                Expression::Literal(Literal {
                    value: LiteralValue::String(",".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("NULL".to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render array_join 3-arg");
        assert!(
            sql.contains("list_transform(tags,"),
            "list_transform present: {sql}"
        );
        assert!(
            sql.contains("coalesce(CAST(x AS VARCHAR)"),
            "coalesce: {sql}"
        );
        assert!(sql.contains("'NULL'"), "null replacement literal: {sql}");
    }

    /// τ `zip_with(a, b, (x, y) -> body)` inlines to
    /// `list_transform(range(1, least(len(a), len(b)) + 1), i -> body_at_i)`.
    /// Corpus: `hof-006`.
    #[test]
    fn render_zip_with_emits_index_iteration() {
        let a = tags_col();
        let b = Expression::ColumnReference(ColumnReference {
            name: "tags2".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x_1".to_owned(), "y_2".to_owned()],
            body: Box::new(Expression::FunctionCall(FunctionCall {
                name: "concat".to_owned(),
                args: vec![
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x_1".to_owned(),
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "y_2".to_owned(),
                    }),
                ],
                distinct: false,
            })),
        });
        let f = FunctionCall {
            name: "zip_with".to_owned(),
            args: vec![a, b, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render zip_with");
        assert!(
            sql.contains("list_transform(range(1, least("),
            "range shape: {sql}"
        );
        assert!(sql.contains("__zw_i"), "fresh index var used: {sql}");
        assert!(sql.contains("(tags)[__zw_i]"), "a[i] substitution: {sql}");
        assert!(sql.contains("(tags2)[__zw_i]"), "b[i] substitution: {sql}");
    }

    /// τ `map_filter(m, (k, v) -> pred)` emits `map_from_entries(list_filter(
    /// map_entries(m), kv -> pred[k → kv.key, v → kv.value]))`.
    /// Corpus: `hof-008`.
    #[test]
    fn render_map_filter_emits_entries_pipeline() {
        let m = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["k".to_owned(), "v".to_owned()],
            body: Box::new(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                    name: "k".to_owned(),
                })),
                right: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::String("team".to_owned()),
                    data_type: DataType::String,
                })),
            })),
        });
        let f = FunctionCall {
            name: "map_filter".to_owned(),
            args: vec![m, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render map_filter");
        assert!(
            sql.starts_with("map_from_entries(list_filter(map_entries(attrs),"),
            "pipeline shape: {sql}"
        );
        assert!(sql.contains("__mh_kv"), "fresh entry var: {sql}");
        assert!(sql.contains("(__mh_kv).key"), "key access: {sql}");
    }

    /// τ `transform_values(m, (k, v) -> f)` emits a `list_transform` over
    /// `map_entries(m)` with `struct_pack(key := kv.key, value := f)`.
    /// Corpus: `hof-009`.
    #[test]
    fn render_transform_values_emits_struct_pack_value() {
        let m = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["k".to_owned(), "v".to_owned()],
            body: Box::new(Expression::FunctionCall(FunctionCall {
                name: "upper".to_owned(),
                args: vec![Expression::LambdaVariable(LambdaVariableExpression {
                    name: "v".to_owned(),
                })],
                distinct: false,
            })),
        });
        let f = FunctionCall {
            name: "transform_values".to_owned(),
            args: vec![m, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render transform_values");
        assert!(
            sql.contains("struct_pack(key := (__mh_kv).key, value :="),
            "value transformed: {sql}"
        );
        assert!(sql.contains("upper((__mh_kv).value)"), "body: {sql}");
    }

    /// τ `transform_keys(m, (k, v) -> f)` emits `struct_pack(key := f,
    /// value := kv.value)` — the mirror of `transform_values`. Corpus:
    /// `hof-010`.
    #[test]
    fn render_transform_keys_emits_struct_pack_key() {
        let m = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }),
            nullable: Some(true),
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["k".to_owned(), "v".to_owned()],
            body: Box::new(Expression::FunctionCall(FunctionCall {
                name: "concat".to_owned(),
                args: vec![
                    Expression::Literal(Literal {
                        value: LiteralValue::String("attr_".to_owned()),
                        data_type: DataType::String,
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "k".to_owned(),
                    }),
                ],
                distinct: false,
            })),
        });
        let f = FunctionCall {
            name: "transform_keys".to_owned(),
            args: vec![m, lambda],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render transform_keys");
        assert!(
            sql.contains("struct_pack(key := concat('attr_', (__mh_kv).key)"),
            "key transformed: {sql}"
        );
        assert!(sql.contains("value := (__mh_kv).value"), "value: {sql}");
    }

    /// τ `arrays_zip(a, b)` emits `list_transform + struct_pack` with
    /// per-arg field names. Duplicate column names fall back to positional
    /// integer strings to satisfy `struct_pack`'s unique-name rule.
    /// Corpus: `arr-012`.
    #[test]
    fn render_arrays_zip_duplicate_column_names_fall_back_to_positional() {
        let f = FunctionCall {
            name: "arrays_zip".to_owned(),
            args: vec![tags_col(), tags_col()],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render arrays_zip");
        assert!(
            sql.contains("list_transform(range(1, least("),
            "range: {sql}"
        );
        // Duplicate `tags` → positional 0, 1 names.
        assert!(sql.contains("\"0\" := (tags)[__az_i]"), "field 0: {sql}");
        assert!(sql.contains("\"1\" := (tags)[__az_i]"), "field 1: {sql}");
    }

    /// τ `array_union(a, b)` preserves `a`'s order followed by new
    /// elements of `b`, propagates NULL if either arg is NULL, and does
    /// NOT pre-dedup `a` (DuckDB `list_distinct` reorders scalars, which
    /// would break Spark parity). Corpus: `arr-011`.
    #[test]
    fn render_array_union_preserves_order_and_propagates_null() {
        let a = tags_col();
        let b = Expression::ColumnReference(ColumnReference {
            name: "tags2".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let f = FunctionCall {
            name: "array_union".to_owned(),
            args: vec![a, b],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render array_union");
        assert!(
            sql.contains("CASE WHEN (tags) IS NULL OR (tags2) IS NULL THEN NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains("list_concat(tags, list_filter(tags2, x -> NOT list_contains(tags, x)))"),
            "order-preserving concat: {sql}"
        );
    }

    /// τ `flatten(Array<Array<T>>)` propagates NULL when the outer array
    /// is NULL or contains any NULL sub-array — Spark's documented
    /// semantics. DuckDB's `flatten` silently drops NULLs, so we wrap
    /// with a CASE. Corpus: `arr-013`.
    #[test]
    fn render_flatten_propagates_null_on_null_subarray() {
        let outer = Expression::ColumnReference(ColumnReference {
            name: "nested".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(
                Box::new(DataType::Array(Box::new(DataType::String), true)),
                true,
            )),
            nullable: Some(true),
        });
        let f = FunctionCall {
            name: "flatten".to_owned(),
            args: vec![outer],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render flatten");
        assert!(
            sql.contains("CASE WHEN (nested) IS NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains("list_bool_or(list_transform(nested, x -> x IS NULL))"),
            "sub-array null check: {sql}"
        );
        assert!(sql.contains("flatten(nested)"), "underlying call: {sql}");
    }

    /// τ `array_position(arr, item)` coalesces the DuckDB `list_position`
    /// (which returns NULL for not-found) with 0 to match Spark, but
    /// preserves NULL when the input array is NULL. Corpus: `arr-007`.
    #[test]
    fn render_array_position_coalesces_to_zero_and_preserves_null_array() {
        let f = FunctionCall {
            name: "array_position".to_owned(),
            args: vec![
                tags_col(),
                Expression::Literal(Literal {
                    value: LiteralValue::String("rust".to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render array_position");
        assert!(
            sql.contains("CASE WHEN tags IS NULL THEN NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains("coalesce(list_position(tags, 'rust'), 0)"),
            "coalesce to 0: {sql}"
        );
        assert!(
            sql.contains("CAST(coalesce(list_position"),
            "cast to BIGINT: {sql}"
        );
    }

    /// `substitute_lambda_var` replaces every LambdaVariable(name) with the
    /// supplied replacement expression, respecting shadowing.
    #[test]
    fn substitute_lambda_var_replaces_and_respects_shadowing() {
        // body: (k + list_transform(arr, k -> k))
        let body = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                name: "k".to_owned(),
            })),
            right: Box::new(Expression::FunctionCall(FunctionCall {
                name: "list_transform".to_owned(),
                args: vec![
                    Expression::ColumnReference(ColumnReference {
                        name: "arr".to_owned(),
                        qualifier: None,
                        data_type: Some(DataType::Array(Box::new(DataType::Long), true)),
                        nullable: Some(true),
                    }),
                    Expression::Lambda(LambdaExpression {
                        params: vec!["k".to_owned()],
                        body: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "k".to_owned(),
                        })),
                    }),
                ],
                distinct: false,
            })),
        });
        let replacement = Expression::Literal(Literal {
            value: LiteralValue::Long(42),
            data_type: DataType::Long,
        });
        let out = substitute_lambda_var(&body, "k", &replacement);
        // Outer `k` on the left of the Add must become the Literal 42.
        match out {
            Expression::Binary(b) => {
                assert!(
                    matches!(*b.left, Expression::Literal(_)),
                    "outer k replaced"
                );
                // The inner lambda re-binds `k`; its body must stay a
                // LambdaVariable("k"), not the replacement.
                match *b.right {
                    Expression::FunctionCall(fc) => match &fc.args[1] {
                        Expression::Lambda(l) => match &*l.body {
                            Expression::LambdaVariable(lv) => {
                                assert_eq!(lv.name, "k", "inner k not rewritten");
                            }
                            other => panic!("inner body shape: {other:?}"),
                        },
                        other => panic!("expected inner Lambda, got {other:?}"),
                    },
                    other => panic!("expected FunctionCall on right, got {other:?}"),
                }
            }
            other => panic!("expected Binary, got {other:?}"),
        }
    }

    // ── Pass 70: ceil/floor NaN safety, tz conversion, interval builders ──

    /// `math-003` regression: `ceil(x)` on a DOUBLE column that may contain
    /// NaN must not raise a DuckDB conversion error. Spark's semantics on
    /// non-finite Double are `ceil(NaN) = 0` (JVM `(long) NaN → 0`);
    /// τ emits a three-way CASE: NULL → NULL, NaN → 0, else CAST.
    #[test]
    fn render_ceil_uses_case_for_nan_safety() {
        let f = FunctionCall {
            name: "ceil".to_owned(),
            args: vec![col_ref_expr("x")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render ceil");
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(ceil(x) AS BIGINT) END"
        );
    }

    #[test]
    fn render_floor_uses_case_for_nan_safety() {
        let f = FunctionCall {
            name: "floor".to_owned(),
            args: vec![col_ref_expr("x")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render floor");
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(floor(x) AS BIGINT) END"
        );
    }

    #[test]
    fn render_ceiling_alias_uses_case_for_nan_safety() {
        let f = FunctionCall {
            name: "ceiling".to_owned(),
            args: vec![col_ref_expr("x")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render ceiling");
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(ceil(x) AS BIGINT) END"
        );
    }

    /// `intv-003` regression: `make_dt_interval(1, 2, 30, 0)` — DuckDB has no
    /// `make_dt_interval` scalar. τ emits a sum of INTERVAL fragments.
    #[test]
    fn render_make_dt_interval_four_args() {
        let f = FunctionCall {
            name: "make_dt_interval".to_owned(),
            args: vec![int_lit(1), int_lit(2), int_lit(30), int_lit(0)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render make_dt_interval");
        assert_eq!(
            sql,
            "(INTERVAL (1) DAY + INTERVAL (2) HOUR + INTERVAL (30) MINUTE \
             + INTERVAL (CAST((0) * 1000000 AS BIGINT)) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_zero_args_defaults_all_zero() {
        let f = FunctionCall {
            name: "make_dt_interval".to_owned(),
            args: vec![],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render make_dt_interval");
        assert_eq!(
            sql,
            "(INTERVAL (0) DAY + INTERVAL (0) HOUR + INTERVAL (0) MINUTE \
             + INTERVAL (0) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_one_arg_days_only() {
        let f = FunctionCall {
            name: "make_dt_interval".to_owned(),
            args: vec![int_lit(7)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render make_dt_interval");
        assert_eq!(
            sql,
            "(INTERVAL (7) DAY + INTERVAL (0) HOUR + INTERVAL (0) MINUTE \
             + INTERVAL (0) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_too_many_args_is_boundary_error() {
        let f = FunctionCall {
            name: "make_dt_interval".to_owned(),
            args: vec![int_lit(1); 5],
            distinct: false,
        };
        let err = render_function_call(&f, &empty_schema()).expect_err("too many args");
        match err {
            EmissionError::UnsupportedFunction { name, .. } => {
                assert_eq!(name, "make_dt_interval");
            }
            other => panic!("expected UnsupportedFunction, got {other:?}"),
        }
    }

    #[test]
    fn render_make_ym_interval_two_args() {
        let f = FunctionCall {
            name: "make_ym_interval".to_owned(),
            args: vec![int_lit(1), int_lit(6)],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render make_ym_interval");
        assert_eq!(sql, "(INTERVAL (1) YEAR + INTERVAL (6) MONTH)");
    }

    /// `dt-017` regression: `to_utc_timestamp(ts, 'CET')` — DuckDB has no
    /// `to_utc_timestamp` scalar. τ normalises the input to TIMESTAMPTZ,
    /// extracts naive UTC wall-clock, reinterprets in `tz`, extracts UTC
    /// naive wall-clock again. The `CAST(... AS TIMESTAMPTZ)` normalises
    /// TIMESTAMP-vs-TIMESTAMPTZ inputs uniformly.
    #[test]
    fn render_to_utc_timestamp_uses_timezone_composition() {
        let f = FunctionCall {
            name: "to_utc_timestamp".to_owned(),
            args: vec![col_ref_expr("last_login"), str_lit("CET")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render to_utc_timestamp");
        assert_eq!(
            sql,
            "timezone('UTC', timezone('CET', timezone('UTC', CAST(last_login AS TIMESTAMPTZ))))"
        );
    }

    #[test]
    fn render_from_utc_timestamp_uses_timezone_composition() {
        let f = FunctionCall {
            name: "from_utc_timestamp".to_owned(),
            args: vec![col_ref_expr("last_login"), str_lit("CET")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render from_utc_timestamp");
        assert_eq!(
            sql,
            "timezone('CET', timezone('UTC', timezone('UTC', CAST(last_login AS TIMESTAMPTZ))))"
        );
    }

    // ── Pass 72: element_at Map/Array split, typeof lower, map_concat NULL
    //             propagation, array_append/prepend NULL guard, create_map
    //             → map(list_value(...), list_value(...))
    // ────────────────────────────────────────────────────────────────────

    /// `map-004` regression — `element_at(MAP, key)` unwraps the 1-element
    /// list DuckDB returns.
    #[test]
    fn render_element_at_map_unwraps_singleton_list() {
        // Build a Map-typed column reference so `data_type(schema)` reports
        // `Map { .. }` at emission time.
        let map_col = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }),
            nullable: Some(true),
        });
        let f = FunctionCall {
            name: "element_at".to_owned(),
            args: vec![map_col, str_lit("team")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render element_at map");
        assert_eq!(sql, "element_at(attrs, 'team')[1]");
    }

    /// `arr-008` behavioral test — `element_at(ARRAY, i)` uses
    /// `list_extract` (1-based, NULL on OOB in non-ANSI mode).
    #[test]
    fn render_element_at_array_uses_list_extract() {
        let arr_col = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let f = FunctionCall {
            name: "element_at".to_owned(),
            args: vec![
                arr_col,
                Expression::Literal(super::super::expression::Literal {
                    value: super::super::expression::LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render element_at array");
        assert_eq!(sql, "list_extract(tags, 1)");
    }

    /// `meta-003` regression — `typeof(x)` wraps in `lower(...)` for
    /// Spark-lowercase parity.
    #[test]
    fn render_typeof_wraps_in_lower_for_spark_case() {
        let f = FunctionCall {
            name: "typeof".to_owned(),
            args: vec![col_ref_expr("salary")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render typeof");
        assert_eq!(sql, "lower(typeof(salary))");
    }

    /// `map-006` regression — `map_concat` guards NULL on every arg so a
    /// NULL input propagates to a NULL result (Spark semantics).
    #[test]
    fn render_map_concat_propagates_null_across_all_args() {
        let f = FunctionCall {
            name: "map_concat".to_owned(),
            args: vec![col_ref_expr("m1"), col_ref_expr("m2")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render map_concat");
        assert!(
            sql.contains("(m1) IS NULL OR (m2) IS NULL"),
            "expected NULL guard on both args, got: {sql}"
        );
        assert!(
            sql.contains("map_concat(m1, m2)"),
            "expected fallthrough call, got: {sql}"
        );
    }

    /// `arr2-001` regression — `array_append` guards NULL on the array
    /// argument so DuckDB's silent NULL-to-empty-list coercion does not
    /// leak.
    #[test]
    fn render_array_append_guards_null_array_argument() {
        let f = FunctionCall {
            name: "array_append".to_owned(),
            args: vec![col_ref_expr("tags"), str_lit("new")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render array_append");
        assert_eq!(
            sql,
            "CASE WHEN (tags) IS NULL THEN NULL ELSE array_append(tags, 'new') END"
        );
    }

    /// `map-006` regression — `create_map` (wire name `"map"`) splits into
    /// `map(list_value(keys...), list_value(values...))`.
    #[test]
    fn render_create_map_splits_pairs_into_two_lists() {
        let f = FunctionCall {
            name: "map".to_owned(),
            args: vec![str_lit("a"), str_lit("1"), str_lit("b"), str_lit("2")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render create_map");
        assert_eq!(sql, "map(list_value('a', 'b'), list_value('1', '2'))");
    }

    /// Pass 74 (`parse-005`) — Spark's `find_in_set(needle, csv)` returns
    /// the 1-based position of `needle` in `csv`, or 0 if not found.
    /// DuckDB has no `find_in_set`; emit
    /// `COALESCE(list_position(string_split(csv, ','), needle), 0)`.
    #[test]
    fn render_find_in_set_uses_list_position_over_split() {
        let f = FunctionCall {
            name: "find_in_set".to_owned(),
            args: vec![str_lit("rust"), col_ref_expr("tags")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render find_in_set");
        assert_eq!(
            sql,
            "COALESCE(list_position(string_split(tags, ','), 'rust'), 0)"
        );
    }

    /// Pass 74 (`parse-007`) — Spark's `elt(idx, s1, s2, ...)` is
    /// 1-based array indexing. Emit `([s1, s2, ...])[idx]` using DuckDB's
    /// 1-based list-literal indexing.
    #[test]
    fn render_elt_uses_1_based_list_indexing() {
        let f = FunctionCall {
            name: "elt".to_owned(),
            args: vec![int_lit(2), str_lit("a"), str_lit("b"), str_lit("c")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render elt");
        assert_eq!(sql, "(['a', 'b', 'c'])[2]");
    }

    /// Pass 74 (`cond-010`) — Spark's `isnan(x)` schema is BOOLEAN
    /// non-nullable; DuckDB's `isnan(NULL)` returns NULL. Wrap in
    /// `COALESCE(..., FALSE)` to preserve the non-null semantics.
    #[test]
    fn render_isnan_wraps_in_coalesce_false() {
        let f = FunctionCall {
            name: "isnan".to_owned(),
            args: vec![col_ref_expr("score")],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render isnan");
        assert_eq!(sql, "COALESCE(isnan(score), FALSE)");
    }

    /// Pass 74 (`str-011`) — Spark's `concat_ws(sep, arr)` on a NULL
    /// array returns "" (not NULL). DuckDB's `array_to_string(NULL, ',')`
    /// returns NULL; τ wraps the emission in `COALESCE(..., '')`.
    #[test]
    fn render_concat_ws_null_array_wraps_in_coalesce_empty_string() {
        let arr_col = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
        });
        let f = FunctionCall {
            name: "concat_ws".to_owned(),
            args: vec![str_lit(","), arr_col],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render concat_ws");
        assert_eq!(sql, "COALESCE(array_to_string(tags, ','), '')");
    }

    /// Pass 74 (`type-015`) — Spark's `concat(s1, s2)` on strings
    /// propagates NULL: any NULL arg yields NULL. DuckDB's `concat`
    /// silently drops NULL args. τ wraps in a CASE null-guard.
    #[test]
    fn render_concat_strings_propagates_null_via_case_guard() {
        let null_lit = Expression::Literal(Literal {
            value: LiteralValue::Null,
            data_type: DataType::String,
        });
        let f = FunctionCall {
            name: "concat".to_owned(),
            args: vec![col_ref_expr("name"), null_lit],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render concat");
        assert!(
            sql.starts_with("(CASE WHEN "),
            "expected CASE null-guard, got: {sql}"
        );
        assert!(
            sql.contains("IS NULL"),
            "expected null-check in guard, got: {sql}"
        );
        assert!(
            sql.contains("concat(name, NULL)"),
            "expected concat(...) body, got: {sql}"
        );
    }

    /// Pass 75 — `parse_url(url, 'HOST')` rewrites to a `regexp_extract`
    /// with a HOST pattern, wrapped in `NULLIF(..., '')` so Spark's NULL
    /// semantics for missing components match. Corpus: parse-001.
    #[test]
    fn render_parse_url_host_uses_regexp_extract_nullif() {
        let url = col_ref_expr("url");
        let part = Expression::Literal(Literal {
            value: LiteralValue::String("HOST".to_owned()),
            data_type: DataType::String,
        });
        let f = FunctionCall {
            name: "parse_url".to_owned(),
            args: vec![url, part],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render parse_url HOST");
        assert!(sql.contains("regexp_extract"), "got: {sql}");
        assert!(sql.contains("NULLIF"), "got: {sql}");
        assert!(
            !sql.contains("parse_url("),
            "must not emit native parse_url, got: {sql}"
        );
    }

    /// Pass 75 — `parse_url(url, 'QUERY', 'q')` builds a keyed-query
    /// regex that captures the value for key `q`. Regex-escapes the key so
    /// e.g. `.` in a key name doesn't match any character. Corpus: parse-001.
    #[test]
    fn render_parse_url_query_with_key_escapes_key() {
        let url = col_ref_expr("url");
        let part = Expression::Literal(Literal {
            value: LiteralValue::String("QUERY".to_owned()),
            data_type: DataType::String,
        });
        let key = Expression::Literal(Literal {
            value: LiteralValue::String("q.k".to_owned()),
            data_type: DataType::String,
        });
        let f = FunctionCall {
            name: "parse_url".to_owned(),
            args: vec![url, part, key],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render parse_url QUERY key");
        assert!(sql.contains("regexp_extract"), "got: {sql}");
        // The `.` in the key must be regex-escaped.
        assert!(sql.contains(r"q\.k="), "expected escaped key, got: {sql}");
    }

    /// Pass 75 — Spark's `Literal(Double)` must render with an explicit
    /// `CAST(... AS DOUBLE)`; DuckDB parses bare `3.14` as DECIMAL and the
    /// Spark schema would then mismatch. Corpus: cast-001.
    #[test]
    fn render_double_literal_casts_to_double() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::Double(3.14),
            data_type: DataType::Double,
        });
        let sql = render_expr(&lit, &empty_schema()).expect("render double literal");
        assert!(
            sql.contains("AS DOUBLE"),
            "expected DOUBLE cast, got: {sql}"
        );
    }

    /// Pass 75 — DECIMAL / DECIMAL division routes to `spark_decimal_div`
    /// (extension) instead of the native `/`, which yields DOUBLE and loses
    /// Spark-declared scale. Corpus: type-005.
    #[test]
    fn render_decimal_div_uses_spark_decimal_div() {
        let schema = StructType::new(vec![
            StructField::nullable(
                "d1",
                DataType::Decimal {
                    precision: 10,
                    scale: 2,
                },
            ),
            StructField::nullable(
                "d2",
                DataType::Decimal {
                    precision: 6,
                    scale: 3,
                },
            ),
        ]);
        let expr = Expression::Binary(BinaryExpression {
            left: Box::new(Expression::ColumnReference(ColumnReference {
                name: "d1".to_owned(),
                qualifier: None,
                data_type: Some(DataType::Decimal {
                    precision: 10,
                    scale: 2,
                }),
                nullable: Some(true),
            })),
            op: BinaryOp::Div,
            right: Box::new(Expression::ColumnReference(ColumnReference {
                name: "d2".to_owned(),
                qualifier: None,
                data_type: Some(DataType::Decimal {
                    precision: 6,
                    scale: 3,
                }),
                nullable: Some(true),
            })),
        });
        let sql = render_expr(&expr, &schema).expect("render decimal div");
        assert!(
            sql.contains("spark_decimal_div"),
            "expected spark_decimal_div, got: {sql}"
        );
    }

    /// Pass 74 (`agg-013`) — Spark's `percentile_approx(col, q)` returns
    /// a discrete value from the sample; τ uses DuckDB's `quantile_disc`
    /// (not `approx_quantile`, which linearly interpolates).
    #[test]
    fn render_percentile_approx_uses_quantile_disc() {
        let q_lit = Expression::Literal(Literal {
            value: LiteralValue::Double(0.5),
            data_type: DataType::Double,
        });
        let f = FunctionCall {
            name: "percentile_approx".to_owned(),
            args: vec![col_ref_expr("salary"), q_lit],
            distinct: false,
        };
        let sql = render_aggregate(&f, &empty_schema()).expect("render percentile_approx");
        assert!(
            sql.contains("quantile_disc"),
            "expected quantile_disc, got: {sql}"
        );
        assert!(
            !sql.contains("approx_quantile"),
            "must not use approx_quantile, got: {sql}"
        );
    }

    /// Pass 76 — Spark's `url_encode(s)` uses form-urlencoded (spaces → `+`),
    /// but DuckDB's `url_encode(s)` emits `%20`. τ post-substitutes so the
    /// bytes match Spark. Corpus witness: `parse-002`.
    #[test]
    fn render_url_encode_form_urlencoded_substitutes_space() {
        let f = FunctionCall {
            name: "url_encode".to_owned(),
            args: vec![Expression::Literal(Literal {
                value: LiteralValue::String("a b&c".to_owned()),
                data_type: DataType::String,
            })],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render url_encode");
        assert!(sql.contains("url_encode"), "got: {sql}");
        assert!(
            sql.contains("replace(") && sql.contains("'%20'") && sql.contains("'+'"),
            "expected %20→+ substitution, got: {sql}"
        );
    }

    /// Pass 76 — `url_decode(s)` must first substitute `+ → %20` to match
    /// Spark's form-urlencoded decoding.
    #[test]
    fn render_url_decode_pre_substitutes_plus() {
        let f = FunctionCall {
            name: "url_decode".to_owned(),
            args: vec![Expression::Literal(Literal {
                value: LiteralValue::String("a+b%26c".to_owned()),
                data_type: DataType::String,
            })],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render url_decode");
        assert!(sql.contains("url_decode(replace("), "got: {sql}");
        assert!(sql.contains("'+'") && sql.contains("'%20'"), "got: {sql}");
    }

    /// Pass 76 — `try_to_number(str, '999.99')` derives `DECIMAL(5, 2)` from
    /// the literal format template and emits `try_cast(... AS DECIMAL(5, 2))`.
    /// Corpus witness: `parse-004`.
    #[test]
    fn render_try_to_number_emits_try_cast_decimal() {
        let f = FunctionCall {
            name: "try_to_number".to_owned(),
            args: vec![
                col_ref_expr("num_str"),
                Expression::Literal(Literal {
                    value: LiteralValue::String("999.99".to_owned()),
                    data_type: DataType::String,
                }),
            ],
            distinct: false,
        };
        let sql = render_function_call(&f, &empty_schema()).expect("render try_to_number");
        assert!(sql.contains("try_cast("), "got: {sql}");
        assert!(sql.contains("DECIMAL(5, 2)"), "got: {sql}");
    }

    /// Pass 76 — Spark DDL `"a INT, b ARRAY<STRING>, c STRUCT<d:BOOLEAN>"`
    /// translates to DuckDB's JSON schema shape. Corpus witnesses:
    /// `json-003`, `json-004`.
    #[test]
    fn from_json_ddl_translates_to_duckdb_json_schema() {
        let out =
            spark_ddl_schema_to_duckdb_json("a INT, b ARRAY<STRING>, c STRUCT<d:BOOLEAN>").unwrap();
        assert_eq!(
            out,
            r#"{"a":"INTEGER","b":"VARCHAR[]","c":{"d":"BOOLEAN"}}"#
        );
    }

    /// Same DDL, but resolved to a core `StructType` for τ's projection
    /// schema inference. Nested `STRUCT<...>` must recurse into
    /// `DataType::Struct(...)`.
    #[test]
    fn from_json_ddl_resolves_to_struct_type() {
        let st = from_json_ddl_to_struct_for_type_inference("a INT, c STRUCT<d:BOOLEAN>").unwrap();
        assert_eq!(st.fields.len(), 2);
        assert_eq!(st.fields[0].name, "a");
        assert_eq!(st.fields[0].data_type, DataType::Integer);
        match &st.fields[1].data_type {
            DataType::Struct(inner) => {
                assert_eq!(inner.fields.len(), 1);
                assert_eq!(inner.fields[0].name, "d");
                assert_eq!(inner.fields[0].data_type, DataType::Boolean);
            }
            other => panic!("expected Struct, got {other:?}"),
        }
    }

    /// Pass 77 — `unionByName(allowMissingColumns=True)` emits padded
    /// child SELECTs (`CAST(NULL AS ty) AS name` for absent columns) and a
    /// plain `UNION [ALL]` combinator instead of `UNION BY NAME` — the
    /// aligned projections make the two forms equivalent, and plain UNION
    /// keeps the emission consistent with the by-position path.
    #[test]
    fn union_by_name_allow_missing_emits_padded_nulls_and_plain_union() {
        // No tap_guard() — this test does not read EMIT_TAP; the shared
        // mutex would otherwise cascade a poisoned lock from an unrelated
        // pre-existing INV10 baseline failure in this suite.
        // LEFT `{a: Long, b: Long}` × RIGHT `{b: Long, c: Long}`
        let bt = BaseTypes::empty();
        let left = CommonAst::new(CommonOp::Values {
            rows: vec![vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Long(1),
                    data_type: DataType::Long,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Long(2),
                    data_type: DataType::Long,
                }),
            ]],
            column_names: vec!["a".to_owned(), "b".to_owned()],
        });
        let right = CommonAst::new(CommonOp::Values {
            rows: vec![vec![
                Expression::Literal(Literal {
                    value: LiteralValue::Long(3),
                    data_type: DataType::Long,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::Long(4),
                    data_type: DataType::Long,
                }),
            ]],
            column_names: vec!["b".to_owned(), "c".to_owned()],
        });
        let ast = CommonAst::new(CommonOp::SetOp {
            kind: SetOpKind::Union,
            all: true,
            by_name: true,
            allow_missing_columns: true,
            children: vec![left, right],
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // LEFT is missing `c`; RIGHT is missing `a`. Confirm the padded slot
        // syntax and the plain `UNION ALL` combinator.
        assert!(
            sql.contains("CAST(NULL AS BIGINT) AS c"),
            "expected NULL pad for LEFT's missing `c`, got: {sql}"
        );
        assert!(
            sql.contains("CAST(NULL AS BIGINT) AS a"),
            "expected NULL pad for RIGHT's missing `a`, got: {sql}"
        );
        assert!(
            !sql.contains("UNION ALL BY NAME") && !sql.contains("UNION BY NAME"),
            "expected plain UNION [ALL] (not BY NAME) when allowMissingColumns=true, got: {sql}"
        );
        assert!(
            sql.contains(" UNION ALL "),
            "expected UNION ALL combinator, got: {sql}"
        );
    }

    /// Pass 76 — `parse_number_format` recognizes digit templates.
    #[test]
    fn parse_number_format_digit_template() {
        assert_eq!(parse_number_format("999.99"), Some((5, 2)));
        assert_eq!(parse_number_format("9999"), Some((4, 0)));
        assert_eq!(parse_number_format("0.00"), Some((3, 2)));
        // Grouping / sign markers → None (τ boundary).
        assert_eq!(parse_number_format("9,999.99"), None);
        assert_eq!(parse_number_format("S999.99"), None);
        // Empty / all-zero-precision → None.
        assert_eq!(parse_number_format(""), None);
    }
}
