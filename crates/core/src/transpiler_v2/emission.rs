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
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

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
        TypedOp::WithColumns { input, assignments } => {
            render_with_columns(input, assignments)
        }
        TypedOp::DropColumns { input, drop_names } => {
            render_drop_columns(input, drop_names)
        }
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
            children,
            widened_schema,
        } => render_set_op(*kind, *all, *by_name, children, widened_schema),

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
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    let slots_sql = if projections.is_empty() {
        "*".to_owned()
    } else {
        let mut buf = String::new();
        for (i, p) in projections.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            buf.push_str(&render_projection_slot(p, input_schema)?);
        }
        buf
    };
    Ok(format!(
        "SELECT {slots_sql} FROM ({child_sql}) AS __td_proj"
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
    let op_kw = match (kind, all, by_name) {
        // BY NAME variants (DuckDB supports UNION [ALL] BY NAME only).
        (SetOpKind::Union, true, true) => "UNION ALL BY NAME",
        (SetOpKind::Union, false, true) => "UNION BY NAME",
        (SetOpKind::Union, true, false) => "UNION ALL",
        (SetOpKind::Union, false, false) => "UNION",
        (SetOpKind::Intersect, true, false) => "INTERSECT ALL",
        (SetOpKind::Intersect, false, false) => "INTERSECT",
        (SetOpKind::Except, true, false) => "EXCEPT ALL",
        (SetOpKind::Except, false, false) => "EXCEPT",
        (kind, _, true) => {
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
        //   - by-name: children have identical name SETS but possibly
        //     different orders (analyzer verified). For each name in the
        //     widened schema, find the child's matching field, CAST to the
        //     widened type, keep the widened name. DuckDB's `UNION BY NAME`
        //     matches on the aliased output name — so keeping the widened
        //     name across children is what makes the union coherent.
        let mut slots = String::new();
        if by_name {
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
    // analyzer's declared order once the SEMI/ANTI restriction or extra
    // implicit resolutions kick in — the arrow batch then decodes with
    // Spark's expected schema and hits type-mismatch failures downstream.
    let is_semi_or_anti = matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti);
    let using_lower: std::collections::HashSet<String> = using_columns
        .iter()
        .map(|s| s.to_lowercase())
        .collect();
    let mut slots = String::new();
    let mut first = true;
    let push = |slots: &mut String, first: &mut bool, s: String| {
        if !*first {
            slots.push_str(", ");
        }
        *first = false;
        slots.push_str(&s);
    };
    // USING columns first (Spark hoists them).
    for c in using_columns {
        push(&mut slots, &mut first, quote_ident(c).into_owned());
    }
    // Left's non-USING columns in declared order.
    for f in &left.resolved_schema.fields {
        if !using_lower.contains(&f.name.to_lowercase()) {
            let qualified = format!("__td_jl.{}", quote_ident(&f.name));
            push(&mut slots, &mut first, qualified);
        }
    }
    // Right's non-USING columns — only when NOT semi/anti (which suppresses
    // right side).
    if !is_semi_or_anti {
        for f in &right.resolved_schema.fields {
            if !using_lower.contains(&f.name.to_lowercase()) {
                let qualified = format!("__td_jr.{}", quote_ident(&f.name));
                push(&mut slots, &mut first, qualified);
            }
        }
    }
    if slots.is_empty() {
        // Fallback for SEMI/ANTI on identical USING columns only.
        slots.push('*');
    }
    Ok(format!(
        "SELECT {slots} FROM ({left_sql}) AS __td_jl {kind} ({right_sql}) AS __td_jr{clause}"
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
    Ok(format!(
        "SELECT {slots} FROM ({child_sql}) AS __td_nafill"
    ))
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
        input_schema.fields.iter().map(|f| f.name.as_str()).collect()
    } else {
        cols.iter().map(|s| s.as_str()).collect()
    };
    if subset.is_empty() {
        return Ok(format!(
            "SELECT * FROM ({child_sql}) AS __td_nadrop"
        ));
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

fn render_deduplicate(
    input: &TypedAst,
    on_columns: &[String],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    if on_columns.is_empty() {
        Ok(format!("SELECT DISTINCT * FROM ({child_sql}) AS __td_dedup"))
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
        Expression::Window(_) => Err(EmissionError::UnsupportedExpression {
            shape: "Window".to_owned(),
            reason: "window functions land in Slice G.windows".to_owned(),
        }),
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
        Expression::Lambda(_) => Err(EmissionError::UnsupportedExpression {
            shape: "Lambda".to_owned(),
            reason: "HOF lambdas land in Slice F".to_owned(),
        }),
        Expression::LambdaVariable(_) => Err(EmissionError::UnsupportedExpression {
            shape: "LambdaVariable".to_owned(),
            reason: "HOF lambdas land in Slice F".to_owned(),
        }),
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
        Expression::UpdateFields(_) => Err(EmissionError::UnsupportedExpression {
            shape: "UpdateFields".to_owned(),
            reason: "complex-type emission lands in Slice F".to_owned(),
        }),
    }
}

fn is_aggregate_name(name: &str) -> bool {
    // `AGGREGATE_NAMES` (in `type_inference.rs`) is all-lowercase ASCII per
    // Slice B; case-insensitive byte comparison matches without allocating the
    // per-call lowercased `String` this function used to build.
    AGGREGATE_NAMES.iter().any(|n| n.eq_ignore_ascii_case(name))
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
fn render_function_call(f: &FunctionCall, schema: &Schema) -> Result<String, EmissionError> {
    let name_lower = f.name.to_ascii_lowercase();
    // Aggregate-name overlap check — if the analyzer classified a FunctionCall
    // as aggregate, `render_expr` routes to `render_aggregate` before this
    // function; anything reaching here is scalar by construction. Defense in
    // depth: any name matching AGGREGATE_NAMES should never be seen here.
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
        "map" => "map",
        // Spark's `struct(a, b, c)` maps to DuckDB's `struct_pack` (which
        // requires named args) — but Spark inline-struct is unnamed. For
        // now emit `row(a, b, c)` which DuckDB understands as an anonymous
        // struct.
        "struct" => "row",
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
            return Ok(format!(
                "CASE WHEN {a} IS NOT NULL THEN {b} ELSE {c} END"
            ));
        }
        "ifnull" => "coalesce",
        // Spark's `unix_timestamp(x)` → seconds-since-epoch. DuckDB uses
        // `epoch(x)`.
        "unix_timestamp" => "epoch",
        // Spark's `startswith`/`endswith`/`contains` — DuckDB spells them
        // `starts_with`/`ends_with`/`contains` (contains is fine, others
        // need underscore).
        "startswith" => "starts_with",
        "endswith" => "ends_with",
        // Spark's `substr` — DuckDB canonical form is `substring` (both
        // spellings accepted actually, but standardize).
        "substr" => "substring",
        // Spark array/list remaps — DuckDB uses `list_*` prefix.
        "sort_array" => "list_sort",
        "slice" => "list_slice",
        "array_contains" => "list_contains",
        "array_distinct" => "list_distinct",
        "array_intersect" => "list_intersect",
        "array_union" => "list_concat_unique",
        "array_except" => "list_filter",
        "array_position" => "list_position",
        "array_max" => "list_max",
        "array_min" => "list_min",
        "array_join" => "list_string_agg",
        "arrays_zip" => "list_zip",
        "size" | "cardinality" => "len",
        // Spark's `to_date(x)` (single-arg) → simple cast to DATE.
        "to_date" => {
            if f.args.len() != 1 {
                // Two-arg form (with format) — leave to per-case follow-up.
                return Err(EmissionError::UnsupportedFunction {
                    name: f.name.clone(),
                    reason: "`to_date` with format arg not yet supported".to_owned(),
                });
            }
            let x = render_expr(&f.args[0], schema)?;
            return Ok(format!("CAST({x} AS DATE)"));
        }
        // Spark's `date_add(date, n)` / `date_sub(date, n)` — DuckDB's
        // versions expect INTERVAL args. Rewrite to arithmetic form.
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
            return Ok(format!("datediff('month', {b}, {a})"));
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
        // Spark's `overlay(str, replacement, position[, length])` maps to
        // the SQL-standard `OVERLAY(str PLACING replacement FROM position
        // [FOR length])` keyword form.
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
            if f.args.len() == 4 {
                let l = render_expr(&f.args[3], schema)?;
                return Ok(format!("OVERLAY({s} PLACING {r} FROM {p} FOR {l})"));
            } else {
                return Ok(format!("OVERLAY({s} PLACING {r} FROM {p})"));
            }
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
    let (duck_name, force_distinct) = match lower.as_str() {
        // Direct pass-through — DuckDB accepts the Spark name unchanged.
        "count" | "sum" | "avg" | "mean" | "min" | "max" | "first" | "last"
        | "first_value" | "last_value" | "any_value" | "approx_count_distinct"
        | "stddev" | "stddev_samp" | "stddev_pop" | "variance" | "var_samp"
        | "var_pop" | "bit_and" | "bit_or" | "bit_xor" | "bool_and" | "bool_or" => {
            (lower.as_str(), false)
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
    if f.args.is_empty() {
        // `count()` with no args (illegal in Spark) — Thunderduck-boundary.
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
            reason: "GROUPING SETS emission requires set-membership metadata; Slice G territory".to_owned(),
        });
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;
    // Aggregates may include folded grouping columns at the SparkSQL path
    // (see `CommonOp::Aggregate` doc). For the DataFrame path, aggregates
    // are pure aggregate calls; grouping carries the keys. Both cases emit
    // identically: SELECT the full `aggregates` list; the GROUP BY clause
    // uses `grouping` when present.
    let mut slots = String::new();
    for (i, agg) in aggregates.iter().enumerate() {
        if i > 0 {
            slots.push_str(", ");
        }
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
        LiteralValue::Double(v) => Ok(format_float(*v)),
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
        LiteralValue::TimestampNtz(micros) => Ok(format!(
            "make_timestamp(CAST({micros} AS BIGINT))"
        )),
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
    if c.try_cast {
        Ok(format!("TRY_CAST({inner} AS {ty})"))
    } else {
        Ok(format!("CAST({inner} AS {ty})"))
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
        let key_escaped = name.replace('\'', "''");
        buf.push('\'');
        buf.push_str(&key_escaped);
        buf.push_str("': ");
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
        LikeExpression, Literal, LiteralValue, StarExpression, UnaryExpression, UnaryOp,
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
}
