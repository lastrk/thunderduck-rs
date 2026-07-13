//! τ's emission substrate.
//!
//! ADR-009 (Approach A hand-written match arms, permanent per Open Decision 7),
//! ADR-021 (τ owns substrate), ADR-022 (τ is the only path; two error
//! categories).
//!
//! **INV3 grep barrier:** no imports from the retired v1 modules
//! (generator / functions / logical / parser) are permitted inside this
//! file. The modules were deleted 2026-07-05; the barrier prevents
//! re-introduction. See `inv3_no_forbidden_use_in_emission` for the check.
//!
//! **INV10:** imports only τ-internal modules + `crate::types::{DataType,
//! StructField, StructType}`.
//!
//! # What lives here
//!
//! - [`dispatch_op`] — the single top-level operator dispatcher: it renders
//!   [`build_unit`]'s [`SqlUnit`] and increments the [`EMIT_TAP`] counter
//!   once per `Ok` (§5.3).
//! - [`build_unit`] — one hand-written match arm per [`TypedOp`] variant.
//!   Block-composable operators build/merge a `sql_block::SelectBlock`
//!   (merge when the clause ordinal and alias-visibility preconditions
//!   hold, wrap under `__td_sub` on slot conflict — see `sql_block.rs`);
//!   the analyzer's per-node `RelScope` stamp is the scope authority.
//!   Self-contained generators (Values, FileScan, Pivot, Sample,
//!   RecursiveCte, …) render via [`legacy_render`] into opaque `Raw` units.
//! - [`render_expr`] — exhaustive match over the [`Expression`] enum.
//! - [`render_cast`] — includes the `try_cast` → `TRY_CAST(...)` branch
//!   (checklist §4.2 first item).
//! - [`quote_ident`] — `Cow`-returning fast path (§5.6).
//! - The one still-unwired helper under Decision 13-A (`render_tail`) —
//!   private, marked `#[allow(dead_code)]`, kept for its §5.4 CTE anchor test.
//! - [`spark_return_cast`] (§5.1) and `spark_aggregate_return_cast` (§5.1,
//!   `#[allow(dead_code)]` — wired by C.3) — two distinct `fn` items.

use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

use super::analyzer::{
    na_fill_value_for, with_columns_plan, Schema, TypedAst, TypedOp, TD_JOIN_LEFT, TD_JOIN_RIGHT,
};
use super::ast::FileFormat;
use super::error::{EmissionError, UnsupportedKind};
use super::expression::{
    int_literal_value, AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression,
    CastExpression, ColumnReference, Expression, FunctionCall, IntervalExpression, Literal,
    LiteralValue, NullOrdering, SortDirection, SortOrder, StarExpression, SubqueryPlan,
    UnaryExpression, UnaryOp,
};
use super::sql_block::{Clause, DefaultSlot, DistinctKind, FromItem, SelectBlock, SqlUnit};
use super::type_inference::{is_aggregate_classifier_name, TypeInferenceEngine};
use crate::types::pyspark_parity::uniquify;
use crate::types::{DataType, StructType};
use crate::{bail_boundary_expr, bail_boundary_fn, bail_boundary_op};

// ── INV2 companion (§5.3) ────────────────────────────────────────────────────

/// Monotonic counter — incremented once per successful SQL string returned by
/// [`dispatch_op`]. τ's emission substrate activates INV2 via
/// `invariants::inv2_dispatch_is_only_sql_writer`.
pub(crate) static EMIT_TAP: AtomicU64 = AtomicU64::new(0);

/// Serializes tests that read / reset [`EMIT_TAP`] (parallel-test flake guard).
///
/// Referenced by `invariants::inv2_dispatch_is_only_sql_writer` and by
/// `emission::tests`; the release build has no consumer, hence
/// `#[allow(dead_code)]`. This is the INV2 (EMIT_TAP companion) tap
/// serializer — see rearchitect ADR-009 (Approach A dispatch shape) and
/// `crates/core/src/transpiler_v2/invariants.rs::inv2_emit_tap_present`.
#[allow(dead_code)] // INV2 companion (rearchitect ADR-009 test tap); release build has no consumer.
pub(crate) static EMIT_TAP_MUTEX: Mutex<()> = Mutex::new(());

// ── Dispatch (Approach A — hand-written match) ───────────────────────────────

/// Top-level dispatch. **INV2 companion:** this function is the ONLY writer to
/// [`EMIT_TAP`]. Every `Ok` return path increments [`EMIT_TAP`] exactly once.
///
/// One hand-written match arm per [`TypedOp`] variant. No table interpreter.
pub fn dispatch_op(op: &TypedOp, schema: &Schema) -> Result<String, EmissionError> {
    let result = build_unit(op, schema).map(|unit| unit.to_sql());
    if result.is_ok() {
        EMIT_TAP.fetch_add(1, Ordering::Relaxed);
    }
    result
}

/// Build the [`SqlUnit`] for `op`: an open [`SelectBlock`] for the
/// block-composable operators, or a `Raw` string from the legacy renderers
/// for everything not yet converted. This is the per-operator merge/wrap
/// decision site of the SELECT-block builder (see `sql_block.rs`).
fn build_unit(op: &TypedOp, schema: &Schema) -> Result<SqlUnit, EmissionError> {
    match op {
        TypedOp::TableScan { table, alias } => {
            Ok(SqlUnit::from(SelectBlock::from_item(FromItem::Relation {
                base: table.clone(),
                alias: alias.clone(),
            })))
        }
        TypedOp::Values { rows, column_names } => build_values(rows, column_names, schema),
        TypedOp::LocalRelation { schema: s, rows } => build_local_relation(s, rows),
        TypedOp::FileScan {
            format,
            paths,
            options,
            ..
        } => build_file_scan(*format, paths, options),
        TypedOp::TableFunction {
            name,
            args,
            with_ordinality,
        } => build_table_function(name, args, *with_ordinality, schema),
        TypedOp::Project { input, projections } => build_project(input, projections),
        TypedOp::Filter { input, condition } => build_filter(input, condition),
        TypedOp::Sort {
            input,
            order,
            limit,
            offset,
        } => build_sort(input, order, *limit, *offset),
        TypedOp::Limit {
            input,
            limit,
            offset,
        } => build_limit(input, *limit, *offset),
        TypedOp::Deduplicate { input, on_columns } => build_deduplicate(input, on_columns),
        TypedOp::AliasedRelation { input, alias } => build_aliased_relation(input, alias),
        TypedOp::Join {
            left,
            right,
            join_type,
            condition,
            using_columns,
            lateral,
            ..
        } => build_join(JoinParts {
            left,
            right,
            join_type: *join_type,
            condition: condition.as_ref(),
            using_columns,
            lateral: *lateral,
        }),
        TypedOp::Aggregate {
            input,
            grouping,
            aggregates,
            grouping_kind,
            grouping_sets,
            having,
        } => build_aggregate(
            input,
            grouping,
            aggregates,
            *grouping_kind,
            grouping_sets,
            having.as_ref(),
        ),
        TypedOp::LateralView {
            input,
            table_alias,
            columns,
        } => build_lateral_view(input, table_alias, columns),
        TypedOp::SetOp {
            kind,
            all,
            by_name,
            allow_missing_columns,
            children,
            widened_schema,
        } => build_set_op(
            *kind,
            *all,
            *by_name,
            *allow_missing_columns,
            children,
            widened_schema,
        ),
        TypedOp::WithColumns { input, assignments } => build_with_columns(input, assignments),
        TypedOp::DropColumns { input, drop_names } => build_drop_columns(input, drop_names),
        TypedOp::WithColumnsRenamed { input, renames } => {
            build_with_columns_renamed(input, renames)
        }
        TypedOp::NaFill {
            input,
            cols,
            values,
        } => build_na_fill(input, cols, values),
        TypedOp::NaDrop {
            input,
            cols,
            min_non_nulls,
        } => build_na_drop(input, cols, *min_non_nulls),
        TypedOp::NaReplace {
            input,
            cols,
            replacements,
        } => build_na_replace(input, cols, replacements),
        other => legacy_render(other, schema).map(SqlUnit::Raw),
    }
}

/// Fill a SELECT slot list over `input`'s open block: merge when the Select
/// ordinal is free and `vis` holds, else wrap. Shared by every
/// projection-shaped operator (WithColumns[Renamed], DropColumns, NaFill,
/// NaReplace). The `slots` closure receives the PRE-wrap block plus whether
/// the wrap fallback was taken; a stranded qualifier on a wrapped
/// expression needs no rewrite here — ADR-023 tier 3e-ii/iii already dropped
/// it at resolution time — so today only [`build_drop_columns`]'s `* EXCLUDE`
/// choice reads the flag.
fn block_with_projections(
    input: &TypedAst,
    vis: impl FnOnce(&SelectBlock) -> bool,
    slots: impl FnOnce(&SelectBlock, bool) -> Result<String, EmissionError>,
) -> Result<SqlUnit, EmissionError> {
    let mut block = open_block(input)?;
    let merges = block.can_accept(Clause::Select) && vis(&block);
    let slots = slots(&block, !merges)?;
    if !merges {
        block = SelectBlock::wrap(block.into());
    }
    block.set_projections(slots);
    Ok(block.into())
}

/// The `TypedOp::Join` fields [`build_join`] consumes (destructured into a
/// struct to stay within the parameter-count guideline).
struct JoinParts<'a> {
    left: &'a TypedAst,
    right: &'a TypedAst,
    join_type: crate::transpiler_v2::ast::JoinType,
    condition: Option<&'a Expression>,
    using_columns: &'a [String],
    lateral: bool,
}

/// One join side's per-ordinal alias coverage in the enclosing FROM scope,
/// from `side.scope.aliases` ∩ `item.exposed()`. Phase 0 (ADR-023 __td_jl/jr
/// retirement, neutral groundwork): preserves the pre-existing `covering_alias`
/// algorithm byte-for-byte. Phase 1 will re-source spans from the ITEM TREE
/// (a `Derived` wrap's own alias covering its range) — a behavior change that
/// flips `using_parent_with_synthetic_scoped_side_stays_wrapped`, NOT done here.
struct FromScope<'a> {
    aliases: &'a [(String, std::ops::Range<usize>)],
    exposed: Vec<String>,
    width: usize,
}

impl<'a> FromScope<'a> {
    fn of(side: &'a TypedAst, item: &FromItem) -> Self {
        Self {
            aliases: &side.scope.aliases,
            exposed: item.exposed(),
            width: side.resolved_schema.len(),
        }
    }

    /// Exact legacy `covering_alias` semantics (first covering range, gated by
    /// exposed, NO dup guard). Backs the two neutral accessors.
    fn covering(&self, i: usize) -> Option<&str> {
        let (name, _) = self.aliases.iter().find(|(_, r)| r.contains(&i))?;
        self.exposed
            .iter()
            .any(|e| e.eq_ignore_ascii_case(name))
            .then_some(name.as_str())
    }

    /// Phase 3b merge-path binding for ordinal `i` of a bare duplicate
    /// `name`: the covering alias iff it is the unique aliases-entry for its
    /// name, uniquely exposed, and `name` is unique within its span.
    /// Deliberately NOT [`Self::alias_for`]: no single-exposed fast path (an
    /// internally-dup span must reject), and analyzer-binding uniqueness is
    /// required, not just exposure.
    fn unique_binding_alias(&self, i: usize, name: &str, schema: &Schema) -> Option<&str> {
        let (alias, range) = self.aliases.iter().find(|(_, r)| r.contains(&i))?;
        // (i) `alias` names exactly ONE aliases entry (ci) — homonym-alias
        // hazard (H8-2): two distinct scope entries sharing the same alias
        // text make "the" covering alias ambiguous even though `i` itself
        // falls in only one of their ranges.
        let alias_entries = self
            .aliases
            .iter()
            .filter(|(a, _)| a.eq_ignore_ascii_case(alias))
            .count();
        // (ii) `alias` appears exactly ONCE in the block's exposed FROM
        // aliases — the merge-visibility counterpart of (i).
        let exposed_count = self
            .exposed
            .iter()
            .filter(|e| e.eq_ignore_ascii_case(alias))
            .count();
        // (iii) `name` occurs exactly ONCE within the covering span — an
        // internally-dup span would leftmost-bind the wrong physical column.
        let within_span = schema.fields[range.clone()]
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(name))
            .count();
        (alias_entries == 1 && exposed_count == 1 && within_span == 1).then_some(alias.as_str())
    }

    /// Canonical per-ordinal accessor: unambiguous exposed alias for `i`, else
    /// None (uncovered OR covering alias exposed >1). Phase 2 (ADR-023
    /// `__td_jl`/`__td_jr` retirement) entry point: backs
    /// [`requalify_join_condition`]'s per-ordinal target lookup.
    fn alias_for(&self, i: usize) -> Option<&str> {
        // Single-exposed fast path (NARROW upgrade — checked before the
        // covering-span lookup, which never sees a fresh/synthetic wrap
        // alias absent from the analyzer's logical `scope.aliases`): a lone
        // exposed item (a `Derived` wrap, including a synthetic/fresh wrap)
        // is addressable only by that one alias covering its WHOLE width —
        // so any in-bounds ordinal binds through it unambiguously, with no
        // span lookup needed. Multi-exposed items keep the exact legacy
        // per-ordinal-span lookup below; `covers_all`/`slot_quals` and the
        // item-tree itself are untouched by this fast path.
        if let [only] = self.exposed.as_slice() {
            return (i < self.width).then_some(only.as_str());
        }
        let name = self.covering(i)?;
        (self
            .exposed
            .iter()
            .filter(|e| e.eq_ignore_ascii_case(name))
            .count()
            == 1)
            .then_some(name)
    }

    /// Replaces `scope_covers_fields`.
    fn covers_all(&self) -> bool {
        (0..self.width).all(|i| self.covering(i).is_some())
    }

    /// Replaces `side_slot_quals`: single-exposed fast path, else per-field covering.
    fn slot_quals(&self) -> Option<Vec<String>> {
        if let [only] = self.exposed.as_slice() {
            return Some(vec![only.clone(); self.width]);
        }
        (0..self.width)
            .map(|i| self.covering(i).map(str::to_owned))
            .collect()
    }
}

/// Whether `fields` (a `(name, qualifier)` sequence — one entry per hoisted
/// slot a join side would contribute) contains two entries that resolve to
/// the SAME qualified reference (`qualifier.name`, case-insensitive) — e.g.
/// a single-alias synthetic wrap over `emp JOIN emp2`, where both sides' `id`
/// end up qualified by the same `__td_jl` and the hoisted list would emit
/// `__td_jl.id` twice, silently dropping the second column's data (join-022
/// round-1). A multi-alias inlined side (F5) never collides this way even
/// when the raw schema repeats a name: each same-named field carries its OWN
/// covering alias (`e.dept_id` vs `d.dept_id`), so the qualified references
/// stay distinct. [`build_join`]'s Change 3 guard uses this — keyed on the
/// qualified pair, not the raw name alone, and over only the NON-USING-key
/// fields (the ones a side actually contributes a qualified slot for) — to
/// decide whether a USING join over a duplicate-name side is genuinely
/// unsafe to hoist.
fn has_unsafe_qualified_duplicate<'a>(
    mut fields: impl Iterator<Item = (&'a str, &'a str)>,
) -> bool {
    let mut seen: HashSet<(String, String)> = HashSet::new();
    fields.any(|(name, qual)| !seen.insert((qual.to_lowercase(), name.to_lowercase())))
}

/// True iff any USING-key name in `using` matches **two or more** fields in
/// `schema` (case-insensitive) — a duplicated USING-key name anywhere in an
/// otherwise-inlinable join side's FLATTENED output (`schema` is that side's
/// full `resolved_schema`, so this catches a dup key nested arbitrarily deep,
/// not just in a direct child).
///
/// Verified live against DuckDB 1.5.0 (ADR-023 Phase 2.1 probe): a flat
/// chain — `emp INNER JOIN dept ON (emp.dept_id) = (dept.dept_id) INNER JOIN
/// emp2 USING (dept_id)` — Binder-errors ("Ambiguous reference `dept_id`")
/// even with real user aliases present, because the key resolves across TWO
/// SIBLING FROM bindings in one flat scope; the SAME key is fine resolved
/// INSIDE a single wrapped input (`(SELECT * FROM emp JOIN dept ON …) AS
/// __td_jl INNER JOIN emp2 USING (dept_id)` prepares OK). The guard is
/// key-specific — a USING key that is unique in the nested side (e.g. `id`)
/// still inlines — so it only trips the flat-chain hazard, never a false
/// positive on an unambiguous key.
fn using_key_duplicated(schema: &Schema, using: &[String]) -> bool {
    using.iter().any(|key| {
        schema
            .fields
            .iter()
            .filter(|f| f.name.eq_ignore_ascii_case(key))
            .count()
            >= 2
    })
}

/// The result of a failed [`requalify_join_condition`] rewrite: which
/// side(s) need a fresh alias before the condition can bind unambiguously.
/// Never both-`false` — that outcome is `Ok`, not `Err`.
#[derive(Debug, Default)]
struct SideNeedsAlias {
    left: bool,
    right: bool,
}

/// Rewrite every join-CONDITION [`ColumnReference`] to the qualifier the
/// EMITTED sides make true, resolved BY IDENTITY from the reference's
/// resolver-stamped `expr_id` (N10-lite stage 2; supersedes the
/// ordinal-keyed ADR-023 Phase 2/3b rewrite, `__td_jl`/`__td_jr` retirement
/// for condition references): a name unique in `cond_schema` binds bare
/// (`qualifier = None`); an ambiguous one binds through
/// [`FromScope::alias_for`] against whichever side the id-resolved slot
/// falls in. Phase 3b: the analyzer never stamps a synthetic
/// `__td_jl`/`__td_jr` qualifier anymore (every plan_id-scoped ref resolves
/// bare+id), so the id-resolved slot's side (`k < left_len`) is the ONLY
/// signal this rewrite ever consults. Sound only because a join's left and
/// right sides can never share an `expr_id` — see the disjointness pin
/// (`self_join_left_right_resolved_schema_ids_are_disjoint`,
/// `analyzer.rs`) and the `debug_assert` below.
///
/// Left untouched: a reference whose `expr_id` is `None` (deferred
/// resolution — see [`super::expression::ColumnReference::expr_id`]'s doc
/// for the analyzer paths that still leave it unstamped) or `Some` but
/// absent from `cond_schema` (D2: a correlated outer reference's id lives in
/// the enclosing plan's schema, never this join's — a loud DuckDB binder
/// error surfaces instead of a silent wrong-column rewrite), and one
/// carrying a real, non-synthetic user-alias qualifier (already binds —
/// e.g. `e.dept_id`).
///
/// Returns the rewritten [`Expression`] tree (clone-then-walk via
/// [`Expression::map_children`], the same fallible-fold primitive the
/// analyzer's own expression walkers use — see `reproject_qualifiers`).
/// `Err(SideNeedsAlias)` when a demanded ambiguous id-resolved slot has no
/// unambiguous covering alias on its side; [`build_join`]'s fixpoint then
/// wraps the flagged side(s) under a fresh alias and retries.
fn requalify_join_condition(
    cond: &Expression,
    left: &TypedAst,
    right: &TypedAst,
    left_item: &FromItem,
    right_item: &FromItem,
    cond_schema: &Schema,
) -> Result<Expression, SideNeedsAlias> {
    let left_len = left.resolved_schema.len();
    let left_scope = FromScope::of(left, left_item);
    let right_scope = FromScope::of(right, right_item);
    // N10-lite stage 2 disjointness guard: `requalify_column_ref`'s
    // `k < left_len` side split is keyed off `expr_id`, so it is sound only
    // if the two sides' id sets never intersect — mechanically pinned by
    // `self_join_left_right_resolved_schema_ids_are_disjoint` (analyzer.rs);
    // this debug_assert catches any future violation of that pin directly
    // at the point the split is consumed.
    debug_assert!(
        left.resolved_schema.fields.iter().all(|lf| right
            .resolved_schema
            .fields
            .iter()
            .all(|rf| lf.expr_id != rf.expr_id)),
        "join left/right resolved_schema expr_id sets must be disjoint"
    );
    let mut needs = SideNeedsAlias::default();
    let rewritten = requalify_expr(
        cond.clone(),
        cond_schema,
        left_len,
        &left_scope,
        &right_scope,
        &mut needs,
    );
    if needs.left || needs.right {
        Err(needs)
    } else {
        Ok(rewritten)
    }
}

/// Structural recursion for [`requalify_join_condition`]: rewrite an
/// immediate [`Expression::ColumnReference`], else recurse into children
/// (subquery bodies excluded per τ's walker convention — see
/// [`Expression::map_children`]/`expression_children!`). Infallible:
/// failures are recorded into `needs` rather than short-circuiting the
/// walk, so a single pass can flag BOTH sides at once (the `<=2`-pass
/// fixpoint bound in [`build_join`] depends on this).
fn requalify_expr(
    expr: Expression,
    cond_schema: &Schema,
    left_len: usize,
    left_scope: &FromScope,
    right_scope: &FromScope,
    needs: &mut SideNeedsAlias,
) -> Expression {
    match expr {
        Expression::ColumnReference(mut c) => {
            requalify_column_ref(
                &mut c,
                cond_schema,
                left_len,
                left_scope,
                right_scope,
                needs,
            );
            Expression::ColumnReference(c)
        }
        other => other
            .map_children(|child| {
                Ok::<_, std::convert::Infallible>(requalify_expr(
                    child,
                    cond_schema,
                    left_len,
                    left_scope,
                    right_scope,
                    needs,
                ))
            })
            .unwrap_or_else(|never: std::convert::Infallible| match never {}),
    }
}

/// The rewrite predicate (H8 boundary), N10-lite stage 2: `c.qualifier`
/// must be `None` with `c.name` ambiguous (count `>= 2`) in `cond_schema`
/// AND `c.expr_id` must name a field actually present in `cond_schema` (at
/// position `k`) — otherwise `c` is left untouched (unstamped/deferred, a
/// correlated outer reference's id absent from `cond_schema` (D2), or a real
/// alias that already binds). Phase 3b: the analyzer never stamps a
/// synthetic `__td_jl`/`__td_jr` qualifier anymore, so this is the only
/// shape a rewritable reference can take.
///
/// This gate is byte-for-byte [`bare_dup_slot`]'s gate (same four checks —
/// qualifier-none, name-count `>= 2`, id lookup, name-agreement assert —
/// just evaluated in a different order, which does not change the result
/// for a pure conjunction of side-effect-free checks), so it is folded into
/// a direct call rather than re-hand-rolled here.
fn requalify_column_ref(
    c: &mut ColumnReference,
    cond_schema: &Schema,
    left_len: usize,
    left_scope: &FromScope,
    right_scope: &FromScope,
    needs: &mut SideNeedsAlias,
) {
    let Some(k) = bare_dup_slot(c, cond_schema) else {
        return;
    };
    let is_left = k < left_len;
    let local = if is_left { k } else { k - left_len };
    let scope = if is_left { left_scope } else { right_scope };
    // H8 assert 3: the local index must be in-bounds for its own side —
    // guards the exact `alias_for`/`i < width` boundary the single-exposed
    // fast path relies on.
    debug_assert!(
        local < scope.width,
        "local index {local} out of bounds for side width {}",
        scope.width
    );
    match scope.alias_for(local) {
        Some(alias) => c.qualifier = Some(alias.to_owned()),
        None if is_left => needs.left = true,
        None => needs.right = true,
    }
}

/// Lower one join side to a [`FromItem`]. Ladder:
///
/// 1. The side's unit is a pure-FROM block → inline its `FromItem` directly,
///    keeping user aliases / table names visible (subsumes the old
///    user-alias hoist, bare-TableScan hoist, and left-spine chain flatten).
///    Guarded: a nested `Join` item inlines only on the left side, and only
///    when the nested join itself is a plain ON/CROSS join (CLAUDE.md
///    gotcha 4 — never fold across semi/anti; lateral correlation must stay
///    isolated). Under a non-USING parent this is unconditional; under a
///    USING parent (F5, widened by Phase 2.1) it additionally requires
///    [`FromScope::covers_all`] to hold for the nested side AND no parent
///    USING-key name to be duplicated in it ([`using_key_duplicated`] — a
///    live-DuckDB-validated guard: a duplicated USING-key name resolves fine
///    INSIDE the nested side's own wrap, but a flat chain binding it across
///    two sibling FROM bindings in one scope is a DuckDB Binder Error) — the
///    USING parent's own hoisted-slot qualifiers must be derivable per field
///    from an alias the nested join's emitted `FromItem` actually exposes,
///    or the side stays wrapped under its synthetic alias instead (see
///    [`FromScope::slot_quals`]). `Raw` FROM bodies (lateral-view chains)
///    never inline.
/// 2. Otherwise → `(side) AS __td_jl/__td_jr`.
///
/// The duplicate-alias guard runs in [`build_join`] across BOTH lowered
/// sides (DuckDB rejects `Duplicate alias` in one FROM scope; Spark permits
/// it) — on collision the offending side falls back to its synthetic wrap.
fn build_join_side(
    side: &TypedAst,
    synthetic_alias: &str,
    may_inline_nested_join: bool,
    parent_using: &[String],
) -> Result<FromItem, EmissionError> {
    let parent_has_using = !parent_using.is_empty();
    let unit = build_unit(&side.op, &side.resolved_schema)?;
    let block = match unit {
        SqlUnit::Select(block) => block,
        raw => {
            return Ok(FromItem::Derived {
                unit: Box::new(raw),
                alias: synthetic_alias.to_owned(),
            })
        }
    };
    // Peek eligibility on the block's FROM item BEFORE consuming it — a
    // block that does not inline still needs its defaults intact for the
    // wrap path below (F2: the former `SelectBlock::from_item(item)` rebuild
    // silently discarded the join builder's hoisted slot list).
    let inline_ok = block.pure_from()
        && match block.from_ref() {
            FromItem::Relation { .. } | FromItem::Derived { .. } => true,
            item @ FromItem::Join { .. } => {
                may_inline_nested_join
                    && (!parent_has_using
                        || (FromScope::of(side, item).covers_all()
                            && !using_key_duplicated(&side.resolved_schema, parent_using)))
                    && matches!(
                        &side.op,
                        TypedOp::Join {
                            join_type,
                            using_columns,
                            lateral: false,
                            ..
                        } if using_columns.is_empty()
                            && !matches!(
                                join_type,
                                super::ast::JoinType::LeftSemi
                                    | super::ast::JoinType::LeftAnti
                            )
                    )
            }
            FromItem::Raw { .. } => false,
        };
    if inline_ok {
        // `pure_from()` above already established this cannot fail; the Err
        // arm is a defensive fallback, never a panic path.
        match block.into_pure_from() {
            Ok(item) => Ok(item),
            Err(block) => Ok(FromItem::Derived {
                unit: Box::new(SqlUnit::Select(block)),
                alias: synthetic_alias.to_owned(),
            }),
        }
    } else {
        Ok(FromItem::Derived {
            unit: Box::new(SqlUnit::Select(block)),
            alias: synthetic_alias.to_owned(),
        })
    }
}

/// Duplicate-alias guard (unconditional — runs every fixpoint pass in
/// [`build_join`], including a no-condition CROSS self-join): DuckDB rejects
/// two `AS x` in one FROM scope, though Spark permits it. If the two lowered
/// sides expose a common name (case-insensitive), the RIGHT side — always
/// movable — is rewrapped under the first fresh name in its own `__td_jr`
/// sequence the LEFT side does not expose (see [`fresh_alias_wrap`]).
fn apply_duplicate_alias_guard(left_item: FromItem, right_item: FromItem) -> (FromItem, FromItem) {
    let left_names = left_item.exposed();
    let collides = right_item
        .exposed()
        .iter()
        .any(|r| left_names.iter().any(|l| l.eq_ignore_ascii_case(r)));
    if !collides {
        return (left_item, right_item);
    }
    let right_item = fresh_alias_wrap(right_item, TD_JOIN_RIGHT, &left_names);
    (left_item, right_item)
}

/// Rewrap `item` as `(item) AS <fresh>`, where `<fresh>` is the first name
/// in the sequence `base`, `base_2`, `base_3`, … (`base` = `__td_jl` or
/// `__td_jr`) that `other_exposed` does not contain (case-insensitive) — the
/// shared fresh-alias rewrap both [`apply_duplicate_alias_guard`] and a
/// [`SideNeedsAlias`] retry in [`build_join`] use.
fn fresh_alias_wrap(item: FromItem, base: &str, other_exposed: &[String]) -> FromItem {
    let alias = std::iter::once(base.to_owned())
        .chain((2..=64).map(|n| format!("{base}_{n}")))
        .find(|cand| !other_exposed.iter().any(|o| o.eq_ignore_ascii_case(cand)))
        // Defensive fallback — never observed; avoids an unbounded loop /
        // `unwrap` if all 64 candidates collide.
        .unwrap_or_else(|| format!("{base}_64"));
    FromItem::Derived {
        unit: Box::new(SqlUnit::from(SelectBlock::from_item(item))),
        alias,
    }
}

fn build_join(parts: JoinParts<'_>) -> Result<SqlUnit, EmissionError> {
    use super::ast::JoinType;
    let JoinParts {
        left,
        right,
        join_type,
        condition,
        using_columns,
        lateral,
    } = parts;
    let is_semi_or_anti = matches!(join_type, JoinType::LeftSemi | JoinType::LeftAnti);

    let mut left_item = build_join_side(left, TD_JOIN_LEFT, true, using_columns)?;
    let mut right_item = build_join_side(right, TD_JOIN_RIGHT, false, using_columns)?;

    // Condition types resolve against the concatenated side schemas (the
    // analyzer stamped every reference; the schema feeds type lookups only).
    // Computed ONCE, before the fixpoint below: a fresh wrap changes WHERE a
    // side's fields are addressable FROM, never their ordinal position in
    // this merged schema.
    let cond_schema = Schema::merge(&left.resolved_schema, &right.resolved_schema);

    // Bounded fixpoint (ADR-023 Phase 2, `<=2` passes, H8 assert 4): the
    // duplicate-alias guard runs unconditionally every pass (DuckDB rejects
    // two `AS x` in one FROM scope; Spark permits it — this also covers a
    // no-condition CROSS self-join, where the requalifier below never runs).
    // When an ON condition is present, `requalify_join_condition` then
    // rewrites it to each emitted side's real alias, wrapping the flagged
    // side(s) under a fresh alias and retrying on failure. A fresh wrap
    // makes a side single-exposed, which `FromScope::alias_for`'s fast path
    // covers unconditionally, so a wrapped side is never re-flagged —
    // termination is guaranteed well inside the bound; the assert below is
    // the review-time safety net documenting it, not a load-bearing runtime
    // guard (release builds skip it, per Rust convention).
    let mut pass = 0usize;
    let rewritten_condition = loop {
        debug_assert!(
            pass < 2,
            "requalifier + duplicate-alias guard must reach fixpoint in <=2 passes"
        );
        let (guarded_left, guarded_right) = apply_duplicate_alias_guard(left_item, right_item);
        left_item = guarded_left;
        right_item = guarded_right;
        let Some(cond) = condition else {
            break None;
        };
        match requalify_join_condition(cond, left, right, &left_item, &right_item, &cond_schema) {
            Ok(expr) => break Some(expr),
            Err(needs) => {
                if needs.left {
                    let other = right_item.exposed();
                    left_item = fresh_alias_wrap(left_item, TD_JOIN_LEFT, &other);
                }
                if needs.right {
                    let other = left_item.exposed();
                    right_item = fresh_alias_wrap(right_item, TD_JOIN_RIGHT, &other);
                }
            }
        }
        pass += 1;
    };
    let condition_for_clause = rewritten_condition.as_ref().or(condition);
    let clause = render_join_clause(join_type, condition_for_clause, using_columns, &cond_schema)?;

    // Hoisted slot list mirroring the analyzer's output-schema order (USING
    // cols first, then left non-USING, then right non-USING; right side
    // suppressed for semi/anti).
    let need_right = !is_semi_or_anti;
    let default_slots = if using_columns.is_empty() {
        // Non-USING joins NEVER build a default slot list (F7 round 2,
        // Change 2). DuckDB expands bare `SELECT *` over a plain
        // ON/CROSS/semi/anti join left-then-right in natural order — exactly
        // the analyzer's resolved-schema order — so a hoisted list adds
        // nothing here. Worse, a single-alias side over a DUPLICATE-name
        // schema (e.g. an inlined `emp JOIN emp2` wrapped as one `__td_jl` /
        // `__td_jr`, per [`build_join_side`]'s ladder) made the OLD
        // name-based slot list emit e.g. `__td_jl.id` twice, double-binding
        // the first `id` — silent corruption (join-022 round-1 residual).
        // `*` is positional and never double-binds. The hoisted list stays a
        // USING-only device below: it alone enforces Spark's key-first
        // output order, which DuckDB's `*` breaks for USING joins.
        None
    } else {
        // USING joins (F5): per-field qualifiers from the RelScope stamp,
        // rather than a single alias per side — this is what lets a
        // multi-alias inlined nested-join side (change 2's
        // `FromScope::covers_all` gate) hoist its slots under a USING parent
        // instead of staying buried under a synthetic wrap alias. Honestly
        // stated (fix round 1): `FromScope::slot_quals` requires every covering
        // RelScope alias to ALSO be one `left_item`/`right_item` actually
        // exposes — a covering alias RelScope reports but the emitted item
        // does not expose (e.g. a nested-join side whose own children are
        // synthetic-wrapped, rendering under `__td_jl`/`__td_jr` instead of
        // their logical aliases) does not count. `build_join_side`'s
        // `inline_ok` gates on that SAME exposure-aware
        // `FromScope::of(side, &item).covers_all()` predicate, against the SAME
        // item, before ever inlining a multi-alias side under a USING
        // parent — so `FromScope::of(left, &left_item).slot_quals()` is guaranteed
        // `Some` for every inlined multi-alias left side by construction.
        // The right side is never inlined under USING
        // (`may_inline_nested_join` is always `false` for it in
        // `build_join_side`), so it stays single-alias and hits the
        // `item.exposed()` fast path in `FromScope::slot_quals` unconditionally.
        // If either is ever `None` here regardless, fall back to bare `*`
        // rather than panic — this function only consumes that guarantee,
        // it does not reprove it locally.
        let left_quals = FromScope::of(left, &left_item).slot_quals();
        let right_quals = if need_right {
            FromScope::of(right, &right_item).slot_quals()
        } else {
            None
        };
        let using_lower: HashSet<String> = using_columns.iter().map(|s| s.to_lowercase()).collect();
        // Change 3 (F7 round 2): a USING join over a side whose non-USING-key
        // fields would collide under the SAME qualified reference (see
        // [`has_unsafe_qualified_duplicate`]) can build neither a safe
        // per-field-qualified slot list (double-binds the duplicate) NOR
        // bare `*` (breaks USING's key-first output order) — an honest
        // Thunderduck-boundary error (ADR-022) is the correct interim. No
        // baseline-green corpus case exercises this shape.
        let side_unsafe = |schema: &Schema, quals: &[String]| {
            has_unsafe_qualified_duplicate(
                schema
                    .fields
                    .iter()
                    .zip(quals.iter())
                    .filter(|(f, _)| !using_lower.contains(&f.name.to_lowercase()))
                    .map(|(f, q)| (f.name.as_str(), q.as_str())),
            )
        };
        if left_quals
            .as_ref()
            .is_some_and(|lq| side_unsafe(&left.resolved_schema, lq))
            || right_quals
                .as_ref()
                .is_some_and(|rq| need_right && side_unsafe(&right.resolved_schema, rq))
        {
            bail_boundary_op!(
                "Join",
                "USING join over an input with duplicate column names is not supported \
                 (per-field qualified slots would double-bind the duplicate and `*` \
                 breaks USING key order — both silently corrupt data)"
            );
        }
        match (left_quals, right_quals) {
            (Some(lq), rq) if !need_right || rq.is_some() => {
                let mut slots: Vec<DefaultSlot> = using_columns
                    .iter()
                    .map(|c| DefaultSlot {
                        name: c.clone(),
                        sql: quote_ident(c).into_owned(),
                    })
                    .collect();
                for (f, qual) in left.resolved_schema.fields.iter().zip(lq.iter()) {
                    if !using_lower.contains(&f.name.to_lowercase()) {
                        slots.push(DefaultSlot {
                            name: f.name.clone(),
                            sql: format!("{}.{}", quote_ident(qual), quote_ident(&f.name)),
                        });
                    }
                }
                if need_right {
                    if let Some(rq) = &rq {
                        for (f, qual) in right.resolved_schema.fields.iter().zip(rq.iter()) {
                            if !using_lower.contains(&f.name.to_lowercase()) {
                                slots.push(DefaultSlot {
                                    name: f.name.clone(),
                                    sql: format!("{}.{}", quote_ident(qual), quote_ident(&f.name)),
                                });
                            }
                        }
                    }
                }
                Some(slots)
            }
            _ => None,
        }
    }
    .filter(|slots: &Vec<DefaultSlot>| !slots.is_empty());

    let mut block = SelectBlock::from_item(FromItem::Join {
        left: Box::new(left_item),
        right: Box::new(right_item),
        kind: join_kind_sql(join_type),
        clause,
        lateral,
    });
    if let Some(slots) = default_slots {
        block.set_default_projections(slots);
    }
    Ok(block.into())
}

fn build_aggregate(
    input: &TypedAst,
    grouping: &[Expression],
    aggregates: &[Expression],
    grouping_kind: crate::transpiler_v2::ast::GroupingKind,
    grouping_sets: &[Vec<usize>],
    having: Option<&Expression>,
) -> Result<SqlUnit, EmissionError> {
    use super::ast::GroupingKind;
    // The SparkSQL front-end populates `grouping_sets` with per-set membership
    // (indices into `grouping`). The DataFrame `groupingSets` path leaves it
    // empty, so it stays a Thunderduck-boundary error (ADR-022).
    if matches!(grouping_kind, GroupingKind::GroupingSets) && grouping_sets.is_empty() {
        bail_boundary_op!(
            "Aggregate[GroupingSets]",
            "GROUPING SETS requires set-membership metadata (DataFrame groupingSets path not implemented in τ)",
        );
    }
    let input_schema = &input.resolved_schema;
    // Rewrite any no-arg `grouping_id()`/`grouping()` calls to pass the
    // grouping columns explicitly — DuckDB has no zero-arg form. Splice
    // against the ORIGINAL `grouping`; the wrap-path reprojection below
    // then walks into the spliced args the same way it walks the flat
    // GROUP BY list, so the two stay textually consistent regardless of
    // splice-then-reproject vs reproject-then-splice ordering.
    let rewritten_aggregates: Vec<Expression> = aggregates
        .iter()
        .map(|a| with_grouping_id_spliced(a, grouping))
        .collect();
    // Splice BEFORE the merge decision (not just before rendering) — the
    // fused merge-path rewrite below must see the SAME having expression the
    // render path later uses, so a bare duplicate-name ordinal inside a
    // spliced `grouping_id()` call is visible to the rewrite too.
    let having_spliced: Option<Expression> = having.map(|h| with_grouping_id_spliced(h, grouping));

    // Open the child block and decide merge-vs-wrap BEFORE rendering — the
    // fused visibility+rewrite check must run over the ORIGINAL (pre-wrap)
    // expressions against the pre-wrap block, exactly like
    // `build_filter`/`build_sort`.
    let mut block = open_block(input)?;
    let merge_set: Option<Vec<Expression>> = block
        .can_accept(Clause::GroupBy)
        .then(|| {
            requalify_visible(
                grouping
                    .iter()
                    .chain(rewritten_aggregates.iter())
                    .chain(having_spliced.iter()),
                &block,
                input,
            )
        })
        .flatten();
    let merge = merge_set.is_some();

    // ADR-023 tier 2: activate the wrap-boundary reprojection only when the
    // wrapped child's output has a duplicate name — the one class
    // resolution's unique-name qualifier drop (tier 3e-ii/iii) cannot cover.
    // `None` on the common (already-unique) case means the reference already
    // resolved bare at analysis time, so every branch below passes the
    // expression through unchanged.
    let uniquified = output_uniquified(input_schema);
    // Choose the expression set to render from: the fused merge-path
    // rewrite's output when merging (already positionally requalified where
    // needed, no cosmetic churn otherwise), split back by length — or each
    // reprojected against the PRE-wrap block when wrapping onto a
    // duplicate-name output.
    let (grouping_r, aggregates_r, having_r): (
        Vec<Expression>,
        Vec<Expression>,
        Option<Expression>,
    ) = if let Some(rewritten) = merge_set {
        let mut it = rewritten.into_iter();
        let grouping_r: Vec<Expression> = (&mut it).take(grouping.len()).collect();
        let aggregates_r: Vec<Expression> = (&mut it).take(rewritten_aggregates.len()).collect();
        let having_r: Option<Expression> = it.next();
        (grouping_r, aggregates_r, having_r)
    } else {
        let reproject =
            |e: &Expression| -> Expression { reproject_or_clone(e, input, &uniquified) };
        let grouping_r = grouping.iter().map(&reproject).collect();
        let aggregates_r = rewritten_aggregates.iter().map(&reproject).collect();
        let having_r = having_spliced.as_ref().map(&reproject);
        (grouping_r, aggregates_r, having_r)
    };

    // N7: `aggregates` IS the complete output list by construction (every
    // front-end builds it that way — see `CommonOp::Aggregate`'s doc), so
    // the SELECT slots are a straight render over `aggregates_r`; `grouping_r`
    // is rendered separately below, for GROUP BY only.
    let slots = sql_join(aggregates_r.iter(), ", ", |e| {
        render_projection_slot(e, input_schema)
    })?;
    let having_sql = having_r
        .as_ref()
        .map(|h| render_expr(h, input_schema))
        .transpose()?;
    // Emit a GROUP BY whenever there are flat grouping columns, OR when this
    // is a GROUPING SETS aggregate with at least one set — the all-empty
    // `GROUP BY GROUPING SETS ((), ())` case still produces one grand-total
    // row PER SET, so dropping the clause would be a silent wrong row-count.
    let emit_group_by = !grouping_r.is_empty()
        || (matches!(grouping_kind, GroupingKind::GroupingSets) && !grouping_sets.is_empty());
    let group_body = if emit_group_by {
        let rendered = render_group_exprs(&grouping_r, input_schema)?;
        Some(match grouping_kind {
            GroupingKind::GroupBy => rendered.join(", "),
            GroupingKind::Rollup => format!("ROLLUP({})", rendered.join(", ")),
            GroupingKind::Cube => format!("CUBE({})", rendered.join(", ")),
            GroupingKind::GroupingSets => {
                let sets: Vec<String> = grouping_sets
                    .iter()
                    .map(|s| {
                        let cols = s
                            .iter()
                            .map(|&i| rendered[i].as_str())
                            .collect::<Vec<_>>()
                            .join(", ");
                        format!("({cols})")
                    })
                    .collect();
                format!("GROUPING SETS ({})", sets.join(", "))
            }
        })
    } else {
        None
    };

    if !merge {
        block = wrap_maybe_reprojected(block.into(), &uniquified);
    }
    if let Some(g) = group_body {
        block.set_group_by(g);
    }
    if let Some(h) = having_sql {
        block.set_having(h);
    }
    block.set_projections(slots);
    Ok(block.into())
}

fn build_lateral_view(
    input: &TypedAst,
    table_alias: &str,
    columns: &[(String, Expression)],
) -> Result<SqlUnit, EmissionError> {
    let input_schema = &input.resolved_schema;
    let inner_select = sql_join(columns.iter(), ", ", |(alias, expr)| {
        let expr_sql = render_expr(expr, input_schema)?;
        Ok(format!("{expr_sql} AS {}", quote_ident(alias)))
    })?;
    let mut block = open_block(input)?;
    // The generator expressions reference the input's FROM scope; extending
    // is sound only on a pure-FROM block whose scope satisfies them.
    let vis = exprs_visible_in(columns.iter().map(|(_, e)| e), &block, &input.scope);
    if !(block.pure_from() && vis) {
        block = SelectBlock::wrap(block.into());
    }
    block.extend_from(
        &format!(
            ", LATERAL (SELECT {inner_select}) AS {}",
            quote_ident(table_alias)
        ),
        vec![table_alias.to_owned()],
    );
    // F3: a merged (not wrapped) block's hoisted default slot list must
    // widen to include the LATERAL VIEW's generated columns, or a
    // downstream merging Project that renders the bare-star default would
    // never see them. A no-op when there are no defaults to extend (a fresh
    // wrap, or a plain-scan child, keeps rendering `*`, which already covers
    // the newly appended FROM columns — cx-007..009 shape unchanged).
    let ta_q = quote_ident(table_alias);
    block.extend_default_projections(
        columns
            .iter()
            .map(|(alias, _)| {
                let a_q = quote_ident(alias);
                DefaultSlot {
                    name: alias.clone(),
                    sql: format!("{ta_q}.{a_q}"),
                }
            })
            .collect(),
    );
    Ok(block.into())
}

/// Build the child's unit and open it as a block: a `Select` unit is
/// returned as-is (merge candidate); a `Raw` unit is wrapped as
/// `(…) AS __td_sub`.
fn open_block(child: &TypedAst) -> Result<SelectBlock, EmissionError> {
    Ok(match build_unit(&child.op, &child.resolved_schema)? {
        SqlUnit::Select(block) => *block,
        raw => SelectBlock::wrap(raw),
    })
}

/// Collect the qualifiers of every analyzer-stamped column reference in
/// `expr` into `out` (immediate tree only — `Expression::children` excludes
/// subquery bodies by the τ walker convention, which is exactly the merge
/// visibility contract: correlated inner refs bind against whatever FROM
/// aliases the enclosing block keeps visible, same as today's shapes).
fn expr_qualifiers<'e>(expr: &'e Expression, out: &mut Vec<&'e str>) {
    match expr {
        Expression::ColumnReference(c) => {
            if let Some(q) = c.qualifier.as_deref() {
                out.push(q);
            }
        }
        Expression::UnresolvedColumn(u) => {
            if let Some(q) = u.qualifier.as_deref() {
                out.push(q);
            }
        }
        Expression::Star(StarExpression { qualifier }) => {
            if let Some(q) = qualifier.as_deref() {
                out.push(q);
            }
        }
        other => {
            for child in other.children() {
                expr_qualifiers(child, out);
            }
        }
    }
}

/// Merge visibility: every qualifier stamped on `exprs` that the input's own
/// [`RelScope`] binds must be an alias the block's FROM scope actually
/// emits. Qualifiers the input scope does NOT know are exempt: they are
/// correlated outer references (DuckDB's correlated-subquery binder resolves
/// them OUTWARD, so no inner FROM shape can bind them), or struct-column
/// qualifiers (rendered as struct access, scope-independent). A failed check
/// falls back to the wrap path (merging only ever WIDENS what a clause can
/// see).
fn exprs_visible_in<'e>(
    exprs: impl IntoIterator<Item = &'e Expression>,
    block: &SelectBlock,
    input_scope: &super::analyzer::RelScope,
) -> bool {
    let mut quals = Vec::new();
    for e in exprs {
        expr_qualifiers(e, &mut quals);
    }
    quals
        .iter()
        .filter(|q| scope_binds(input_scope, q))
        .all(|q| block.exposes(q))
}

/// Whether the analyzer's stamped scope binds `q` (case-insensitive, any
/// number of matches — ambiguity is the resolver's concern, not vis's).
fn scope_binds(scope: &super::analyzer::RelScope, q: &str) -> bool {
    scope
        .aliases
        .iter()
        .any(|(name, _)| name.eq_ignore_ascii_case(q))
}

/// N10-lite shared predicate (H8 boundary): `Some(k)` iff `c.qualifier` is
/// `None`, `c.name` is duplicated (`>=2`, case-insensitive) in `schema`, AND
/// `c.expr_id` names a field actually present in `schema` (at position `k`)
/// — the exact shape a bare duplicate-name reference must have to be
/// rewritten by identity. The governing invariant: the binding key is the
/// reference's [`super::schema::ExprId`], not a trusted stamped position —
/// same id in two slots binds the FIRST occurrence (value-correct: one
/// schema, one id, one per-row value, regardless of which of its slots is
/// addressed); an id ABSENT from `schema` leaves the reference untouched,
/// surfacing as a loud DuckDB binder error rather than a silent
/// wrong-column rewrite. Shared by [`requalify_visible`]'s merge-path
/// rewrite, [`reproject_qualifiers`]'s wrap-path bare-dup arm, and
/// [`requalify_column_ref`]'s gate (its exact same shape, folded in) — single
/// authority for the id-lookup + debug_assert guard
/// ([`super::schema::ResolvedSchema::field_by_id`]), so all call sites stay
/// in lockstep.
fn bare_dup_slot(c: &ColumnReference, schema: &Schema) -> Option<usize> {
    if c.qualifier.is_some() {
        return None;
    }
    let name_count = schema
        .fields
        .iter()
        .filter(|f| f.name.eq_ignore_ascii_case(&c.name))
        .count();
    if name_count < 2 {
        return None;
    }
    let id = c.expr_id?;
    let (k, _) = schema.field_by_id(id, &c.name)?;
    Some(k)
}

/// Merge visibility + id-keyed requalification, fused (ADR-023 Phase 3b).
/// `Some(rewritten)` iff (a) every scope-bound qualifier `exprs` carries is
/// exposed by `block`'s FROM (the [`exprs_visible_in`] contract) AND (b)
/// every bare duplicate-name reference ([`bare_dup_slot`]) binds through a
/// UNIQUE covering alias
/// ([`FromScope::unique_binding_alias`]) — rewritten to it. `None` — the
/// caller wraps — the moment either condition fails for any expression in
/// the set: a partial rewrite would be unsound (the wrap path re-derives the
/// whole set from scratch instead).
///
/// All other references pass through untouched: `expr_id: None` (deferred
/// pre-analysis resolution, or one of the analyzer gaps enumerated on
/// [`super::expression::ColumnReference::expr_id`]'s doc), `expr_id: Some`
/// but naming no field in this schema (D2: a correlated outer reference —
/// its id lives in the enclosing plan's schema, never this one's), a real
/// (already-rewritten) qualifier already binds, or a unique name that
/// resolution already left bare.
fn requalify_visible<'e>(
    exprs: impl IntoIterator<Item = &'e Expression>,
    block: &SelectBlock,
    input: &TypedAst,
) -> Option<Vec<Expression>> {
    let exprs: Vec<&Expression> = exprs.into_iter().collect();
    if !exprs_visible_in(exprs.iter().copied(), block, &input.scope) {
        return None;
    }
    let scope = FromScope::of(input, block.from_ref());
    let schema = &input.resolved_schema;

    fn walk(expr: &mut Expression, scope: &FromScope, schema: &Schema) -> bool {
        match expr {
            Expression::ColumnReference(c) => match bare_dup_slot(c, schema) {
                None => true,
                Some(k) => match scope.unique_binding_alias(k, &c.name, schema) {
                    Some(alias) => {
                        c.qualifier = Some(alias.to_owned());
                        true
                    }
                    None => false,
                },
            },
            other => other.children_mut().all(|child| walk(child, scope, schema)),
        }
    }

    let mut rewritten = Vec::with_capacity(exprs.len());
    for e in exprs {
        let mut cloned = e.clone();
        if !walk(&mut cloned, &scope, schema) {
            return None;
        }
        rewritten.push(cloned);
    }
    Some(rewritten)
}

/// ADR-023 tier 2 activation gate: `schema`'s field names, [`uniquify`]d,
/// iff they contain a duplicate — `None` when the names are already unique,
/// the common case. Every wrap site checks this FIRST and only reaches the
/// tier-2 reprojection path (`wrap_reprojected` + [`reproject_qualifiers`])
/// on `Some`; the `None` (common) case now simply passes each expression
/// through unchanged — the unique-name case's qualifier drop happens at
/// RESOLUTION (ADR-023 tier 3e-ii: tier-(e) drops a unique-name qualifier as
/// part of resolving the reference; tier 3e-iii extends this to
/// folded-aggregate lineage), so emission has nothing left to rewrite here.
fn output_uniquified(schema: &Schema) -> Option<Vec<String>> {
    let names: Vec<&str> = schema.fields.iter().map(|f| f.name.as_str()).collect();
    let mut seen: HashSet<&str> = HashSet::with_capacity(names.len());
    let all_unique = names.iter().all(|n| seen.insert(n));
    (!all_unique).then(|| uniquify(&names))
}

/// The position in `schema` that `(q, name)` resolves to, iff `scope` binds
/// `q` to a field range (first match — the same first-match convention
/// [`scope_binds`]/`FromScope::covering` use) containing a field named
/// `name` case-insensitively. `None` when `q` isn't a scope alias at all (the F10
/// dead-alias class [`reproject_qualifiers`] must leave untouched) or `name`
/// isn't found within the range `q` binds.
fn scope_position(
    scope: &super::analyzer::RelScope,
    q: &str,
    name: &str,
    schema: &Schema,
) -> Option<usize> {
    let (_, range) = scope
        .aliases
        .iter()
        .find(|(alias, _)| alias.eq_ignore_ascii_case(q))?;
    schema.fields[range.clone()]
        .iter()
        .position(|f| f.name.eq_ignore_ascii_case(name))
        .map(|offset| range.start + offset)
}

/// ADR-023 tier 2: the duplicate-output-name counterpart of the
/// resolution-time unique-name qualifier drop (tier 3e-ii/iii), paired with
/// [`SelectBlock::wrap_reprojected`]. That wrap re-exposes the wrapped
/// child's columns under `uniquified`'s names, positionally — so, unlike a
/// unique-name reference (which resolution already leaves bare and needs no
/// rewrite here), this rewrite is always safe for any qualifier
/// `input.scope` resolves: `q.name` becomes the bare `uniquified[pos]` at
/// `name`'s resolved position, which the reprojected wrap guarantees is
/// bindable.
///
/// A qualifier `input.scope` does NOT bind (the F10 dead-alias class) is
/// left untouched — Tier 3's job — as is a `q` that doubles as a struct
/// column access on the input schema (mirrors resolution's own
/// struct-precedence guard: struct access survives a wrap as
/// column-dot-field syntax and needs no rewrite). Qualified stars are not
/// rewritten, and subquery bodies are opaque `CommonAst` plans (not
/// expression children per the τ walker convention).
fn reproject_qualifiers(expr: &Expression, input: &TypedAst, uniquified: &[String]) -> Expression {
    fn walk(
        expr: &mut Expression,
        scope: &super::analyzer::RelScope,
        schema: &Schema,
        uniquified: &[String],
    ) {
        let resolve = |q: &str, name: &str| -> Option<usize> {
            if TypeInferenceEngine::struct_qualifier_info(name, q, schema).is_some() {
                return None;
            }
            scope_position(scope, q, name, schema)
        };
        match expr {
            Expression::ColumnReference(c) => {
                if let Some(pos) = c.qualifier.as_deref().and_then(|q| resolve(q, &c.name)) {
                    c.qualifier = None;
                    c.name = uniquified[pos].clone();
                } else if c.qualifier.is_none() {
                    // N10-lite: a bare duplicate-name ref binds through the
                    // reprojected wrap by identity — see `bare_dup_slot`'s
                    // doc for the governing invariant.
                    if let Some(k) = bare_dup_slot(c, schema) {
                        c.name = uniquified[k].clone();
                    }
                }
            }
            Expression::UnresolvedColumn(u) => {
                if let Some(pos) = u.qualifier.as_deref().and_then(|q| resolve(q, &u.name)) {
                    u.qualifier = None;
                    u.name = uniquified[pos].clone();
                }
            }
            other => {
                for child in other.children_mut() {
                    walk(child, scope, schema, uniquified);
                }
            }
        }
    }
    let mut rewritten = expr.clone();
    walk(
        &mut rewritten,
        &input.scope,
        &input.resolved_schema,
        uniquified,
    );
    rewritten
}

/// Reproject `expr`'s qualifiers against the pre-wrap `input` when the wrap
/// boundary reshaped output names (`Some`), else clone it unchanged (`None`) —
/// the shared body of every wrap-path expression rewrite
/// (`build_project`/`build_filter`/`build_sort`/`build_aggregate`).
fn reproject_or_clone(
    expr: &Expression,
    input: &TypedAst,
    uniquified: &Option<Vec<String>>,
) -> Expression {
    match uniquified {
        Some(u) => reproject_qualifiers(expr, input, u),
        None => expr.clone(),
    }
}

/// Wrap `unit` as `(…) AS __td_sub`: the reprojected (column-aliased) wrap
/// when the output has a duplicate name (`Some`), the plain wrap otherwise
/// (`None`). The wrap-site counterpart of [`reproject_or_clone`].
fn wrap_maybe_reprojected(unit: SqlUnit, uniquified: &Option<Vec<String>>) -> SelectBlock {
    match uniquified {
        Some(u) => SelectBlock::wrap_reprojected(unit, u),
        None => SelectBlock::wrap(unit),
    }
}

/// Render `build_project`'s merge-path SELECT list: identical to
/// [`render_projection_slots`] EXCEPT a bare unqualified `Star` expands to
/// the merged-into block's hoisted default slot list (F4) instead of a
/// literal `*` — a raw `*` sitting inside a multi-slot list shadows the join
/// builder's hoisted USING-key ordering, the same shadowing `* EXCLUDE`
/// suffers without the F1 fix. A no-op (falls through to
/// `render_projection_slots` verbatim) when there are no default slots to
/// substitute, or no bare star to substitute them for.
fn render_project_merge_slots(
    projections: &[Expression],
    input_schema: &Schema,
    default_slots: Option<&[DefaultSlot]>,
) -> Result<String, EmissionError> {
    let has_bare_star = projections
        .iter()
        .any(|p| matches!(p, Expression::Star(StarExpression { qualifier: None })));
    let Some(slots) = default_slots.filter(|_| has_bare_star) else {
        return render_projection_slots(projections, input_schema);
    };
    let star_sql = slots
        .iter()
        .map(|s| s.sql.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    sql_join(projections.iter(), ", ", |p| {
        if matches!(p, Expression::Star(StarExpression { qualifier: None })) {
            Ok(star_sql.clone())
        } else {
            render_projection_slot(p, input_schema)
        }
    })
}

/// Wrap-boundary star rewrite (F12): a `Star` (`q.*`) has no bare-name
/// equivalent for a reshaped output, so neither the resolution-time
/// unique-name qualifier drop nor `reproject_qualifiers` ever touches one —
/// a stranded relation-qualified star sails through untouched and
/// `render_star` emits `q.*` verbatim over `__td_sub` — a qualifier DuckDB
/// can no longer bind once the wrap buries the pre-wrap block's own FROM
/// alias (`Referenced table "q" not found`). The strand precondition: the
/// PRE-wrap `block` must actually expose `q` (`block.exposes(q)`) — that is
/// precisely what the wrap is about to bury; a `q` the pre-wrap block does
/// NOT expose is a correlated OUTER reference (resolved outward through the
/// wrap by DuckDB's correlated binder) and must stay qualified verbatim.
///
/// Given the strand precondition holds, the rewrite itself is safe only when
/// `q` covers the WHOLE input relation — exactly one
/// [`RelScope`](super::analyzer::RelScope) alias entry named `q`, spanning
/// the full `0..input.resolved_schema.len()` range — because then the wrap's
/// output IS exactly that input's columns, positionally: `q.*` is
/// semantically the bare `*` over `__td_sub`. `q` binding a PARTIAL range
/// (one side of a join) is left untouched: expanding it to bare names could
/// collide with the other side's duplicate names under the wrap (documented
/// residual, un-witnessed — a join side's own alias usually stays exposed
/// through the wrap and so rarely strands here at all). `q` binding 2+
/// ranges is ambiguous and is likewise left untouched.
fn expand_stranded_whole_relation_star(
    expr: &Expression,
    block: &SelectBlock,
    input: &TypedAst,
) -> Expression {
    if let Expression::Star(StarExpression { qualifier: Some(q) }) = expr {
        if block.exposes(q) {
            let full = 0..input.resolved_schema.len();
            let mut matching = input
                .scope
                .aliases
                .iter()
                .filter(|(name, _)| name.eq_ignore_ascii_case(q));
            if let (Some((_, range)), None) = (matching.next(), matching.next()) {
                if *range == full {
                    return Expression::Star(StarExpression { qualifier: None });
                }
            }
        }
    }
    expr.clone()
}

fn build_project(input: &TypedAst, projections: &[Expression]) -> Result<SqlUnit, EmissionError> {
    // A lone unqualified `*` is a pure identity projection: return the child
    // unit verbatim (ADR-001 cosmetic "SELECT * over SELECT *" collapse).
    // This subsumes the former USING-join delegate branch — a `*` over a
    // USING join yields the join renderer's explicit hoisted slot list,
    // which is exactly the column order the resolved schema declares; plain
    // ON/CROSS joins expand `*` left-then-right, which already matches.
    if is_unqualified_star_only(projections) {
        return build_unit(&input.op, &input.resolved_schema);
    }
    let mut block = open_block(input)?;
    if block.can_accept(Clause::Select) {
        if let Some(rewritten) = requalify_visible(projections, &block, input) {
            let slots_sql = render_project_merge_slots(
                &rewritten,
                &input.resolved_schema,
                block.default_slots(),
            )?;
            block.set_projections(slots_sql);
            return Ok(block.into());
        }
    }
    let uniquified = output_uniquified(&input.resolved_schema);
    let projections: Vec<Expression> = projections
        .iter()
        .map(|p| expand_stranded_whole_relation_star(p, &block, input))
        .map(|p| reproject_or_clone(&p, input, &uniquified))
        .collect();
    let mut wrapped = wrap_maybe_reprojected(block.into(), &uniquified);
    wrapped.set_projections(render_projection_slots(
        &projections,
        &input.resolved_schema,
    )?);
    Ok(wrapped.into())
}

fn build_filter(input: &TypedAst, condition: &Expression) -> Result<SqlUnit, EmissionError> {
    let mut block = open_block(input)?;
    if block.can_accept(Clause::Where) {
        if let Some(rewritten) = requalify_visible([condition], &block, input) {
            block.push_where(render_expr(&rewritten[0], &input.resolved_schema)?);
            return Ok(block.into());
        }
    }
    let uniquified = output_uniquified(&input.resolved_schema);
    let condition = reproject_or_clone(condition, input, &uniquified);
    let mut wrapped = wrap_maybe_reprojected(block.into(), &uniquified);
    wrapped.push_where(render_expr(&condition, &input.resolved_schema)?);
    Ok(wrapped.into())
}

fn build_sort(
    input: &TypedAst,
    order: &[SortOrder],
    limit: Option<i64>,
    offset: Option<i64>,
) -> Result<SqlUnit, EmissionError> {
    let mut block = open_block(input)?;
    // Over an occupied SELECT list, only bare output-name references merge:
    // ORDER BY resolves them against the select list's aliases; expression
    // keys would re-bind against FROM columns and could diverge. Over a
    // select-free block, the fused merge-path rewrite additionally binds any
    // bare duplicate-name ordinal key through its unique covering alias.
    let merged_keys: Option<Vec<Expression>> = if block.select_free() {
        requalify_visible(order.iter().map(|so| so.expr.as_ref()), &block, input)
    } else {
        let keys_bind = order.iter().all(|so| {
            matches!(so.expr.as_ref(),
                Expression::ColumnReference(c)
                    if c.qualifier.is_none()
                        && bare_dup_slot(c, &input.resolved_schema).is_none())
        });
        keys_bind.then(|| order.iter().map(|so| (*so.expr).clone()).collect())
    };
    if block.can_accept(Clause::OrderBy) && block.distinct_allows_order() {
        if let Some(exprs) = merged_keys {
            let keys = order
                .iter()
                .zip(exprs)
                .map(|(so, expr)| {
                    let mut reprojected = so.clone();
                    *reprojected.expr = expr;
                    render_sort_key(&reprojected, &input.resolved_schema)
                })
                .collect::<Result<Vec<_>, _>>()?;
            block.set_order_by(keys, limit, offset);
            return Ok(block.into());
        }
    }
    let uniquified = output_uniquified(&input.resolved_schema);
    let keys = order
        .iter()
        .map(|so| {
            let mut reprojected = so.clone();
            *reprojected.expr = reproject_or_clone(&so.expr, input, &uniquified);
            render_sort_key(&reprojected, &input.resolved_schema)
        })
        .collect::<Result<Vec<_>, _>>()?;
    let mut wrapped = wrap_maybe_reprojected(block.into(), &uniquified);
    wrapped.set_order_by(keys, limit, offset);
    Ok(wrapped.into())
}

fn build_limit(
    input: &TypedAst,
    limit: i64,
    offset: Option<i64>,
) -> Result<SqlUnit, EmissionError> {
    let mut block = open_block(input)?;
    if !block.can_accept(Clause::LimitOffset) {
        block = SelectBlock::wrap(block.into());
    }
    block.set_limit(limit, offset);
    Ok(block.into())
}

fn build_deduplicate(input: &TypedAst, on_columns: &[String]) -> Result<SqlUnit, EmissionError> {
    let mut block = open_block(input)?;
    if on_columns.is_empty() {
        if !block.can_accept(Clause::Distinct) {
            block = SelectBlock::wrap(block.into());
        }
        block.set_distinct(DistinctKind::Distinct);
    } else {
        let cols = sql_join(on_columns.iter(), ", ", |c| Ok(quote_ident(c).into_owned()))?;
        // DISTINCT ON picks an arbitrary representative row per key; it only
        // merges into a star block (no computed select list, no prior
        // DISTINCT) so the row-choice surface stays identical to today's
        // wrapped form.
        if !(block.can_accept(Clause::Distinct) && block.select_free()) {
            block = SelectBlock::wrap(block.into());
        }
        block.set_distinct(DistinctKind::DistinctOn(cols));
    }
    Ok(block.into())
}

fn build_aliased_relation(input: &TypedAst, alias: &str) -> Result<SqlUnit, EmissionError> {
    // `df.alias("e")` over a bare table scan collapses to `FROM emp AS e`;
    // anything else becomes a derived table under the user alias. Either
    // way the alias is the block's whole scope — the analyzer's RelScope
    // for AliasedRelation binds exactly this alias.
    let item = if let TypedOp::TableScan { table, alias: None } = &input.op {
        FromItem::Relation {
            base: table.clone(),
            alias: Some(alias.to_owned()),
        }
    } else {
        FromItem::Derived {
            unit: Box::new(build_unit(&input.op, &input.resolved_schema)?),
            alias: alias.to_owned(),
        }
    };
    Ok(SqlUnit::from(SelectBlock::from_item(item)))
}

/// The pre-SELECT-block string renderers, one arm per not-yet-converted
/// operator. Arms migrate from here into [`build_unit`] phase by phase;
/// the match stays exhaustive so a new `TypedOp` variant is a compile error.
fn legacy_render(op: &TypedOp, schema: &Schema) -> Result<String, EmissionError> {
    let result: Result<String, EmissionError> = match op {
        // ── C.1 wired ────────────────────────────────────────────────────
        TypedOp::SingleRow => render_single_row(),
        TypedOp::Unpivot {
            input,
            ids,
            values,
            variable_column_name,
            value_column_name,
        } => render_unpivot(input, ids, values, variable_column_name, value_column_name),
        TypedOp::Describe { input, cols } => render_describe(input, cols),
        TypedOp::Summary {
            input,
            cols,
            statistics,
        } => render_summary(input, cols, statistics),
        TypedOp::FreqItems {
            input,
            cols,
            support,
        } => render_freq_items(input, cols, *support),
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
        TypedOp::Sample {
            input,
            lower_bound,
            upper_bound,
            with_replacement,
            seed,
        } => render_sample(input, *lower_bound, *upper_bound, *with_replacement, *seed),
        TypedOp::SampleBy {
            input,
            col,
            fractions,
            seed,
        } => render_sample_by(input, col, fractions, *seed),

        TypedOp::RecursiveCte {
            name,
            anchor,
            recursive_term,
        } => render_recursive_cte(name, anchor, recursive_term, schema),

        // ── future τ work owns (analyzer PuntedOperator today; defensive) ──────
        TypedOp::Unnest { .. } => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::Op,
            name: "Unnest".to_owned(),
            reason: "unnest emission (not implemented in τ)".to_owned(),
        }),

        // Converted to the SELECT-block builder — reaching a legacy arm for
        // these is a `build_unit` dispatch bug.
        TypedOp::Values { .. }
        | TypedOp::LocalRelation { .. }
        | TypedOp::FileScan { .. }
        | TypedOp::TableFunction { .. }
        | TypedOp::TableScan { .. }
        | TypedOp::Project { .. }
        | TypedOp::Filter { .. }
        | TypedOp::Sort { .. }
        | TypedOp::Limit { .. }
        | TypedOp::Deduplicate { .. }
        | TypedOp::AliasedRelation { .. }
        | TypedOp::Join { .. }
        | TypedOp::Aggregate { .. }
        | TypedOp::LateralView { .. }
        | TypedOp::SetOp { .. }
        | TypedOp::WithColumns { .. }
        | TypedOp::DropColumns { .. }
        | TypedOp::WithColumnsRenamed { .. }
        | TypedOp::NaFill { .. }
        | TypedOp::NaDrop { .. }
        | TypedOp::NaReplace { .. } => {
            unreachable!("block-composable operator routed to legacy_render: {op:?}")
        }
    };
    result
}

// ── Operator renderers ───────────────────────────────────────────────────────

/// Render each item with `f` and join the results with `sep`. Fallible
/// equivalent of `Itertools::join` — replaces the hand-rolled
/// `if i > 0 { push_str(sep) }` loops throughout this file. Output is
/// byte-identical to those loops (`Vec::join` inserts `sep` between elements
/// only).
fn sql_join<T>(
    items: impl IntoIterator<Item = T>,
    sep: &str,
    f: impl FnMut(T) -> Result<String, EmissionError>,
) -> Result<String, EmissionError> {
    let parts = items
        .into_iter()
        .map(f)
        .collect::<Result<Vec<String>, EmissionError>>()?;
    Ok(parts.join(sep))
}

fn render_single_row() -> Result<String, EmissionError> {
    // DuckDB requires a subquery to have a projection list — bare `SELECT`
    // parses at top-level but fails inside `FROM (...)`. Emit `SELECT 1` so
    // `SingleRow` is subquery-safe under `Project` (which wraps the Raw unit
    // as `SELECT expr FROM (<child>) AS __td_sub` — the placeholder column is
    // unused because Project provides its own SELECT list). The analyzer
    // stamps SingleRow with an empty schema; no legitimate operator resolves
    // the placeholder column from downstream code, so its presence is inert.
    Ok("SELECT 1".to_owned())
}

/// A leaf block over a `(VALUES …) AS <alias>(cols)` derived table — the
/// shared shape of `Values` and non-empty `LocalRelation`. The alias is
/// exposed truthfully in the block scope; nothing qualifies through it
/// today (both ops have an empty `RelScope`).
fn values_leaf_block(rendered_rows: &str, alias: &str, cols: &str) -> SqlUnit {
    SelectBlock::from_item(FromItem::Raw {
        sql: format!("(VALUES {rendered_rows}) AS {alias}({cols})"),
        exposed: vec![alias.to_owned()],
    })
    .into()
}

fn build_values(
    rows: &[Vec<Expression>],
    column_names: &[String],
    schema: &Schema,
) -> Result<SqlUnit, EmissionError> {
    if rows.is_empty() {
        bail_boundary_op!("Values", "empty VALUES relations are not supported");
    }
    let rendered_rows = sql_join(rows.iter(), ", ", |row| {
        let cells = sql_join(row.iter(), ", ", |cell| render_expr(cell, schema))?;
        Ok(format!("({cells})"))
    })?;
    let cols = sql_join(column_names.iter(), ", ", |c| {
        Ok(quote_ident(c).into_owned())
    })?;
    Ok(values_leaf_block(&rendered_rows, "__td_values", &cols))
}

fn build_local_relation(
    schema_decl: &StructType,
    rows: &[Vec<Expression>],
) -> Result<SqlUnit, EmissionError> {
    if schema_decl.fields.is_empty() {
        bail_boundary_op!(
            "LocalRelation",
            "LocalRelation with empty schema is not representable"
        );
    }
    // Special case: no rows → emit an empty relation with the correct schema.
    // This is a genuine SELECT (no FROM item exists), so it stays a Raw unit.
    if rows.is_empty() {
        // `SELECT CAST(NULL AS T) AS c, ... WHERE 1=0` — zero rows, right shape.
        let cols = sql_join(schema_decl.fields.iter(), ", ", |f| {
            let ty = render_data_type(&f.data_type);
            let name = quote_ident(&f.name);
            Ok(format!("CAST(NULL AS {ty}) AS {name}"))
        })?;
        return Ok(SqlUnit::Raw(format!("SELECT {cols} WHERE 1=0")));
    }
    // `render_expr` needs a `&Schema` (`ResolvedSchema`); `schema_decl` stays a
    // plain `StructType` (it's the declared row shape, not a resolved
    // relation), so bridge once here via the sanctioned `minted` door — row
    // cells are literal expressions that never read column identity, so the
    // freshly minted ids are inert scratch, not a laundering site.
    let cell_schema = Schema::minted(schema_decl.clone());
    let rendered_rows = sql_join(rows.iter(), ", ", |row| {
        let cells = sql_join(row.iter().enumerate(), ", ", |(idx, cell)| {
            let inner = render_expr(cell, &cell_schema)?;
            // Ensure each cell carries the declared type — a naked NULL literal
            // would otherwise adopt DuckDB's inferred column type across rows.
            let field = &schema_decl.fields[idx];
            let ty = render_data_type(&field.data_type);
            Ok(format!("CAST({inner} AS {ty})"))
        })?;
        Ok(format!("({cells})"))
    })?;
    let cols = sql_join(schema_decl.fields.iter(), ", ", |f| {
        Ok(quote_ident(&f.name).into_owned())
    })?;
    Ok(values_leaf_block(&rendered_rows, "__td_local", &cols))
}

/// Build the DuckDB reader-call SQL fragment for a file scan, e.g.
/// `read_parquet('/path/to/file.parquet')` or
/// `read_csv(['/a.csv', '/b.csv'], header='true')`.
///
/// This is the shared core between [`render_file_scan`] (which wraps it in
/// `SELECT * FROM ...`) and the schema-less Parquet discovery path in
/// `connect-server`'s `resolve_implicit_pivots` (which wraps it in
/// `SELECT * FROM ... LIMIT 0`).
pub fn build_file_reader_sql(
    format: FileFormat,
    paths: &[String],
    options: &[(String, String)],
) -> Result<String, EmissionError> {
    if paths.is_empty() {
        bail_boundary_op!("FileScan", "FileScan requires at least one path");
    }
    let paths_sql = if paths.len() == 1 {
        format!("'{}'", escape_sql_string(&paths[0]))
    } else {
        let items = sql_join(paths.iter(), ", ", |p| {
            Ok(format!("'{}'", escape_sql_string(p)))
        })?;
        format!("[{items}]")
    };
    let reader = match format {
        FileFormat::Parquet => "read_parquet",
        FileFormat::Csv => "read_csv",
        FileFormat::Json => "read_json",
        FileFormat::Orc => {
            bail_boundary_op!(
                "FileScan[Orc]",
                "ORC file scanning is not supported by DuckDB"
            );
        }
        FileFormat::Delta => {
            if paths.len() > 1 {
                bail_boundary_op!(
                    "FileScan[Delta]",
                    "Delta Lake tables are a single directory; multiple paths are not supported"
                );
            }
            "delta_scan"
        }
    };
    let opts_sql = if options.is_empty() {
        String::new()
    } else {
        let opts = sql_join(options.iter(), ", ", |(k, v)| {
            Ok(format!("{k}='{}'", escape_sql_string(v)))
        })?;
        format!(", {opts}")
    };
    Ok(format!("{reader}({paths_sql}{opts_sql})"))
}

fn build_file_scan(
    format: FileFormat,
    paths: &[String],
    options: &[(String, String)],
) -> Result<SqlUnit, EmissionError> {
    let reader_call = build_file_reader_sql(format, paths, options)?;
    // The reader call is a FROM-item generator: parents merge onto it
    // (`SELECT cols FROM read_parquet(…) WHERE …`) instead of wrapping.
    // No alias is exposed — a FileScan's RelScope is empty; nothing
    // qualifies through it.
    Ok(SelectBlock::from_item(FromItem::Raw {
        sql: reader_call,
        exposed: Vec::new(),
    })
    .into())
}

/// True iff `projections` is exactly one unqualified `*` (i.e. `SELECT *`),
/// whose emitted column order is delegated to the FROM clause rather than an
/// explicit slot list.
fn is_unqualified_star_only(projections: &[Expression]) -> bool {
    matches!(
        projections,
        [Expression::Star(StarExpression { qualifier: None })]
    )
}

fn render_projection_slots(
    projections: &[Expression],
    input_schema: &Schema,
) -> Result<String, EmissionError> {
    if projections.is_empty() {
        return Ok("*".to_owned());
    }
    sql_join(projections.iter(), ", ", |p| {
        render_projection_slot(p, input_schema)
    })
}

/// Map a [`JoinType`] to its DuckDB join keyword. DuckDB requires `SEMI JOIN`
/// / `ANTI JOIN` WITHOUT the `LEFT` prefix (checklist §5 / CLAUDE.md Known
/// Gotcha #5) — this is the single site for that mapping.
fn join_kind_sql(join_type: crate::transpiler_v2::ast::JoinType) -> &'static str {
    use super::ast::JoinType;
    match join_type {
        JoinType::Inner => "INNER JOIN",
        JoinType::Left => "LEFT OUTER JOIN",
        JoinType::Right => "RIGHT OUTER JOIN",
        JoinType::Full => "FULL OUTER JOIN",
        JoinType::Cross => "CROSS JOIN",
        JoinType::LeftSemi => "SEMI JOIN",
        JoinType::LeftAnti => "ANTI JOIN",
    }
}

/// Build the join clause (leading space included). USING wins over ON when
/// both are present per Spark semantics (Spark's `on="col"` maps to USING);
/// a CROSS join takes no clause; any other join without ON/USING is a
/// Thunderduck-boundary error. `cond_schema` resolves the ON-condition
/// expression — the one axis on which the two join renderers differ.
fn render_join_clause(
    join_type: crate::transpiler_v2::ast::JoinType,
    condition: Option<&Expression>,
    using_columns: &[String],
    cond_schema: &Schema,
) -> Result<String, EmissionError> {
    use super::ast::JoinType;
    if !using_columns.is_empty() {
        let cols = sql_join(using_columns, ", ", |c| Ok(quote_ident(c).into_owned()))?;
        Ok(format!(" USING ({cols})"))
    } else if let Some(cond) = condition {
        let cond_sql = render_expr(cond, cond_schema)?;
        Ok(format!(" ON {cond_sql}"))
    } else if matches!(join_type, JoinType::Cross) {
        Ok(String::new())
    } else {
        bail_boundary_op!("Join", "non-cross join without ON or USING clause");
    }
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

// ── Unwired renderer (Decision 13-A) ─────────────────────────────────────────
//
// `render_tail` is the one remaining renderer without a `TypedOp` sink in τ's
// analyzer substrate. It exists so the §5.4 CTE anchor test lives in code
// today; its former Decision 13-A siblings are all wired via `dispatch_op`.

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

/// Render a `RecursiveCte`:
/// `WITH RECURSIVE {name}({cols}) AS ({anchor_sql} UNION ALL {rec_sql}) SELECT * FROM {name}`.
fn render_recursive_cte(
    name: &str,
    anchor: &TypedAst,
    recursive_term: &TypedAst,
    schema: &Schema,
) -> Result<String, EmissionError> {
    let anchor_sql = dispatch_op(&anchor.op, &anchor.resolved_schema)?;
    let recursive_sql = dispatch_op(&recursive_term.op, &recursive_term.resolved_schema)?;

    let quoted_name = quote_ident(name);
    let col_list: String = schema
        .fields
        .iter()
        .map(|f| quote_ident(&f.name).into_owned())
        .collect::<Vec<_>>()
        .join(", ");

    Ok(format!(
        "WITH RECURSIVE {quoted_name}({col_list}) AS ({anchor_sql} UNION ALL {recursive_sql}) SELECT * FROM {quoted_name}"
    ))
}

/// Render a `SetOp` (UNION / INTERSECT / EXCEPT). Each child is wrapped
/// with a per-column `CAST(col AS <widened_type>)` projection so the union'd
/// column types match the analyzer's widened schema (per ADR-006 refinement +
/// Open Decision 5). `UNION BY NAME` is deferred (analyzer surfaces it as
/// `PuntedOperator`); it never reaches this renderer.
fn build_set_op(
    kind: crate::transpiler_v2::ast::SetOpKind,
    all: bool,
    by_name: bool,
    allow_missing_columns: bool,
    children: &[TypedAst],
    widened_schema: &Schema,
) -> Result<SqlUnit, EmissionError> {
    use super::ast::SetOpKind;
    if children.is_empty() {
        bail_boundary_op!("SetOp", "set-op with no children");
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
            bail_boundary_op!(
                format!("SetOp[{kind:?} BY NAME]"),
                "DuckDB supports BY NAME only for UNION",
            );
        }
    };
    let mut parts: Vec<String> = Vec::with_capacity(children.len());
    for child in children {
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
        let slots = sql_join(
            widened_schema.fields.iter().enumerate(),
            ", ",
            |(i, widened_field)| {
                // Resolve this child's source column for the widened slot:
                //   - by-name + allow_missing_columns: optional (missing
                //     names pad with NULL);
                //   - by-name (strict): required — children have identical
                //     name SETS (analyzer verified);
                //   - by-position: positional index — children have
                //     identical arity (analyzer verified).
                let child_field = if by_name && allow_missing_columns {
                    child.resolved_schema.field_by_name(&widened_field.name)
                } else if by_name {
                    Some(
                        child
                            .resolved_schema
                            .fields
                            .iter()
                            .find(|f| f.name.eq_ignore_ascii_case(&widened_field.name))
                            .expect("analyzer guaranteed name match"),
                    )
                } else {
                    child.resolved_schema.fields.get(i)
                };
                let ty = render_data_type(&widened_field.data_type);
                let widened_name = quote_ident(&widened_field.name);
                Ok(match child_field {
                    Some(cf) => {
                        let col = quote_ident(&cf.name);
                        format!("CAST({col} AS {ty}) AS {widened_name}")
                    }
                    None => format!("CAST(NULL AS {ty}) AS {widened_name}"),
                })
            },
        )?;
        // Every child is a block barrier: the cast list references the
        // child's OUTPUT names, which are unambiguous only across a
        // derived-table boundary (a join child's FROM scope could bind the
        // same unqualified name on both sides).
        let child_unit = build_unit(&child.op, &child.resolved_schema)?;
        let mut cast_block = SelectBlock::wrap(child_unit);
        cast_block.set_projections(slots);
        parts.push(cast_block.to_sql());
    }
    // The chain stays a bare `a UNION b …` Raw unit; a parent embedding it
    // as a FROM item adds the parentheses via its Derived wrap.
    Ok(SqlUnit::Raw(parts.join(&format!(" {op_kw} "))))
}

fn build_with_columns(
    input: &TypedAst,
    assignments: &[(String, Expression)],
) -> Result<SqlUnit, EmissionError> {
    let input_schema = &input.resolved_schema;
    // Column-order contract with the analyzer is single-homed in
    // [`with_columns_plan`] (see its doc): input columns emit in their
    // original positions (replaced in place if named by an assignment), and
    // net-new assignments append at the end in assignment order — the
    // analyzer builds the resolved schema from the same plan, so the SELECT
    // slots and the advertised schema stay aligned by construction.
    let plan = with_columns_plan(input_schema, assignments);
    block_with_projections(
        input,
        |block| exprs_visible_in(assignments.iter().map(|(_, e)| e), block, &input.scope),
        |_block, _wrapped| {
            // Post strip-removal WithColumns no longer needs the wrap-vs-merge
            // distinction: assignments render identically on either path.
            let render_assignment = |expr: &Expression| render_expr(expr, input_schema);
            let mut slots: Vec<String> = Vec::new();
            for (f, replaced_by) in input_schema.fields.iter().zip(&plan.replaced) {
                if let Some(idx) = replaced_by {
                    let (_, expr) = &assignments[*idx];
                    let expr_sql = render_assignment(expr)?;
                    let name_q = quote_ident(&f.name);
                    slots.push(format!("{expr_sql} AS {name_q}"));
                } else {
                    slots.push(quote_ident(&f.name).into_owned());
                }
            }
            for &i in &plan.appended {
                let (name, expr) = &assignments[i];
                let expr_sql = render_assignment(expr)?;
                let name_q = quote_ident(name);
                slots.push(format!("{expr_sql} AS {name_q}"));
            }
            Ok(slots.join(", "))
        },
    )
}

fn build_drop_columns(input: &TypedAst, drop_names: &[String]) -> Result<SqlUnit, EmissionError> {
    let dropped = sql_join(drop_names.iter(), ", ", |n| Ok(quote_ident(n).into_owned()))?;
    block_with_projections(
        input,
        |_| true,
        |block, wrapped| {
            // A merging (un-wrapped) block whose hoisted default slots are
            // still live can filter them directly by name (F1) — `* EXCLUDE`
            // over a USING join lets DuckDB keep the excluded set's sibling
            // column at ITS natural (non-hoisted) FROM position, silently
            // un-hoisting the USING key. A wrapped child already rendered its
            // own defaults inside `__td_sub`'s `*`, so `* EXCLUDE` there is
            // correct as-is.
            if !wrapped {
                if let Some(slots) = block.default_slots() {
                    let remaining: Vec<&str> = slots
                        .iter()
                        .filter(|s| !drop_names.iter().any(|d| d.eq_ignore_ascii_case(&s.name)))
                        .map(|s| s.sql.as_str())
                        .collect();
                    if !remaining.is_empty() {
                        return Ok(remaining.join(", "));
                    }
                }
            }
            Ok(format!("* EXCLUDE ({dropped})"))
        },
    )
}

/// Render `df.na.fill(values, subset=cols)`. For each column in the input
/// schema, if it's in `cols` (or `cols` is empty and it's the first value's
/// compatible type), emit `COALESCE(col, value) AS col`; else pass through.
/// Single-value form (`values.len()==1`) applies that value to all cols in
/// the subset. Per-column form (`values.len()==cols.len()`) pairs
/// position-wise.
fn build_na_fill(
    input: &TypedAst,
    cols: &[String],
    values: &[Expression],
) -> Result<SqlUnit, EmissionError> {
    let input_schema = &input.resolved_schema;
    if values.is_empty() {
        bail_boundary_op!("NaFill", "NaFill requires at least one fill value");
    }
    // Per-column fill selection — shared with `analyze_na_fill` via
    // [`na_fill_value_for`] so the stamped schema and the emitted SQL apply
    // the identical Spark `fillValue` contract (type-incompatible columns
    // pass through untouched; a mixed-type COALESCE would be a DuckDB
    // binder error, whereas Spark silently skips such columns).
    let value_for = |col_name: &str, col_type: &DataType| -> Option<&Expression> {
        na_fill_value_for(cols, values, input_schema, col_name, col_type)
    };
    let slots = sql_join(input_schema.fields.iter(), ", ", |f| {
        let name_q = quote_ident(&f.name);
        if let Some(v) = value_for(&f.name, &f.data_type) {
            let v_sql = render_expr(v, input_schema)?;
            Ok(format!("COALESCE({name_q}, {v_sql}) AS {name_q}"))
        } else {
            Ok(name_q.into_owned())
        }
    })?;
    block_with_projections(input, |_| true, |_, _| Ok(slots))
}

/// Render `df.na.drop(how, subset, thresh)`. `min_non_nulls=None` means
/// how="any" (drop if ANY subset col is null); `Some(1)` means how="all"
/// (drop only if ALL subset cols are null); other values are Spark's
/// `thresh` semantic.
fn build_na_drop(
    input: &TypedAst,
    cols: &[String],
    min_non_nulls: Option<i32>,
) -> Result<SqlUnit, EmissionError> {
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
        // Nothing to test — the operator is an identity.
        return build_unit(&input.op, input_schema);
    }
    let condition = if let Some(thresh) = min_non_nulls {
        // Row kept iff at least `thresh` of subset cols are non-null.
        // Emit: (CAST(col1 IS NOT NULL AS INT) + ... ) >= thresh.
        let sum = sql_join(subset.iter(), " + ", |c| {
            let q = quote_ident(c);
            Ok(format!("CAST({q} IS NOT NULL AS INTEGER)"))
        })?;
        format!("({sum}) >= {thresh}")
    } else {
        // how="any": all subset cols must be non-null.
        sql_join(subset.iter(), " AND ", |c| {
            let q = quote_ident(c);
            Ok(format!("{q} IS NOT NULL"))
        })?
    };
    let mut block = open_block(input)?;
    if !block.can_accept(Clause::Where) {
        block = SelectBlock::wrap(block.into());
    }
    block.push_where(condition);
    Ok(block.into())
}

/// Render `df.na.replace([old_vals], [new_vals], subset=cols)`. Emit
/// `SELECT CASE WHEN col = old1 THEN new1 ... ELSE col END AS col` for each
/// column in subset (or all cols if empty).
fn build_na_replace(
    input: &TypedAst,
    cols: &[String],
    replacements: &[(Expression, Expression)],
) -> Result<SqlUnit, EmissionError> {
    let input_schema = &input.resolved_schema;
    let in_subset = |name: &str| -> bool {
        cols.is_empty() || cols.iter().any(|c| c.eq_ignore_ascii_case(name))
    };
    let slots = sql_join(input_schema.fields.iter(), ", ", |f| {
        let name_q = quote_ident(&f.name);
        if in_subset(&f.name) && !replacements.is_empty() {
            let mut case = String::from("CASE ");
            for (old, new) in replacements {
                let old_sql = render_expr(old, input_schema)?;
                let new_sql = render_expr(new, input_schema)?;
                case.push_str(&format!("WHEN {name_q} = {old_sql} THEN {new_sql} "));
            }
            case.push_str(&format!("ELSE {name_q} END AS {name_q}"));
            Ok(case)
        } else {
            Ok(name_q.into_owned())
        }
    })?;
    block_with_projections(input, |_| true, |_, _| Ok(slots))
}

/// Render `df.unpivot(ids, values, var_col, val_col)`.
///
/// Emits DuckDB's `UNPIVOT` shape:
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
        bail_boundary_op!("Unpivot", "unpivot requires at least one value column");
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let var_col = quote_ident(variable_column_name);
    let val_col = quote_ident(value_column_name);

    // Pre-select only `ids + values` so DuckDB doesn't fold extra input
    // columns into the id set.
    let select_list = sql_join(ids.iter().chain(values.iter()), ", ", |c| {
        Ok(quote_ident(c).into_owned())
    })?;
    let value_cols = sql_join(values.iter(), ", ", |c| Ok(quote_ident(c).into_owned()))?;

    Ok(format!(
        "UNPIVOT (SELECT {select_list} FROM ({child_sql}) AS __td_unpivot_src) ON {value_cols} INTO NAME {var_col} VALUE {val_col}"
    ))
}

/// Render `df.describe(cols...)` — a `UNION ALL` of five aggregate rows
/// (`count`, `mean`, `stddev`, `min`, `max`) over the materialised `cols`.
fn render_describe(input: &TypedAst, cols: &[String]) -> Result<String, EmissionError> {
    const DESCRIBE_STATS: &[&str] = &["count", "mean", "stddev", "min", "max"];
    let stats: Vec<String> = DESCRIBE_STATS.iter().map(|s| (*s).to_owned()).collect();
    render_stats_union(input, cols, &stats)
}

/// Render `df.summary(statistics...)` — a `UNION ALL` of one aggregate row
/// per statistic over the full input column list. Analyzer materialises
/// both `cols` (always full schema) and `statistics` (empty ⇒
/// `DEFAULT_SUMMARY_STATS`) before this arm sees them.
fn render_summary(
    input: &TypedAst,
    cols: &[String],
    statistics: &[String],
) -> Result<String, EmissionError> {
    render_stats_union(input, cols, statistics)
}

/// Render `df.stat.freqItems(cols, support)` — one `ARRAY<T>` output column
/// per input col via a correlated `LIST(...)` subquery filtered by
/// `HAVING COUNT(*) >= support * total_rows`, matching Spark's contract.
///
/// Emission shape:
/// ```sql
/// WITH __freq_input__ AS MATERIALIZED (<child>)
/// SELECT
///   (SELECT LIST("col" ORDER BY "col") FROM (
///      SELECT "col", COUNT(*) AS __cnt FROM __freq_input__
///      WHERE "col" IS NOT NULL GROUP BY "col"
///      HAVING COUNT(*) >= <support> * (SELECT COUNT(*) FROM __freq_input__)
///   )) AS "col_freqItems",
///   ...
/// ```
///
/// The `AS MATERIALIZED` CTE prevents multi-scan of the child (Pass 80 lesson).
fn render_freq_items(
    input: &TypedAst,
    cols: &[String],
    support: f64,
) -> Result<String, EmissionError> {
    if cols.is_empty() {
        // Defensive guard — PySpark client rejects empty cols on the client
        // side, but keep the emission stage honest.
        bail_boundary_op!("FreqItems", "freqItems requires at least one column");
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let subqueries: Vec<String> = cols
        .iter()
        .map(|col| {
            let qcol = quote_ident(col);
            let alias_name = format!("{col}_freqItems");
            let qalias = quote_ident(&alias_name);
            // Use f64 debug formatting (`{support:?}`): `{:?}` preserves the
            // trailing `.0` on whole-number f64 values (e.g. `1.0` vs `1`),
            // which DuckDB parses correctly in both cases; using Debug form
            // avoids ambiguity and keeps small values round-trip lossless.
            format!(
                "(SELECT LIST({qcol} ORDER BY {qcol}) FROM (\
                 SELECT {qcol}, COUNT(*) AS __cnt FROM __freq_input__ \
                 WHERE {qcol} IS NOT NULL GROUP BY {qcol} \
                 HAVING COUNT(*) >= {support:?} * (SELECT COUNT(*) FROM __freq_input__)\
                 )) AS {qalias}"
            )
        })
        .collect();
    let projections = subqueries.join(", ");
    Ok(format!(
        "WITH __freq_input__ AS MATERIALIZED ({child_sql}) SELECT {projections}"
    ))
}

/// Shared helper for Describe/Summary emission. Emits
/// `WITH __stats_input__ AS (<child>) SELECT '<stat>' AS summary, <agg>...
/// FROM __stats_input__ UNION ALL ...` with one row per statistic.
fn render_stats_union(
    input: &TypedAst,
    cols: &[String],
    stats: &[String],
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let rows: Vec<String> = stats
        .iter()
        .map(|stat| {
            let col_exprs: Vec<String> = cols
                .iter()
                .map(|col| {
                    let q = quote_ident(col);
                    format!("{} AS {q}", stat_to_agg_expr(stat, &q))
                })
                .collect();
            let summary_lit = sql_string_literal(stat);
            let cols_sql = if col_exprs.is_empty() {
                String::new()
            } else {
                format!(", {}", col_exprs.join(", "))
            };
            format!("SELECT {summary_lit} AS summary{cols_sql} FROM __stats_input__")
        })
        .collect();
    Ok(format!(
        "WITH __stats_input__ AS MATERIALIZED ({child_sql}) {}",
        rows.join(" UNION ALL ")
    ))
}

/// Map a statistic name to the aggregate SQL expression for a single column.
///
/// Percentile stats emit `quantile_disc(TRY_CAST(col AS DOUBLE), frac)`
/// (function-call form) instead of `PERCENTILE_DISC WITHIN GROUP (ORDER BY
/// ...)` — same DuckDB function chosen for τ's `percentile_approx` at
/// Pass 74 (`emission.rs::render_function_call`).
///
/// Uses `TRY_CAST(col AS DOUBLE)` for numeric aggregates so that non-numeric
/// columns return NULL instead of erroring (matches Spark's behaviour).
fn stat_to_agg_expr(stat: &str, quoted_col: &str) -> String {
    match stat {
        "count" => format!("CAST(COUNT({quoted_col}) AS VARCHAR)"),
        "mean" => format!("CAST(AVG(TRY_CAST({quoted_col} AS DOUBLE)) AS VARCHAR)"),
        "stddev" => format!("CAST(STDDEV_SAMP(TRY_CAST({quoted_col} AS DOUBLE)) AS VARCHAR)"),
        "min" => format!("CAST(MIN({quoted_col}) AS VARCHAR)"),
        "max" => format!("CAST(MAX({quoted_col}) AS VARCHAR)"),
        "count_distinct" => format!("CAST(COUNT(DISTINCT {quoted_col}) AS VARCHAR)"),
        "approx_count_distinct" => {
            format!("CAST(APPROX_COUNT_DISTINCT({quoted_col}) AS VARCHAR)")
        }
        s if s.ends_with('%') => match s.trim_end_matches('%').parse::<f64>() {
            Ok(p) => {
                let frac = p / 100.0;
                format!(
                    "CAST(quantile_disc(TRY_CAST({quoted_col} AS DOUBLE), {frac:.17}) AS VARCHAR)"
                )
            }
            Err(_) => "CAST(NULL AS VARCHAR)".to_owned(),
        },
        _ => "CAST(NULL AS VARCHAR)".to_owned(),
    }
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
        bail_boundary_op!("Pivot", "PIVOT requires at least one aggregate expression");
    }
    if pivot_values.is_empty() {
        // Analyzer punts implicit-values as PuntedOperator; defensive guard here.
        bail_boundary_op!(
            "Pivot[implicit-values]",
            "pivot without explicit values requires eager DISTINCT query",
        );
    }
    // Pass 60 M1: output column names for the (pivot_value × aggregate) pairs
    // are stamped by the analyzer into `output_schema.fields[grouping.len()..]`.
    // Emission reads them from the schema rather than re-deriving from the
    // pivot literals — the two derivations MUST stay in lockstep for Spark
    // parity (float `1.0` → `"1.0"`, null rejection, etc.). Single source of
    // truth = the analyzer.
    let expected_output_cols = grouping.len() + pivot_values.len() * aggregates.len();
    if output_schema.fields.len() != expected_output_cols {
        bail_boundary_op!(
            "Pivot",
            format!(
                "output schema arity mismatch: expected {expected_output_cols} fields (grouping={} \
                 + pivot_values×aggregates={}×{}), got {}",
                grouping.len(),
                pivot_values.len(),
                aggregates.len(),
                output_schema.fields.len()
            ),
        );
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let input_schema = &input.resolved_schema;

    // Pivot column — strip any wrapping Alias so the CASE reference is bare.
    let pivot_col_sql = render_expr(pivot_column.unaliased(), input_schema)?;

    // Assemble the SELECT slots: grouping columns first, then one
    // conditional-aggregate slot per (pivot_value, aggregate) pair.
    let mut slots: Vec<String> = Vec::new();
    for g in grouping {
        // Grouping expressions keep any alias; render_projection_slot
        // handles Spark-return casts + alias suffix.
        slots.push(render_projection_slot(g, input_schema)?);
    }

    let mut out_idx = grouping.len();
    for pv in pivot_values {
        // Strip any wrapping Alias so the CASE comparison references the bare
        // value; the alias only carries the output column name (already read
        // from the analyzer-stamped output schema below).
        let pv_sql = render_expr(pv.unaliased(), input_schema)?;
        for a in aggregates {
            let bare_agg = a.unaliased();
            // Read the stamped output name from the analyzer's schema.
            let out_name = &output_schema.fields[out_idx].name;
            out_idx += 1;
            let agg_sql =
                build_conditional_aggregate(bare_agg, &pivot_col_sql, &pv_sql, input_schema)?;
            slots.push(format!("{agg_sql} AS {}", quote_ident(out_name)));
        }
    }
    let slots = slots.join(", ");

    // GROUP BY clause — grouping columns, aliases stripped.
    let mut sql = format!("SELECT {slots} FROM ({child_sql}) AS __td_pivot_src");
    if !grouping.is_empty() {
        sql.push_str(" GROUP BY ");
        sql.push_str(&render_group_exprs(grouping, input_schema)?.join(", "));
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
    // count(*) → count(1) inside CASE: DuckDB rejects bare `*` anywhere
    // except as an expression root.  Mirrors Spark's own count(*)→count(1)
    // rewrite (cf. v2_lowering.rs FILTER desugar).  Scoped to unqualified
    // Star only — qualified `tbl.*` has different NULL-skip semantics.
    let first_arg_sql = if f.name == "count"
        && matches!(
            &f.args[0],
            Expression::Star(StarExpression { qualifier: None })
        ) {
        "1".to_owned()
    } else {
        render_expr(&f.args[0], input_schema)?
    };
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
    let is_count = f.name == "count" || f.name == "count_if" || f.name == "count_star";
    Ok(if is_count {
        format!("NULLIF({call}, 0)")
    } else {
        call
    })
}

/// Render `df.sample(fraction, seed)` → DuckDB `TABLESAMPLE BERNOULLI(...
/// PERCENT) [REPEATABLE(seed)]`. `with_replacement = true` is a permanent
/// Thunderduck-boundary case per ADR-022 — DuckDB has no row-level sampling
/// with replacement.
fn render_sample(
    input: &TypedAst,
    lower_bound: f64,
    upper_bound: f64,
    with_replacement: bool,
    seed: Option<i64>,
) -> Result<String, EmissionError> {
    if with_replacement {
        bail_boundary_op!(
            "Sample[with_replacement]",
            "df.sample(withReplacement=True) is not supported; \
                     DuckDB has no row-level sampling with replacement",
        );
    }
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let pct = (upper_bound - lower_bound) * 100.0;
    let seed_clause = match seed {
        Some(s) => format!(" REPEATABLE({s})"),
        None => String::new(),
    };
    Ok(format!(
        "SELECT * FROM ({child_sql}) AS __td_sample TABLESAMPLE BERNOULLI({pct:.4} PERCENT){seed_clause}"
    ))
}

/// Render `df.sampleBy(col, fractions, seed)` — stratified sampling as a
/// `WHERE (col = k1 AND RANDOM() < f1) OR ...` filter. When `seed` is
/// present, DuckDB's session RNG is seeded via
/// `(SELECT setseed(seed_f)) IS NULL AND (...)`. Empty `fractions` degrades
/// to `WHERE FALSE` (matches Spark: unspecified strata are dropped).
fn render_sample_by(
    input: &TypedAst,
    col: &Expression,
    fractions: &[(Literal, f64)],
    seed: Option<i64>,
) -> Result<String, EmissionError> {
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let col_sql = render_expr(col, &input.resolved_schema)?;
    if fractions.is_empty() {
        return Ok(format!(
            "SELECT * FROM ({child_sql}) AS __td_sample_by WHERE FALSE"
        ));
    }
    let mut conditions: Vec<String> = Vec::with_capacity(fractions.len());
    for (lit, frac) in fractions {
        let lit_sql = render_expr(&Expression::Literal(lit.clone()), &input.resolved_schema)?;
        conditions.push(format!("({col_sql} = {lit_sql} AND RANDOM() < {frac})"));
    }
    let where_body = conditions.join(" OR ");
    let where_clause = if let Some(s) = seed {
        let seed_f = (s.rem_euclid(1_000_000) as f64) / 1_000_000.0;
        format!("(SELECT setseed({seed_f:.6})) IS NULL AND ({where_body})")
    } else {
        where_body
    };
    Ok(format!(
        "SELECT * FROM ({child_sql}) AS __td_sample_by WHERE {where_clause}"
    ))
}

/// `df.toDF(...)` (via `ToDf`) and `df.withColumnsRenamed(...)` both funnel
/// here. Renames the child's output POSITIONALLY via DuckDB's native
/// derived-table column-alias-list syntax (`(<child>) AS __td_wcr(new1,
/// new2, ...)`) rather than referencing each old column BY NAME (`<old> AS
/// <new>`).
///
/// The live reason a by-name rename list is unsafe: the child can have
/// DUPLICATE column names (Spark allows this — e.g. two `SELECT *`-joined
/// sides both projecting `id`), and `SELECT old AS new, old AS new2, ...`
/// is ambiguous SQL once `old` is not unique — DuckDB has no way to tell
/// which same-named source column each `old AS ...` entry means. DuckDB's
/// positional derived-table alias list has no such ambiguity: it renames by
/// ORDINAL, so duplicate input names are a non-issue. Positional renaming
/// also never needs to know the child's actual emitted SQL column names at
/// all (N8 — every computed SELECT entry is aliased with τ's tracked name —
/// makes tracked and emitted names agree globally, but positional renaming
/// doesn't even lean on that guarantee).
///
/// (Historical: before N8 landed, this was ALSO the fix for a
/// tracked≠emitted mismatch on unaliased compound expressions — tbl-013.
/// N8 has since closed that gap, but the duplicate-name hazard above is
/// independent and remains the rationale to keep positional renaming.)
///
/// The duplicate-name hazard is not fully closed inside this path either:
/// THIS function's own `rename_map` (the by-name `HashMap` a few lines
/// down) collapses duplicate old names last-wins. That is unreachable for
/// genuine `withColumnsRenamed` (PySpark dict keys are unique, and renaming
/// every same-named occurrence per entry IS Spark's semantics), but
/// `toDF(...)`/SQL `AS t(...)` lower positional pairs like
/// `[("id","a"), ("id","b")]` through here — the map collapses them to
/// `id→b`, emitting `__td_wcr(b, b)` against a tracked schema of `[a, b]`
/// (an N8 tracked==emitted violation; masked on terminal collect by the
/// positional arrow_schema_stamp rewrite, loud DuckDB binder error on any
/// downstream by-name reference). Tracked as `F-todf-dupname`,
/// tasks/v2-corpus-followups.md; the fix is a POSITIONAL rename list
/// through `TypedOp::WithColumnsRenamed`, keyed by index, at this site.
fn build_with_columns_renamed(
    input: &TypedAst,
    renames: &[(String, String)],
) -> Result<SqlUnit, EmissionError> {
    let rename_map: std::collections::HashMap<String, String> = renames
        .iter()
        .map(|(old, new)| (old.to_lowercase(), new.clone()))
        .collect();
    let dst_names: Vec<&str> = input
        .resolved_schema
        .fields
        .iter()
        .map(|f| {
            rename_map
                .get(&f.name.to_lowercase())
                .map(String::as_str)
                .unwrap_or(f.name.as_str())
        })
        .collect();
    let child_sql = dispatch_op(&input.op, &input.resolved_schema)?;
    let cols = sql_join(dst_names, ", ", |n| Ok(quote_ident(n).into_owned()))?;
    Ok(SqlUnit::Raw(format!(
        "SELECT * FROM ({child_sql}) AS __td_wcr({cols})"
    )))
}

/// Emit a table-valued function. Only `range` is implemented; the analyzer has
/// already rejected everything else (`PuntedOperator`), so a non-`range` name
/// here is a defensive τ-internal boundary.
///
/// Spark `range` arities normalize to `(start, end, step)`; DuckDB `range` is
/// also end-exclusive (1:1 with Spark — never `generate_series`, which is
/// inclusive). Synthesized `start`/`step` defaults are TYPED `Long` literals
/// rendered through [`render_expr`] (ADR-004 — no raw SQL string injection).
/// The `AS __td_range(id)` column alias renames DuckDB's `range` output column
/// to Spark's `id`, which the enclosing `SELECT id` then binds. `numPartitions`
/// is a single-node no-op and is dropped.
fn build_table_function(
    name: &str,
    args: &[Expression],
    _with_ordinality: bool,
    schema: &Schema,
) -> Result<SqlUnit, EmissionError> {
    // N5: `name` arrives already canonical lowercase from
    // `v2_lowering::table_function_node`, the single construction site — no
    // per-consumer re-derivation needed.
    match name {
        "range" => {
            let long_lit = |v: i64| {
                Expression::Literal(Literal {
                    value: LiteralValue::Long(v),
                    data_type: DataType::Long,
                })
            };
            let (start, end, step): (Expression, Expression, Expression) = match args {
                [end] => (long_lit(0), end.clone(), long_lit(1)),
                [start, end] => (start.clone(), end.clone(), long_lit(1)),
                // A 4th `numPartitions` argument is a single-node no-op — drop it.
                [start, end, step] | [start, end, step, _] => {
                    (start.clone(), end.clone(), step.clone())
                }
                _ => bail_boundary_op!(
                    "TableFunction",
                    "range() requires 1..=4 arguments (start, end, step, numPartitions)"
                ),
            };
            let start_sql = render_expr(&start, schema)?;
            let end_sql = render_expr(&end, schema)?;
            let step_sql = render_expr(&step, schema)?;
            // `range` is end-EXCLUSIVE in both Spark and DuckDB; single `id`
            // column. The FROM-item alias renames DuckDB's `range` output
            // column to Spark's `id`; the DEFAULT projection performs the
            // bind (`SELECT id`) so a merging parent that overwrites it sees
            // the renamed column, while a bare dispatch keeps today's shape.
            let mut block = SelectBlock::from_item(FromItem::Raw {
                sql: format!("range({start_sql}, {end_sql}, {step_sql}) AS __td_range(id)"),
                exposed: vec!["__td_range".to_owned()],
            });
            block.set_default_projections(vec![DefaultSlot {
                name: "id".to_owned(),
                sql: "id".to_owned(),
            }]);
            Ok(block.into())
        }
        // Bare `FROM explode(array(1,2,3))` — uncorrelated generator as a TVF.
        // Build the canonical FunctionCall and render via the existing render_expr
        // path, which already handles UNNEST emission for explode/explode_outer
        // (single-homed — no duplication of the CASE wrapper logic).
        "explode" | "explode_outer" => {
            // Defensive: the analyzer guarantees exactly 1 output column.
            if schema.fields.len() != 1 {
                bail_boundary_op!(
                    "TableFunction",
                    format!(
                        "explode TVF schema must have exactly 1 field, got {}",
                        schema.fields.len()
                    )
                );
            }
            let fc = FunctionCall {
                name: name.to_owned(),
                args: args.to_vec(),
                distinct: false,
            };
            let unnest_sql = render_function_call(&fc, schema)?;
            let col_name = quote_ident(&schema.fields[0].name);
            // A FROM-less `SELECT unnest(…) AS col` — a genuine SELECT
            // statement, not a FROM-item generator; stays a Raw unit.
            Ok(SqlUnit::Raw(format!("SELECT {unnest_sql} AS {col_name}")))
        }
        _ => bail_boundary_op!(
            "TableFunction",
            format!("table-function `{name}` emission (not implemented in τ)")
        ),
    }
}

// ── Expression rendering ─────────────────────────────────────────────────────

/// Render a subquery's inner plan to a bare `SELECT …` string. The plan must
/// be `Analyzed` — a stray `Unanalyzed` means the analyzer pass did not run
/// (a τ bug, not a user input), surfaced as a defensive boundary error.
fn render_subquery(plan: &SubqueryPlan) -> Result<String, EmissionError> {
    match plan {
        SubqueryPlan::Analyzed(inner) => dispatch_op(&inner.op, &inner.resolved_schema),
        SubqueryPlan::Unanalyzed(_) => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::Expression,
            name: "subquery".to_owned(),
            reason: "inner plan not analyzed — analyzer pass did not run".to_owned(),
        }),
    }
}

/// Exhaustive match over the [`Expression`] enum.
pub(crate) fn render_expr(expr: &Expression, schema: &Schema) -> Result<String, EmissionError> {
    match expr {
        Expression::Literal(l) => render_literal(l),
        Expression::ColumnReference(c) => render_column_reference(c),
        Expression::UnresolvedColumn(u) => bail_boundary_expr!(
            "UnresolvedColumn",
            format!("analyzer must resolve column `{}` before emission", u.name),
        ),
        Expression::UnresolvedRegex(_) => bail_boundary_expr!(
            "UnresolvedRegex",
            "analyzer must expand regex projections in Project pre-pass",
        ),
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
        Expression::Window(w) => render_window(w, schema),
        Expression::Alias(a) => render_alias(a, schema),
        Expression::Star(s) => render_star(s),
        // Uncorrelated subqueries render node-local from the analyzed inner
        // plan carried in the variant (ADR-007 A / INV2). No SQL string
        // pre/post-processing — the inner SELECT is built from its own typed
        // AST via `dispatch_op` (SQL-gen principles #1/#2).
        Expression::ScalarSubquery(s) => {
            let inner = render_subquery(&s.subquery)?;
            Ok(format!("({inner})"))
        }
        Expression::InSubquery(i) => {
            let lhs = render_expr(&i.expr, schema)?;
            let inner = render_subquery(&i.subquery)?;
            let not = if i.negated { "NOT " } else { "" };
            Ok(format!("{lhs} {not}IN ({inner})"))
        }
        Expression::ExistsSubquery(e) => {
            let inner = render_subquery(&e.subquery)?;
            let not = if e.negated { "NOT " } else { "" };
            Ok(format!("{not}EXISTS ({inner})"))
        }
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
                let params = sql_join(l.params.iter(), ", ", |p| Ok(quote_ident(p).into_owned()))?;
                Ok(format!("({params}) -> {body}"))
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
            // Dispatch on the CHILD's static type, mirroring
            // `extract_value_data_type` (expression.rs) — Struct→field,
            // Array→element, Map→value. Keying on the extraction literal shape
            // alone (the prior behavior) mis-emitted a map string key as struct
            // dot access. Corpus witnesses: cx-001 (array), cx-002 (map),
            // struct-003 / json-004 (struct).
            match ev.child.data_type(schema) {
                DataType::Struct(_) => extract_struct_field(&child_sql, &ev.extraction, schema),
                DataType::Array(_, _) => {
                    // Spark GetArrayItem `arr[i]`: 0-indexed. In ANSI mode
                    // (ADR-016) `GetArrayItem` sets `failOnError = true` and
                    // THROWS `[INVALID_ARRAY_INDEX]` when `i < 0` or
                    // `i >= numElements` — it does NOT return NULL (that is the
                    // non-ANSI behavior). This is a DISTINCT class from
                    // `element_at`'s `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`. A NULL
                    // array short-circuits to NULL (Spark `nullSafeEval`).
                    // DuckDB `list_extract` is 1-based, so shift the in-bounds
                    // index by +1. Corpus witness: cx-001 (`[0]`, in-bounds,
                    // stays green).
                    let idx = render_expr(&ev.extraction, schema)?;
                    let err = super::spark_errors::SparkError::InvalidArrayIndexSubscript {
                        idx_sql: idx.clone(),
                        arr_sql: child_sql.clone(),
                    }
                    .throw_expr();
                    Ok(format!(
                        "CASE WHEN ({child_sql}) IS NULL THEN NULL \
                         WHEN ({idx}) < 0 OR ({idx}) >= len(({child_sql})) THEN {err} \
                         ELSE list_extract(({child_sql}), ({idx}) + 1) END"
                    ))
                }
                DataType::Map { .. } => {
                    // Spark GetMapValue `map[k]`: value or NULL on miss, never
                    // throws. DuckDB `element_at(map, key)` returns a 1-element
                    // list; `[1]` unwraps it to the scalar value (NULL on miss).
                    let key = render_expr(&ev.extraction, schema)?;
                    Ok(format!("element_at(({child_sql}), ({key}))[1]"))
                }
                // Unresolved child: reuse the struct-field heuristic
                // (String literal → `.field`; else → `[expr]`).
                _ => extract_struct_field(&child_sql, &ev.extraction, schema),
            }
        }
        Expression::RowConstructor(_) => bail_boundary_expr!(
            "RowConstructor",
            "complex-type emission (not implemented in τ)",
        ),
        Expression::UpdateFields(u) => render_update_fields(u, schema),
    }
}

/// Render a `WindowFunction`'s `OVER (PARTITION BY ... ORDER BY ...
/// [frame])` clause text (without the leading function SQL). ORDER BY items
/// share [`render_sort_key`] with the `Sort` operator (byte-identical
/// `{expr} {ASC|DESC} {NULLS FIRST|NULLS LAST}` shape). Extracted from
/// `render_window` so [`render_decimal_avg`] can splice the same OVER text
/// onto `spark_avg(...)` before the outer Spark-parity CAST — `CAST(...)
/// OVER (...)` is invalid SQL, so the OVER must land *inside* the CAST.
fn render_over_clause(
    w: &crate::transpiler_v2::expression::WindowFunction,
    schema: &Schema,
) -> Result<String, EmissionError> {
    let mut over = String::from("OVER (");
    let mut had_content = false;
    if !w.partition_by.is_empty() {
        over.push_str("PARTITION BY ");
        over.push_str(&sql_join(w.partition_by.iter(), ", ", |p| {
            render_expr(p, schema)
        })?);
        had_content = true;
    }
    if !w.order_by.is_empty() {
        if had_content {
            over.push(' ');
        }
        over.push_str("ORDER BY ");
        over.push_str(&sql_join(w.order_by.iter(), ", ", |s| {
            render_sort_key(s, schema)
        })?);
        had_content = true;
    }
    // Frame clause emission.
    if let Some(frame) = &w.frame {
        use super::expression::{FrameBoundary, FrameUnit};
        if had_content {
            over.push(' ');
        }
        let unit_kw = match frame.unit {
            FrameUnit::Rows => "ROWS",
            FrameUnit::Range => "RANGE",
        };
        let render_bound = |b: &FrameBoundary| -> Result<String, EmissionError> {
            match b {
                FrameBoundary::UnboundedPreceding => Ok("UNBOUNDED PRECEDING".to_owned()),
                FrameBoundary::UnboundedFollowing => Ok("UNBOUNDED FOLLOWING".to_owned()),
                FrameBoundary::CurrentRow => Ok("CURRENT ROW".to_owned()),
                FrameBoundary::Preceding(e) => {
                    let n = render_expr(e, schema)?;
                    Ok(format!("{n} PRECEDING"))
                }
                FrameBoundary::Following(e) => {
                    let n = render_expr(e, schema)?;
                    Ok(format!("{n} FOLLOWING"))
                }
            }
        };
        let lo = render_bound(&frame.lower)?;
        let up = render_bound(&frame.upper)?;
        over.push_str(&format!("{unit_kw} BETWEEN {lo} AND {up}"));
    }
    over.push(')');
    Ok(over)
}

/// Render a window function application:
/// `<func> OVER (PARTITION BY ... ORDER BY ... [frame])`.
///
/// Decimal `avg`/`mean` are intercepted before the generic
/// `render_expr(&w.func)` path: [`render_decimal_avg`] must wrap the whole
/// `spark_avg(...) OVER (...)` expression in the outer Spark-parity CAST,
/// since `CAST(...) OVER (...)` is invalid SQL — the generic path below
/// (which appends OVER after an already-rendered function) cannot produce
/// that shape.
fn render_window(
    w: &crate::transpiler_v2::expression::WindowFunction,
    schema: &Schema,
) -> Result<String, EmissionError> {
    if let Expression::FunctionCall(f) = w.func.as_ref() {
        if is_decimal_avg(f, schema) {
            let over = render_over_clause(w, schema)?;
            return render_decimal_avg(f, Some(&over), schema);
        }
    }
    let func_sql = render_expr(&w.func, schema)?;
    let over = render_over_clause(w, schema)?;
    Ok(format!("{func_sql} {over}"))
}

/// Emit a struct field access for an [`Expression::ExtractValue`] whose child
/// is (statically or heuristically) a struct. A `String`-literal extraction is
/// a field name → `(child).field`; any other extraction falls back to a
/// runtime-typed `(child)[expr]` subscript.
fn extract_struct_field(
    child_sql: &str,
    extraction: &Expression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    match extraction {
        Expression::Literal(l) => match &l.value {
            crate::transpiler_v2::expression::LiteralValue::String(name) => {
                let field = quote_ident(name);
                Ok(format!("({child_sql}).{field}"))
            }
            _ => {
                let idx = render_expr(extraction, schema)?;
                Ok(format!("({child_sql})[{idx}]"))
            }
        },
        _ => {
            let idx = render_expr(extraction, schema)?;
            Ok(format!("({child_sql})[{idx}]"))
        }
    }
}

/// Render each grouping expression bare (alias-stripped — GROUP BY doesn't
/// take aliases, so `GROUP BY (expr) AS name` would be a parse error).
/// Shared by [`render_pivot`] and [`render_aggregate_op`] (whose GROUPING
/// SETS arm indexes into the returned Vec per set).
fn render_group_exprs(
    grouping: &[Expression],
    schema: &Schema,
) -> Result<Vec<String>, EmissionError> {
    grouping
        .iter()
        .map(|g| render_expr(g.unaliased(), schema))
        .collect()
}

fn is_aggregate_name(name: &str) -> bool {
    // Classifier roster lives in the `AGG_SPECS` table (`type_inference.rs`);
    // the lookup is case-insensitive without allocating a lowercased `String`.
    is_aggregate_classifier_name(name)
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
///
/// Exact special case of [`substitute_lambda_var`] — the replacement is the
/// fixed `(index_var - 1)` expression instead of an arbitrary caller-supplied
/// sub-expression.
fn substitute_index_var(body: &Expression, index_var: &str) -> Expression {
    let replacement = Expression::Binary(BinaryExpression {
        op: BinaryOp::Sub,
        left: Box::new(Expression::LambdaVariable(
            super::expression::LambdaVariableExpression {
                name: index_var.to_owned(),
            },
        )),
        right: Box::new(Expression::Literal(Literal {
            value: LiteralValue::Long(1),
            data_type: DataType::Long,
        })),
    });
    substitute_lambda_var(body, index_var, &replacement)
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
///
/// Traversal is structural via [`Expression::map_children`], so every
/// composite variant recurses (the previous hand walker's catch-all silently
/// skipped `MapLiteral`, `StructLiteral`, `RowConstructor`, `UpdateFields` —
/// leaving lambda variables unsubstituted there). Subqueries and `Window`
/// stay opaque; see the explicit arms.
fn substitute_lambda_var(
    body: &Expression,
    var_name: &str,
    replacement: &Expression,
) -> Expression {
    match body {
        // Hit: exact (case-sensitive) name match on the bound variable.
        Expression::LambdaVariable(lv) if lv.name == var_name => replacement.clone(),
        // Shadowing: an inner lambda that re-binds `var_name` makes its body
        // opaque — the inner binding wins, so descent stops here.
        Expression::Lambda(inner) if inner.params.iter().any(|p| p == var_name) => body.clone(),
        // Opacity: the hand walker never descended into subqueries or window
        // expressions (they fell to its catch-all). A subquery or window
        // inside a HOF lambda body is pathological — DuckDB cannot evaluate
        // either per-element — so preserve the historical skip exactly rather
        // than adopt `map_children`'s child set there (it would visit
        // `InSubquery`'s LHS and `Window`'s func/partition/order children).
        Expression::InSubquery(_)
        | Expression::ExistsSubquery(_)
        | Expression::ScalarSubquery(_)
        | Expression::Window(_) => body.clone(),
        // Everything else (including non-shadowing lambdas, whose only child
        // is their body): recurse structurally over the immediate children.
        other => {
            let mapped = other.clone().map_children(|child| {
                Ok::<_, std::convert::Infallible>(substitute_lambda_var(
                    &child,
                    var_name,
                    replacement,
                ))
            });
            match mapped {
                Ok(e) => e,
                Err(never) => match never {},
            }
        }
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
        bail_boundary_fn!(
            f.name.clone(),
            "map higher-order function requires a lambda argument"
        );
    };
    if lam.params.len() != 2 {
        bail_boundary_fn!(
            f.name.clone(),
            "map higher-order lambda must take exactly 2 arguments (key, value)",
        );
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

/// Extract a string-literal argument at `idx` from a function call, failing
/// loudly if it is absent or not a string literal. Used for arguments that τ's
/// front-ends demote to a literal (e.g. the datetime UNIT of `timestampadd` /
/// `timestampdiff`), which must never reach emission as a column reference.
fn string_literal_arg(f: &FunctionCall, idx: usize, what: &str) -> Result<String, EmissionError> {
    match f.args.get(idx) {
        Some(Expression::Literal(Literal {
            value: LiteralValue::String(s),
            ..
        })) => Ok(s.clone()),
        _ => Err(EmissionError::Unsupported {
            kind: UnsupportedKind::Function,
            name: f.name.clone(),
            reason: format!("{what} must be a string literal"),
        }),
    }
}

/// If `e` is a boolean literal expression, return its value. Otherwise return
/// `None`. Used by the arms that recognise (or drop) Spark's trailing
/// boolean-literal flags (`ignoreNulls`, `sort_array` asc, `mode`).
fn bool_literal(e: &Expression) -> Option<bool> {
    match e {
        Expression::Literal(Literal {
            value: LiteralValue::Boolean(b),
            ..
        }) => Some(*b),
        _ => None,
    }
}

/// Render the first `N` arguments of `f` positionally via [`render_expr`].
/// The caller must have already established (match guard or explicit arity
/// check) that at least `N` arguments are present.
fn rendered_args<const N: usize>(
    f: &FunctionCall,
    schema: &Schema,
) -> Result<[String; N], EmissionError> {
    let rendered = f.args[..N]
        .iter()
        .map(|a| render_expr(a, schema))
        .collect::<Result<Vec<String>, EmissionError>>()?;
    Ok(<[String; N]>::try_from(rendered).expect("sliced to exactly N elements above"))
}

/// Bail with `msg` (verbatim, same `bail_boundary_fn!` error shape, `f.name`
/// as the function name) unless the call has exactly `N` arguments; then
/// render each argument positionally via [`render_expr`].
fn exact_args<const N: usize>(
    f: &FunctionCall,
    schema: &Schema,
    msg: &str,
) -> Result<[String; N], EmissionError> {
    if f.args.len() != N {
        bail_boundary_fn!(f.name.clone(), msg);
    }
    rendered_args(f, schema)
}

/// [`exact_args`] variant for "at least `N`" arities: bails with `msg` unless
/// the call has `N` or more arguments, then renders the FIRST `N` arguments
/// (any extras are intentionally handled — or dropped — by the calling arm).
fn min_args<const N: usize>(
    f: &FunctionCall,
    schema: &Schema,
    msg: &str,
) -> Result<[String; N], EmissionError> {
    if f.args.len() < N {
        bail_boundary_fn!(f.name.clone(), msg);
    }
    rendered_args(f, schema)
}

/// Order-preserving "distinct by first occurrence" over the list-valued SQL
/// expression `list_sql`, matching Spark's array set-op semantics (a
/// linked-hash-set scan, not a sort). DuckDB's own `list_distinct` reorders
/// by hash — verified directly (`list_distinct([1,1,2,3,2])` → `[3, 2,
/// 1]`), which breaks Spark parity. `list_position(list, x) = i` keeps only
/// the index where `x` FIRST occurs, including for a NULL element (DuckDB's
/// `list_position` finds NULL elements positionally, unlike
/// `list_contains` — see [`null_safe_member`]). `list_sql` is evaluated
/// twice (filter target + inside `list_position`); callers must pass an
/// expression that's safe to repeat (a column reference or a
/// `list_concat(...)` built from column references, as in `array_union`).
fn order_preserving_distinct(list_sql: &str) -> String {
    format!("list_filter({list_sql}, (x, i) -> list_position({list_sql}, x) = i)")
}

/// Null-safe "is the lambda-bound `x` present in `list_sql`" predicate,
/// always TRUE/FALSE (never NULL) given a non-NULL `list_sql`. DuckDB's
/// `list_contains(list, needle)` returns NULL — not FALSE — whenever
/// `needle` is NULL, even if `list` itself contains a NULL element
/// (verified directly: `list_contains([1,2,NULL], NULL)` → NULL). Spark's
/// array set-ops treat NULL as an ordinary comparable value, so a NULL
/// element common to both arrays must count as "contains" (verified live
/// against Spark 4.1.1: `array_intersect(array(1, NULL, 2), array(NULL,
/// 2))` → `[NULL, 2]`). `list_position(list, x) IS NOT NULL` finds a NULL
/// element positionally and always yields a boolean.
fn null_safe_member(list_sql: &str) -> String {
    format!("list_position({list_sql}, x) IS NOT NULL")
}

/// Negation of [`null_safe_member`] — "is `x` absent from `list_sql`",
/// always TRUE/FALSE. Used by `array_except`'s set-difference filter.
fn not_null_safe_member(list_sql: &str) -> String {
    format!("list_position({list_sql}, x) IS NULL")
}

/// Compose Spark's `make_dt_interval` / `make_interval` / `make_ym_interval`
/// as a sum of `INTERVAL (expr) UNIT` summands — DuckDB has none of the three
/// scalars. One summand per entry of `units` (missing arguments default to 0,
/// Spark's documented behavior); when `with_seconds` is set, a trailing
/// fractional-seconds argument is preserved via `MICROSECOND` with
/// `* 1_000_000` (DuckDB `INTERVAL (expr) SECOND` truncates to integer
/// seconds). Argument counts beyond `units.len() + with_seconds` bail with
/// `max_msg` verbatim.
fn render_make_interval(
    f: &FunctionCall,
    schema: &Schema,
    units: &[&str],
    with_seconds: bool,
    max_msg: &str,
) -> Result<String, EmissionError> {
    let max_args = units.len() + usize::from(with_seconds);
    if f.args.len() > max_args {
        bail_boundary_fn!(f.name.clone(), max_msg);
    }
    let zero = "0".to_owned();
    let arg = |i: usize| -> Result<String, EmissionError> {
        if i >= f.args.len() {
            Ok(zero.clone())
        } else {
            render_expr(&f.args[i], schema)
        }
    };
    let mut parts: Vec<String> = Vec::with_capacity(max_args);
    for (i, unit) in units.iter().enumerate() {
        parts.push(format!("INTERVAL ({}) {unit}", arg(i)?));
    }
    if with_seconds {
        // Seconds are DECIMAL(8,6) in Spark. DuckDB `INTERVAL (expr) SECOND`
        // truncates to integer seconds; use MICROSECOND with `* 1_000_000`
        // to preserve fractional seconds.
        let s_micros = if f.args.len() < max_args {
            zero.clone()
        } else {
            let s = render_expr(&f.args[max_args - 1], schema)?;
            format!("CAST(({s}) * 1000000 AS BIGINT)")
        };
        parts.push(format!("INTERVAL ({s_micros}) MICROSECOND"));
    }
    Ok(format!("({})", parts.join(" + ")))
}

/// Build the DuckDB interval expression for Spark's `timestampadd(unit, n, ts)`.
/// DuckDB has no `QUARTER` interval keyword, so a quarter becomes `(n * 3)
/// MONTH`. Unsupported units surface a Thunderduck-boundary error (ADR-022) —
/// never a silent, wrong emission.
fn spark_add_interval_sql(fn_name: &str, unit: &str, n: &str) -> Result<String, EmissionError> {
    let duck_unit = match unit.to_ascii_uppercase().as_str() {
        "YEAR" | "YEARS" => "YEAR",
        "QUARTER" | "QUARTERS" => return Ok(format!("INTERVAL (({n}) * 3) MONTH")),
        "MONTH" | "MONTHS" => "MONTH",
        "WEEK" | "WEEKS" => "WEEK",
        "DAY" | "DAYS" => "DAY",
        "HOUR" | "HOURS" => "HOUR",
        "MINUTE" | "MINUTES" => "MINUTE",
        "SECOND" | "SECONDS" => "SECOND",
        "MILLISECOND" | "MILLISECONDS" => "MILLISECOND",
        "MICROSECOND" | "MICROSECONDS" => "MICROSECOND",
        other => {
            return Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                name: fn_name.to_owned(),
                reason: format!("`timestampadd` unit `{other}` is not supported by τ"),
            });
        }
    };
    Ok(format!("INTERVAL ({n}) {duck_unit}"))
}

/// Emit Spark's `timestampdiff(unit, start, end)` (BIGINT, truncated toward
/// zero). Fixed-length units divide the microsecond delta by the unit's micros.
/// Calendar units (MONTH/QUARTER/YEAR) require day-of-month-aware arithmetic τ
/// does not yet emit — surface an honest Thunderduck-boundary error (ADR-022)
/// rather than the boundary-counting `date_diff`, which diverges from Spark.
fn spark_diff_sql(
    fn_name: &str,
    unit: &str,
    start: &str,
    end: &str,
) -> Result<String, EmissionError> {
    let delta = format!("(epoch_us({end}) - epoch_us({start}))");
    let micros: i64 = match unit.to_ascii_uppercase().as_str() {
        "MICROSECOND" | "MICROSECONDS" => return Ok(format!("CAST({delta} AS BIGINT)")),
        "MILLISECOND" | "MILLISECONDS" => 1_000,
        "SECOND" | "SECONDS" => 1_000_000,
        "MINUTE" | "MINUTES" => 60_000_000,
        "HOUR" | "HOURS" => 3_600_000_000,
        "DAY" | "DAYS" => 86_400_000_000,
        "WEEK" | "WEEKS" => 604_800_000_000,
        other @ ("MONTH" | "MONTHS" | "QUARTER" | "QUARTERS" | "YEAR" | "YEARS") => {
            return Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                name: fn_name.to_owned(),
                reason: format!(
                    "`timestampdiff` calendar unit `{other}` is not yet implemented in τ \
                     (Spark's day-of-month-aware calendar diff)"
                ),
            });
        }
        other => {
            return Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                name: fn_name.to_owned(),
                reason: format!("`timestampdiff` unit `{other}` is not supported by τ"),
            });
        }
    };
    // `trunc` truncates toward zero (Spark's semantics for integral unit diff);
    // the outer CAST converts the already-integral DOUBLE back to BIGINT.
    Ok(format!(
        "CAST(trunc(CAST({delta} AS DOUBLE) / {micros}.0) AS BIGINT)"
    ))
}

/// Render Spark `ceil`/`floor` to DuckDB SQL. `duck_fn` is the DuckDB substrate
/// (`"ceil"` / `"floor"`). The CAST target is derived from the SAME
/// [`TypeInferenceEngine::ceil_floor_type`] the analyzer used, so the physical
/// type equals `resolved_schema`.
///
/// - **Long** (integral / float / double, 1-arg): NaN-guarded `CAST(.. AS
///   BIGINT)` — byte-identical to the historical emission (math-003 pin).
/// - **Decimal, 1-arg**: `CAST(fn(a) AS DECIMAL(p, s))` (a DECIMAL can never be
///   NaN, so no guard).
/// - **Decimal, 2-arg, t >= 0**: `CAST(fn((a) * 10^t) / 10^t AS DECIMAL(p, s))`.
/// - **2-arg negative scale**: Thunderduck boundary (no corpus witness).
fn render_ceil_floor(
    f: &FunctionCall,
    schema: &Schema,
    duck_fn: &str,
) -> Result<String, EmissionError> {
    if f.args.is_empty() {
        bail_boundary_fn!(
            f.name.clone(),
            format!("`{}` requires at least 1 argument", f.name)
        );
    }
    let a = render_expr(&f.args[0], schema)?;
    let input_ty = f.args[0].data_type(schema);
    let scale_opt = (f.args.len() == 2)
        .then(|| int_literal_value(&f.args[1]))
        .flatten();
    match TypeInferenceEngine::ceil_floor_type(&input_ty, scale_opt) {
        DataType::Long => Ok(format!(
            "CASE WHEN ({a}) IS NULL THEN NULL \
             WHEN isnan(CAST(({a}) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST({duck_fn}({a}) AS BIGINT) END"
        )),
        DataType::Decimal { precision, scale } => match scale_opt {
            None => Ok(format!(
                "CAST({duck_fn}({a}) AS DECIMAL({precision}, {scale}))"
            )),
            Some(t) if t >= 0 => {
                // `t` is user-controlled; 10^t overflows i128 at t >= 39.
                // Spark caps DECIMAL scale at 38, so a larger scale has no
                // valid result — surface an honest boundary rather than panic.
                let Some(pow) = 10i128.checked_pow(t as u32) else {
                    bail_boundary_fn!(
                        f.name.clone(),
                        "ceil/floor target scale too large (exceeds DECIMAL max scale 38)"
                    );
                };
                Ok(format!(
                    "CAST({duck_fn}(({a}) * {pow}) / {pow} AS DECIMAL({precision}, {scale}))"
                ))
            }
            Some(_) => bail_boundary_fn!(
                f.name.clone(),
                "ceil/floor with negative target scale not implemented in τ"
            ),
        },
        _ => bail_boundary_fn!(f.name.clone(), "ceil/floor: unsupported argument type"),
    }
}

/// Number of leading arguments to KEEP when trimming Spark's trailing
/// `ignoreNulls` boolean from a first/last/nth_value/lag/lead call — DuckDB's
/// equivalents do not accept the flag. Single source for the per-function
/// keep-arity, shared by the two trim sites. The sites' trim GUARDS are
/// deliberately divergent — do NOT unify them without a corpus witness:
///
/// * [`render_function_call`] (reached by `nth_value`/`lag`/`lead`, whose
///   aggregate-classifier bit is false) trims ONLY when every extra trailing
///   arg is a boolean literal — it never drops a real value. Anchor: corpus
///   win-006.
/// * [`render_aggregate`] (reached by `first`/`last`/`first_value`/
///   `last_value`, which the classifier routes there) trims UNCONDITIONALLY
///   whenever extra args are present (corpus uses ignorenulls=True, which
///   matches DuckDB's default).
fn trailing_ignore_nulls_keep_arity(name_lower: &str) -> Option<usize> {
    match name_lower {
        "first" | "last" | "first_value" | "last_value" => Some(1),
        "nth_value" => Some(2),
        "lag" | "lead" => Some(3), // (col, offset, default)
        _ => None,
    }
}

fn render_function_call(f: &FunctionCall, schema: &Schema) -> Result<String, EmissionError> {
    let sql = render_function_call_dispatch(f, schema)?;
    if needs_date_return_cast(f) {
        return Ok(format!("CAST({sql} AS DATE)"));
    }
    Ok(sql)
}

/// Emission-owned divergence roster: Date-typed Spark functions whose
/// DuckDB-rendered substrate form natively returns TIMESTAMP (DATE±INTERVAL
/// promotion / date_trunc). Single home of the corrective cast.
/// Mechanically gated by `date_typed_functions_return_date_in_duckdb`.
///
/// Future-author rule: a NEW Date-returning function is added to
/// [`TypeInferenceEngine`]'s `DATE_RETURNING_FNS` ALWAYS (the type
/// authority), and to this roster ONLY when its emitted DuckDB form
/// diverges from DATE — the audit test fails until the two agree with
/// reality, so a wrong guess cannot land silently.
fn needs_date_return_cast(f: &FunctionCall) -> bool {
    match f.name.as_str() {
        "add_months" | "date_add" | "date_sub" => true,
        "trunc" => f.args.len() == 2, // 1-arg trunc is not the date form
        _ => false,
    }
}

fn render_function_call_dispatch(
    f: &FunctionCall,
    schema: &Schema,
) -> Result<String, EmissionError> {
    // N5: `f.name` is already canonical lowercase — `name_lower` is kept as
    // an owned `String` (rather than renamed to a borrow) purely so the
    // ~1900-line match below, which threads it through several `&name_lower`
    // / `format!` sites, needs no further edits.
    let name_lower = f.name.clone();
    // Aggregate-name overlap check — if the analyzer classified a FunctionCall
    // as aggregate, `render_expr` routes to `render_aggregate` before this
    // function; anything reaching here is scalar by construction. Defense in
    // depth: any name in the classifier roster should never be seen here.
    //
    // Window-only functions with a trailing `ignoreNulls` argument that PySpark
    // serializes verbatim — DuckDB's `nth_value(col, n)` / `lag`/`lead`/
    // `first_value`/`last_value` do not accept the boolean flag. Drop the
    // trailing bool; keep-arity is single-homed in
    // [`trailing_ignore_nulls_keep_arity`]. Anchor: corpus win-006.
    if matches!(
        name_lower.as_str(),
        "nth_value" | "first_value" | "last_value" | "lag" | "lead"
    ) {
        if let Some(arity_keep) = trailing_ignore_nulls_keep_arity(&name_lower) {
            // Only apply the trim if the extra trailing arg is a boolean literal
            // (Spark's ignoreNulls flag). Never silently drop a real value.
            if f.args.len() > arity_keep {
                let extras = &f.args[arity_keep..];
                let all_bool_literals = extras.iter().all(|e| bool_literal(e).is_some());
                if all_bool_literals {
                    let parts = sql_join(f.args.iter().take(arity_keep), ", ", |arg| {
                        render_expr(arg, schema)
                    })?;
                    return Ok(format!("{name_lower}({parts})"));
                }
            }
        }
    }
    let args_sql = sql_join(f.args.iter(), ", ", |arg| render_expr(arg, schema))?;
    // Handful of Spark-name → DuckDB-name remappings where the direct
    // pass-through wouldn't work. Everything else passes through unchanged.
    let duck_name: &str = match name_lower.as_str() {
        // DuckDB parses `not` as a keyword; Spark sends unary NOT as a
        // function. Emit as a keyword expression.
        "not" => {
            let [a] = exact_args(f, schema, "`not` requires exactly one argument")?;
            return Ok(format!("(NOT {a})"));
        }
        // Spark's `array()` literal — DuckDB uses `[a, b, c]` or
        // `list_value(a, b, c)`. Emit the list_value form since it accepts
        // zero-or-more args uniformly.
        "array" => "list_value",
        // Spark's `map()` literal — takes flat key/value pairs; DuckDB uses
        // `map { k: v, ... }` or `map_from_entries`. For a variable pair
        // count, emit via `map(list_value(k1,k2,...), list_value(v1,v2,...))`
        // — but that requires splitting args, so this uses the more
        // permissive `map_from_entries` shape if args come pre-paired.
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
            if !f.args.len().is_multiple_of(2) {
                bail_boundary_fn!(f.name.clone(), "`create_map` requires an even arg count");
            }
            let keys = sql_join(f.args.iter().step_by(2), ", ", |k| render_expr(k, schema))?;
            let vals = sql_join(f.args.iter().skip(1).step_by(2), ", ", |v| {
                render_expr(v, schema)
            })?;
            return Ok(format!("map(list_value({keys}), list_value({vals}))"));
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
            let parts = sql_join(f.args.iter().enumerate(), ", ", |(i, arg)| {
                let name = super::struct_names::derive_struct_field_name(arg, i);
                let val = render_expr(arg.unaliased(), schema)?;
                let name_q = quote_ident(&name);
                Ok(format!("{name_q} := {val}"))
            })?;
            return Ok(format!("struct_pack({parts})"));
        }
        // Spark's `locate(needle, haystack[, start])` → DuckDB's
        // `strpos(haystack, needle)` (no start-position support).
        "locate" => {
            let [needle, haystack] = min_args(f, schema, "`locate` requires at least 2 arguments")?;
            return Ok(format!("strpos({haystack}, {needle})"));
        }
        // Spark's `btrim(str[, trimStr])` trims characters in `trimStr`
        // (default: whitespace) from both ends of `str`. DuckDB's `trim`
        // has the identical signature and semantics — same name, same arg
        // order; just rename.
        "btrim" => "trim",
        // Spark's `substring_index(str, delim, count)` returns the substring
        // of `str` before the `count`-th occurrence of `delim`: `count > 0`
        // counts from the left (keep the first `count` delimited pieces),
        // `count < 0` counts from the right (keep the last `|count|`
        // pieces), `count == 0` yields an empty string. DuckDB has no
        // direct equivalent; emulate via `string_split` + `list_slice` +
        // `array_to_string`. `list_slice` clamps out-of-range bounds (a
        // count larger than the number of occurrences returns the whole
        // list, matching Spark) and propagates NULL through a NULL `str`
        // unchanged, so no separate NULL guard is needed. Empirically
        // verified against live Spark 4.1.1 for count > 0 / < 0 / == 0, a
        // count exceeding the occurrence count, and `delim` absent from
        // `str`. Corpus: `test_substring_index`.
        "substring_index" => {
            let [s, delim, count] =
                exact_args(f, schema, "`substring_index` requires exactly 3 arguments")?;
            return Ok(format!(
                "CASE WHEN ({count}) >= 0 \
                 THEN array_to_string(list_slice(string_split({s}, {delim}), 1, ({count})), {delim}) \
                 ELSE array_to_string(list_slice(string_split({s}, {delim}), ({count}), -1), {delim}) \
                 END"
            ));
        }
        // Spark's `dayofweek(x)` returns 1..7 (Sunday=1); DuckDB's returns
        // 0..6 (Sunday=0). Add 1 to align with Spark.
        "dayofweek" => {
            let [a] = exact_args(f, schema, "`dayofweek` requires exactly 1 argument")?;
            return Ok(format!("(dayofweek({a}) + 1)"));
        }
        // Spark's `date_format(date, fmt)` → DuckDB `strftime(date, fmt)`.
        // Note: Spark uses Java SimpleDateFormat tokens (yyyy/MM/dd) while
        // DuckDB uses strftime tokens (%Y/%m/%d). We do a best-effort
        // token translation for the most common patterns; complex format
        // strings will diverge and require per-case follow-ups.
        "date_format" if f.args.len() == 2 => {
            let [d, fmt] = rendered_args(f, schema)?;
            // Translate Spark tokens to strftime tokens at emission time
            // — supports yyyy/MM/dd/HH/mm/ss and common variants.
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("strftime({d}, {duck_fmt})"));
        }
        // Spark's `to_char(x, fmt)` — corpus witness is the DATE form (a
        // number-format model would need its own translation and is out of
        // scope here). DuckDB has no native `to_char`; mirror `date_format`
        // above via `strftime` + the shared Spark→DuckDB token helper.
        // Corpus: `test_to_char` (test_string_collection_differential).
        "to_char" if f.args.len() == 2 => {
            let [d, fmt] = rendered_args(f, schema)?;
            let duck_fmt = spark_fmt_to_duckdb(&fmt);
            return Ok(format!("strftime({d}, {duck_fmt})"));
        }
        // Spark's `trunc(date, format)` → DuckDB `date_trunc(format, date)`.
        // Spark's arg order is (date, fmt); DuckDB's is (fmt, date). DuckDB's
        // `date_trunc` natively returns TIMESTAMP; the `render_function_call`
        // wrapper (via `needs_date_return_cast`) supplies the CAST back to
        // Spark's DATE return type — no cast here.
        "trunc" if f.args.len() == 2 => {
            let [d, fmt] = rendered_args(f, schema)?;
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
            let [a] = exact_args(f, schema, "`explode` requires exactly 1 argument")?;
            return Ok(format!("UNNEST({a})"));
        }
        "explode_outer" => {
            let [a] = exact_args(f, schema, "`explode_outer` requires exactly 1 argument")?;
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
            let [a] = exact_args(f, schema, "`posexplode_pos` requires exactly 1 argument")?;
            // DuckDB's `generate_subscripts(list, 1)` is 1-indexed; Spark's
            // `posexplode` is 0-indexed. Subtract 1 to align.
            return Ok(format!("(generate_subscripts({a}, 1) - 1)"));
        }
        "posexplode_val" => {
            let [a] = exact_args(f, schema, "`posexplode_val` requires exactly 1 argument")?;
            return Ok(format!("UNNEST({a})"));
        }
        // Synthetic FunctionCall names produced by the v2 converter when it
        // splits `F.explode(map_col).alias("k", "v")` into two projections.
        // Emission expands each column via `UNNEST(map_keys(m))` /
        // `UNNEST(map_values(m))` — DuckDB's MAP is a list-pair internally,
        // so co-UNNESTed sibling projections stay row-aligned. Corpus: map-007.
        "map_explode_key" => {
            let [m] = exact_args(f, schema, "`map_explode_key` requires exactly 1 argument")?;
            return Ok(format!("UNNEST(map_keys({m}))"));
        }
        "map_explode_val" => {
            let [m] = exact_args(f, schema, "`map_explode_val` requires exactly 1 argument")?;
            return Ok(format!("UNNEST(map_values({m}))"));
        }
        // piv-006 — synthetic per-column call produced by the analyzer's
        // Project pre-pass (`expand_stack_projections`) when it fans out a
        // `stack(N, ...) AS (a1, ..., aK)` fragment. Args are the K row
        // values for one output column; emission emits
        // `UNNEST([v1, v2, ..., vN])`. Sibling `stack_col` UNNESTs in a
        // SELECT list co-multiply row-aligned, matching the `inline_field`
        // consolidation observed in Pass 90 smoke tests.
        //
        // Type coercion across rows is Spark's job (`Stack.checkInputDataTypes`
        // requires the K expressions per column to share a type; users write
        // explicit `CAST` in the fragment). DuckDB's list-literal element
        // widening handles the well-typed shape; a genuinely mixed-type
        // input arrives here only if Spark itself would have rejected it,
        // in which case DuckDB emits its own type-mismatch error.
        "stack_col" => {
            if f.args.is_empty() {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`stack_col` requires at least 1 argument (one per stack row)"
                );
            }
            let items = sql_join(f.args.iter(), ", ", |a| render_expr(a, schema))?;
            return Ok(format!("UNNEST([{items}])"));
        }
        // Pass 90 — synthetic FunctionCall names produced by the analyzer's
        // Project pre-pass (`expand_inline_projections`) when it fans
        // `F.inline(arr)` / `F.inline_outer(arr)` out into N per-struct-field
        // projections. Args: `(arr : Array<Struct<...>>, field_name : STRING)`.
        // Sibling `UNNEST(<arr>)` calls in a SELECT are consolidated by
        // DuckDB into a single row-multiplication (empirically verified —
        // see Pass 90 smoke). Corpus: inl-001, inl-002.
        "inline_field" => {
            if f.args.len() != 2 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`inline_field` requires exactly 2 arguments (arr, field_name)",
                );
            }
            let arr_sql = render_expr(&f.args[0], schema)?;
            let field_name = string_literal_arg(f, 1, "`inline_field` second argument")?;
            let field_q = quote_ident(&field_name);
            return Ok(format!("UNNEST({arr_sql}).{field_q}"));
        }
        "inline_outer_field" => {
            if f.args.len() != 2 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`inline_outer_field` requires exactly 2 arguments (arr, field_name)",
                );
            }
            let arr_sql = render_expr(&f.args[0], schema)?;
            let field_name = string_literal_arg(f, 1, "`inline_outer_field` second argument")?;
            // Build the struct-typed NULL sentinel from the resolved
            // `Array<Struct<...>>` schema so a NULL / empty array yields
            // exactly one all-NULL row (mirrors `explode_outer`'s
            // `[NULL]` sentinel, specialised to structs).
            let arr_ty = f.args[0].data_type(schema);
            let struct_fields = match &arr_ty {
                DataType::Array(inner, _) => match inner.as_ref() {
                    DataType::Struct(st) => &st.fields,
                    other => {
                        bail_boundary_fn!(
                            f.name.clone(),
                            format!(
                                "`inline_outer_field` requires `Array<Struct<...>>`, got `Array<{other:?}>`"
                            ),
                        );
                    }
                },
                other => {
                    bail_boundary_fn!(
                        f.name.clone(),
                        format!(
                            "`inline_outer_field` requires `Array<Struct<...>>`, got `{other:?}`"
                        ),
                    );
                }
            };
            let sentinel_fields = sql_join(struct_fields.iter(), ", ", |f0| {
                let name_q = quote_ident(&f0.name);
                let ty = render_data_type(&f0.data_type);
                Ok(format!("{name_q} := CAST(NULL AS {ty})"))
            })?;
            let sentinel = format!("struct_pack({sentinel_fields})");
            let field_q = quote_ident(&field_name);
            return Ok(format!(
                "UNNEST(CASE WHEN {arr_sql} IS NULL OR len({arr_sql}) = 0 THEN [{sentinel}] ELSE {arr_sql} END).{field_q}"
            ));
        }
        // Pass 91 — synthetic FunctionCall produced by the analyzer's Project
        // pre-pass (`expand_json_tuple_projections`) when it fans
        // `F.json_tuple(json, k1, ..., kN)` out into N per-key projections.
        // Args: `(json_expr, key : STRING literal)`. Emit
        // `json_extract_string(<json>, '$.<key>')` — same substrate as the
        // `get_json_object` session macro (matches Spark's `JsonTuple`
        // scalar-value semantics: quotes stripped, JSON null → NULL, missing
        // key → NULL). Analyzer pre-pass rejects unsafe key chars, so the
        // interpolated key is a safe single-quoted SQL literal. Corpus: json-002.
        "json_tuple_field" => {
            // These runtime guards are LOAD-BEARING, not duplication of the
            // `expand_json_tuple_projections` choke point: that pre-pass
            // covers only the calls IT synthesizes from `json_tuple`.
            // Nothing stops a user from invoking `json_tuple_field(...)`
            // directly (τ forwards unknown function names to emission by
            // design, with no allowlist — SQL front-end and DataFrame
            // converter alike), so a wrong-arity or unsafe-key call reaches
            // this arm un-choke-pointed and must get a graceful boundary
            // error, same as the sibling `inline_field` arms.
            if f.args.len() != 2 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`json_tuple_field` requires exactly 2 arguments (json, key)",
                );
            }
            let json_sql = render_expr(&f.args[0], schema)?;
            let key = string_literal_arg(f, 1, "`json_tuple_field` second argument")?;
            if key
                .chars()
                .any(|c| matches!(c, '\'' | '"' | '\\' | '.' | '[' | ']') || c.is_ascii_control())
            {
                bail_boundary_fn!(
                    f.name.clone(),
                    format!(
                        "`json_tuple_field` key `{key}` contains an unsafe character; \
                         it would path-walk or break the SQL literal"
                    ),
                );
            }
            let key_lit = sql_string_literal(&format!("$.{key}"));
            return Ok(format!("json_extract_string({json_sql}, {key_lit})"));
        }
        // Spark → thdck_spark_funcs extension remaps.
        // These functions require the ext6 extension, loaded at session
        // start by `DuckDbSession`.
        "hash" | "murmur3" => "spark_hash",
        "xxhash64" => "spark_xxhash64",
        "try_divide" => "spark_try_divide",
        // Spark's `crc32(binary)` — no `spark_crc32` in `thdck_spark_funcs`
        // ext6, so τ ships a bit-exact CRC-32-IEEE session macro
        // (`java.util.zip.CRC32` emulation) registered by
        // `DuckDbSession::spawn`. Long-term the C++ extension may absorb this;
        // the dispatch arm stays either way. Corpus: `hash-001`.
        "crc32" => "spark_crc32",
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
        // Spark's `json_object_keys(jsonStr)` returns the top-level object's
        // keys as `Array<String>` (NULL for a NULL/non-object input).
        // DuckDB's native `json_keys` already returns `VARCHAR[]` with the
        // same NULL-in/NULL-out and empty-array-for-non-object shape the
        // corpus witnesses exercise — a direct rename, no CAST needed.
        // Corpus: `test_json_object_keys`.
        "json_object_keys" => "json_keys",
        // Spark's `to_json(col[, options])` runs `JacksonGenerator` with
        // `SQLConf.JSON_GENERATOR_IGNORE_NULL_FIELDS=true` by default, which
        // omits object entries whose value is JSON `null` at every nesting
        // level (array elements that are null are preserved as `null`, and
        // empty-object containers stay as `{}`). DuckDB's native `to_json`
        // has no such option, so wrap the call with DuckDB's recursive
        // `json_strip_nulls` (JSON extension, already loaded at session
        // start per ADR-020) to match Spark exactly. When the caller
        // passes an explicit `MapLiteral{'ignoreNullFields': 'false'}`
        // options map, emit the bare `to_json` instead. Any other options
        // key or non-`MapLiteral` second argument is a Thunderduck-boundary
        // error (ADR-022). Corpus: `json-005`.
        "to_json" => match f.args.len() {
            1 => {
                let a = render_expr(&f.args[0], schema)?;
                return Ok(format!("json_strip_nulls(to_json({a}))"));
            }
            2 => {
                let a = render_expr(&f.args[0], schema)?;
                match parse_to_json_ignore_null_fields(&f.args[1]) {
                    Some(true) => return Ok(format!("json_strip_nulls(to_json({a}))")),
                    Some(false) => return Ok(format!("to_json({a})")),
                    None => {
                        bail_boundary_fn!(
                            f.name.clone(),
                            "`to_json` options: only \
                                     {'ignoreNullFields': 'true'|'false'} is supported \
                                     — τ boundary",
                        );
                    }
                }
            }
            _ => {
                bail_boundary_fn!(f.name.clone(), "`to_json` requires 1 or 2 arguments");
            }
        },
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
                bail_boundary_fn!(f.name.clone(), "`to_csv` requires exactly 1 argument");
            }
            let struct_args = match &f.args[0] {
                Expression::FunctionCall(inner)
                    if inner.name == "struct" || inner.name == "named_struct" =>
                {
                    // For `struct(a, b, c)` every arg is a field value.
                    // For `named_struct(k1, v1, k2, v2, ...)` only the
                    // odd-indexed args (v1, v2, ...) are field values.
                    if inner.name == "named_struct" {
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
                    bail_boundary_fn!(
                        f.name.clone(),
                        "τ boundary: `to_csv` currently supports only \
                                 a literal `struct(...)` / `named_struct(...)` \
                                 argument — got a different expression shape",
                    );
                }
            };
            let parts = sql_join(struct_args.iter(), ", ", |arg| {
                let val = render_expr(arg.unaliased(), schema)?;
                Ok(format!("CAST({val} AS VARCHAR)"))
            })?;
            return Ok(format!("concat_ws(',', {parts})"));
        }
        // Spark's `regexp_replace(str, pat, repl)` replaces ALL matches.
        // DuckDB's `regexp_replace(str, pat, repl)` replaces only the FIRST;
        // the 4th arg 'g' flag makes it global.
        "regexp_replace" => {
            if !(3..=4).contains(&f.args.len()) {
                bail_boundary_fn!(f.name.clone(), "`regexp_replace` requires 3 or 4 arguments");
            }
            let [s, p, r] = rendered_args(f, schema)?;
            return Ok(format!("regexp_replace({s}, {p}, {r}, 'g')"));
        }
        // Spark null-handling remaps (DuckDB uses coalesce).
        "nvl" => "coalesce",
        "nvl2" => {
            // Spark's `nvl2(a, b, c)` = if a is not null then b else c.
            let [a, b, c] = exact_args(f, schema, "`nvl2` requires exactly 3 arguments")?;
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
            let parts = sql_join(f.args[1..].iter(), ", ", |arg| {
                let dt = arg.data_type(schema);
                let arg_sql = render_expr(arg, schema)?;
                if matches!(dt, DataType::Array(_, _)) {
                    Ok(format!("array_to_string({arg_sql}, {sep})"))
                } else {
                    Ok(arg_sql)
                }
            })?;
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
        "ceil" | "ceiling" => return render_ceil_floor(f, schema, "ceil"),
        "floor" => return render_ceil_floor(f, schema, "floor"),
        // Spark `signum` returns Double; DuckDB `sign` returns the arg's
        // type. Cast to DOUBLE at emission.
        "sign" | "signum" => {
            let [a] = min_args(f, schema, "`signum` requires at least 1 argument")?;
            return Ok(format!("CAST(sign({a}) AS DOUBLE)"));
        }
        // Spark's `positive(x)` (`UnaryPositive`) is the identity — DuckDB
        // has no native `positive` scalar, so emit the argument unchanged
        // (parenthesized). Corpus: `test_positive`
        // (test_math_bitwise_date_differential).
        "positive" => {
            let [a] = exact_args(f, schema, "`positive` requires exactly 1 argument")?;
            return Ok(format!("({a})"));
        }
        // Spark's `bit_get`/`getbit(x, pos)` return the bit at 0-indexed
        // `pos` (from the LSB) of integral `x`, as a Byte (TINYINT). DuckDB
        // has no integral `bit_get` (`get_bit` only accepts BIT); compose
        // via shift + mask, cast to TINYINT to match type_inference. Corpus:
        // `test_bit_get` (test_math_bitwise_date_differential).
        "bit_get" | "getbit" => {
            let [x, pos] = exact_args(f, schema, "`bit_get` requires exactly 2 arguments")?;
            return Ok(format!("CAST((({x} >> {pos}) & 1) AS TINYINT)"));
        }
        // Spark's `make_dt_interval([days[, hours[, mins[, secs]]]])` builds a
        // day-time INTERVAL. DuckDB has no `make_dt_interval` scalar but
        // accepts `INTERVAL (expr) UNIT` arithmetic. Compose the interval by
        // summing each present component (missing components default to 0,
        // Spark's documented behavior). Corpus anchor: `intv-003`.
        "make_dt_interval" => {
            return render_make_interval(
                f,
                schema,
                &["DAY", "HOUR", "MINUTE"],
                true,
                "`make_dt_interval` takes at most 4 arguments",
            );
        }
        // Spark's `make_interval(years, months, weeks, days[, hours[, mins[, secs]]])`
        // builds a CalendarInterval. DuckDB has no `make_interval` scalar;
        // compose from individual `INTERVAL <n> UNIT` summands. Corpus: `intv-001`.
        "make_interval" | "try_make_interval" => {
            return render_make_interval(
                f,
                schema,
                &["YEAR", "MONTH", "WEEK", "DAY", "HOUR", "MINUTE"],
                true,
                "`make_interval` takes at most 7 arguments",
            );
        }
        // Spark's `make_ym_interval([years[, months]])` builds a year-month
        // INTERVAL. Same principle as `make_dt_interval`.
        "make_ym_interval" => {
            return render_make_interval(
                f,
                schema,
                &["YEAR", "MONTH"],
                false,
                "`make_ym_interval` takes at most 2 arguments",
            );
        }
        // Spark's `F.window(ts, "N unit")` — tumbling time-window over a
        // Timestamp column. Returns a `Struct{start: Timestamp, end: Timestamp}`
        // representing the bucket the row belongs to. τ transliterates to a
        // DuckDB `struct_pack` over `time_bucket` (unix-epoch aligned origin
        // matches Spark's `TimeWindow` default for tumbling `slide == window`,
        // `startTime == 0`). Corpus anchor: `win2-002`.
        //
        // Scope (2-arg tumbling only):
        //  - `args[1]` MUST be `Expression::Literal(String("N unit"))`; parsed
        //    via [`parse_window_duration_literal`].
        //  - 3+ arg (sliding / offset), non-literal duration, compound
        //    (`"1 day 3 hours"`), month/year (variable-length buckets diverge
        //    from `time_bucket`'s fixed-width semantics), signed / fractional /
        //    empty / unknown unit → boundary reject with `[TDCK-BOUNDARY]`.
        //
        // `"end"` is a DuckDB reserved keyword — quoted via `quote_ident`,
        // proven-safe idiom (same pattern as `named_struct` at ~L3667).
        "window" => {
            if f.args.len() != 2 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "[TDCK-BOUNDARY] `window`: only the 2-arg tumbling form \
                             (window(ts, duration)) is implemented; sliding / offset \
                             forms are not",
                );
            }
            let dur_str = match &f.args[1] {
                Expression::Literal(Literal {
                    value: LiteralValue::String(s),
                    ..
                }) => s.clone(),
                _ => {
                    bail_boundary_fn!(
                        f.name.clone(),
                        "[TDCK-BOUNDARY] `window`: duration must be a string \
                                 literal (`\"N unit\"` for {second,minute,hour,day,week})",
                    );
                }
            };
            let (n, unit) = parse_window_duration_literal(&dur_str).ok_or_else(|| {
                EmissionError::Unsupported {
                    kind: UnsupportedKind::Function,
                    name: f.name.clone(),
                    reason: format!(
                        "[TDCK-BOUNDARY] `window`: unsupported duration literal \
                         `{dur_str}`; accepted grammar is `\"N unit\"` where N is a \
                         positive integer and unit is one of \
                         {{second,minute,hour,day,week}} (singular or plural). \
                         Compound / month / year / fractional / signed / empty forms \
                         are not implemented."
                    ),
                }
            })?;
            let ts_sql = render_expr(&f.args[0], schema)?;
            let start_q = quote_ident("start");
            let end_q = quote_ident("end");
            return Ok(format!(
                "struct_pack({start_q} := time_bucket(INTERVAL '{n} {unit}', {ts_sql}), \
                 {end_q} := time_bucket(INTERVAL '{n} {unit}', {ts_sql}) + INTERVAL '{n} {unit}')"
            ));
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
            let [ts, tz] = rendered_args(f, schema)?;
            return Ok(format!(
                "timezone('UTC', timezone({tz}, timezone('UTC', CAST({ts} AS TIMESTAMPTZ))))"
            ));
        }
        // Spark's `from_utc_timestamp(ts, tz)` is the inverse — interpret
        // `ts` as UTC and convert to local wall-clock time in `tz`.
        "from_utc_timestamp" if f.args.len() == 2 => {
            let [ts, tz] = rendered_args(f, schema)?;
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
            let [arr, lambda] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL WHEN len({arr}) = 0 THEN false ELSE list_bool_or(list_transform({arr}, {lambda})) END"
            ));
        }
        // Spark's `forall(arr, x -> pred)` — mirror of `exists`. DuckDB has no
        // `list_all`; use `list_bool_and(list_transform(...))` with the
        // Spark-parity empty/NULL guard: NULL list → NULL, empty list → true.
        // Anchors: corpus hof-005.
        "forall" if f.args.len() == 2 => {
            let [arr, lambda] = rendered_args(f, schema)?;
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
        //   list_transform(range(0, least(len(a), len(b))),
        //                  i -> f_body[x → a[i], y → b[i]])
        // `a[i]` / `b[i]` are built as `ExtractValue` over array children, whose
        // emission implements Spark's 0-based GetArrayItem (index+1 into DuckDB's
        // 1-based `list_extract`, guarded). The iteration therefore ranges over
        // 0-based indices `0..least(len(a), len(b))`. Corpus: `hof-006`.
        "zip_with" if f.args.len() == 3 => {
            let [a_sql, b_sql] = rendered_args(f, schema)?;
            let Expression::Lambda(lam) = &f.args[2] else {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`zip_with` requires a lambda third argument"
                );
            };
            if lam.params.len() != 2 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`zip_with` lambda must take exactly 2 arguments",
                );
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
                "list_transform(range(0, least(len({a_sql}), len({b_sql}))), {idx_var} -> {body_sql})"
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
            let [arr, init, lambda] = rendered_args(f, schema)?;
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
            let order = match bool_literal(&f.args[1]) {
                Some(true) => "'ASC'".to_owned(),
                Some(false) => "'DESC'".to_owned(),
                None => {
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
            let [arr, sep] = rendered_args(f, schema)?;
            // Skip NULL elements to match Spark's default behavior.
            return Ok(format!(
                "array_to_string(list_filter({arr}, x -> x IS NOT NULL), {sep})"
            ));
        }
        "array_join" if f.args.len() == 3 => {
            let [arr, sep, null_repl] = rendered_args(f, schema)?;
            return Ok(format!(
                "array_to_string(list_transform({arr}, x -> coalesce(CAST(x AS VARCHAR), {null_repl})), {sep})"
            ));
        }
        // Spark array/list remaps — DuckDB uses `list_*` prefix.
        "sort_array" => "list_sort",
        "slice" => "list_slice",
        "array_contains" => "list_contains",
        // Spark's `array_distinct(a)` — distinct elements of `a`,
        // preserving the order elements FIRST appear (Spark's
        // `ArrayDistinct` is a linked-hash-set scan, not a sort). DuckDB's
        // `list_distinct` reorders by hash — wrong for Spark parity (same
        // defect `array_union`/`array_except`/`array_intersect` below all
        // avoid). Compose dedup-by-first-occurrence via
        // `list_position(a, x) = i` (keep only the index where `x` first
        // occurs). NULL propagates without an explicit guard: DuckDB's
        // `list_filter` returns NULL for a NULL list argument as-is
        // (verified against DuckDB directly). A NULL element is deduped
        // like any other value — Spark keeps exactly one, at its
        // first-occurrence position (verified live against Spark 4.1.1).
        // Corpus: `arr-005` (`schema_only` — a future pass can lift that
        // flag now that value order is fixed).
        "array_distinct" if f.args.len() == 1 => {
            let [a] = rendered_args(f, schema)?;
            return Ok(order_preserving_distinct(&a));
        }
        // Spark's `reverse` is overloaded: on a STRING it reverses the
        // characters (DuckDB's native `reverse` already matches — fall
        // through to the verbatim tail below); on an ARRAY it reverses
        // element order, which DuckDB's `reverse` does not accept
        // (`reverse(VARCHAR)` only) — dispatch to `list_reverse`.
        "reverse"
            if f.args.len() == 1
                && matches!(f.args[0].data_type(schema), DataType::Array(_, _)) =>
        {
            "list_reverse"
        }
        // Spark's `array_union(a, b)` — distinct elements of `a` followed
        // by distinct-new elements of `b`, preserving first-occurrence
        // order across the whole `a ++ b` sequence (Spark's `ArrayUnion`
        // is a single linked-hash-set scan over `a` then `b`, not a
        // sort). That's exactly `array_distinct(list_concat(a, b))`: `a`'s
        // own duplicates collapse to their first occurrence (since `a`
        // comes first in the concat), and `b`'s elements collapse to
        // their first occurrence within `b`, dropping any already seen in
        // `a`. NULL propagates: if either argument is NULL, the result is
        // NULL — DuckDB's `list_concat` treats a NULL list as empty
        // rather than propagating (verified directly), so an explicit
        // guard is required here (unlike plain `array_distinct` above,
        // where `list_filter` already propagates NULL on its own). Corpus:
        // `arr-011`.
        "array_union" if f.args.len() == 2 => {
            let [a, b] = rendered_args(f, schema)?;
            let concat = format!("list_concat({a}, {b})");
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR ({b}) IS NULL THEN NULL ELSE {} END",
                order_preserving_distinct(&concat)
            ));
        }
        // Spark's `array_except(a, b)` — distinct elements of `a` not
        // present in `b`, preserving the order elements first appear in
        // `a` (Spark's `ArrayExcept` is a linear hash-set scan over `a`,
        // not a sort). NULL propagates: if either argument is NULL, the
        // result is NULL. DuckDB has no order-preserving distinct-filter
        // (`list_distinct` reorders by hash, breaking Spark parity — see
        // `array_union` above); compose dedup-by-first-occurrence via
        // `list_position(a, x) = i` (keep only the index where `x` first
        // occurs) together with a null-safe "not present in `b`" check.
        // DuckDB's `list_contains(b, x)` returns NULL — not FALSE — when
        // `x` is NULL, even if `b` itself contains a NULL element
        // (verified directly), which would wrongly drop a NULL that
        // should survive; `list_position(b, x) IS NULL` is null-safe
        // (verified live against Spark 4.1.1: `array_except(array(1, NULL,
        // 2), array(3, 4))` keeps the NULL). Corpus: `arr2-005`.
        "array_except" if f.args.len() == 2 => {
            let [a, b] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR ({b}) IS NULL THEN NULL ELSE list_filter({a}, (x, i) -> list_position({a}, x) = i AND {}) END",
                not_null_safe_member(&b)
            ));
        }
        // Spark's `array_intersect(a, b)` — distinct elements of `a` also
        // present in `b`, preserving the order elements first appear in
        // `a` (mirrors `array_except`'s shape with the membership test
        // inverted). DuckDB's `list_intersect` sorts/reorders — wrong for
        // Spark parity (verified directly: `list_intersect([3,1,2,1],
        // [2,1])` returns `[1, 2]`, not `a`'s order). NULL propagates: if
        // either argument is NULL, the result is NULL. The membership test
        // must be null-safe the same way as `array_except` — verified live
        // against Spark 4.1.1: `array_intersect(array(1, NULL, 2),
        // array(NULL, 2))` returns `[NULL, 2]` (a NULL common to both
        // sides is kept), which `list_contains` alone cannot reproduce.
        "array_intersect" if f.args.len() == 2 => {
            let [a, b] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR ({b}) IS NULL THEN NULL ELSE list_filter({a}, (x, i) -> list_position({a}, x) = i AND {}) END",
                null_safe_member(&b)
            ));
        }
        // Spark's `array_position(arr, item)` returns a 1-based index or
        // `0` if the item is not found (NULL only when the array itself is
        // NULL). DuckDB's `list_position` returns NULL for not-found.
        // Coalesce with 0, but propagate NULL for a NULL array. Corpus:
        // `arr-007`.
        "array_position" if f.args.len() == 2 => {
            let [arr, item] = rendered_args(f, schema)?;
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
            let [a] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({a}) IS NULL OR list_bool_or(list_transform({a}, x -> x IS NULL)) THEN NULL ELSE flatten({a}) END"
            ));
        }
        "arrays_zip" if !f.args.is_empty() => {
            let arg_sqls: Vec<String> = f
                .args
                .iter()
                .map(|a| render_expr(a, schema))
                .collect::<Result<_, _>>()?;
            // Derive per-arg field names. Alias / column ref wins;
            // everything else uses the positional integer string.
            let mut names: Vec<String> = f
                .args
                .iter()
                .enumerate()
                .map(|(i, arg)| super::struct_names::derive_zip_field_name(arg, i))
                .collect();
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
                let lens = arg_sqls
                    .iter()
                    .map(|s| format!("len({s})"))
                    .collect::<Vec<_>>()
                    .join(", ");
                format!("least({lens})")
            };
            let struct_fields = sql_join(
                names.iter().zip(arg_sqls.iter()),
                ", ",
                |(name, arg_sql)| {
                    let name_q = quote_ident(name);
                    Ok(format!("{name_q} := ({arg_sql})[{idx_var}]"))
                },
            )?;
            let struct_body = format!("struct_pack({struct_fields})");
            return Ok(format!(
                "list_transform(range(1, {len_expr} + 1), {idx_var} -> {struct_body})"
            ));
        }
        // Spark's `arrays_overlap(a, b)` → Boolean; DuckDB uses
        // `list_has_any(a, b)` (no `arrays_overlap` function).
        // Corpus: `arr-011`.
        "arrays_overlap" => "list_has_any",
        // Spark's `size`/`cardinality` on a MAP → element count. DuckDB's
        // `len` rejects MAP (`len(VARCHAR|BIT|ANY[])` only); `cardinality`
        // is the MAP-only counterpart (DuckDB rejects `cardinality` on a
        // LIST). DuckDB's `cardinality` returns UBIGINT — Arrow UInt64,
        // which PySpark's Arrow→Spark type conversion rejects outright
        // (`UNSUPPORTED_DATA_TYPE_FOR_ARROW_CONVERSION`); cast down to
        // BIGINT (signed) to match the sibling `len` branch's native
        // return type and keep the wire type Arrow-safe. Array/other args
        // keep the existing `len` rename.
        "size" | "cardinality"
            if f.args.len() == 1 && matches!(f.args[0].data_type(schema), DataType::Map { .. }) =>
        {
            let [a] = rendered_args(f, schema)?;
            return Ok(format!("CAST(cardinality({a}) AS BIGINT)"));
        }
        "size" | "cardinality" => "len",
        // Spark's `element_at(coll, k)` — for Array, DuckDB's
        // `element_at(list, i)` returns a 1-element list containing the
        // element (or an empty list on OOB); for Map, it returns a
        // 1-element list containing the value (or empty on missing key).
        // Both cases need the trailing `[1]` extractor to unwrap; the
        // wrapped list's `[1]` yields NULL on empty. Corpus: `map-004`,
        // `arr-008`.
        "element_at" if f.args.len() == 2 => {
            let [coll, key] = rendered_args(f, schema)?;
            let coll_ty = f.args[0].data_type(schema);
            if let DataType::Map { .. } = coll_ty {
                // Unwrap the 1-element list DuckDB returns from
                // `element_at(MAP, key)`. Empty list (missing key) yields
                // NULL on `[1]`, matching Spark's map miss semantics.
                return Ok(format!("element_at({coll}, {key})[1]"));
            }
            // Array (or unknown, which the type inferencer routes here by
            // default): DuckDB's `list_extract` is 1-based but returns NULL
            // silently on OOB / index-0 / negative-OOB. Spark 4.1 ANSI
            // instead throws `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`; wrap the
            // call in a CASE so the guard raises at DuckDB level with the
            // Spark-verbatim class + message. The parenthesization of `idx`
            // and `arr` inside the CASE lets negative literals (`-4`) and
            // arbitrary expressions bind correctly (see `error(...)`
            // helper). NULL array short-circuits to NULL (Spark
            // `nullSafeEval`). Corpus: `arr-008`.
            let err = super::spark_errors::SparkError::InvalidArrayIndex {
                idx_sql: key.clone(),
                arr_sql: coll.clone(),
            }
            .throw_expr();
            return Ok(format!(
                "CASE WHEN ({coll}) IS NULL THEN NULL WHEN ({key}) = 0 OR abs(({key})) > len(({coll})) THEN {err} ELSE list_extract(({coll}), ({key})) END"
            ));
        }
        // Spark's `try_element_at(coll, k)` is the never-throw variant of
        // `element_at` — OOB / invalid index yields NULL instead of raising
        // `INVALID_ARRAY_INDEX_IN_ELEMENT_AT`. DuckDB's `list_extract`
        // matches that non-throwing semantic natively (see the Array arm
        // above); Map preserves the unwrap. No corpus witness today, but
        // the nullability arm at `expression.rs:1035` already declares
        // `try_element_at` — colocated here so future adds work
        // end-to-end.
        "try_element_at" if f.args.len() == 2 => {
            let [coll, key] = rendered_args(f, schema)?;
            let coll_ty = f.args[0].data_type(schema);
            if let DataType::Map { .. } = coll_ty {
                return Ok(format!("element_at({coll}, {key})[1]"));
            }
            return Ok(format!("list_extract({coll}, {key})"));
        }
        // Spark's `typeof(x)` returns lowercase type strings (`double`,
        // `decimal(9,2)`, `array<string>`); DuckDB's `typeof` returns
        // uppercase (`DOUBLE`, `DECIMAL(9,2)`). Wrap with `lower()` for
        // Spark parity. Corpus: `meta-003`.
        "typeof" => {
            let [a] = exact_args(f, schema, "`typeof` requires exactly 1 argument")?;
            return Ok(format!("lower(typeof({a}))"));
        }
        // Spark's `array_append(arr, elem)` / `array_prepend(elem, arr)`
        // propagate NULL: if the array argument is NULL the result is NULL.
        // DuckDB's `array_append`/`array_prepend` return `[elem]` for a NULL
        // array, silently coercing NULL to an empty list. Wrap with a NULL
        // guard on the array side to match Spark. Corpus: `arr2-001`.
        "array_append" if f.args.len() == 2 => {
            let [arr, elem] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL ELSE array_append({arr}, {elem}) END"
            ));
        }
        "array_prepend" if f.args.len() == 2 => {
            // Spark signature is `array_prepend(arr, elem)`; the session
            // macro (see `session.rs`) rewrites this to DuckDB's
            // `list_prepend(elem, arr)`. Preserve NULL on the array arg.
            let [arr, elem] = rendered_args(f, schema)?;
            return Ok(format!(
                "CASE WHEN ({arr}) IS NULL THEN NULL ELSE array_prepend({arr}, {elem}) END"
            ));
        }
        // Spark's `to_date(x)` (single-arg) → simple cast to DATE.
        // Two-arg form `to_date(str, fmt)` uses Spark SimpleDateFormat tokens;
        // DuckDB parses with `strptime` (strftime tokens) — translate + cast.
        "to_date" => {
            if !(1..=2).contains(&f.args.len()) {
                bail_boundary_fn!(f.name.clone(), "`to_date` requires 1 or 2 arguments");
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
                bail_boundary_fn!(f.name.clone(), "`to_timestamp` requires 1 or 2 arguments");
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
                bail_boundary_fn!(f.name.clone(), "`unix_timestamp` requires 1 or 2 arguments");
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
                bail_boundary_fn!(f.name.clone(), "`from_unixtime` requires 1 or 2 arguments");
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
            let [a, b] = exact_args(f, schema, "`nanvl` requires exactly 2 arguments")?;
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
                bail_boundary_fn!(
                    f.name.clone(),
                    format!("`{}` requires 1 or 2 arguments", name_lower),
                );
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
            let [x, n] = exact_args(f, schema, "`shiftleft` requires exactly 2 arguments")?;
            return Ok(format!("({x} * (1::BIGINT << ({n})))"));
        }
        // Spark's `shiftright(x, n)` — arithmetic (sign-preserving) right
        // shift. DuckDB's `>>` on signed integers is arithmetic on BIGINT,
        // so a direct emit works for non-negative and matches Spark for
        // negative on 2's-complement. Pass-through the shift via arithmetic
        // division form for parity across widths: `x >> n` in DuckDB is
        // legal for negative x (unlike `<<`), so we can pass through.
        "shiftright" => {
            let [x, n] = exact_args(f, schema, "`shiftright` requires exactly 2 arguments")?;
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
                bail_boundary_fn!(f.name.clone(), "`bround` requires 1 or 2 arguments");
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
            let [a, b] = exact_args(f, schema, "`hypot` requires exactly 2 arguments")?;
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
                bail_boundary_fn!(f.name.clone(), "`conv` requires exactly 3 arguments");
            }
            let s = render_expr(&f.args[0], schema)?;
            // Rendered only for its error path: an unrenderable from_base
            // expression must still surface its boundary error (the rendered
            // value itself is unused).
            let _from_base = render_expr(&f.args[1], schema)?;
            // Spark's `conv(str, from_base, to_base)` renders the value as
            // UNSIGNED 64-bit. DuckDB's `to_base(bigint, base)` produces
            // signed output. For base 2 and base 16, DuckDB's `bin` and
            // `hex` on BIGINT emit the two's-complement (unsigned) bytes,
            // matching Spark. For other to_base values, boundary-error.
            // Corpus witness: `math-013` uses to_base ∈ {2}.
            //
            // DEVIATION: `int_literal_value` maps an i32-overflowing Long
            // to `None` (boundary error) where the old inline match wrapped
            // with `as i32` — a pathological, corpus-unwitnessed input.
            let to_base_lit = int_literal_value(&f.args[2]);
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
                    bail_boundary_fn!(
                        f.name.clone(),
                        "`conv` only implemented for to_base ∈ {2, 16}",
                    );
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
            let [a] = exact_args(f, schema, "`hex` requires exactly 1 argument")?;
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
            if !f.args.len().is_multiple_of(2) || f.args.is_empty() {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`named_struct` requires an even, non-zero arg count",
                );
            }
            let parts = sql_join(f.args.chunks(2), ", ", |pair| {
                let Some(key) = literal_string_arg(&pair[0]) else {
                    bail_boundary_fn!(
                        f.name.clone(),
                        "`named_struct` keys must be string literals",
                    );
                };
                let val = render_expr(&pair[1], schema)?;
                let key_q = quote_ident(&key);
                Ok(format!("{key_q} := {val}"))
            })?;
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
            let arg_sqls: Vec<String> = f
                .args
                .iter()
                .map(|a| render_expr(a, schema))
                .collect::<Result<_, _>>()?;
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
            let [a] = exact_args(f, schema, "`isnull` requires exactly 1 argument")?;
            return Ok(format!("({a} IS NULL)"));
        }
        "isnotnull" => {
            let [a] = exact_args(f, schema, "`isnotnull` requires exactly 1 argument")?;
            return Ok(format!("({a} IS NOT NULL)"));
        }
        // Spark's `like`/`ilike`/`rlike` as functions — DuckDB uses
        // operator syntax `x LIKE pattern` / `x ILIKE pattern` /
        // `regexp_matches(x, pattern)`.
        "like" => {
            let [a, b] = exact_args(f, schema, "`like` requires exactly 2 arguments")?;
            return Ok(format!("({a} LIKE {b})"));
        }
        "ilike" => {
            let [a, b] = exact_args(f, schema, "`ilike` requires exactly 2 arguments")?;
            return Ok(format!("({a} ILIKE {b})"));
        }
        "rlike" | "regexp_like" | "regexp" => {
            let [a, b] = exact_args(f, schema, "`rlike` requires exactly 2 arguments")?;
            return Ok(format!("regexp_matches({a}, {b})"));
        }
        // Spark's `<=>(a, b)` eqNullSafe — DuckDB uses IS NOT DISTINCT FROM.
        "eqnullsafe" | "<=>" => {
            let [a, b] = exact_args(f, schema, "`eqNullSafe` requires exactly 2 arguments")?;
            return Ok(format!("({a} IS NOT DISTINCT FROM {b})"));
        }
        // Spark's `split(str, pattern[, limit])` — DuckDB's `split(str,
        // pat)` has no limit argument. `limit <= 0` means "unlimited",
        // identical to the 2-arg form (verified live against Spark 4.1.1:
        // trailing empty strings survive at limit 0 same as limit < 0).
        // `limit > 0` caps the result at `limit` elements: the first
        // `limit - 1` pieces come from the unlimited split verbatim, and
        // the last piece is the delimiter-rejoined remainder (Java/Spark
        // `String.split(regex, limit)` semantics — the pattern is applied
        // at most `limit - 1` times, so the tail is never re-split).
        "split" => {
            if f.args.len() >= 3 {
                let [a, b, c] = min_args(f, schema, "`split` requires at least 2 arguments")?;
                let full = format!("split({a}, {b})");
                let full_len = format!("len({full})");
                return Ok(format!(
                    "CASE WHEN ({a}) IS NULL OR ({b}) IS NULL OR ({c}) IS NULL THEN NULL \
WHEN ({c}) <= 0 OR {full_len} <= ({c}) THEN {full} \
ELSE list_slice({full}, 1, ({c}) - 1) || [array_to_string(list_slice({full}, ({c}), {full_len}), {b})] END"
                ));
            }
            let [a, b] = min_args(f, schema, "`split` requires at least 2 arguments")?;
            return Ok(format!("split({a}, {b})"));
        }
        // Spark bitwise ops arriving as function calls (name is symbolic).
        // DuckDB uses operator form.
        "&" | "bitwise_and" | "bitwiseand" => {
            let [a, b] = exact_args(f, schema, "`bitwiseAND` requires exactly 2 arguments")?;
            return Ok(format!("({a} & {b})"));
        }
        "|" | "bitwise_or" | "bitwiseor" => {
            let [a, b] = exact_args(f, schema, "`bitwiseOR` requires exactly 2 arguments")?;
            return Ok(format!("({a} | {b})"));
        }
        "^" | "bitwise_xor" | "bitwisexor" => {
            let [a, b] = exact_args(f, schema, "`bitwiseXOR` requires exactly 2 arguments")?;
            return Ok(format!("xor({a}, {b})"));
        }
        // (signum handled above with explicit DOUBLE cast.)
        // Spark's `sha2(str, bits)` → DuckDB `sha256(str)` (Spark defaults
        // bits=256; we ignore the bits arg — non-256 surfaces later as
        // per-case follow-up if it fires).
        "sha2" => {
            let [s] = min_args(f, schema, "`sha2` requires at least 1 argument")?;
            return Ok(format!("sha256({s})"));
        }
        // Spark `sha`/`sha1` → DuckDB `sha1`.
        "sha" => "sha1",
        // Spark's `add_months(date, n)` — DuckDB uses `date + INTERVAL n MONTH`,
        // but DuckDB promotes `DATE + INTERVAL` to TIMESTAMP. Spark's
        // `add_months` always returns DATE; the `render_function_call`
        // wrapper (via `needs_date_return_cast`) supplies the CAST back to
        // DATE so collected values come back as date, not datetime. DuckDB's
        // end-of-month clamp (e.g. Jan 31 + 1 month = Feb 29 in a leap year)
        // survives the CAST.
        "add_months" => {
            let [d, n] = exact_args(f, schema, "`add_months` requires exactly 2 arguments")?;
            return Ok(format!("({d} + INTERVAL ({n}) MONTH)"));
        }
        // Spark's `datediff(end, start)` (2 args, days-diff) → DuckDB's
        // `datediff('day', start, end)` (3 args, unit-prefixed).
        "datediff" => {
            let [end, start] = exact_args(f, schema, "`datediff` requires exactly 2 arguments")?;
            return Ok(format!("datediff('day', {start}, {end})"));
        }
        // Spark's `timestampadd(unit, quantity, ts)` → `ts + n * INTERVAL 1 unit`.
        // The leading UNIT is a string literal (demoted in the SparkSQL lowering /
        // proto converter). DuckDB has no `QUARTER` interval keyword, so a quarter
        // is emitted as `(n * 3) MONTH`. The projection-slot CAST stamps the
        // Spark-parity TIMESTAMP return type. Corpus witness: `intv-006`
        // (`timestampadd(MONTH, 3, last_login)`).
        "timestampadd" => {
            if f.args.len() != 3 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`timestampadd` requires exactly 3 arguments (unit, quantity, ts)",
                );
            }
            let unit = string_literal_arg(f, 0, "timestampadd unit")?;
            let n = render_expr(&f.args[1], schema)?;
            let ts = render_expr(&f.args[2], schema)?;
            let interval = spark_add_interval_sql(&f.name, &unit, &n)?;
            return Ok(format!("({ts} + {interval})"));
        }
        // Spark's `timestampdiff(unit, start, end)` returns BIGINT — the whole
        // number of `unit`s from `start` to `end`, truncated toward zero. For the
        // fixed-length units this is `(epoch_us(end) - epoch_us(start)) / micros`.
        // Calendar units (MONTH/QUARTER/YEAR) need day-of-month-aware calendar
        // arithmetic that τ does not yet emit — surface an honest
        // Thunderduck-boundary error (ADR-022) rather than the boundary-counting
        // `date_diff`, which diverges from Spark for sub-unit remainders.
        "timestampdiff" => {
            if f.args.len() != 3 {
                bail_boundary_fn!(
                    f.name.clone(),
                    "`timestampdiff` requires exactly 3 arguments (unit, start, end)",
                );
            }
            let unit = string_literal_arg(f, 0, "timestampdiff unit")?;
            let start = render_expr(&f.args[1], schema)?;
            let end = render_expr(&f.args[2], schema)?;
            return spark_diff_sql(&f.name, &unit, &start, &end);
        }
        "months_between" => {
            let [a, b] = min_args(f, schema, "`months_between` requires at least 2 arguments")?;
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
        // DuckDB promotes `DATE + INTERVAL` to TIMESTAMP; Spark's `date_add`
        // always returns DATE. The `render_function_call` wrapper (via
        // `needs_date_return_cast`) supplies the CAST back to DATE (same
        // rule as `add_months` above).
        "date_add" => {
            let [d, n] = exact_args(f, schema, "`date_add` requires exactly 2 arguments")?;
            return Ok(format!("({d} + INTERVAL ({n}) DAY)"));
        }
        "date_sub" => {
            let [d, n] = exact_args(f, schema, "`date_sub` requires exactly 2 arguments")?;
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
            let arg_sqls: Vec<String> = f
                .args
                .iter()
                .map(|a| render_expr(a, schema))
                .collect::<Result<_, _>>()?;
            let null_guard = arg_sqls
                .iter()
                .map(|s| format!("({s}) IS NULL"))
                .collect::<Vec<_>>()
                .join(" OR ");
            let inner = arg_sqls.join(", ");
            return Ok(format!(
                "(CASE WHEN {null_guard} THEN NULL ELSE concat({inner}) END)"
            ));
        }
        // Spark's `isnan(x)` — schema is BOOLEAN non-nullable. DuckDB's
        // `isnan(NULL)` returns NULL; wrap in `COALESCE(..., FALSE)` to
        // match Spark's non-null semantics. Corpus witness: `cond-010`.
        "isnan" | "is_nan" => {
            let [a] = exact_args(f, schema, "`isnan` requires exactly 1 argument")?;
            return Ok(format!("COALESCE(isnan({a}), FALSE)"));
        }
        // Spark's `find_in_set(needle, csv)` — 1-based position of `needle`
        // in comma-separated `csv`, or 0 if not found. DuckDB has no
        // `find_in_set`; emit `COALESCE(list_position(string_split(csv, ','), needle), 0)`.
        "find_in_set" => {
            let [needle, csv] =
                exact_args(f, schema, "`find_in_set` requires exactly 2 arguments")?;
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
                bail_boundary_fn!(f.name.clone(), "`elt` requires at least 2 arguments");
            }
            let idx = render_expr(&f.args[0], schema)?;
            let items = sql_join(f.args[1..].iter(), ", ", |arg| render_expr(arg, schema))?;
            return Ok(format!("([{items}])[{idx}]"));
        }
        // Spark's `from_json(json_str, schema_ddl[, options])` parses a
        // JSON string per a Spark DDL schema literal (e.g.
        // `"a INT, b ARRAY<STRING>"`). DuckDB's `from_json(str, json_schema)`
        // takes a JSON-object schema (`'{"a": "INTEGER"}'`) instead — τ
        // translates the Spark DDL literal into DuckDB's JSON schema shape
        // for the common no-options case. Nested `STRUCT<...>` fields
        // recurse. Corpus witnesses: `json-003`, `json-004`. Emits a
        // Thunderduck-boundary error when the schema is non-literal or uses
        // shapes τ does not currently translate (MAP, DECIMAL(p,s)). The
        // three-arg options-map form is rejected explicitly by the
        // `!= 2` guard below — otherwise it would silently pass through
        // as literal `from_json(...)` and DuckDB would raise an opaque
        // scalar-not-found error.
        "from_json" if f.args.len() != 2 => {
            bail_boundary_fn!(
                f.name.clone(),
                "`from_json` options-map form (3-arg) not supported — τ boundary",
            );
        }
        "from_json" if f.args.len() == 2 => {
            let json_str = render_expr(&f.args[0], schema)?;
            if let Some(ddl) = literal_string_arg(&f.args[1]) {
                if let Some(duck_schema) = spark_ddl_schema_to_duckdb_json(&ddl) {
                    // Emit the schema as a single-quoted DuckDB JSON literal;
                    // internal double-quotes are safe (no `'` inside).
                    return Ok(format!("from_json({json_str}, '{duck_schema}')"));
                }
            }
            bail_boundary_fn!(
                f.name.clone(),
                "`from_json` with a non-literal DDL schema or unsupported \
                         DDL shape (τ handles the digit-schema field-list form)",
            );
        }
        // Spark's `from_csv(csv_str, schema_ddl[, options])` parses a
        // comma-separated string per a Spark DDL schema literal (e.g.
        // `"qty INT, label STRING, price DOUBLE"`). DuckDB has no scalar
        // `from_csv` — τ synthesizes an equivalent struct via
        // `split_part(csv, ',', i)` per field plus `try_cast(..., '') AS T`
        // for numerics and `nullif(..., '')` for strings (matching Spark's
        // default `nullValue = ""`). The whole expression is guarded by
        // `CASE WHEN csv IS NULL THEN NULL ELSE ... END` so a NULL input
        // yields a NULL struct (not a struct-of-NULLs). Corpus witness:
        // `json-007`. Emits a Thunderduck-boundary error when the schema
        // is non-literal or uses shapes τ does not currently translate
        // (nested STRUCT/ARRAY/MAP, DECIMAL(p,s)) — Spark's `from_csv`
        // itself is a flat-primitive schema function, so the 2-arg surface
        // covers the intended shape. The three-arg options-map form is
        // rejected explicitly by the `!= 2` guard below — otherwise it
        // would silently pass through as literal `from_csv(...)` and
        // DuckDB would raise an opaque scalar-not-found error.
        //
        // KNOWN DEVIATION: this manual split ignores CSV quoting rules
        // (embedded commas, quoted strings, escapes). The corpus witness
        // uses simple unquoted values; documenting the gap for future work.
        "from_csv" if f.args.len() != 2 => {
            bail_boundary_fn!(
                f.name.clone(),
                "`from_csv` options-map form (3-arg) not supported — τ boundary",
            );
        }
        "from_csv" if f.args.len() == 2 => {
            let csv_str = render_expr(&f.args[0], schema)?;
            if let Some(ddl) = literal_string_arg(&f.args[1]) {
                if let Some(st) = from_csv_ddl_to_struct(&ddl) {
                    let parts = sql_join(st.fields.iter().enumerate(), ", ", |(i, field)| {
                        let idx = i + 1;
                        let split = format!("split_part({csv_str}, ',', {idx})");
                        let name_q = quote_ident(&field.name);
                        let field_expr = match &field.data_type {
                            DataType::String => format!("nullif({split}, '')"),
                            other => {
                                let ty = render_data_type(other);
                                format!("try_cast(nullif({split}, '') AS {ty})")
                            }
                        };
                        Ok(format!("{name_q} := {field_expr}"))
                    })?;
                    return Ok(format!(
                        "CASE WHEN ({csv_str}) IS NULL THEN NULL ELSE struct_pack({parts}) END"
                    ));
                }
            }
            bail_boundary_fn!(
                f.name.clone(),
                "`from_csv` with a non-literal DDL schema or unsupported \
                         DDL shape (τ handles the flat primitive field-list form)",
            );
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
            let (_, cast, _) = to_number_parts(
                f,
                schema,
                &ToNumberMsgs {
                    arity: "`try_to_number` requires exactly 2 arguments",
                    fmt_literal:
                        "`try_to_number` requires a string literal for the format argument",
                    fmt_unsupported: |fmt| {
                        format!(
                            "`try_to_number`: unsupported format string `{fmt}` (τ only \
                             handles `9`/`0`/`.` digit templates)"
                        )
                    },
                },
            )?;
            return Ok(cast);
        }
        // Spark's `to_number(str, fmt)` mirrors `try_to_number` when parsing
        // succeeds but ANSI-throws `[INVALID_FORMAT.MISMATCH_INPUT]` on
        // format mismatch (row-level; Spark 4.1's ToNumberParser). τ
        // emulates by wrapping `try_cast(str AS DECIMAL(p, s))` in a
        // CASE guard: NULL input passes through (Spark's `nullSafeEval`),
        // non-NULL input that fails `try_cast` raises the ANSI class.
        // Corpus witness: `parse-003`.
        "to_number" => {
            let (s, cast, fmt) = to_number_parts(
                f,
                schema,
                &ToNumberMsgs {
                    arity: "`to_number` requires exactly 2 arguments",
                    fmt_literal: "`to_number` requires a string literal for the format argument",
                    fmt_unsupported: |fmt| {
                        format!(
                            "`to_number`: unsupported format string `{fmt}` (τ only \
                             handles `9`/`0`/`.`/`,` digit templates)"
                        )
                    },
                },
            )?;
            // IS NOT NULL guard AND error message reference the RAW input `s`
            // (what the user passed), NOT the grouping-stripped form — so the
            // reported input matches user-visible text and the guard doesn't
            // trip on empty-string-after-strip artefacts.
            let throw = super::spark_errors::SparkError::InvalidFormatMismatch {
                fmt,
                input_sql: s.clone(),
            }
            .throw_expr();
            return Ok(format!(
                "CASE WHEN ({s}) IS NOT NULL AND ({cast}) IS NULL THEN {throw} ELSE {cast} END"
            ));
        }
        // Spark's `url_encode(s)` uses application/x-www-form-urlencoded
        // encoding: spaces become `+`, everything else is `%HH`. DuckDB's
        // `url_encode(s)` uses RFC 3986 percent-encoding (spaces → `%20`).
        // Bridge by post-substituting `%20 → +`. Corpus witness: `parse-002`.
        "url_encode" => {
            let [s] = exact_args(f, schema, "`url_encode` requires exactly 1 argument")?;
            return Ok(format!("replace(url_encode({s}), '%20', '+')"));
        }
        // Spark's `url_decode(s)` mirrors form-urlencoded (accepts `+` as
        // space). DuckDB's `url_decode(s)` leaves `+` literal. Bridge by
        // pre-substituting `+` → `%20` before decoding.
        "url_decode" => {
            let [s] = exact_args(f, schema, "`url_decode` requires exactly 1 argument")?;
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
                bail_boundary_fn!(f.name.clone(), "`parse_url` requires 2 or 3 arguments");
            }
            let url = render_expr(&f.args[0], schema)?;
            let part = string_literal_arg_or(
                &f.args[1],
                &f.name,
                "`parse_url` requires a string literal for the part argument",
            )?;
            let part_upper = part.to_ascii_uppercase();
            let pattern: String = match part_upper.as_str() {
                "HOST" => "^[^:]+://(?:[^@/]+@)?([^:/?#]+)".to_owned(),
                "PROTOCOL" => "^([^:]+)://".to_owned(),
                "PATH" => "^[^:]+://[^/?#]*([^?#]*)".to_owned(),
                "QUERY" => {
                    if f.args.len() == 3 {
                        let key = string_literal_arg_or(
                            &f.args[2],
                            &f.name,
                            "`parse_url` with 3 arguments requires a string literal key",
                        )?;
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
                    bail_boundary_fn!(
                        f.name.clone(),
                        format!("`parse_url` part `{other}` not supported"),
                    );
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
                bail_boundary_fn!(f.name.clone(), "`overlay` requires 3 or 4 arguments");
            }
            let [s, r, p] = rendered_args(f, schema)?;
            let length_expr = if f.args.len() == 4 {
                render_expr(&f.args[3], schema)?
            } else {
                format!("length({r})")
            };
            return Ok(format!(
                "(substring({s}, 1, ({p}) - 1) || {r} || substring({s}, ({p}) + ({length_expr})))"
            ));
        }
        // ADR-006: Spark ANSI `pmod`/`mod` throw REMAINDER_BY_ZERO on a zero
        // divisor; DuckDB's `pmod` macro / `mod` return NULL. Guard the call so
        // a zero second argument raises Spark's error class.
        "pmod" | "mod" if f.args.len() == 2 => {
            let call = format!("{name_lower}({args_sql})");
            if is_nonzero_literal(&f.args[1]) {
                return Ok(call);
            }
            let divisor = render_expr(&f.args[1], schema)?;
            return Ok(super::spark_errors::ansi_throw_if(
                &format!("({divisor}) = 0"),
                super::spark_errors::SparkError::RemainderByZero,
                &call,
            ));
        }
        _ => &name_lower,
    };
    Ok(format!("{duck_name}({args_sql})"))
}

/// Is `f` a single-argument `avg`/`mean` call over a `DECIMAL` argument?
/// Shared predicate between the grouped ([`render_aggregate`]) and windowed
/// ([`render_window`]) decimal-avg interception points — `sum`/`try_sum`,
/// `try_avg`, and integer/float `avg` all return `false` here and keep
/// their existing (unrelated) emission paths.
fn is_decimal_avg(f: &FunctionCall, schema: &Schema) -> bool {
    matches!(f.name.as_str(), "avg" | "mean")
        && f.args.len() == 1
        && matches!(f.args[0].data_type(schema), DataType::Decimal { .. })
}

/// Render a decimal `avg`/`mean` call routed through the ext6 extension's
/// `spark_avg` aggregate, which returns DECIMAL natively (unlike DuckDB's
/// native `avg`, which widens a DECIMAL argument to DOUBLE). `over`, when
/// `Some`, is a pre-rendered `OVER (...)` clause text (from
/// [`render_over_clause`]) spliced onto `spark_avg(...)` — the OVER must
/// land *inside* the outer CAST since `CAST(...) OVER (...)` is invalid SQL.
///
/// The outer CAST targets the aggregate's Spark-analyzer-declared return
/// type (`TypeInferenceEngine::aggregate_return_type`'s `AvgLike` formula —
/// the same source [`render_projection_slot`]/[`spark_return_cast`] use for
/// their Spark-parity casts). The shipped `spark_avg` already returns this
/// exact `(precision, scale)` for the corpus's decimal shapes (pass-13
/// probe), so the CAST is idempotent there; it stays for correctness on any
/// other precision/scale and to make the emitted type explicit.
fn render_decimal_avg(
    f: &FunctionCall,
    over: Option<&str>,
    schema: &Schema,
) -> Result<String, EmissionError> {
    let arg_type = f.args[0].data_type(schema);
    let distinct = if f.distinct { "DISTINCT " } else { "" };
    let arg_sql = render_expr(&f.args[0], schema)?;
    let inner = format!("spark_avg({distinct}{arg_sql})");
    let inner = match over {
        Some(o) => format!("{inner} {o}"),
        None => inner,
    };
    let ret_type = TypeInferenceEngine::aggregate_return_type(&f.name, &arg_type);
    Ok(format!("CAST({inner} AS {})", render_data_type(&ret_type)))
}

/// Render an aggregate function call. Primitives (`count`, `sum`, `avg`,
/// `min`, `max`, `count_distinct`) pass through with Spark-parity CASTs
/// applied by [`spark_aggregate_return_cast`]. Unknown aggregate names
/// surface as a `Function`-kinded Thunderduck-boundary
/// [`EmissionError::Unsupported`] per ADR-022.
fn render_aggregate(f: &FunctionCall, schema: &Schema) -> Result<String, EmissionError> {
    // N5: `f.name` is already canonical lowercase — `lower` is kept as an
    // owned `String` (not renamed to a borrow) so the rest of this function,
    // which threads it through several `&lower` / `format!` sites, needs no
    // further edits.
    let lower = f.name.clone();
    // Guard-based arms MUST come before the pass-through arm (else the
    // pass-through catches `first`/`last` first and the guard never fires).
    if matches!(
        lower.as_str(),
        "first" | "last" | "first_value" | "last_value"
    ) && f.args.len() >= 2
    {
        // Spark's `first(col, ignorenulls)` / `last(col, ignorenulls)` —
        // DuckDB's first/last are single-arg. Drop the ignorenulls flag
        // UNCONDITIONALLY (corpus uses ignorenulls=True which matches
        // DuckDB's default); keep-arity is single-homed in
        // [`trailing_ignore_nulls_keep_arity`] — see its doc for the
        // deliberate guard divergence from `render_function_call`.
        if let Some(keep) = trailing_ignore_nulls_keep_arity(&lower) {
            let distinct = if f.distinct { "DISTINCT " } else { "" };
            let parts = sql_join(f.args.iter().take(keep), ", ", |arg| {
                render_expr(arg, schema)
            })?;
            return Ok(format!("{lower}({distinct}{parts})"));
        }
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
    // Spark `percentile(col, p)` = exact CONTINUOUS (linear-interpolation)
    // quantile → DuckDB `quantile_cont` (percentile_approx above uses
    // `quantile_disc` = discrete sample value). CAST the quantile to DOUBLE
    // since Spark sends it as Decimal. Corpus witness: `agg-019`
    // (percentile(salary, 0.5) = 91500 continuous, not 88000 discrete).
    if lower == "percentile" && f.args.len() >= 2 {
        let col = render_expr(&f.args[0], schema)?;
        let q = render_expr(&f.args[1], schema)?;
        return Ok(format!("quantile_cont({col}, CAST({q} AS DOUBLE))"));
    }
    // Spark `avg`/`mean` over a DECIMAL argument — DuckDB's native `avg`
    // returns DOUBLE over a DECIMAL input (precision loss vs Spark, which
    // widens to a wider DECIMAL per `AggRet::AvgLike`). Route through the
    // ext6 extension's `spark_avg`, which already returns DECIMAL natively
    // (re-honors rearchitect ADR-020; `try_avg` from the same extension
    // family is wired above at the `try_avg` arm). Guard MUST come before
    // the `"avg" | "mean"` pass-through arm below (else the pass-through
    // catches it first). Integer/float `avg` and `try_avg` are untouched —
    // `is_decimal_avg` only fires on a single DECIMAL argument.
    if is_decimal_avg(f, schema) {
        return render_decimal_avg(f, None, schema);
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
        // `collect_list` / `collect_set` are macro-backed (registered at
        // session startup: collect_list → LIST(x) FILTER (WHERE x IS NOT
        // NULL), collect_set → LIST(DISTINCT x) FILTER (...)), not
        // DuckDB-native aggregates. Pass the name through verbatim;
        // force_distinct stays false — collect_set's DISTINCT lives inside
        // the macro and Spark never sets distinct=true here.
        | "collect_list"
        | "collect_set"
        | "grouping"
        | "grouping_id" => (lower.as_str(), false),
        // Spark's population-formula `skewness` — DuckDB's `skewness` uses
        // the sample formula. The ext6 extension provides `spark_skewness`
        // with Spark-parity semantics (checklist §4.1).
        "skewness" => ("spark_skewness", false),
        // Spark's `max_by(x, y)` / `min_by(x, y)` — DuckDB's native
        // `arg_max(x, y)` / `arg_min(x, y)` are the same 2-arg shape
        // (value column, ordering column), so a name rename is the whole
        // fix — args pass through unchanged via `args_sql` below.
        "max_by" => ("arg_max", false),
        "min_by" => ("arg_min", false),
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
            let trailing_bool_only = f.args.iter().skip(1).all(|e| bool_literal(e).is_some());
            if let Some(arg) = first {
                if trailing_bool_only {
                    let distinct = if f.distinct { "DISTINCT " } else { "" };
                    // Peek through any wrapping Alias for the type check.
                    let inner = arg.unaliased();
                    let a = render_expr(inner, schema)?;
                    // Boolean sniff: either the analyzer-resolved type is
                    // Boolean, OR the argument is a boolean literal.
                    let is_bool = matches!(inner.data_type(schema), DataType::Boolean)
                        || bool_literal(inner).is_some();
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
                bail_boundary_fn!(f.name.clone(), "`count_if` requires exactly 1 argument");
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
            bail_boundary_fn!(
                f.name.clone(),
                "aggregate function not yet in the primitive arm set",
            );
        }
    };
    // Zero-arg aggregate calls are legal for a handful of Spark functions
    // (grouping_id() picks up the ambient GROUP BY). Handle by emitting
    // the empty arg list.
    let zero_arg_ok = matches!(duck_name, "grouping_id" | "grouping") && f.args.is_empty();
    if f.args.is_empty() && !zero_arg_ok {
        bail_boundary_fn!(f.name.clone(), "aggregate function call has no arguments");
    }
    // Spark's `count(DISTINCT a, b, ...)` counts distinct (a, b, ...) tuples,
    // excluding any row where ANY argument is NULL — verified empirically
    // against live Spark 4.1.1: a row with a NULL in either column is
    // dropped from the distinct count entirely, not counted as a distinct
    // NULL-bearing tuple. DuckDB's ROW/STRUCT constructor is non-NULL even
    // when every field is NULL, so a bare `count(DISTINCT (a, b))` counts
    // such rows as one extra distinct tuple (probed: 4 vs Spark's 2 on the
    // corpus's 4-row witness). Guard by collapsing the tuple to SQL NULL
    // whenever any argument is NULL; COUNT DISTINCT's ordinary NULL-skip
    // then drops those rows, matching Spark. Single-arg `count(DISTINCT x)`
    // is unaffected (DuckDB's own NULL-skip already matches Spark there).
    // Corpus: `test_count_distinct_multiple_columns`.
    if (f.distinct || force_distinct) && duck_name == "count" && f.args.len() > 1 {
        let cols = f
            .args
            .iter()
            .map(|arg| render_expr(arg, schema))
            .collect::<Result<Vec<String>, EmissionError>>()?;
        let null_check = cols
            .iter()
            .map(|c| format!("{c} IS NULL"))
            .collect::<Vec<_>>()
            .join(" OR ");
        let tuple = format!("({})", cols.join(", "));
        return Ok(format!(
            "count(DISTINCT CASE WHEN {null_check} THEN NULL ELSE {tuple} END)"
        ));
    }
    let args_sql = sql_join(f.args.iter(), ", ", |arg| render_expr(arg, schema))?;
    let distinct = if f.distinct || force_distinct {
        "DISTINCT "
    } else {
        ""
    };
    Ok(format!("{duck_name}({distinct}{args_sql})"))
}

/// Rewrite no-arg `grouping_id()` / `grouping()` anywhere in an aggregate
/// slot to `grouping_id(<grouping cols>)` — DuckDB has no zero-arg form
/// (it is a parse error). Also applied to the HAVING predicate, which can
/// legally carry these grouping functions over ROLLUP/CUBE/GROUPING SETS.
/// Uses a generic `children_mut` walk to reach nested occurrences (pass-3
/// kept this narrow pending a corpus witness; `grouping_id() + 1` via
/// `Binary` is that witness — see tasks/v2-simplification-pass-log.md flag
/// #6). Widening is safe: an unrewritten zero-arg call is a guaranteed
/// whole-query DuckDB parse error, so the walk only converts errors into
/// the Spark-intended emission — it cannot change working output.
/// Subquery bodies stay opaque per the `children`/`children_mut` walker
/// convention, which is the correct scoping here: an inner aggregate's
/// `grouping_id()` binds to the inner GROUP BY via its own
/// `render_aggregate_op` call, not the outer one.
fn rewrite_grouping_id(expr: &mut Expression, grouping: &[Expression]) {
    if let Expression::FunctionCall(f) = expr {
        if (f.name == "grouping_id" || f.name == "grouping")
            && f.args.is_empty()
            && !grouping.is_empty()
        {
            // Splice bare (alias-stripped) grouping exprs as explicit args.
            f.args = grouping.iter().map(|g| g.unaliased().clone()).collect();
            return; // do not walk the newly spliced args
        }
    }
    for child in expr.children_mut() {
        rewrite_grouping_id(child, grouping);
    }
}

/// Clone `expr` and splice in any no-arg `grouping_id()`/`grouping()` calls
/// via [`rewrite_grouping_id`]. Shared by the aggregate-slot and HAVING
/// rendering paths in `render_aggregate_op`.
fn with_grouping_id_spliced(expr: &Expression, grouping: &[Expression]) -> Expression {
    let mut e = expr.clone();
    rewrite_grouping_id(&mut e, grouping);
    e
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
            // a plain INTEGER day offset (root cause 026: `DATE + INTERVAL`
            // promotes to TIMESTAMP in DuckDB, but `DATE + INTEGER` stays
            // DATE, exactly matching Spark's DATE literal type).
            Ok(format!("(DATE '1970-01-01' + ({days}))"))
        }
        LiteralValue::Timestamp(micros) => Ok(format!(
            "CAST(make_timestamp(CAST({micros} AS BIGINT)) AS TIMESTAMP WITH TIME ZONE)"
        )),
        LiteralValue::TimestampNtz(micros) => {
            Ok(format!("make_timestamp(CAST({micros} AS BIGINT))"))
        }
        LiteralValue::Binary(bytes) => {
            // DuckDB does NOT accept `x'..'` as a blob literal — it parses that
            // as the VARCHAR "x..". The canonical DuckDB blob literal is a
            // single-quoted string of `\xHH` escapes cast to BLOB, e.g.
            // `CAST('\x1F\x2A' AS BLOB)`. Every byte becomes exactly `\x` + two
            // hex digits, so the string never contains a raw quote or backslash
            // that would need further escaping.
            let escaped: String = bytes.iter().map(|b| format!("\\x{b:02X}")).collect();
            Ok(format!("CAST('{escaped}' AS BLOB)"))
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

// ── ADR-006 ANSI divide/mod-by-zero guards ──────────────────────────────────
//
// Spark (ANSI, the corpus reference default) THROWS on divide/mod-by-zero;
// DuckDB returns NULL/inf. τ wraps the emitted operator so a zero divisor
// raises Spark's error class via DuckDB's `error()` scalar. The runtime layer
// (`session.rs`) re-wraps that throw into a Spark-classed wire error, and the
// differential harness keys on the leading `[TOKEN]`. The message text is
// copied verbatim from Spark 4.1 so τ's error is byte-identical, not merely
// class-identical.
//
// NOTE (ADR-006 follow-up): the architecturally cleaner home for these throws
// is a `thdck_spark_funcs` extension function (`spark_div`/`spark_pmod`) that
// raises with the class at the throw site, mirroring `spark_decimal_div` — it
// avoids CASE-wrapping every division. The emitted-SQL guard below is the
// in-repo interim; migrate when the extension gains those functions.
// Pass 10 (OPP-C): the `array_index_error_expr` and `ansi_zero_guard` free
// helpers were unified with [`super::spark_errors::SparkError`] +
// [`super::spark_errors::ansi_throw_if`]. Call sites migrated inline; see
// `render_element_at` (InvalidArrayIndex) and `render_binary` / pmod-mod
// arm in `render_scalar_function_call` (DivideByZero / RemainderByZero).
// Pass 11 (OPP-J) relocated the message-text consts into `spark_errors.rs`.

/// True when `e` is a numeric literal that is provably non-zero, so the ANSI
/// zero-guard can be skipped (the divisor can never be 0).
fn is_nonzero_literal(e: &Expression) -> bool {
    use super::expression::{Literal, LiteralValue as LV};
    let Expression::Literal(Literal { value, .. }) = e else {
        return false;
    };
    match value {
        LV::Byte(v) => *v != 0,
        LV::Short(v) => *v != 0,
        LV::Int(v) => *v != 0,
        LV::Long(v) => *v != 0,
        LV::Float(v) => *v != 0.0,
        LV::Double(v) => *v != 0.0,
        // Decimal is carried as a string; non-zero unless every digit is 0.
        LV::Decimal { value, .. } => value.bytes().any(|b| b.is_ascii_digit() && b != b'0'),
        _ => false,
    }
}

/// `true` when `expr` is a `Cast` node — implicit OR user-written — whose
/// target is exactly `ty`. Used by [`render_binary`]'s decimal-Div routing
/// to avoid double-wrapping an operand that is already a `Cast` to this
/// exact type: N4's materialized widened side
/// (`materialize_binary_coercions`), or an equivalent user cast. Bare
/// rendering of either is a genuine `DECIMAL(p, s)`, so skipping the wrap
/// is safe in both cases; a cast to a DIFFERENT decimal type still wraps.
fn is_cast_to(expr: &Expression, ty: &DataType) -> bool {
    matches!(expr, Expression::Cast(c) if &c.to_type == ty)
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
    //
    // A DECIMAL operand divided by a plain integral one (e.g. `decimal_col /
    // int_col`) is ALSO decimal division in Spark. The analyzer's N4
    // materialization pass (`materialize_binary_coercions`) inserts an
    // implicit CAST widening the lone integral side to `Decimal` directly
    // into the tree, so both operands' own `data_type` already report
    // `Decimal` in lockstep with the analyzer by the time emission sees this
    // node (tpcds-q066: `sum(x)/w_sq_ft`, DuckDB's native DECIMAL/BIGINT `/`
    // yields DOUBLE where Spark yields DECIMAL) — no re-derivation needed
    // here.
    // TODO(ADR-006): decimal divide-by-zero is not yet ANSI-guarded here.
    if b.op == BinaryOp::Div {
        let lt = b.left.data_type(schema);
        let rt = b.right.data_type(schema);
        if let (
            DataType::Decimal {
                precision: lprec,
                scale: lscale,
            },
            DataType::Decimal {
                precision: rprec,
                scale: rscale,
            },
        ) = (&lt, &rt)
        {
            // A DuckDB-native aggregate operand (e.g. `avg` over DECIMAL) is
            // emitted as DOUBLE at the DuckDB level even though it's typed
            // Decimal here — passing it raw makes the extension reject
            // non-DECIMAL args. Cast each operand to the DECIMAL(p,s) its own
            // declared type already is: a no-op re-cast for a genuine decimal
            // column, and — UNLESS the operand is already a materialized N4
            // `Cast` to this exact type (the widened side) — a coercion for a
            // native-double/integral operand. Skipping the re-wrap there
            // avoids a redundant double CAST around N4's own implicit one.
            let lty = render_data_type(&DataType::Decimal {
                precision: *lprec,
                scale: *lscale,
            });
            let rty = render_data_type(&DataType::Decimal {
                precision: *rprec,
                scale: *rscale,
            });
            let lsql = if is_cast_to(&b.left, &lt) {
                l.clone()
            } else {
                format!("CAST(({l}) AS {lty})")
            };
            let rsql = if is_cast_to(&b.right, &rt) {
                r.clone()
            } else {
                format!("CAST(({r}) AS {rty})")
            };
            return Ok(format!("spark_decimal_div({lsql}, {rsql})"));
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
    let inner = format!("({l}) {op} ({r})");
    // Date ± Interval: DuckDB promotes `DATE ± INTERVAL` to TIMESTAMP, and so
    // does Spark for a sub-day-field interval (R1-6) — the analyzer's
    // `date_like_interval_result` seam already resolves that case to
    // `Timestamp`, matching DuckDB's native output with no cast needed. Only
    // the day-only/year-month shapes Spark keeps as DATE need a corrective
    // cast; the analyzer's N4 materialization pass
    // (`materialize_binary_coercions`) already wraps the whole node in an
    // implicit `CAST(.. AS DATE)` (rendered by `render_cast`) when this
    // node's inferred type is Date, so no corrective cast belongs here.
    // ADR-006: guard divide/mod-by-zero with Spark's ANSI error class.
    let guard = match b.op {
        BinaryOp::Div | BinaryOp::IntDiv => Some(super::spark_errors::SparkError::DivideByZero),
        BinaryOp::Mod => Some(super::spark_errors::SparkError::RemainderByZero),
        _ => None,
    };
    match guard {
        Some(err) if !is_nonzero_literal(&b.right) => Ok(super::spark_errors::ansi_throw_if(
            &format!("({r}) = 0"),
            err,
            &inner,
        )),
        _ => Ok(inner),
    }
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
// access) remain future τ work territory.

fn render_array_literal(
    a: &crate::transpiler_v2::expression::ArrayLiteralExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    if a.elements.is_empty() {
        // Empty array — DuckDB requires a type annotation to disambiguate.
        let ty = render_data_type(&a.element_type);
        return Ok(format!("CAST([] AS {ty}[])"));
    }
    let elems = sql_join(a.elements.iter(), ", ", |e| render_expr(e, schema))?;
    Ok(format!("[{elems}]"))
}

fn render_map_literal(
    m: &crate::transpiler_v2::expression::MapLiteralExpression,
    schema: &Schema,
) -> Result<String, EmissionError> {
    if m.entries.is_empty() {
        return Ok("MAP()".to_owned());
    }
    let entries = sql_join(m.entries.iter(), ", ", |(k, v)| {
        let k_sql = render_expr(k, schema)?;
        let v_sql = render_expr(v, schema)?;
        Ok(format!("{k_sql}: {v_sql}"))
    })?;
    Ok(format!("MAP {{{entries}}}"))
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
        bail_boundary_expr!(
            "UpdateFields",
            "withField/dropFields requires the base expression to be a StructType",
        );
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
    let parts = sql_join(fields.iter(), ", ", |(name, src)| {
        let value = match src {
            FieldSource::FromBase => {
                let key = sql_string_literal(name);
                format!("struct_extract({base_sql}, {key})")
            }
            FieldSource::Value(expr) => render_expr(expr, schema)?,
        };
        Ok(format!("{} := {value}", quote_ident(name)))
    })?;
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
    // DuckDB struct literal keys are single-quoted string literals.
    let fields = sql_join(s.fields.iter(), ", ", |(name, expr)| {
        Ok(format!(
            "{}: {}",
            sql_string_literal(name),
            render_expr(expr, schema)?
        ))
    })?;
    Ok(format!("{{{fields}}}"))
}

// ── Return-type CAST helpers (§5.1 — SEPARATE `fn` items) ────────────────────

/// Projection-slot Spark-parity return-type CAST.
///
/// Wraps `expr_sql` in `CAST(... AS T)` iff the expression's Spark-typed
/// result type requires a cast that DuckDB won't apply automatically. At
/// τ's emission substrate this handles integer-integer division (Spark → Double)
/// plus the scalar-function Spark-parity table.
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
        if matches!(
            fc.name.as_str(),
            "array" | "list_value" | "make_array" | "list"
        ) && !fc.args.is_empty()
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
/// Handles integer SUM/AVG widening (BIGINT), decimal aggregate widening, etc.
/// In practice τ's decimal `avg`/`mean` delegates to the `thdck_spark_funcs`
/// extension's `spark_avg` in a dedicated [`render_decimal_avg`] (own outer
/// CAST, per rearchitect ADR-020); `sum` stays on DuckDB's native pass-through
/// (already value- and scale-exact over DECIMAL — only Arrow-wire precision
/// differs, which is harness-invisible). So this function is currently
/// unwired — but the §5.1 anchor test
/// (`spark_return_cast_and_spark_aggregate_return_cast_are_distinct`) requires
/// it to exist as a distinct `fn` item from `spark_return_cast`.
///
/// **§5.1 anchor.** MUST NOT share body with [`spark_return_cast`].
#[allow(dead_code)] // §5.1 anchor requires the item; decimal-avg is wired through render_decimal_avg instead.
fn spark_aggregate_return_cast(agg_sql: String, agg: &FunctionCall, schema: &Schema) -> String {
    if let Some(arg) = agg.args.first() {
        let arg_type = arg.data_type(schema);
        match agg.name.as_str() {
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
    "at",
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

/// Escape embedded single quotes (`'` → `''`) for interpolation into a
/// DuckDB single-quoted string literal. The canonical escape helper for τ —
/// `pub(super)` so sibling modules (`spark_errors`) reuse it instead of
/// hand-rolling the replace.
pub(super) fn escape_sql_string(s: &str) -> String {
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
    super::expression::as_string_literal(e).map(str::to_owned)
}

/// [`literal_string_arg`] variant that fails loudly: returns the string
/// literal's value, or a `Function`-kinded [`EmissionError::Unsupported`]
/// carrying `fn_name` and the caller's verbatim `reason`.
fn string_literal_arg_or(
    e: &Expression,
    fn_name: &str,
    reason: &str,
) -> Result<String, EmissionError> {
    literal_string_arg(e).ok_or_else(|| EmissionError::Unsupported {
        kind: UnsupportedKind::Function,
        name: fn_name.to_owned(),
        reason: reason.to_owned(),
    })
}

/// Recognise the one Spark option τ supports on `to_json`: a `MapLiteral`
/// with exactly one entry `('ignoreNullFields', 'true' | 'false')`. Returns
/// `Some(true|false)` for the recognised shape and `None` for anything else
/// (unrecognised key, non-string literal value, multi-entry map, or a
/// non-`MapLiteral` expression). Case-sensitive to match Spark's
/// `JSONOptions` parsing. Callers surface `None` as a Thunderduck-boundary
/// error (ADR-022). Pass 89 witness: `json-005`.
fn parse_to_json_ignore_null_fields(e: &Expression) -> Option<bool> {
    let m = match e {
        Expression::MapLiteral(m) => m,
        _ => return None,
    };
    if m.entries.len() != 1 {
        return None;
    }
    let (k, v) = &m.entries[0];
    let key = literal_string_arg(k)?;
    if key != "ignoreNullFields" {
        return None;
    }
    match literal_string_arg(v)?.as_str() {
        "true" => Some(true),
        "false" => Some(false),
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

/// Parse Spark's `F.window` duration-literal grammar for the 2-arg tumbling
/// form: `"N unit"` where `N` is a positive integer and `unit ∈
/// {second, minute, hour, day, week}` (singular or plural, case-insensitive).
///
/// Returns `Some((n, canonical_unit))` on success. Canonical unit is the
/// singular form emitted into the DuckDB `INTERVAL '{n} {unit}'` literal.
///
/// Returns `None` for any rejection:
/// - compound (`"1 day 3 hours"`)
/// - month / year (variable-length buckets — Spark accepts them, but
///   `time_bucket` semantics diverge on month boundaries; boundary-reject
///   per ADR-015 rather than emit divergent SQL)
/// - signed (`"-1 day"`) or fractional (`"0.5 day"`)
/// - empty / whitespace-only / trailing garbage
/// - unknown unit (`"1 fortnight"`)
///
/// Caller wraps `None` in a `Function`-kinded Thunderduck-boundary
/// `EmissionError::Unsupported` per ADR-022 (τ-boundary error).
///
/// Corpus witness: `win2-002`.
pub(crate) fn parse_window_duration_literal(s: &str) -> Option<(u64, &'static str)> {
    let mut it = s.trim().split_ascii_whitespace();
    let n_tok = it.next()?;
    let unit_tok = it.next()?;
    // Trailing garbage / compound intervals → reject.
    if it.next().is_some() {
        return None;
    }
    // Reject signs and fractional forms explicitly — `u64::from_str` already
    // rejects them, but this makes the intent explicit and future-proof.
    if n_tok.starts_with('-') || n_tok.starts_with('+') || n_tok.contains('.') {
        return None;
    }
    let n: u64 = n_tok.parse().ok()?;
    let unit = unit_tok.to_ascii_lowercase();
    let canonical: &'static str = match unit.as_str() {
        "second" | "seconds" => "second",
        "minute" | "minutes" => "minute",
        "hour" | "hours" => "hour",
        "day" | "days" => "day",
        "week" | "weeks" => "week",
        _ => return None,
    };
    Some((n, canonical))
}

/// Parse a Spark DDL schema string (field list, e.g. `"a INT, b
/// ARRAY<STRING>"`, or `struct<...>` wrapper) into a [`StructType`] using the
/// shared strict Spark-DDL parser ([`crate::types::spark_ddl`] — pass-2
/// simplification consolidated the two legacy grammars there). Returns
/// `None` when τ cannot translate the DDL — the caller then falls back
/// to the shared type-inference default. Pass 76 witnesses: `json-003`,
/// `json-004`.
///
/// Acceptance is strictly-additively wider than the legacy emission-local
/// parser (union grammar: decimal, intervals, null/void, extra primitive
/// aliases, NOT NULL qualifiers, `struct<...>` wrapper form); everything the
/// legacy parser accepted parses identically, and unknown types still yield
/// `None` → the same boundary error.
pub(crate) fn from_json_ddl_to_struct_for_type_inference(ddl: &str) -> Option<StructType> {
    crate::types::spark_ddl::parse_spark_schema(ddl)
}

/// Parse a Spark DDL schema string for `from_csv`. Spark's
/// `from_csv` accepts only flat primitive schemas (no nested STRUCT / ARRAY
/// / MAP) — this helper enforces that narrower surface so we fail loud on
/// shapes Spark itself would reject. Returns `None` when τ cannot translate
/// the DDL. Pass 87 witness: `json-007`.
pub(crate) fn from_csv_ddl_to_struct(ddl: &str) -> Option<StructType> {
    let st = crate::types::spark_ddl::parse_spark_schema(ddl)?;
    // from_csv is flat-only: reject nested/composite types (Spark
    // itself would reject these too — from_csv operates row-per-row on
    // a single delimited line).
    if st.fields.iter().any(|f| {
        matches!(
            f.data_type,
            DataType::Struct(_) | DataType::Array(_, _) | DataType::Map { .. }
        )
    }) {
        return None;
    }
    Some(st)
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
///
/// Parses ONCE via the typed DDL parser
/// ([`from_json_ddl_to_struct_for_type_inference`], the same grammar the
/// type-inference side uses) and renders the JSON schema from the resulting
/// [`StructType`] — the old parallel string-walking grammar is gone.
fn spark_ddl_schema_to_duckdb_json(ddl: &str) -> Option<String> {
    let st = from_json_ddl_to_struct_for_type_inference(ddl)?;
    struct_type_to_duckdb_json(&st)
}

/// Render a parsed [`StructType`] as DuckDB's JSON-schema object literal.
/// An empty field list renders as `{}` (matching the historical walker).
fn struct_type_to_duckdb_json(st: &StructType) -> Option<String> {
    let mut out = String::from("{");
    for (i, field) in st.fields.iter().enumerate() {
        if i > 0 {
            out.push(',');
        }
        out.push('"');
        out.push_str(&field.name);
        out.push_str("\":");
        out.push_str(&data_type_to_duckdb_json_value(&field.data_type)?);
    }
    out.push('}');
    Some(out)
}

/// Render one field's [`DataType`] as its DuckDB JSON-schema value.
fn data_type_to_duckdb_json_value(dt: &DataType) -> Option<String> {
    match dt {
        // STRUCT<...> → nested object.
        DataType::Struct(st) => struct_type_to_duckdb_json(st),
        // ARRAY<T> → "<duckdb_T>[]". The typed parser accepts
        // `ARRAY<STRUCT<...>>` / nested arrays, but DuckDB's JSON-schema
        // shape has no spelling for those — keep rejecting non-primitive
        // element types (matches the historical walker).
        DataType::Array(elem, _) => {
            let name = duckdb_primitive_name(elem)?;
            Some(format!("\"{name}[]\""))
        }
        // Primitive.
        other => Some(format!("\"{}\"", duckdb_primitive_name(other)?)),
    }
}

/// DuckDB's canonical type-name spelling for the primitive [`DataType`]s the
/// `from_json` DDL path supports. `INT` / `INTEGER` both accept in DuckDB;
/// use `INTEGER` for clarity.
///
/// NOTE: this path emits `TIMESTAMP` for BOTH timestamp flavors — NOT
/// [`render_data_type`]'s `TIMESTAMP WITH TIME ZONE`. Keep the mappings
/// separate.
fn duckdb_primitive_name(dt: &DataType) -> Option<&'static str> {
    match dt {
        DataType::Integer => Some("INTEGER"),
        DataType::Long => Some("BIGINT"),
        DataType::Short => Some("SMALLINT"),
        DataType::Byte => Some("TINYINT"),
        DataType::Float => Some("FLOAT"),
        DataType::Double => Some("DOUBLE"),
        DataType::Boolean => Some("BOOLEAN"),
        DataType::String => Some("VARCHAR"),
        DataType::Binary => Some("BLOB"),
        DataType::Date => Some("DATE"),
        DataType::Timestamp | DataType::TimestampNtz => Some("TIMESTAMP"),
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
            // Grouping separator (Spark's `G` / `,`): permitted only in the
            // integer part; contributes no digit slot to precision or scale.
            // Corpus witness: `parse-003` uses `'9,999.99'`.
            ',' if !seen_dot => {}
            _ => return None,
        }
    }
    let precision_u32 = pre + post;
    if precision_u32 == 0 || precision_u32 > 38 {
        return None;
    }
    Some((precision_u32 as u8, post as u8))
}

/// Render the `try_cast(<input> AS DECIMAL(p, s))` payload for the
/// `to_number` / `try_to_number` emission arms.
///
/// When `fmt` carries a grouping separator (`,`), the raw input string is
/// pre-processed with `replace(<input>, ',', '')` before the cast — DuckDB's
/// numeric cast does not strip grouping separators, so a legitimately
/// parseable Spark input like `'1,234.56'` under `'9,999.99'` would otherwise
/// silently fall to NULL (`try_to_number`) or ANSI-throw
/// `INVALID_FORMAT.MISMATCH_INPUT` (`to_number`). See ADR-015 (Spark parity
/// is the only emission target).
///
/// The stripping happens **only** in the value fed to `try_cast`; callers
/// still reference the RAW input in guard predicates and error messages so
/// the reported input matches what the user passed.
fn render_to_number_cast(input_sql: &str, precision: u8, scale: u8, fmt: &str) -> String {
    let cast_input = if fmt.contains(',') {
        format!("replace({input_sql}, ',', '')")
    } else {
        input_sql.to_owned()
    };
    format!("try_cast({cast_input} AS DECIMAL({precision}, {scale}))")
}

/// The per-arm reason strings for [`to_number_parts`]. Each arm passes its
/// three EXACT strings verbatim — the `to_number` / `try_to_number` texts
/// differ (name prefix AND supported-template list); never derive one arm's
/// string from the other's.
struct ToNumberMsgs {
    /// Reason when the call does not have exactly 2 arguments.
    arity: &'static str,
    /// Reason when the format argument is not a string literal.
    fmt_literal: &'static str,
    /// Reason when [`parse_number_format`] rejects the format string;
    /// rendered with the offending format string.
    fmt_unsupported: fn(&str) -> String,
}

/// Shared body of the `to_number` / `try_to_number` emission arms: arity
/// check, literal format extraction, format → `DECIMAL(p, s)` parse, and the
/// `try_cast` emission. Returns `(raw_input_sql, cast_sql, fmt)` — the raw
/// input and format feed `to_number`'s ANSI throw guard; `try_to_number`
/// returns the cast alone.
fn to_number_parts(
    f: &FunctionCall,
    schema: &Schema,
    msgs: &ToNumberMsgs,
) -> Result<(String, String, String), EmissionError> {
    if f.args.len() != 2 {
        bail_boundary_fn!(f.name.clone(), msgs.arity);
    }
    let fmt = string_literal_arg_or(&f.args[1], &f.name, msgs.fmt_literal)?;
    let (precision, scale) =
        parse_number_format(&fmt).ok_or_else(|| EmissionError::Unsupported {
            kind: UnsupportedKind::Function,
            name: f.name.clone(),
            reason: (msgs.fmt_unsupported)(&fmt),
        })?;
    let s = render_expr(&f.args[0], schema)?;
    let cast = render_to_number_cast(&s, precision, scale, &fmt);
    Ok((s, cast, fmt))
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
pub(crate) fn render_data_type(dt: &DataType) -> String {
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
            // DuckDB's `STRUCT(name TYPE, …)` CAST syntax rejects duplicate
            // field names (`Binder Error: Duplicate STRUCT type argument
            // name`). Spark's `StructType` permits duplicates (e.g.
            // `arrays_zip("tags","tags")` → `Struct<tags, tags>`), so dedup
            // to substrate-safe names before emission. Convention matches
            // PySpark's `_dedup_names` (`tags`, `tags` → `tags_0`, `tags_1`)
            // so the same dedup convention applies uniformly across:
            //   - this CAST target,
            //   - the outbound Arrow-schema stamp in `connect-server`,
            //   - PySpark's client-side `ArrowTableToRowsConversion.convert`.
            // The τ analyzer's `resolved_schema` still carries the original
            // duplicate names (Spark-visible); dedup happens ONLY on the
            // DuckDB-substrate SQL side.
            let names: Vec<&str> = st.fields.iter().map(|f| f.name.as_str()).collect();
            let deduped = dedup_struct_field_names(&names);
            let inner: Vec<String> = st
                .fields
                .iter()
                .zip(deduped.iter())
                .map(|(f, name)| {
                    let name_q = quote_ident(name);
                    format!("{name_q} {}", render_data_type(&f.data_type))
                })
                .collect();
            format!("STRUCT({})", inner.join(", "))
        }
    }
}

/// PySpark parity dedup for struct field names — thin call site for the
/// shared [`crate::types::pyspark_parity::dedup_names`] helper.
///
/// Used by [`render_data_type`] so the DuckDB substrate for
/// `CAST(x AS STRUCT(...))` never carries duplicate field names, which
/// DuckDB's binder refuses. The outbound Arrow-schema stamp in the
/// `connect-server` crate consumes the same helper, so DuckDB's
/// substrate names and the stamp's target names line up bit-for-bit.
fn dedup_struct_field_names(names: &[&str]) -> Vec<String> {
    crate::types::pyspark_parity::dedup_names(names)
}

// ── Extension allow-list (§4.1 stub — populated by τ's extension-target wiring) ──────────────────

/// The set of DuckDB extension function names τ emits. Currently empty; τ's
/// extension-target wiring will populate this with the ext6 allow-list and
/// activate INV6 (`transpiler_v2/invariants.rs::inv6_extension_targets_exist`,
/// currently DEFER-marked).
#[allow(dead_code)] // INV6 activator (currently DEFER); populated when extension-target wiring lands.
pub(crate) fn extension_targets() -> HashSet<&'static str> {
    HashSet::new()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::ast::{
        CommonAst, CommonOp, JoinType, PivotGrouping, SetOpKind, UnpivotIds,
    };
    use crate::transpiler_v2::base_types::BaseTypes;
    use crate::transpiler_v2::expression::{
        materialize_binary_coercions, AliasExpression, BetweenExpression, BinaryExpression,
        BinaryOp, CaseWhenExpression, CastExpression, ColumnReference, ExtractValueExpression,
        FunctionCall, InListExpression, IntervalExpression, IntervalKind, LambdaExpression,
        LambdaVariableExpression, LikeExpression, Literal, LiteralValue, MapLiteralExpression,
        StarExpression, UnaryExpression, UnaryOp, UpdateFieldsExpression,
    };
    use crate::transpiler_v2::schema::Attribute;
    use crate::transpiler_v2::{analyze, generate, AnalyzerError};
    use crate::types::StructField;

    fn tap_guard() -> std::sync::MutexGuard<'static, ()> {
        EMIT_TAP_MUTEX.lock().expect("EMIT_TAP_MUTEX poisoned")
    }

    fn empty_schema() -> Schema {
        Schema::empty()
    }

    fn emp_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ])
    }

    // ── timestampadd / timestampdiff emission (intv-006) ───────────────────
    // Pure helpers — no schema, no EMIT_TAP; safe from the tap-mutex cascade.

    #[test]
    fn timestampadd_interval_sql_maps_units() {
        assert_eq!(
            spark_add_interval_sql("timestampadd", "MONTH", "3").unwrap(),
            "INTERVAL (3) MONTH"
        );
        // Case-insensitive unit.
        assert_eq!(
            spark_add_interval_sql("timestampadd", "month", "3").unwrap(),
            "INTERVAL (3) MONTH"
        );
        // DuckDB has no QUARTER interval keyword → 3 months.
        assert_eq!(
            spark_add_interval_sql("timestampadd", "QUARTER", "2").unwrap(),
            "INTERVAL ((2) * 3) MONTH"
        );
        assert_eq!(
            spark_add_interval_sql("timestampadd", "SECOND", "n").unwrap(),
            "INTERVAL (n) SECOND"
        );
        // Unknown unit → honest Thunderduck-boundary error.
        assert!(matches!(
            spark_add_interval_sql("timestampadd", "FORTNIGHT", "1"),
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                ..
            })
        ));
    }

    #[test]
    fn timestampdiff_sql_fixed_units_and_calendar_boundary() {
        assert_eq!(
            spark_diff_sql("timestampdiff", "DAY", "a", "b").unwrap(),
            "CAST(trunc(CAST((epoch_us(b) - epoch_us(a)) AS DOUBLE) / 86400000000.0) AS BIGINT)"
        );
        // Microsecond delta is exact — no division.
        assert_eq!(
            spark_diff_sql("timestampdiff", "MICROSECOND", "a", "b").unwrap(),
            "CAST((epoch_us(b) - epoch_us(a)) AS BIGINT)"
        );
        // Calendar units need day-of-month-aware arithmetic τ does not emit yet
        // → honest Thunderduck-boundary error (ADR-022), never wrong SQL.
        assert!(matches!(
            spark_diff_sql("timestampdiff", "MONTH", "a", "b"),
            Err(EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                ..
            })
        ));
    }

    fn base_types_with_emp() -> BaseTypes {
        let plan = scan("emp");
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

    fn double_lit(v: f64) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Double(v),
            data_type: DataType::Double,
        })
    }

    fn decimal_lit(value: &str, precision: u8, scale: u8) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::Decimal {
                value: value.to_owned(),
                precision,
                scale,
            },
            data_type: DataType::Decimal { precision, scale },
        })
    }

    fn str_lit(s: &str) -> Expression {
        Expression::Literal(Literal {
            value: LiteralValue::String(s.to_owned()),
            data_type: DataType::String,
        })
    }

    fn col_with_type(name: &str, dt: DataType) -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: name.to_owned(),
            qualifier: None,
            data_type: Some(dt),
            nullable: Some(true),
            expr_id: None,
        })
    }

    fn col_ref_expr(name: &str) -> Expression {
        col_with_type(name, DataType::String)
    }

    fn ts_col_ref(name: &str) -> Expression {
        col_with_type(name, DataType::Timestamp)
    }

    fn scan(table: &str) -> CommonAst {
        CommonAst::new(CommonOp::TableScan {
            table: table.to_owned(),
            alias: None,
        })
    }

    fn fcall(name: &str, args: Vec<Expression>) -> FunctionCall {
        FunctionCall {
            name: name.to_owned(),
            args,
            distinct: false,
        }
    }

    fn fexpr(name: &str, args: Vec<Expression>) -> Expression {
        Expression::FunctionCall(fcall(name, args))
    }

    fn render_fn(name: &str, args: Vec<Expression>) -> String {
        render_function_call(&fcall(name, args), &empty_schema()).expect("render")
    }

    fn render_fn_on(schema: &Schema, name: &str, args: Vec<Expression>) -> String {
        render_function_call(&fcall(name, args), schema).expect("render")
    }

    /// Asserts `err` is [`EmissionError::Unsupported`] with the given `kind`
    /// and `name`, and that `reason` contains every fragment in
    /// `reason_frags`.
    #[track_caller]
    fn expect_unsupported(
        err: EmissionError,
        kind: UnsupportedKind,
        name: &str,
        reason_frags: &[&str],
    ) {
        match err {
            EmissionError::Unsupported {
                kind: got_kind,
                name: got_name,
                reason,
            } => {
                assert_eq!(
                    got_kind, kind,
                    "unexpected UnsupportedKind; reason: {reason}"
                );
                assert_eq!(got_name, name, "unexpected boundary name; reason: {reason}");
                for frag in reason_frags {
                    assert!(
                        reason.contains(frag),
                        "reason must contain {frag:?}; got: {reason}"
                    );
                }
            }
            other @ EmissionError::SparkEmulated { .. } => {
                panic!("expected EmissionError::Unsupported, got: {other:?}")
            }
        }
    }

    // ── ceil/floor emission (num-001/002/003) ────────────────────────────

    #[test]
    fn ceil_1arg_long_is_bigint_nan_guard() {
        let _g = tap_guard();
        // Integer input → Long → the NaN-guarded BIGINT shape (math-003 pin).
        let sql = render_fn("ceil", vec![int_lit(5)]);
        assert_eq!(
            sql,
            "CASE WHEN (5) IS NULL THEN NULL \
             WHEN isnan(CAST((5) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(ceil(5) AS BIGINT) END"
        );
    }

    #[test]
    fn ceil_1arg_decimal_casts_to_scale0_decimal() {
        let _g = tap_guard();
        let sql = render_fn("ceil", vec![decimal_lit("1.25", 10, 2)]);
        assert!(sql.ends_with("AS DECIMAL(9, 0))"), "got: {sql}");
        assert!(sql.starts_with("CAST(ceil("), "got: {sql}");
    }

    #[test]
    fn floor_2arg_scaled_decimal() {
        let _g = tap_guard();
        // 2-arg over double → decimal(18,2), synthesized as fn((a)*100)/100.
        let sql = render_fn("ceil", vec![double_lit(1.5), int_lit(2)]);
        assert!(sql.starts_with("CAST(ceil("), "got: {sql}");
        assert!(sql.contains(") * 100) / 100"), "got: {sql}");
        assert!(sql.ends_with("AS DECIMAL(18, 2))"), "got: {sql}");
    }

    #[test]
    fn ceil_2arg_negative_scale_is_boundary() {
        let _g = tap_guard();
        let f = fcall("ceil", vec![decimal_lit("1.25", 10, 2), int_lit(-1)]);
        let err = render_function_call(&f, &empty_schema()).expect_err("negative scale");
        assert!(matches!(
            err,
            EmissionError::Unsupported {
                kind: UnsupportedKind::Function,
                ..
            }
        ));
    }

    // ── Pass 106 — uncorrelated subquery emission ────────────────────────

    fn analyzed_select_id_from_emp() -> SubqueryPlan {
        let inner = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![ColumnReference::untyped("id")],
        });
        let typed = analyze(inner, &base_types_with_emp()).expect("analyze inner");
        SubqueryPlan::Analyzed(Box::new(typed))
    }

    #[test]
    fn scalar_subquery_renders_parenthesized_select() {
        let _g = tap_guard();
        use super::super::expression::ScalarSubquery;
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: analyzed_select_id_from_emp(),
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render scalar");
        assert!(sql.starts_with("(SELECT"), "got: {sql}");
        assert!(sql.ends_with(')'), "got: {sql}");
        assert!(sql.contains("FROM emp"), "got: {sql}");
    }

    #[test]
    fn in_subquery_renders_lhs_in_select() {
        let _g = tap_guard();
        use super::super::expression::InSubquery;
        let expr = Expression::InSubquery(InSubquery {
            expr: Box::new(int_lit(1)),
            subquery: analyzed_select_id_from_emp(),
            negated: false,
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render IN");
        assert!(sql.starts_with("1 IN (SELECT"), "got: {sql}");
        assert!(sql.ends_with(')'), "got: {sql}");
    }

    /// Regression pin (sq-003/sq-004 corpus cluster): a correlated scalar
    /// subquery's inner plan — Aggregate over Filter over
    /// `AliasedRelation(e2)` with the filter referencing the OUTER alias `e`
    /// — must merge into ONE inner block. The correlated qualifier `e` is
    /// not bound by the inner scope, so merge visibility must EXEMPT it
    /// (DuckDB's correlated binder resolves it outward); treating it as a
    /// visibility failure wraps the inner FROM under `__td_sub`, burying
    /// `e2` and breaking `e2.dept_id`.
    #[test]
    fn correlated_scalar_subquery_inner_filter_merges_into_one_block() {
        let _g = tap_guard();
        use super::super::expression::ScalarSubquery;
        // Inner: SELECT avg(e2.salary) FROM emp e2 WHERE e2.dept_id = e.dept_id
        let inner = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(CommonAst::new(CommonOp::Filter {
                input: Box::new(aliased_scan("emp", "e2")),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(qcol("e2", "dept_id")),
                    right: Box::new(qcol("e", "dept_id")),
                }),
            })),
            grouping: vec![],
            aggregates: vec![Expression::FunctionCall(FunctionCall {
                name: "avg".to_owned(),
                args: vec![qcol("e2", "salary")],
                distinct: false,
            })],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        // Outer: SELECT name, (<inner>) AS dept_avg FROM emp e
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased_scan("emp", "e")),
            projections: vec![Expression::Alias(AliasExpression {
                expr: Box::new(Expression::ScalarSubquery(ScalarSubquery {
                    subquery: SubqueryPlan::Unanalyzed(Box::new(inner)),
                })),
                alias: "dept_avg".to_owned(),
            })],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze correlated scalar");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("FROM emp AS e2 WHERE"),
            "inner filter must merge into the aliased block (correlated \
             qualifier exempt from visibility), got: {sql}"
        );
        assert!(
            !sql.contains("__td_sub"),
            "no wrap may bury the inner alias, got: {sql}"
        );
    }

    // ── ADR-023 tier 2: wrap-boundary re-projection over duplicate names ──
    //
    // `output_uniquified` gates every wrap site's pass-through vs.
    // `reproject_qualifiers` choice on whether the wrapped child's output
    // has a duplicate name. The common (unique) case renders the expression
    // unchanged — the unique-name reference already resolved bare at
    // analysis time (tier 3e-ii/iii); the duplicate case (see
    // `ambiguous_output_name_wrap_reprojects_to_unique_position` above) must
    // reproject uniquely and rewrite the outer reference by position.

    /// The common case: the wrapped child's output names are already
    /// unique, so `output_uniquified` returns `None` and every wrap site
    /// wraps with `SelectBlock::wrap` and renders the expression unchanged —
    /// zero delta on the shape tier 2 does not target.
    #[test]
    fn wrap_over_unique_names_is_unchanged() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Sort {
                input: Box::new(aliased_scan("emp", "e")),
                order: vec![asc_key(ColumnReference::untyped("id"))],
                limit: Some(5),
                offset: None,
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(qcol("e", "salary")),
                right: Box::new(int_lit(60000)),
            }),
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT * FROM (SELECT * FROM emp AS e ORDER BY id ASC NULLS FIRST LIMIT 5) \
             AS __td_sub WHERE (salary) > (60000)"
        );
    }

    /// ADR-023 tier 2's namesake pin: a duplicate name across a self-join
    /// output forces `build_filter`'s wrap path onto `wrap_reprojected` +
    /// `reproject_qualifiers` instead of a plain pass-through — the
    /// scope-resolvable `a.name` reference is rewritten to the unique
    /// positional name (`name`, the left side's occurrence) rather than
    /// stranding qualified over the buried `a` alias.
    #[test]
    fn wrap_over_duplicate_names_reprojects_uniquely() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(CommonAst::new(CommonOp::Join {
                    left: Box::new(aliased_scan("emp", "a")),
                    right: Box::new(aliased_scan("emp", "b")),
                    join_type: JoinType::Inner,
                    condition: Some(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Eq,
                        left: Box::new(qcol("a", "id")),
                        right: Box::new(qcol("b", "id")),
                    })),
                    using_columns: vec![],
                    natural: false,
                    lateral: false,
                    left_plan_ids: vec![],
                    right_plan_ids: vec![],
                })),
                limit: 5,
                offset: None,
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("b", "name")),
                right: Box::new(str_lit("y")),
            }),
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT * FROM (SELECT * FROM emp AS a INNER JOIN emp AS b ON (a.id) = (b.id) LIMIT 5) \
             AS __td_sub(id, name, dept_id, salary, id_1, name_1, dept_id_1, salary_1) \
             WHERE (name_1) = ('y')"
        );
    }

    // ── Wrap-boundary qualifier rewriting (strand-class retirement) ──────
    //
    // filt-016/filt-017 witness class: a qualified reference above a
    // slot-conflict wrap resolves to its bare output name at RESOLUTION
    // time (ADR-023 tier 3e-ii/iii) instead of stranding the alias behind
    // `__td_sub` (DuckDB: `Referenced table "e" not found`), so emission has
    // nothing left to rewrite for this class. The keep-side of the matrix
    // (ambiguous names, unexposed/correlated qualifiers, struct precedence)
    // must stay verbatim.

    fn asc_key(expr: Expression) -> SortOrder {
        SortOrder {
            expr: Box::new(expr),
            direction: SortDirection::Ascending,
            null_ordering: NullOrdering::NullsFirst,
        }
    }

    /// filt-016 shape: `emp.alias("e").orderBy("id").limit(5)
    /// .filter(col("e.salary") > 60000)` — WHERE cannot merge past an
    /// occupied LIMIT slot; `e.salary` is dropped to a bare name at resolution
    /// (unique name), so it renders bare over the wrap.
    #[test]
    fn filter_above_limit_drops_alias_qualifier_at_resolution() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Sort {
                input: Box::new(aliased_scan("emp", "e")),
                order: vec![asc_key(ColumnReference::untyped("id"))],
                limit: Some(5),
                offset: None,
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(qcol("e", "salary")),
                right: Box::new(int_lit(60000)),
            }),
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_sub WHERE (salary) > (60000)"),
            "qualifier must be dropped at resolution to the bare output name, got: {sql}"
        );
        assert!(!sql.contains("e.salary"), "got: {sql}");
    }

    /// filt-017 shape: `emp.alias("e").select(...).distinct()
    /// .filter(col("e.dept_id") == 101)` — the analyzer resolves `e.dept_id`
    /// at analysis time (tier-(e)/(f)) to a bare name (the Project's scope is
    /// empty, so it resolves projected-through), so emission renders it bare —
    /// there is no emission-side strip.
    #[test]
    fn filter_above_distinct_drops_alias_qualifier_at_resolution() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Deduplicate {
                input: Box::new(CommonAst::new(CommonOp::Project {
                    input: Box::new(aliased_scan("emp", "e")),
                    projections: vec![qcol("e", "dept_id"), qcol("e", "name")],
                })),
                on_columns: vec![],
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(101)),
            }),
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_sub WHERE (dept_id) = (101)"),
            "stranded qualifier must be stripped to the bare output name, got: {sql}"
        );
    }

    #[test]
    fn sort_above_limit_drops_alias_qualifier_at_resolution() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Sort {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(aliased_scan("emp", "e")),
                limit: 5,
                offset: None,
            })),
            order: vec![asc_key(qcol("e", "name"))],
            limit: None,
            offset: None,
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_sub ORDER BY name ASC NULLS FIRST"),
            "stranded sort key must be stripped to the bare output name, got: {sql}"
        );
        assert!(!sql.contains("e.name"), "got: {sql}");
    }

    #[test]
    fn project_above_limit_strips_stranded_alias_qualifier() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(aliased_scan("emp", "e")),
                limit: 5,
                offset: None,
            })),
            projections: vec![qcol("e", "salary")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("__td_sub"), "got: {sql}");
        assert!(
            !sql.contains("e.salary"),
            "stranded projection must be stripped to the bare output name, got: {sql}"
        );
    }

    /// proj-016 (F12): `emp.alias("e").orderBy("id").limit(2).select("e.*")`
    /// — the select cannot merge below the occupied LIMIT slot, so
    /// `build_project` wraps under `__td_sub`. Confirmed via a temporary
    /// probe that `analyze` keeps the projection as `Star{qualifier:
    /// Some("e")}` all the way into `build_project` (no analyzer-side
    /// pre-expansion): before the fix, this dispatched to
    /// `SELECT e.* FROM (...) AS __td_sub ...` — an unbindable qualifier
    /// (DuckDB: `Referenced table "e" not found`). After the fix, `e.*`
    /// (which covers the WHOLE input relation) expands to the bare `*`.
    #[test]
    fn qualified_star_over_limit_wrap_expands_to_bare_star() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::Sort {
                input: Box::new(aliased_scan("emp", "e")),
                order: vec![asc_key(qcol("e", "id"))],
                limit: Some(2),
                offset: None,
            })),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("e".to_owned()),
            })],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // ADR-023 3e-ii: `id` is unique in `emp`, so the qualifier is dropped
        // at resolution (a bare bind is positionally equivalent).
        assert_eq!(
            sql,
            "SELECT * FROM (SELECT * FROM emp AS e ORDER BY id ASC NULLS FIRST LIMIT 2) AS __td_sub",
            "got: {sql}"
        );
        assert!(
            !sql.contains("e.*"),
            "no stranded qualified star, got: {sql}"
        );
    }

    /// Regression (merge path): `emp.alias("e").select("e.*")` with no
    /// occupied slots ahead of it merges straight into the aliased scan's
    /// still-open block, which still exposes `e` — the fix's wrap-only gate
    /// (`block.exposes(q)` checked against the PRE-wrap block, only reached
    /// on the wrap path) must not perturb this merge-path rendering.
    #[test]
    fn qualified_star_that_merges_keeps_alias() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(aliased_scan("emp", "e")),
            projections: vec![Expression::Star(StarExpression {
                qualifier: Some("e".to_owned()),
            })],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(sql, "SELECT e.* FROM emp AS e", "got: {sql}");
        assert!(
            !sql.contains("__td_sub"),
            "merge path must not wrap, got: {sql}"
        );
    }

    #[test]
    fn with_columns_above_limit_strips_stranded_alias_qualifier() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::WithColumns {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(aliased_scan("emp", "e")),
                limit: 5,
                offset: None,
            })),
            assignments: vec![("bonus".to_owned(), qcol("e", "salary"))],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("__td_sub"), "got: {sql}");
        assert!(
            !sql.contains("e.salary"),
            "assignment qualifier must be dropped at resolution to the bare output name, got: {sql}"
        );
    }

    /// ADR-023 tier 2: a name appearing on BOTH sides of a self-join output
    /// is ambiguous once bare-stripped to its ORIGINAL name — the pre-tier-2
    /// fallback therefore left it qualified (a loud binder failure over the
    /// buried `a` alias). Tier 2 instead reprojects the wrapped join under
    /// per-column unique names and rewrites `a.name` to the unique name at
    /// its position (`name`, the left side's first occurrence) — resolving
    /// correctly instead of failing loudly.
    #[test]
    fn ambiguous_output_name_wrap_reprojects_to_unique_position() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(CommonAst::new(CommonOp::Join {
                    left: Box::new(aliased_scan("emp", "a")),
                    right: Box::new(aliased_scan("emp", "b")),
                    join_type: JoinType::Inner,
                    condition: Some(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Eq,
                        left: Box::new(qcol("a", "id")),
                        right: Box::new(qcol("b", "id")),
                    })),
                    using_columns: vec![],
                    natural: false,
                    lateral: false,
                    left_plan_ids: vec![],
                    right_plan_ids: vec![],
                })),
                limit: 5,
                offset: None,
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("a", "name")),
                right: Box::new(str_lit("x")),
            }),
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT * FROM (SELECT * FROM emp AS a INNER JOIN emp AS b ON (a.id) = (b.id) LIMIT 5) \
             AS __td_sub(id, name, dept_id, salary, id_1, name_1, dept_id_1, salary_1) \
             WHERE (name) = ('x')"
        );
    }

    /// Keep-side: a qualifier the pre-wrap block does NOT expose is a
    /// correlated OUTER reference — DuckDB's correlated binder resolves it
    /// outward through the wrap, so it must stay qualified verbatim.
    ///
    /// ADR-023 3d: `analyze()` itself now correctly REJECTS `outer_e.salary`
    /// at the top level (there is no real outer scope here to resolve it
    /// against — the old pass permissively resolved it by bare name alone,
    /// which is exactly the F8-class bug 3d closes). This test is
    /// EMISSION-only in that it hand-stamps a resolved TypedAst (a correlated
    /// qualifier the resolver kept) and checks emission renders it verbatim.
    /// The wrap/strip logic no longer exists.
    #[test]
    fn unexposed_qualifier_survives_wrap_verbatim() {
        let _g = tap_guard();
        let scan = typed_table_scan("emp", Some("e"), emp_schema());
        let limit = TypedAst::new(
            TypedOp::Limit {
                input: Box::new(scan),
                limit: 5,
                offset: None,
            },
            Schema::minted(emp_schema()),
        );
        let filter = TypedAst::new(
            TypedOp::Filter {
                input: Box::new(limit),
                condition: Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(Expression::ColumnReference(ColumnReference {
                        name: "salary".to_owned(),
                        qualifier: Some("outer_e".to_owned()),
                        data_type: Some(DataType::Double),
                        nullable: Some(true),
                        expr_id: None,
                    })),
                    right: Box::new(int_lit(1)),
                }),
            },
            Schema::minted(emp_schema()),
        );
        let sql = dispatch_op(&filter.op, &filter.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("outer_e.salary"),
            "unexposed (correlated) qualifier must stay verbatim, got: {sql}"
        );
    }

    /// Keep-side: a qualifier that resolves as STRUCT-column access
    /// (`resolve_column`'s struct-precedence tier) survives because resolution
    /// never drops a struct qualifier (the struct-precedence tier runs at
    /// analysis time); the old strip's misread hazard no longer applies.
    #[test]
    fn struct_qualifier_survives_wrap_verbatim() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Filter {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(scan("addr")),
                limit: 5,
                offset: None,
            })),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("addr", "city")),
                right: Box::new(str_lit("x")),
            }),
        });
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "addr" => Some(StructType::new(vec![StructField::nullable(
                "addr",
                DataType::Struct(StructType::new(vec![StructField::nullable(
                    "city",
                    DataType::String,
                )])),
            )])),
            _ => None,
        });
        let typed = analyze(plan, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("addr.city"),
            "struct-column access must NOT be stripped, got: {sql}"
        );
    }

    #[test]
    fn not_in_subquery_renders_lhs_not_in_select() {
        let _g = tap_guard();
        use super::super::expression::InSubquery;
        let expr = Expression::InSubquery(InSubquery {
            expr: Box::new(int_lit(1)),
            subquery: analyzed_select_id_from_emp(),
            negated: true,
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render NOT IN");
        assert!(sql.starts_with("1 NOT IN (SELECT"), "got: {sql}");
    }

    #[test]
    fn exists_subquery_renders_exists_select() {
        let _g = tap_guard();
        use super::super::expression::ExistsSubquery;
        let expr = Expression::ExistsSubquery(ExistsSubquery {
            subquery: analyzed_select_id_from_emp(),
            negated: false,
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render EXISTS");
        assert!(sql.starts_with("EXISTS (SELECT"), "got: {sql}");
    }

    #[test]
    fn not_exists_subquery_renders_not_exists_select() {
        let _g = tap_guard();
        use super::super::expression::ExistsSubquery;
        let expr = Expression::ExistsSubquery(ExistsSubquery {
            subquery: analyzed_select_id_from_emp(),
            negated: true,
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render NOT EXISTS");
        assert!(sql.starts_with("NOT EXISTS (SELECT"), "got: {sql}");
    }

    #[test]
    fn unanalyzed_subquery_is_defensive_boundary_error() {
        let _g = tap_guard();
        use super::super::expression::ScalarSubquery;
        let expr = Expression::ScalarSubquery(ScalarSubquery {
            subquery: SubqueryPlan::Unanalyzed(Box::new(CommonAst::new(CommonOp::SingleRow))),
        });
        let err = render_expr(&expr, &empty_schema()).expect_err("unanalyzed must error");
        assert!(matches!(
            err,
            EmissionError::Unsupported {
                kind: UnsupportedKind::Expression,
                ..
            }
        ));
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
        let ast = scan("emp");
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

    // ── TableFunction (range) — pass-141 ─────────────────────────────────

    /// Build `range(<args>)`, analyze, and emit — exercises the whole
    /// L2-analyzer + L3-emission path for the TVF node.
    fn emit_range(args: Vec<Expression>) -> String {
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "range".to_owned(),
            args,
            with_ordinality: false,
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze range");
        dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch range")
    }

    #[test]
    fn dispatch_range_one_arg_synthesizes_start_zero_step_one() {
        let _g = tap_guard();
        // `range(5)` → start=0, step=1 (both synthesized as typed BIGINT).
        assert_eq!(
            emit_range(vec![int_lit(5)]),
            "SELECT id FROM range(CAST(0 AS BIGINT), 5, CAST(1 AS BIGINT)) AS __td_range(id)"
        );
    }

    #[test]
    fn dispatch_range_two_args_synthesizes_step_one() {
        let _g = tap_guard();
        assert_eq!(
            emit_range(vec![int_lit(2), int_lit(5)]),
            "SELECT id FROM range(2, 5, CAST(1 AS BIGINT)) AS __td_range(id)"
        );
    }

    #[test]
    fn dispatch_range_three_args_uses_explicit_step() {
        let _g = tap_guard();
        assert_eq!(
            emit_range(vec![int_lit(2), int_lit(10), int_lit(2)]),
            "SELECT id FROM range(2, 10, 2) AS __td_range(id)"
        );
    }

    #[test]
    fn dispatch_range_four_args_drops_num_partitions() {
        let _g = tap_guard();
        // The 4th `numPartitions` arg is a single-node no-op — dropped.
        assert_eq!(
            emit_range(vec![int_lit(2), int_lit(10), int_lit(2), int_lit(4)]),
            "SELECT id FROM range(2, 10, 2) AS __td_range(id)"
        );
    }

    #[test]
    fn dispatch_project_over_range_binds_id_column() {
        let _g = tap_guard();
        // Full `SELECT id FROM range(5)` — the range TVF is a FROM-item leaf
        // block whose DEFAULT projection performs the `id` bind; a merging
        // Project overwrites it and MUST still see the renamed column
        // (tbl-006; tasks/select-block-follow-ups.md item 1 pin: merge, not
        // wrap — the `AS __td_range(id)` rename is part of the FROM item).
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::TableFunction {
                name: "range".to_owned(),
                args: vec![int_lit(5)],
                with_ordinality: false,
            })),
            projections: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )],
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze project-over-range");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT id FROM range(CAST(0 AS BIGINT), 5, CAST(1 AS BIGINT)) AS __td_range(id)"
        );
    }

    /// A BARE range dispatch (no Project) must keep the `id` bind via the
    /// block's default projection — `SELECT *` would emit DuckDB's raw
    /// `range` column name instead of Spark's `id`.
    #[test]
    fn bare_range_dispatch_keeps_id_default_projection() {
        let _g = tap_guard();
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: "range".to_owned(),
            args: vec![int_lit(3)],
            with_ordinality: false,
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze bare range");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT id FROM range(CAST(0 AS BIGINT), 3, CAST(1 AS BIGINT)) AS __td_range(id)"
        );
    }

    // ── TableFunction (explode) — pass-13 ──────────────────────────────

    /// Build `explode(<args>)` or `explode_outer(<args>)`, analyze, and emit.
    fn emit_explode_tvf(name: &str, args: Vec<Expression>) -> String {
        let ast = CommonAst::new(CommonOp::TableFunction {
            name: name.to_owned(),
            args,
            with_ordinality: false,
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze explode TVF");
        dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch explode TVF")
    }

    /// Pass 13 — tbl-007: `SELECT * FROM explode(array(1,2,3))` → SQL
    /// contains `UNNEST` and emits the column as `col`.
    #[test]
    fn dispatch_explode_tvf_emits_unnest_as_col() {
        let _g = tap_guard();
        let arr = Expression::FunctionCall(FunctionCall {
            name: "array".to_owned(),
            args: vec![int_lit(1), int_lit(2), int_lit(3)],
            distinct: false,
        });
        let sql = emit_explode_tvf("explode", vec![arr]);
        assert_eq!(sql, "SELECT UNNEST(list_value(1, 2, 3)) AS col");
    }

    /// Pass 13 — explode_outer as TVF wraps with the CASE/NULL sentinel.
    #[test]
    fn dispatch_explode_outer_tvf_emits_case_wrapper() {
        let _g = tap_guard();
        let arr = Expression::FunctionCall(FunctionCall {
            name: "array".to_owned(),
            args: vec![int_lit(1), int_lit(2)],
            distinct: false,
        });
        let sql = emit_explode_tvf("explode_outer", vec![arr]);
        assert!(
            sql.contains("UNNEST(CASE WHEN"),
            "explode_outer must emit the CASE wrapper; got: {sql}"
        );
        assert!(
            sql.contains("AS col"),
            "output column must be named 'col'; got: {sql}"
        );
    }

    // ── 4-6. render_project ──────────────────────────────────────────────

    #[test]
    fn render_literal_binary_emits_duckdb_blob_escape() {
        // Spark `X'1F2A'` lowers to a Binary literal; DuckDB's blob literal is a
        // `\xHH`-escaped string cast to BLOB (NOT `x'..'`, which DuckDB parses as
        // the VARCHAR "x.."). Corpus: fn-020.
        let sql = render_literal(&Literal {
            value: LiteralValue::Binary(vec![0x1F, 0x2A]),
            data_type: DataType::Binary,
        })
        .expect("render");
        assert_eq!(sql, r"CAST('\x1F\x2A' AS BLOB)");
    }

    /// Root cause 026: the DATE literal used to be built as
    /// `DATE '1970-01-01' + INTERVAL (n) DAY`, which DuckDB promotes to
    /// TIMESTAMP (`DATE ± INTERVAL` → TIMESTAMP). Build it with a plain
    /// INTEGER day offset instead — `DATE + INTEGER` stays DATE in DuckDB —
    /// so a bare `DATE '...'` literal collects back as date, not datetime.
    /// Corpus: test_interval_date_arithmetic's `d` column.
    #[test]
    fn render_literal_date_uses_integer_offset_not_interval() {
        let sql = render_literal(&Literal {
            value: LiteralValue::Date(20103),
            data_type: DataType::Date,
        })
        .expect("render");
        assert_eq!(sql, "(DATE '1970-01-01' + (20103))");
        assert!(
            !sql.contains("INTERVAL"),
            "DATE literal must not use INTERVAL construction (promotes to \
             TIMESTAMP in DuckDB); got: {sql}"
        );
    }

    #[test]
    fn render_project_simple_select() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
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
        // Project over a bare TableScan inlines `FROM emp` (Fix B, pass 126) —
        // no `__td_proj` wrap.
        assert_eq!(sql, "SELECT id FROM emp");
    }

    #[test]
    fn render_project_qualified_ref_binds_over_table_scan() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: Some("emp".to_owned()),
                    plan_id: None,
                },
            )],
        });
        let typed = analyze(ast, &bt).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // ADR-023 3e-ii: `id` is unique in `emp`, so the qualifier is dropped
        // at resolution (a bare bind is positionally equivalent).
        assert_eq!(sql, "SELECT id FROM emp");
        assert!(!sql.contains("__td_proj"), "got: {sql}");
    }

    // ── Aliased-join inlining (jn-001/002/003 root fix) ──────────────────

    fn dept_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("dept_id", DataType::Integer),
            StructField::nullable("dept_name", DataType::String),
        ])
    }

    fn base_types_emp_dept(plan: &CommonAst) -> BaseTypes {
        BaseTypes::build_from_plan(plan, |name| match name {
            "emp" => Some(emp_schema()),
            "dept" => Some(dept_schema()),
            _ => None,
        })
    }

    /// `AliasedRelation { TableScan { alias: None }, alias }` — the node both
    /// front-ends now produce for an aliased table (INV7).
    fn aliased_scan(table: &str, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(scan(table)),
            alias: alias.to_owned(),
        })
    }

    fn qcol(qualifier: &str, name: &str) -> Expression {
        Expression::UnresolvedColumn(crate::transpiler_v2::expression::UnresolvedColumn {
            name: name.to_owned(),
            qualifier: Some(qualifier.to_owned()),
            plan_id: None,
        })
    }

    // ── FromScope (Phase 0, ADR-023 __td_jl/jr retirement groundwork) ───────

    /// Placeholder `SqlUnit` for a `FromItem::Derived` wrap in tests that
    /// only exercise `FromScope`'s alias bookkeeping, never render this unit.
    fn placeholder_unit(base: &str) -> SqlUnit {
        SqlUnit::from(SelectBlock::from_item(FromItem::Relation {
            base: base.to_owned(),
            alias: None,
        }))
    }

    #[test]
    fn from_scope_bare_relation_side() {
        let _g = tap_guard();
        let typed = analyze(scan("emp"), &base_types_with_emp()).expect("analyze bare emp scan");
        let item = FromItem::Relation {
            base: "emp".to_owned(),
            alias: None,
        };
        let fs = FromScope::of(&typed, &item);
        assert_eq!(fs.alias_for(0), Some("emp"));
        assert!(fs.covers_all());
        assert_eq!(
            fs.slot_quals(),
            Some(vec!["emp".to_owned(); typed.resolved_schema.len()])
        );
    }

    #[test]
    fn from_scope_derived_wrapped_side() {
        let _g = tap_guard();
        let plan = aliased_scan("emp", "e");
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            _ => None,
        });
        let typed = analyze(plan, &bt).expect("analyze aliased emp scan");
        let item = FromItem::Derived {
            unit: Box::new(placeholder_unit("emp")),
            alias: "__td_jl".to_owned(),
        };
        let fs = FromScope::of(&typed, &item);
        // ADR-023 Phase 2: `alias_for`'s single-exposed fast path now
        // resolves this to the item's own (sole) exposed alias, regardless
        // of the analyzer's logical `e` alias.
        assert_eq!(fs.alias_for(0), Some("__td_jl"));
        assert!(!fs.covers_all());
        assert_eq!(
            fs.slot_quals(),
            Some(vec!["__td_jl".to_owned(); typed.resolved_schema.len()])
        );
    }

    #[test]
    fn from_scope_inlined_nested_join_side() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze bare emp/dept cross join");
        let k = emp_schema().len();
        let n = typed.resolved_schema.len();
        let item = FromItem::Join {
            left: Box::new(FromItem::Relation {
                base: "emp".to_owned(),
                alias: Some("e".to_owned()),
            }),
            right: Box::new(FromItem::Relation {
                base: "dept".to_owned(),
                alias: Some("d".to_owned()),
            }),
            kind: "JOIN",
            clause: String::new(),
            lateral: false,
        };
        let fs = FromScope::of(&typed, &item);
        assert_eq!(fs.alias_for(0), Some("e"));
        assert_eq!(fs.alias_for(k), Some("d"));
        assert!(fs.covers_all());
        let mut expected = vec!["e".to_owned(); k];
        expected.extend(vec!["d".to_owned(); n - k]);
        assert_eq!(fs.slot_quals(), Some(expected));
    }

    #[test]
    fn from_scope_dup_exposed_side_covers_but_is_ambiguous() {
        let _g = tap_guard();
        let plan = aliased_scan("emp", "t");
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            _ => None,
        });
        let typed = analyze(plan, &bt).expect("analyze aliased emp scan");
        let n = typed.resolved_schema.len();
        // The covering alias `t` is exposed TWICE by this (contrived) item —
        // documents why `covers_all`/`slot_quals` key off `covering` (any
        // exposed match), not `alias_for` (unambiguous match).
        let item = FromItem::Join {
            left: Box::new(FromItem::Relation {
                base: "x".to_owned(),
                alias: Some("t".to_owned()),
            }),
            right: Box::new(FromItem::Relation {
                base: "y".to_owned(),
                alias: Some("t".to_owned()),
            }),
            kind: "JOIN",
            clause: String::new(),
            lateral: false,
        };
        let fs = FromScope::of(&typed, &item);
        assert_eq!(fs.alias_for(0), None);
        assert!(fs.covers_all());
        assert_eq!(fs.slot_quals(), Some(vec!["t".to_owned(); n]));
    }

    #[test]
    fn render_project_star_over_using_join_emits_hoisted_slot_list() {
        let _g = tap_guard();
        // jn-008 downstream: `SELECT * FROM emp NATURAL JOIN dept` desugars to a
        // USING(dept_id) join. resolved_schema hoists dept_id to the front, but
        // DuckDB's `*` over USING keeps it in its natural (left) position. The
        // lone-`*`-over-USING-join path must delegate to the generic join
        // renderer's explicit hoisted slot list so wire order == declared order.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(join),
            projections: vec![Expression::Star(StarExpression { qualifier: None })],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze star-over-using");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // Hoisted explicit slot list: dept_id first, no bare `SELECT *`.
        assert!(
            sql.starts_with("SELECT dept_id,"),
            "USING key must be hoisted first; got: {sql}"
        );
        assert!(
            !sql.starts_with("SELECT *"),
            "outer projection must not delegate `*` order to DuckDB for a USING join; got: {sql}"
        );
        // The declared schema's first column is the hoisted key.
        assert_eq!(typed.resolved_schema.fields[0].name, "dept_id");
    }

    // ── Plan 006 F1-F4: structured hoisted-slot-list pins ────────────────

    #[test]
    fn drop_over_using_join_renders_hoisted_slots() {
        let _g = tap_guard();
        // F1 regression pin (review findings #1): `emp.join(dept,
        // on='dept_id').drop('dept_name')` must keep the join builder's
        // USING-key-first hoisted slot list — a bare `* EXCLUDE (dept_name)`
        // would let DuckDB's `*` place `dept_id` at its natural (dept-side)
        // position instead of the analyzer's hoisted-first schema order.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::DropColumns {
            input: Box::new(join),
            drop_names: vec!["dept_name".to_owned()],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze drop-over-using-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.starts_with("SELECT dept_id,"),
            "USING key must stay hoisted first; got: {sql}"
        );
        assert!(!sql.contains("* EXCLUDE"), "got: {sql}");
        assert!(
            !sql.contains("dept_name"),
            "dropped column must be absent from the slot list; got: {sql}"
        );
    }

    #[test]
    fn drop_above_occupied_block_keeps_exclude() {
        let _g = tap_guard();
        // Over an already-occupied block (Select cannot merge downstream of
        // Limit's LimitOffset ordinal), DropColumns wraps in `__td_sub` and
        // must keep today's `* EXCLUDE (...)` shape — the wrapped child
        // already rendered its own defaults (if any) inside that `*`.
        let plan = CommonAst::new(CommonOp::DropColumns {
            input: Box::new(CommonAst::new(CommonOp::Limit {
                input: Box::new(scan("emp")),
                limit: 5,
                offset: None,
            })),
            drop_names: vec!["salary".to_owned()],
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze drop-over-limit");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("* EXCLUDE (salary)") && sql.contains("AS __td_sub"),
            "occupied block must fall back to `* EXCLUDE` over the wrap; got: {sql}"
        );
    }

    #[test]
    fn multi_slot_star_over_using_join_expands_hoisted_slots() {
        let _g = tap_guard();
        // F4 regression pin: a bare `*` mixed into a multi-slot projection
        // list must expand to the join builder's hoisted slot list, not
        // render a raw `*` token that shadows the USING-key-first order
        // (the same shadowing `* EXCLUDE` suffers without the F1 fix).
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(join),
            projections: vec![
                Expression::Star(StarExpression { qualifier: None }),
                Expression::Alias(AliasExpression {
                    expr: Box::new(int_lit(1)),
                    alias: "one".to_owned(),
                }),
            ],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze multi-slot star over using join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.starts_with("SELECT dept_id,"),
            "hoisted slot list must expand in place of the bare star; got: {sql}"
        );
        assert!(sql.contains("1 AS one"), "got: {sql}");
        assert!(
            !sql.contains("*,"),
            "bare `*` must not shadow the hoisted list; got: {sql}"
        );
    }

    #[test]
    fn using_join_side_wrap_preserves_hoisted_slots() {
        let _g = tap_guard();
        // F2 regression pin (review findings #2): the RIGHT side of an
        // outer ON join is itself a USING join (`dept JOIN emp2 USING
        // (dept_id)`). The right side never inlines (`may_inline_nested_join`
        // is always false for the right side), so it always wraps — the wrap
        // must carry the block's hoisted USING-key-first defaults into `AS
        // __td_jr`, not rebuild a bare `SELECT *` shell that lets DuckDB's
        // `*` place `dept_id` back at its natural (dept-side) position.
        let nested_using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("dept")),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(nested_using_join),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(1)),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze nested USING join-side wrap");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains(
                "SELECT dept_id, dept.dept_name, emp2.id, emp2.country \
                 FROM dept INNER JOIN emp2 USING (dept_id)) AS __td_jr"
            ),
            "wrapped __td_jr body must preserve the hoisted USING slot list; got: {sql}"
        );
        assert!(
            !sql.contains("(SELECT * FROM dept INNER JOIN emp2 USING (dept_id)) AS __td_jr"),
            "must not discard hoisted slots for a bare `SELECT *`; got: {sql}"
        );
    }

    // ── Plan 007 F5: inline under USING parents; RelScope-qualified
    // hoisted slots ───────────────────────────────────────────────────────

    #[test]
    fn alias_ref_above_using_parent_inlines_and_binds() {
        let _g = tap_guard();
        // join-021 (F5 regression pin): a plain-ON nested join (`emp e JOIN
        // dept d ON e.dept_id = d.dept_id`) is the LEFT side of an outer
        // USING(dept_id) join against `emp2`. Before F5, the USING parent's
        // `parent_has_using` guard unconditionally refused to inline the
        // nested join, burying `e` under `AS __td_jl` — and the outer USING
        // join's EMPTY `RelScope` made the qualifier vis-exempt in
        // `exprs_visible_in`, so the merge went ahead anyway and emitted an
        // unbindable `e.name`. F5 widened the guard to inline whenever the
        // nested join's own `RelScope` covers every field (it does here:
        // `e` covers 0..4, `d` covers 4..6).
        //
        // ADR-023 Phase 2.1: `dept_id` is ALSO the nested join's own join
        // key (`emp.dept_id` / `dept.dept_id`), so it is duplicated in the
        // nested side's flattened `resolved_schema` — `using_key_duplicated`
        // now refuses the flat inline under this USING parent (a live
        // DuckDB probe Binder-errors "Ambiguous reference \"dept_id\"" on
        // that exact flat chain). The side falls back to rung 3's single
        // Derived wrap instead: `e` is not renamed or dropped, just nested
        // one level deeper (`(... AS e ...) AS __td_jl`) — F5's actual
        // concern (the reference must BIND, independent of the FROM scope's
        // shape) still holds: `e.name` resolves (source_quals-tracked,
        // ADR-023 3e-i) to bare, unqualified `name` regardless of the nested
        // join's own inline-vs-wrap outcome, and `name` stays unambiguous in
        // the now-wrapped FROM scope.
        let nested_on_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_on_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_using_join),
            projections: vec![qcol("e", "name")],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze alias-ref-above-using-parent");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS e"),
            "left alias must not be dropped or renamed, even nested inside the dup-key wrap; got: {sql}"
        );
        // ADR-023 3e-i: the outer USING join is now `source_quals`-tracked, so
        // `e.name` resolves projected-through (source_quals `{e}`, single hit)
        // to qualifier=None → emission drops the qualifier and renders bare
        // `name`, which binds positionally over the join. This holds
        // independent of the nested join's own inline-vs-wrap outcome: the
        // reference never needed to carry the `e.` qualifier to bind.
        assert!(
            sql.contains("SELECT name") && !sql.contains("e.name"),
            "projection must resolve projected-through to bare `name`; got: {sql}"
        );
        // ADR-023 Phase 2.1: `dept_id` is duplicated in the nested side's
        // flattened schema (emp.dept_id / dept.dept_id) — `using_key_duplicated`
        // refuses the flat inline under this USING parent, so the side wraps.
        assert!(
            sql.contains("AS __td_jl"),
            "the nested join must wrap under the synthetic alias — dept_id is \
             duplicated in its flattened schema and the flat USING chain is a \
             DuckDB Binder Error; got: {sql}"
        );
    }

    #[test]
    fn using_parent_hoisted_slots_qualify_by_covering_alias() {
        let _g = tap_guard();
        // Retargeted (ADR-023 Phase 2.1) to a non-duplicated USING key: the
        // outer parent USES `USING (id)` rather than `USING (dept_id)`.
        // `id` is unique to `emp` within the nested side's flattened
        // schema, so `using_key_duplicated` does not trip the Phase 2.1
        // guard, and — since the nested join's own `FromScope` fully
        // covers both `e` and `d` — the left side inlines. The join
        // builder's own hoisted default-slot list must qualify each
        // non-key left field by whichever alias's `RelScope` range covers
        // it: `dept_id` is duplicated between `e` and `d` (both a real,
        // DuckDB-bindable relation in the flattened FROM), so both
        // `e.dept_id` and `d.dept_id` appear, distinctly qualified. (The
        // old `USING (dept_id)` shape this test used to exercise is now a
        // dup-key WRAP case — see the sibling
        // `using_parent_hoisted_slots_dup_key_still_wraps` below.)
        let nested_on_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_on_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze bare using-parent join (id key)");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("e.dept_id") && sql.contains("d.dept_id"),
            "non-key dept_id fields from both sides of the nested join must \
             be distinctly qualified by their covering alias; got: {sql}"
        );
        assert!(
            sql.contains("USING (id)"),
            "outer join must render as a USING join on the retargeted key; \
             got: {sql}"
        );
        assert!(
            !sql.contains("__td_jl"),
            "left side must inline (id is not duplicated in the flattened \
             left schema, so the Phase 2.1 guard does not trip); got: {sql}"
        );
    }

    #[test]
    fn using_parent_hoisted_slots_dup_key_still_wraps() {
        let _g = tap_guard();
        // The OLD shape `using_parent_hoisted_slots_qualify_by_covering_alias`
        // used to exercise: outer `USING (dept_id)` parent over the LEFT
        // nested ON-join `emp e JOIN dept d ON e.dept_id = d.dept_id`.
        // `dept_id` is duplicated in the nested side's OWN flattened
        // schema (both `e.dept_id` and `d.dept_id`), which DuckDB's binder
        // rejects for a `USING` key (live-validated, ADR-023 Phase 2.1):
        // `using_key_duplicated` now trips the guard even though
        // `FromScope::covers_all()` succeeds, forcing the left side to
        // wrap under `AS __td_jl` instead of inlining a DuckDB-invalid
        // flattened form.
        let nested_on_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_on_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze dup-key using-parent join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_jl"),
            "duplicated USING key in the flattened left schema must force \
             a wrap (DuckDB-invalid otherwise); got: {sql}"
        );
        assert!(
            sql.contains("USING (dept_id)"),
            "outer join must still render as a USING join on dept_id; \
             got: {sql}"
        );
    }

    #[test]
    fn using_parent_over_transitive_dup_key_ancestor_still_wraps() {
        let _g = tap_guard();
        // Phase 2.1 transitive-case witness: THREE levels — a USING parent
        // (outer) whose left is a plain ON-join (middle), whose OWN left is
        // a further-nested dup-key ON-join (innermost: `e.dept_id =
        // d.dept_id`, duplicating `dept_id` two levels down). Every level
        // is individually inlinable (all ON-joins, no USING among them, all
        // covered `FromScope`s) — so `middle`'s own build pass flattens
        // `innermost` bare into its FROM, and `middle`'s resulting
        // `resolved_schema` still carries the `dept_id` duplication
        // transitively, from `innermost` alone (`middle`'s own extra
        // operand — `loc`, unrelated columns only — must NOT itself
        // collide on any non-key name, isolating the assertion to the
        // transitive `dept_id` duplication rather than an incidental
        // same-name collision elsewhere). `using_key_duplicated` reads
        // `side.resolved_schema` — the WHOLE flattened schema, not just
        // `middle`'s immediate children — so it still trips even though
        // the duplication did not originate in `middle`'s own direct
        // operands, forcing the outer's left (`middle`) to wrap under `AS
        // __td_jl` instead of flattening a DuckDB-invalid USING
        // binder-ambiguity into one FROM scope.
        let innermost = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let middle = CommonAst::new(CommonOp::Join {
            left: Box::new(innermost),
            right: Box::new(aliased_scan("loc", "l")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "id")),
                right: Box::new(qcol("l", "loc_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Join {
            left: Box::new(middle),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            "dept" => Some(dept_schema()),
            "loc" => Some(StructType::new(vec![
                StructField::not_null("loc_id", DataType::Long),
                StructField::nullable("loc_name", DataType::String),
            ])),
            _ => None,
        });
        let typed = analyze(plan, &bt).expect("analyze transitive dup-key using-parent join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_jl"),
            "a dup-key that originates two levels down must still be seen \
             via the flattened resolved_schema and force the outer's left \
             to wrap; got: {sql}"
        );
        assert!(
            sql.contains("USING (dept_id)"),
            "outer join must still render as a USING join on dept_id; \
             got: {sql}"
        );
    }

    #[test]
    fn using_parent_with_uncoverable_side_still_wraps() {
        let _g = tap_guard();
        // Residual gap (plan 007, tracked not fixed here): when the nested
        // join's OWN children re-scope (each a `Project` over a scan, whose
        // `RelScope` is empty per `RelScope::of`), the nested join's own
        // `RelScope` has zero coverage — `scope_covers_fields` fails, so the
        // left side must still wrap under `AS __td_jl` exactly as before
        // F5. F5 only WIDENS inlining to coverable multi-alias sides; it
        // never removes the wrap fallback for an uncoverable one.
        let left_child = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                ColumnReference::untyped("id"),
                ColumnReference::untyped("dept_id"),
            ],
        });
        let right_child = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![
                ColumnReference::untyped("dept_id"),
                ColumnReference::untyped("dept_name"),
            ],
        });
        let nested_join = CommonAst::new(CommonOp::Join {
            left: Box::new(left_child),
            right: Box::new(right_child),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_using_join),
            projections: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze uncoverable-side USING wrap");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_jl"),
            "an uncoverable multi-alias side must still wrap; got: {sql}"
        );
    }

    #[test]
    fn using_parent_with_synthetic_scoped_side_stays_wrapped() {
        let _g = tap_guard();
        // Post-collapse (ADR-023 Phase 2): the nested join's OWN condition
        // (plan_id-tagged `dept_id` refs across `emp`/`dept`) is its own
        // demand only — no ancestor references either of ITS sides. The
        // nested join's children now inline bare (`emp INNER JOIN dept ON
        // (emp.dept_id) =
        // (dept.dept_id)`) INSIDE the derived body. The wrap this test
        // pins is now driven by an entirely different mechanism (ADR-023
        // Phase 2.1): the outer parent is `USING (dept_id)`, and
        // `dept_id` is duplicated in the nested side's own flattened
        // schema (`emp.dept_id` and `dept.dept_id`) — DuckDB rejects a
        // duplicated USING key, so `using_key_duplicated` trips the guard
        // and forces the left side to wrap under `AS __td_jl` regardless
        // of `FromScope::covers_all()`. The inner ON clause legitimately
        // reads bare `emp.`/`dept.` INSIDE the derived body (those names
        // are exposed there); only the OUTER select list must avoid
        // referencing them, since the OUTER FROM only exposes `__td_jl`.
        let nested_on_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(10),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(20),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![10],
            right_plan_ids: vec![20],
        });
        let outer_using_join = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_on_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&outer_using_join);
        let typed = analyze(outer_using_join, &bt).expect("analyze synthetic-scoped side");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_jl"),
            "the duplicated USING key in the nested side's flattened schema \
             must still force a wrap (Phase 2.1 guard), even though neither \
             side is synthetic-stamped anymore; got: {sql}"
        );
        let outer_select_list = sql
            .split_once(" FROM ")
            .map(|(select, _)| select)
            .unwrap_or(&sql);
        assert!(
            !outer_select_list.contains("emp.") && !outer_select_list.contains("dept."),
            "the OUTER select list must not reference the now-invisible \
             `emp`/`dept` names (the outer FROM only exposes `__td_jl`), \
             even though the inner ON clause legitimately reads bare \
             `emp.`/`dept.` INSIDE the derived body; got: {sql}"
        );
    }

    #[test]
    fn render_project_over_join_hoists_user_aliases() {
        let _g = tap_guard();
        // SELECT e.name, d.dept_name FROM emp e JOIN dept d ON e.dept_id = d.dept_id
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(join),
            projections: vec![qcol("e", "name"), qcol("d", "dept_name")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze project-over-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // User aliases hoisted into the subquery aliases; no synthetic alias,
        // so the ON clause and projection bind against `e` / `d`.
        assert!(sql.contains("emp AS e INNER JOIN "), "got: {sql}");
        assert!(sql.contains("dept AS d ON "), "got: {sql}");
        assert!(sql.contains("(e.dept_id) = (d.dept_id)"), "got: {sql}");
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn render_project_over_filter_over_join_inlines_to_single_select() {
        let _g = tap_guard();
        // SELECT e.name, d.dept_name FROM emp e, dept d WHERE e.dept_id = d.dept_id
        // lowers to Project → Filter → CrossJoin.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            }),
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(filter),
            projections: vec![qcol("e", "name"), qcol("d", "dept_name")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze project-over-filter-over-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // Project→Filter→Join collapses into one SELECT: aliases hoisted, the
        // predicate lands as an outer WHERE (not a buried subquery filter).
        assert!(sql.contains("emp AS e CROSS JOIN "), "got: {sql}");
        assert!(sql.contains("dept AS d WHERE "), "got: {sql}");
        assert!(sql.contains("(e.dept_id) = (d.dept_id)"), "got: {sql}");
        assert!(!sql.contains("__td_filter"), "got: {sql}");
        assert!(!sql.contains("__td_proj"), "got: {sql}");
    }

    #[test]
    fn render_project_over_filter_over_aliased_relation_inlines() {
        let _g = tap_guard();
        // Correlated EXISTS body: `SELECT * FROM emp e WHERE e.dept_id = ...`
        // lowers to Project → Filter → AliasedRelation. The alias `e` must
        // become the FROM table name so the WHERE's `e.dept_id` binds.
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(aliased_scan("emp", "e")),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(1)),
            }),
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(filter),
            projections: vec![],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze project-over-filter-over-aliased");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e WHERE "), "got: {sql}");
        assert!(!sql.contains("__td_filter"), "got: {sql}");
        assert!(!sql.contains("__td_proj"), "got: {sql}");
    }

    #[test]
    fn render_aggregate_over_filter_over_aliased_relation_inlines() {
        let _g = tap_guard();
        // Correlated scalar subquery body: `SELECT max(e.salary) FROM emp e
        // WHERE e.dept_id = ...` lowers to Aggregate → Filter →
        // AliasedRelation. Alias `e` must be the FROM name so both the
        // aggregate arg and the WHERE bind to it in one SELECT.
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(aliased_scan("emp", "e")),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(int_lit(1)),
            }),
        });
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(filter),
            grouping: vec![],
            aggregates: vec![fexpr("max", vec![qcol("e", "salary")])],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-filter-over-aliased");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e WHERE "), "got: {sql}");
        assert!(!sql.contains("__td_filter"), "got: {sql}");
        assert!(!sql.contains("__td_agg"), "got: {sql}");
    }

    // ── Pass 6: alias-transparent FROM for aggregate inputs & nested join
    // sides (jn-013/jn-015/sq-015) ────────────────────────────────────────

    /// Third table for three-way-join / nested-join-side tests — `emp2` in
    /// the diagnostic's jn-013 shape.
    fn emp2_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("country", DataType::String),
        ])
    }

    fn base_types_emp_dept_emp2(plan: &CommonAst) -> BaseTypes {
        BaseTypes::build_from_plan(plan, |name| match name {
            "emp" => Some(emp_schema()),
            "dept" => Some(dept_schema()),
            "emp2" => Some(emp2_schema()),
            _ => None,
        })
    }

    #[test]
    fn render_aggregate_over_join_hoists_user_aliases_no_td_agg_or_td_jl() {
        let _g = tap_guard();
        // jn-015: SELECT d.dept_name, avg(e.salary) AS avg_sal FROM emp e
        //   JOIN dept d ON e.dept_id = d.dept_id GROUP BY d.dept_name
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(join),
            grouping: vec![qcol("d", "dept_name")],
            aggregates: vec![
                qcol("d", "dept_name"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr("avg", vec![qcol("e", "salary")])),
                    alias: "avg_sal".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e INNER JOIN "), "got: {sql}");
        assert!(sql.contains("dept AS d ON "), "got: {sql}");
        // ADR-023 3e-ii: `dept_name` is unique across `emp`+`dept`, so the
        // qualifier is dropped at resolution (a bare bind is positionally
        // equivalent).
        assert!(sql.contains("GROUP BY dept_name"), "got: {sql}");
        assert!(!sql.contains("GROUP BY d.dept_name"), "got: {sql}");
        assert!(!sql.contains("__td_agg"), "got: {sql}");
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn render_aggregate_over_aliased_relation_with_having_no_td_agg() {
        let _g = tap_guard();
        // sq-015: SELECT e.dept_id, count(*) AS n FROM emp e GROUP BY
        //   e.dept_id HAVING count(*) > 1
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(aliased_scan("emp", "e")),
            grouping: vec![qcol("e", "dept_id")],
            aggregates: vec![
                qcol("e", "dept_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr(
                        "count",
                        vec![Expression::Star(StarExpression { qualifier: None })],
                    )),
                    alias: "n".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: Some(count_star_gt_one()),
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-aliased-relation+having");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e"), "got: {sql}");
        // ADR-023 3e-ii: `dept_id` is unique in `emp`, so the qualifier is
        // dropped at resolution (a bare bind is positionally equivalent).
        assert!(sql.contains("GROUP BY dept_id"), "got: {sql}");
        assert!(!sql.contains("GROUP BY e.dept_id"), "got: {sql}");
        assert!(sql.contains("HAVING "), "got: {sql}");
        assert!(!sql.contains("__td_agg"), "got: {sql}");
    }

    /// agg-025 (F9): `emp.alias("e").orderBy("id").limit(5)
    /// .groupBy(col("e.dept_id")).count()` — GROUP BY cannot merge past an
    /// occupied LIMIT slot; the qualifier is dropped at resolution (unique
    /// name) so both the grouping key and its SELECT copy render bare
    /// (structure-preserving; only the qualifier drops), mirroring
    /// `filter_above_limit_drops_alias_qualifier_at_resolution` /
    /// `sort_above_limit_drops_alias_qualifier_at_resolution` for `build_filter`
    /// / `build_sort`.
    #[test]
    fn aggregate_over_limit_drops_alias_qualifier_at_resolution() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(CommonAst::new(CommonOp::Sort {
                input: Box::new(aliased_scan("emp", "e")),
                order: vec![asc_key(ColumnReference::untyped("id"))],
                limit: Some(5),
                offset: None,
            })),
            grouping: vec![qcol("e", "dept_id")],
            aggregates: vec![
                qcol("e", "dept_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr(
                        "count",
                        vec![Expression::Star(StarExpression { qualifier: None })],
                    )),
                    alias: "count".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-limit");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("AS __td_sub GROUP BY dept_id"), "got: {sql}");
        assert!(!sql.contains("e.dept_id"), "got: {sql}");
    }

    /// Merge-path regression pin: the same `emp e` groupBy `e.dept_id` shape
    /// with NO occupied clause above it merges into a single SELECT — the
    /// reorder must not perturb this common case (alias stays exposed, no
    /// `__td_sub`).
    #[test]
    fn aggregate_over_aliased_relation_merges_keeps_alias() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(aliased_scan("emp", "e")),
            grouping: vec![qcol("e", "dept_id")],
            aggregates: vec![
                qcol("e", "dept_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr(
                        "count",
                        vec![Expression::Star(StarExpression { qualifier: None })],
                    )),
                    alias: "count".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze aggregate-merges");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e"), "got: {sql}");
        // ADR-023 3e-ii: `dept_id` is unique in `emp`, so the qualifier is
        // dropped at resolution (a bare bind is positionally equivalent).
        assert!(sql.contains("GROUP BY dept_id"), "got: {sql}");
        assert!(!sql.contains("GROUP BY e.dept_id"), "got: {sql}");
        assert!(!sql.contains("__td_sub"), "got: {sql}");
    }

    #[test]
    fn render_aggregate_over_filter_over_join_chains_from_and_where_before_group_by() {
        let _g = tap_guard();
        // SELECT d.dept_name, avg(e.salary) FROM emp e, dept d
        //   WHERE e.dept_id = d.dept_id GROUP BY d.dept_name
        // (comma-join lowers to Aggregate → Filter → Join.)
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            }),
        });
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(filter),
            grouping: vec![qcol("d", "dept_name")],
            aggregates: vec![
                qcol("d", "dept_name"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr("avg", vec![qcol("e", "salary")])),
                    alias: "avg_sal".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-filter-over-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e CROSS JOIN "), "got: {sql}");
        assert!(sql.contains("dept AS d WHERE "), "got: {sql}");
        assert!(sql.contains("(e.dept_id) = (d.dept_id)"), "got: {sql}");
        // ADR-023 3e-ii: `dept_name` is unique across `emp`+`dept`, so the
        // qualifier is dropped at resolution (a bare bind is positionally
        // equivalent). `dept_id` is duplicated (both sides), so it keeps its
        // qualifier — asserted above.
        assert!(sql.contains("GROUP BY dept_name"), "got: {sql}");
        assert!(!sql.contains("GROUP BY d.dept_name"), "got: {sql}");
        assert!(!sql.contains("__td_agg"), "got: {sql}");
        assert!(!sql.contains("__td_filter"), "got: {sql}");
        let where_pos = sql.find(" WHERE ").expect("where clause present");
        let group_pos = sql.find(" GROUP BY ").expect("group by clause present");
        assert!(where_pos < group_pos, "WHERE must precede GROUP BY: {sql}");
    }

    #[test]
    fn render_project_over_nested_join_flattens_three_way_chain() {
        let _g = tap_guard();
        // jn-013: SELECT e.name, d.dept_name FROM emp e JOIN dept d
        //   ON e.dept_id = d.dept_id JOIN emp2 m ON d.dept_id = m.dept_id
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(aliased_scan("emp2", "m")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("d", "dept_id")),
                right: Box::new(qcol("m", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![qcol("e", "name"), qcol("d", "dept_name")],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze three-way join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("emp AS e INNER JOIN "), "got: {sql}");
        assert!(sql.contains("dept AS d ON "), "got: {sql}");
        assert!(sql.contains("emp2 AS m ON "), "got: {sql}");
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn render_project_over_nested_join_duplicate_alias_refuses_flatten() {
        let _g = tap_guard();
        // Reviewer pass-6 Medium: a nested join whose flattened chain would
        // reuse a user alias must NOT flatten, or DuckDB rejects the FROM with
        // "Duplicate alias". Here the inner join's left is `emp m` and the
        // OUTER right is `emp2 m` — flattening would put two `AS m` in one FROM
        // scope.
        //
        // ADR-023 3b-i: the OUTER join's own condition (`d.dept_id ==
        // m.dept_id`) resolves `m` against the outer join's combined scope,
        // where `m` is now bound TWICE (once via the inner join's `emp AS m`
        // inherited on the left, once via `emp2 AS m` on the right) — a
        // qualifier binding 2+ ranges is exactly the ambiguity this chunk
        // makes `resolve_column` catch. Analysis now correctly rejects this
        // input as `AmbiguousColumn` before emission's SQL-shape flatten
        // guard is ever reached, which is the right outcome: this AST is a
        // genuinely ambiguous reference (Spark would reject the equivalent
        // query too), not merely a defensive SQL-rendering concern. The
        // flatten guard itself (`build_join`'s sibling-collision check) is
        // unchanged and still fires for inputs where the duplicated alias is
        // never referenced by qualifier.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "m")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("m", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(aliased_scan("emp2", "m")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("d", "dept_id")),
                right: Box::new(qcol("m", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![qcol("d", "dept_name")],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let err = analyze(plan, &bt).unwrap_err();
        assert!(
            matches!(err, AnalyzerError::AmbiguousColumn { .. }),
            "expected AmbiguousColumn, got {err:?}"
        );
    }

    #[test]
    fn render_join_from_dataframe_plan_id_contract_flattens_and_binds_positionally() {
        let _g = tap_guard();
        // DataFrame join-of-join, plan_id-tagged outer condition:
        // `df.join(df2).join(df3, ...)`. Post-collapse (ADR-023 Phase 2):
        // a join's own condition never force-wraps its sides (the demand-flag
        // machinery is retired), so the nested LEFT side inlines. The nested
        // join (`emp CROSS JOIN dept`) is a plain, bare-aliased chain and
        // fully flattens into the outer FROM. `requalify_join_condition`
        // then resolves `id` positionally: it is duplicated in the FULL
        // outer merged schema (`emp.id` vs. `emp2.id`), but within the
        // LEFT side's own `FromScope`, `emp` is the sole exposed relation
        // covering that ordinal, so the qualifier resolves to the real
        // table name `emp` — no synthetic wrap is needed at all. This test
        // was originally named/written to pin the OLD wrapped,
        // non-flattened shape (`keeps_td_jl_no_flatten`); it is renamed
        // here to describe the new, collapsed behavior it now
        // demonstrates. Same DATA as before: `emp CROSS JOIN dept` then
        // `INNER JOIN emp2 ON emp.id = emp2.country` binds identically
        // whether `emp`/`dept` are addressed via bare names or a buried
        // `__td_jl` alias.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        // `id` (unique to `emp` within the inner join's OWN merged schema,
        // but duplicated against `emp2.id` in the OUTER join's full merged
        // schema) and `country` (unique across the whole outer schema)
        // avoid an unrelated `AmbiguousColumn` from the `dept_id` name that
        // both `dept` and `emp2` share.
        let outer_condition = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: Some(1),
                },
            )),
            right: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "country".to_owned(),
                    qualifier: None,
                    plan_id: Some(2),
                },
            )),
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(outer_condition),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze DataFrame join-of-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // ADR-023 Phase 2: the nested join fully flattens (no ancestor
        // demand forces either side to wrap), and `id`'s ordinal resolves
        // positionally to the real table name `emp` (the sole exposed
        // relation covering that column within the LEFT side's own
        // `FromScope`) even though `id` is duplicated over the full outer
        // merged schema. `country` is unique to `emp2` and drops its
        // qualifier entirely.
        assert!(
            sql.contains("FROM emp CROSS JOIN dept INNER JOIN emp2 ON (emp.id) = (country)"),
            "expected a fully flattened chain with the condition resolved to \
             the real `emp` alias; got: {sql}"
        );
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn within_side_duplicate_name_binds_the_correct_leftmost_occurrence() {
        let _g = tap_guard();
        // Boundary/double-bind witness: `dept_id` is duplicated WITHIN the
        // nested join's OWN flattened schema (`emp.dept_id` at local index
        // 2, `dept.dept_id` at local index 4) — not just against the
        // OUTER's right side. The outer condition references it
        // unqualified via the nested join's own plan_id (DataFrame-style,
        // mirrors `render_join_from_dataframe_plan_id_contract_flattens_and_binds_positionally`);
        // `resolve_column`'s plan_id arm resolves a name-only lookup
        // within that plan's range by first match (leftmost — Spark's own
        // resolution order for this class of ambiguity), landing on
        // `emp.dept_id` (ordinal 2), NOT `dept.dept_id` (ordinal 4). At
        // emission, `dept_id` is ALSO ambiguous against `emp2.dept_id` on
        // the outer right, so the full rewrite path runs: ordinal 2 (`<
        // left_len`) resolves via the LEFT side's own `FromScope` to the
        // real table name `emp` — proving the two same-named left-side
        // occurrences resolve DISTINCTLY and correctly by ordinal, not by
        // name (a name-based lookup could not tell `emp.dept_id` from
        // `dept.dept_id` and risks a silent double-bind).
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_condition = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: Some(1),
                },
            )),
            right: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "country".to_owned(),
                    qualifier: None,
                    plan_id: Some(2),
                },
            )),
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(outer_condition),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze within-side-duplicate join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("FROM emp CROSS JOIN dept INNER JOIN emp2 ON (emp.dept_id) = (country)"),
            "the leftmost within-side occurrence (emp.dept_id) must bind \
             distinctly and correctly, not dept.dept_id and not a synthetic \
             wrap; got: {sql}"
        );
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn condition_over_uncoverable_inlined_side_wraps_fresh_and_retries() {
        let _g = tap_guard();
        // SideNeedsAlias retry witness: the nested join's own children are
        // `Project`s (whose `RelScope` is empty per `RelScope::of`), so the
        // nested join itself has NO alias coverage — exactly the
        // `using_parent_with_uncoverable_side_still_wraps` premise — but
        // this time it sits under a plain ON-join parent (no USING), so
        // `build_join_side`'s `inline_ok` (whose USING-coverage guard is
        // gated on `parent_has_using`) inlines it bare regardless: the
        // nested side flattens to `emp CROSS JOIN dept` in the outer FROM.
        // The outer condition's `id` is ambiguous in `cond_schema` (also
        // present on `emp2`) and its ordinal falls in the nested side, so
        // `FromScope::alias_for` must resolve it against that flattened,
        // multi-exposed (`["emp", "dept"]`), UNCOVERED side —
        // `covering()` finds no covering alias (empty `scope.aliases`) and
        // returns `None`, flagging `needs.left`. `build_join`'s fixpoint
        // then wraps the left side fresh under `__td_jl` (single-exposed
        // now, so `alias_for`'s fast path binds unconditionally) and
        // retries — resolving in the bounded `<=2` passes.
        let left_child = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("emp")),
            projections: vec![
                ColumnReference::untyped("id"),
                ColumnReference::untyped("dept_id"),
            ],
        });
        let right_child = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![
                ColumnReference::untyped("dept_id"),
                ColumnReference::untyped("dept_name"),
            ],
        });
        let nested_join = CommonAst::new(CommonOp::Join {
            left: Box::new(left_child),
            right: Box::new(right_child),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_condition = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: Some(1),
                },
            )),
            right: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "country".to_owned(),
                    qualifier: None,
                    plan_id: Some(2),
                },
            )),
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(nested_join),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(outer_condition),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze uncoverable-side condition retry");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("__td_jl.id"),
            "the ambiguous left-side reference must bind through the fresh \
             wrap alias; got: {sql}"
        );
        assert!(
            sql.contains("AS __td_jl"),
            "the uncoverable nested side must wrap fresh once the outer \
             condition demands it; got: {sql}"
        );
        assert!(
            sql.contains("= (country)") || sql.contains("(country)"),
            "the unambiguous right-side reference stays bare; got: {sql}"
        );
    }

    #[test]
    fn adr023_phase1_unique_name_plan_id_condition_inlines_both_sides() {
        let _g = tap_guard();
        // ADR-023 Phase 1: `id` (emp-only) and `dept_name` (dept-only) are
        // each unique in the merged emp⋈dept condition schema, so the
        // resolved condition drops its synthetic qualifier and neither join
        // side needs a __td_jl/__td_jr wrap — both inline directly into a
        // single FROM ... INNER JOIN.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "id".to_owned(),
                        qualifier: None,
                        plan_id: Some(1),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_name".to_owned(),
                        qualifier: None,
                        plan_id: Some(2),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let bt = base_types_emp_dept(&join);
        let typed = analyze(join, &bt).expect("analyze unique-name plan_id join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
        assert!(
            sql.contains("FROM emp INNER JOIN dept ON "),
            "both sides must inline directly, no synthetic wrap; got: {sql}"
        );
        assert!(
            sql.contains("(id) = (dept_name)"),
            "ON clause must render bare names, not __td_jl./__td_jr.-qualified; got: {sql}"
        );
    }

    #[test]
    fn asymmetric_schema_left_heavier_binds_correct_side() {
        let _g = tap_guard();
        // ADR-023 Phase 2, H8 hiding places 1+2 (merged-vs-local ordinal
        // confusion; ordinal/name drift): LEFT (`emp`, 4 cols: id, name,
        // dept_id, salary) is WIDER than RIGHT (`dept`, 2 cols: dept_id,
        // dept_name). `dept_id`'s merged ordinal on the left is 2
        // (`< left_len == 4` → local 2); on the right it is 4
        // (`>= left_len` → local `4 - 4 == 0`). A left_len/local
        // subtraction bug would either bind the wrong physical column or
        // trip the `local < scope.width` debug_assert. Both sides are
        // real user aliases (single-exposed `FromScope`), so the fixpoint
        // resolves in one pass with no synthetic wrap.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(1),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(2),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let bt = base_types_emp_dept(&join);
        let typed = analyze(join, &bt).expect("analyze left-heavier asymmetric join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql, "SELECT * FROM emp AS e INNER JOIN dept AS d ON (e.dept_id) = (d.dept_id)",
            "the wider LEFT (emp) must bind its own field via `e`, and the \
             narrower RIGHT (dept) via `d` — a swapped/miscomputed local \
             index would bind the wrong table's dept_id; got: {sql}"
        );
    }

    #[test]
    fn asymmetric_schema_right_heavier_binds_correct_side() {
        let _g = tap_guard();
        // Companion to `asymmetric_schema_left_heavier_binds_correct_side`
        // with the widths SWAPPED (RIGHT, `emp`, 4 cols, now wider than
        // LEFT, `dept`, 2 cols) — guards against a hardcoded "left is
        // always wider/narrower" assumption. `dept_id`'s merged ordinal on
        // the left (`dept`) is 0 (`< left_len == 2`); on the right
        // (`emp`) it is 2 (`>= left_len` → local `2 - 2 == 0`). Side-swap
        // changes which physical table each alias binds to — a
        // regression here would silently swap the join's DATA, not just
        // its SQL surface.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("dept", "d")),
            right: Box::new(aliased_scan("emp", "e")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(1),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(2),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let bt = base_types_emp_dept(&join);
        let typed = analyze(join, &bt).expect("analyze right-heavier asymmetric join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql, "SELECT * FROM dept AS d INNER JOIN emp AS e ON (d.dept_id) = (e.dept_id)",
            "the narrower LEFT (dept) must bind via `d`, and the wider \
             RIGHT (emp) via `e`; got: {sql}"
        );
    }

    #[test]
    fn render_join_side_plan_id_condition_overrides_aliased_relation_hoist() {
        let _g = tap_guard();
        // `df.alias("e").join(df2.alias("d"), ...)` with a plan_id-tagged
        // (not user-qualified) condition. Post-collapse (ADR-023 Phase 2):
        // a join's own condition never force-wraps its sides (the demand-flag
        // machinery is retired). Both sides are
        // user `AliasedRelation`s and inline directly under their own real
        // aliases (`e`/`d`); `requalify_join_condition` resolves the
        // plan_id-tagged, unqualified `dept_id` refs positionally to those
        // same real aliases — no synthetic `__td_jl`/`__td_jr` wrap is
        // needed at all. (This test previously pinned the OLD behavior,
        // where the plan_id contract forced both sides to wrap under
        // synthetic aliases even though real user aliases were available
        // and sufficient.)
        let condition = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: Some(1),
                },
            )),
            right: Box::new(Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: Some(2),
                },
            )),
        });
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(condition),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(join),
            projections: vec![],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze plan_id-tagged aliased join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql, "SELECT * FROM emp AS e INNER JOIN dept AS d ON (e.dept_id) = (d.dept_id)",
            "both sides must inline under their real user aliases with the \
             condition resolved positionally; got: {sql}"
        );
        assert!(!sql.contains("__td_jl"), "got: {sql}");
        assert!(!sql.contains("__td_jr"), "got: {sql}");
    }

    #[test]
    fn render_aggregate_over_join_using_inlines_from_no_td_agg() {
        let _g = tap_guard();
        // USING-under-aggregate regression pin: a direct (non-nested) USING
        // join under Aggregate still inlines through
        // `render_alias_transparent_from` (no `__td_agg`); USING's own
        // column-order semantics are unaffected (join_chain_flattenable only
        // governs whether a NESTED join side flattens).
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("dept")),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(join),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr(
                        "count",
                        vec![Expression::Star(StarExpression { qualifier: None })],
                    )),
                    alias: "n".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze aggregate-over-using-join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("USING (dept_id)"), "got: {sql}");
        assert!(sql.contains("GROUP BY dept_id"), "got: {sql}");
        assert!(!sql.contains("__td_agg"), "got: {sql}");
    }

    // ── tbl-013 regression: derived-table column-alias-list over a qualified
    // aggregate arg (ToDf/WithColumnsRenamed positional rename) ────────────

    #[test]
    fn render_todf_over_aggregate_with_qualified_agg_arg_renames_positionally() {
        let _g = tap_guard();
        // `SELECT b, count(*) AS n FROM (SELECT e.dept_id, count(d.dept_id)
        //   FROM emp e LEFT OUTER JOIN dept d ON e.dept_id = d.dept_id
        //   GROUP BY e.dept_id) AS t (a, b) GROUP BY b` (tbl-013).
        //
        // The inner aggregate's second output column is UNALIASED and
        // computed over a QUALIFIED arg: τ's Spark-`toPrettySQL`-parity
        // tracked name for it is `count(dept_id)` (qualifier stripped — this
        // matches live Spark 4.1.1's own `df.columns`), but DuckDB's own
        // default name for that same unaliased expression KEEPS the
        // qualifier (`count(d.dept_id)`). A by-name `"count(dept_id)" AS b`
        // reference (the old, broken rewiring) binds to a column DuckDB does
        // not have. The fix renames the child positionally instead.
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Left,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let inner_agg = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(join),
            grouping: vec![qcol("e", "dept_id")],
            aggregates: vec![
                qcol("e", "dept_id"),
                fexpr("count", vec![qcol("d", "dept_id")]),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let todf = CommonAst::new(CommonOp::ToDf {
            input: Box::new(inner_agg),
            column_names: vec!["a".to_owned(), "b".to_owned()],
        });
        let aliased = CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(todf),
            alias: "t".to_owned(),
        });
        let outer_agg = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(aliased),
            grouping: vec![ucol("b")],
            aggregates: vec![
                ucol("b"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr(
                        "count",
                        vec![Expression::Star(StarExpression { qualifier: None })],
                    )),
                    alias: "n".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept(&outer_agg);
        let typed = analyze(outer_agg, &bt).expect("analyze tbl-013 shape");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // The rename must reference the wrapped child POSITIONALLY (DuckDB's
        // native derived-table column-alias-list), never BY NAME through the
        // qualifier-stripped tracked name.
        assert!(
            sql.contains("AS __td_wcr(a, b)") || sql.contains("AS __td_wcr(\"a\", \"b\")"),
            "expected a positional derived-table column-alias-list rename; got: {sql}"
        );
        // N8: the inner aggregate's second entry is itself unaliased in the
        // SOURCE plan, so the analyzer now wraps it as
        // `Alias(count(d.dept_id), "count(dept_id)")` — the tracked name is
        // thus explicitly declared on the child (`... AS "count(dept_id)"`),
        // not merely implied by DuckDB's own (qualifier-keeping) naming. The
        // OUTER reference must still be positional (`__td_wcr(a, b)`,
        // asserted above), never a BY-NAME reference to that tracked name —
        // assert the outer GROUP BY binds through the wrap alias `b`, not
        // through a bare `"count(dept_id)"` identifier.
        assert!(
            !sql.contains("GROUP BY \"count(dept_id)\""),
            "outer reference must bind through the positional wrap alias `b`, \
             not by the child's tracked name; got: {sql}"
        );
    }

    #[test]
    fn render_project_over_nested_semi_join_side_breaks_chain_flatten() {
        let _g = tap_guard();
        // SEMI/ANTI-break regression pin: an inner SEMI JOIN, as the left
        // side of an outer join, must not flatten into the outer join's
        // chained FROM (CLAUDE.md gotcha 4) — `join_chain_flattenable`
        // rejects LeftSemi/LeftAnti, so it keeps its own generic
        // `render_join` wrap (correct `SEMI JOIN`, never `LEFT SEMI JOIN`).
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::LeftSemi,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("d", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(aliased_scan("emp2", "m")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("m", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(outer_join),
            projections: vec![qcol("e", "name")],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze semi-join-chain");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("SEMI JOIN"), "got: {sql}");
        assert!(!sql.contains("LEFT SEMI JOIN"), "got: {sql}");
        assert!(sql.contains("__td_jl"), "got: {sql}");
        assert!(sql.contains("emp2 AS m ON "), "got: {sql}");
    }

    // ── F7 round 2: duplicate `__td_jr` join-side alias collision ───────────

    #[test]
    fn contract_collision_wraps_left_keeps_right_name() {
        let _g = tap_guard();
        // join-022 unit mirror, COLLAPSED under ADR-023 Phase 2. `inner =
        // emp.join(emp2, emp.dept_id == emp2.dept_id)`: the plan_id-tagged
        // condition never force-wraps the inner join's sides (the demand-flag
        // machinery is retired). Both `emp` and `emp2` inline bare, and
        // `requalify_join_condition` rewrites the condition positionally to
        // `emp.dept_id` / `emp2.dept_id` — no buried inner `__td_jr` exists
        // anymore. The inner join is a plain-ON join, so it inlines as the
        // OUTER left too (F5 chain flatten), producing a natural 3-way FROM
        // chain. `d3 = dept.select("dept_id", "dept_name")` is the outer
        // RIGHT; it is a bare `Project` (not a pure-FROM item), so
        // `build_join_side`'s ladder falls through to its `AS __td_jr` wrap
        // regardless of any ancestor demand. With no buried inner `__td_jr`
        // left to collide with, there is nothing for the duplicate-alias
        // guard to do here.
        //
        // Phase 3b delta (D2): the ancestor `Filter`'s plan_id=3 reference
        // now resolves bare+ordinal (no synthetic `__td_jr` qualifier
        // stamped at all). `d3` is a bare `Project` with no user alias, so
        // the analyzer's own `RelScope` has no aliases entry covering its
        // span — `FromScope::unique_binding_alias` can't find a covering
        // alias for the merge-path rewrite, and the outer `Filter` falls to
        // the wrap path: `(…) AS __td_sub(…)` reprojects the 9-field merged
        // schema (uniquified — `dept_id` collides 3-way), and the filter
        // binds against the uniquified name for `d3.dept_id`
        // (`dept_id_2`). Same DATA as before — only the SQL surface changed.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(1),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(2),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let d3 = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![ucol("dept_id"), ucol("dept_name")],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(d3),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(ucol("dept_name")),
                right: Box::new(str_lit("Data")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![3],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(outer_join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(3),
                    },
                )),
                right: Box::new(int_lit(20)),
            }),
        });
        let bt = base_types_emp_dept_emp2(&filter);
        let typed = analyze(filter, &bt).expect("analyze join-022 shape");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // Post-Phase-3b (D2 behavior delta — see the comment above): the
        // inner join still fully inlines and chain-flattens into the outer
        // FROM, and `d3` still wraps `AS __td_jr` (unchanged from Phase 2),
        // but the ancestor filter no longer merges — it wraps the whole
        // chain under a reprojected `__td_sub` and binds against the
        // uniquified `dept_id_2` instead of a synthetic-qualified ref.
        assert!(
            sql.contains(
                "SELECT * FROM emp INNER JOIN emp2 ON (emp.dept_id) = (emp2.dept_id) \
                 INNER JOIN (SELECT dept_id, dept_name FROM dept) AS __td_jr ON \
                 (dept_name) = ('Data')"
            ),
            "the inner join must fully inline and chain-flatten into the outer \
             FROM, and the outer right (a bare Project, never inlineable) must \
             keep its __td_jr wrap; got: {sql}"
        );
        assert!(
            sql.contains(
                "AS __td_sub(id, name, dept_id, salary, id_1, dept_id_1, country, \
                 dept_id_2, dept_name)"
            ),
            "the ancestor filter's plan_id ref has no covering analyzer alias for \
             d3 (a bare Project, no user alias) so the merge-path rewrite can't \
             bind it — the filter falls back to the reprojected wrap; got: {sql}"
        );
        assert!(
            sql.ends_with("WHERE (dept_id_2) = (20)"),
            "outer filter must bind against the uniquified name for d3.dept_id; \
             got: {sql}"
        );
    }

    #[test]
    fn free_collision_renames_right_wrap() {
        let _g = tap_guard();
        // Same shape as `contract_collision_wraps_left_keeps_right_name` but
        // WITHOUT the ancestor filter. Post-collapse (ADR-023 Phase 2): the
        // inner join's own condition is never a wrap demand, so `emp`/`emp2`
        // inline bare with no buried `__td_jr` at all. The
        // "free" collision this test used to exercise — the inner's buried
        // `__td_jr` colliding with the outer's default `__td_jr` for
        // `d3` — no longer arises, because the buried inner alias it used
        // to collide with is gone. `d3` (a `Project`, not a bare scan) is
        // still wrapped as a derived subquery, but now keeps the plain
        // default alias `__td_jr` unrenamed — there is nothing left to
        // collide with.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(1),
                    },
                )),
                right: Box::new(Expression::UnresolvedColumn(
                    crate::transpiler_v2::expression::UnresolvedColumn {
                        name: "dept_id".to_owned(),
                        qualifier: None,
                        plan_id: Some(2),
                    },
                )),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let d3 = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![ucol("dept_id"), ucol("dept_name")],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(d3),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(ucol("dept_name")),
                right: Box::new(str_lit("Data")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&outer_join);
        let typed = analyze(outer_join, &bt).expect("analyze free-collision shape");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.starts_with(
                "SELECT * FROM emp INNER JOIN emp2 ON (emp.dept_id) = (emp2.dept_id) \
                 INNER JOIN "
            ),
            "the inner join must fully inline (no buried __td_jl/__td_jr — its \
             own condition is no longer a demand) and chain-flatten into the \
             outer FROM; got: {sql}"
        );
        assert!(
            sql.contains(
                "(SELECT dept_id, dept_name FROM dept) AS __td_jr ON (dept_name) = ('Data')"
            ),
            "d3 must still wrap as a derived subquery under its plain default \
             alias; got: {sql}"
        );
        assert_eq!(
            sql.matches("__td_jr").count(),
            1,
            "no collision remains post-collapse (the buried inner __td_jr is \
             gone), so no rename is needed; got: {sql}"
        );
    }

    // ── Phase 3b: merge-path fusion (requalify_visible) + wrap-path ordinal
    // arm (reproject_qualifiers) ───────────────────────────────────────────

    /// A bare plan_id-tagged column reference (the shape `resolve_column`'s
    /// plan_id arm always produces post-Phase-3b).
    fn pidcol(name: &str, plan_id: i64) -> Expression {
        Expression::UnresolvedColumn(crate::transpiler_v2::expression::UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: Some(plan_id),
        })
    }

    /// Shared shape for the D1 merge tests: `emp JOIN emp2 ON emp.dept_id =
    /// emp2.dept_id`, both sides bare (no user alias) — `emp`/`emp2` are
    /// themselves each an unambiguous, uniquely-exposed covering alias in
    /// `RelScope`, so an ancestor's bare duplicate-name ordinal ref merges
    /// through `requalify_visible` instead of forcing a wrap.
    fn emp_join_emp2() -> CommonAst {
        CommonAst::new(CommonOp::Join {
            left: Box::new(scan("emp")),
            right: Box::new(scan("emp2")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 1)),
                right: Box::new(pidcol("dept_id", 2)),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        })
    }

    #[test]
    fn d1_ancestor_filter_merges_with_bare_ordinal_bound_to_covering_alias() {
        let _g = tap_guard();
        // D1: an ancestor Filter referencing emp2's OWN plan_id lands on a
        // bare `dept_id` ordinal duplicated in the join's merged schema
        // (`emp.dept_id` / `emp2.dept_id`); `emp2` is the unique, uniquely-
        // exposed covering alias for that ordinal, so `requalify_visible`
        // rewrites it to `emp2.dept_id` and the filter merges into the join
        // — no `__td_jl`/`__td_jr`/`__td_sub` wrap at all.
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(emp_join_emp2()),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 2)),
                right: Box::new(int_lit(20)),
            }),
        });
        let bt = base_types_emp_dept_emp2(&filter);
        let typed = analyze(filter, &bt).expect("analyze d1 filter merge");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT * FROM emp INNER JOIN emp2 ON (emp.dept_id) = (emp2.dept_id) \
             WHERE (emp2.dept_id) = (20)"
        );
        assert!(
            !sql.contains("__td_"),
            "no wrap of any kind expected; got: {sql}"
        );
    }

    #[test]
    fn d1_ancestor_project_merges_with_bare_ordinals_bound_to_covering_aliases() {
        let _g = tap_guard();
        // D1: an ancestor Project referencing emp's `id` (duplicated with
        // emp2's `id`) and emp2's `dept_id` (duplicated with emp's) both
        // merge, each rewritten to its own unique covering alias.
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_join_emp2()),
            projections: vec![pidcol("id", 1), pidcol("dept_id", 2)],
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze d1 project merge");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT emp.id, emp2.dept_id FROM emp INNER JOIN emp2 ON \
             (emp.dept_id) = (emp2.dept_id)"
        );
        assert!(
            !sql.contains("__td_"),
            "no wrap of any kind expected; got: {sql}"
        );
    }

    #[test]
    fn d1_ancestor_aggregate_merges_grouping_key_bound_to_covering_alias() {
        let _g = tap_guard();
        // D1: an ancestor Aggregate's GROUP BY key is a bare duplicate-name
        // ordinal (emp's `dept_id`) that merges through the fused
        // requalify_visible rewrite, same as filter/project.
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(emp_join_emp2()),
            grouping: vec![pidcol("dept_id", 1)],
            // N7: `aggregates` IS the complete output list by construction —
            // the grouping key is folded in ahead of the aggregate call.
            aggregates: vec![
                pidcol("dept_id", 1),
                fexpr("max", vec![pidcol("salary", 1)]),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze d1 aggregate merge");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("GROUP BY emp.dept_id"), "got: {sql}");
        assert!(
            !sql.contains("__td_"),
            "no wrap of any kind expected; got: {sql}"
        );
    }

    #[test]
    fn d1_ancestor_sort_merges_key_bound_to_covering_alias() {
        let _g = tap_guard();
        // D1: an ancestor Sort's ORDER BY key is a bare duplicate-name
        // ordinal (emp2's `dept_id`) over a select-free (pure-FROM) block —
        // merges via `requalify_visible`, same as the other three builders.
        let plan = CommonAst::new(CommonOp::Sort {
            input: Box::new(emp_join_emp2()),
            order: vec![asc_key(pidcol("dept_id", 2))],
            limit: None,
            offset: None,
        });
        let bt = base_types_emp_dept_emp2(&plan);
        let typed = analyze(plan, &bt).expect("analyze d1 sort merge");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("ORDER BY emp2.dept_id"), "got: {sql}");
        assert!(
            !sql.contains("__td_"),
            "no wrap of any kind expected; got: {sql}"
        );
    }

    #[test]
    fn sort_bare_dup_name_ordinal_key_wraps_and_uniquifies() {
        let _g = tap_guard();
        // Family B (tpcds-q039a/q039b/q064 shape): the SELECT list projects
        // the SAME output name (`dept_id`) from BOTH join sides — a genuine
        // duplicate, like the self-join CTE's two `w_warehouse_sk`/`cnt`
        // columns. The ORDER BY key sources from `emp2`'s `dept_id`
        // specifically; over the Project (whose own `RelScope` binds no
        // local aliases), `resolve_column`'s tier-(f) source_quals arm
        // resolves it by the target attribute's identity and DROPS the
        // qualifier, landing at emission as a bare `ColumnReference`
        // carrying that attribute's `expr_id` — and that id's slot (1) IS
        // duplicated in the Project's own `resolved_schema`. Before this
        // fix, `build_sort`'s `keys_bind`
        // predicate admitted this bare key and merged it straight into the
        // occupied (duplicate-name) SELECT block, emitting an ambiguous bare
        // `ORDER BY dept_id` DuckDB cannot bind (two same-named SELECT
        // items). The fix routes it through the existing wrap+uniquify path
        // instead, so the ORDER BY binds against the uniquified alias.
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_join_emp2()),
            projections: vec![pidcol("dept_id", 1), pidcol("dept_id", 2)],
        });
        let sort = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(qcol("emp2", "dept_id"))],
            limit: None,
            offset: None,
        });
        let bt = base_types_emp_dept_emp2(&sort);
        let typed = analyze(sort, &bt).expect("analyze dup-name-project-then-sort");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT * FROM (SELECT emp.dept_id, emp2.dept_id FROM emp INNER JOIN emp2 ON \
             (emp.dept_id) = (emp2.dept_id)) AS __td_sub(dept_id, dept_id_1) ORDER BY \
             dept_id_1 ASC NULLS FIRST"
        );
        assert!(
            sql.contains("__td_sub"),
            "a bare dup-name ordinal ORDER BY key over an occupied SELECT list \
             must reject the merge and fall back to the reprojected wrap; got: {sql}"
        );
        assert!(
            !sql.contains("ORDER BY dept_id ASC") && !sql.contains("ORDER BY dept_id\n"),
            "must never emit a bare ORDER BY key that binds ambiguously against \
             duplicate-named SELECT items; got: {sql}"
        );
    }

    #[test]
    fn sort_bare_unique_name_ordinal_key_still_merges() {
        let _g = tap_guard();
        // Guard against over-wrapping: the SAME shape as the test above, but
        // the Project selects only ONE occurrence of each name (`id` from
        // `emp`, `dept_id` from `emp2`) so the Project's own output schema
        // has no duplicate name. The ORDER BY key (`emp2.dept_id`) still
        // resolves through the same tier-(f) source_quals arm to a bare
        // ordinal ref, but `bare_dup_slot` returns `None` (the name is
        // unique in the schema) — `keys_bind` must stay `true` and the key
        // must still merge into the occupied SELECT block, with NO wrap.
        let project = CommonAst::new(CommonOp::Project {
            input: Box::new(emp_join_emp2()),
            projections: vec![pidcol("id", 1), pidcol("dept_id", 2)],
        });
        let sort = CommonAst::new(CommonOp::Sort {
            input: Box::new(project),
            order: vec![asc_key(qcol("emp2", "dept_id"))],
            limit: None,
            offset: None,
        });
        let bt = base_types_emp_dept_emp2(&sort);
        let typed = analyze(sort, &bt).expect("analyze unique-name-project-then-sort");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql,
            "SELECT emp.id, emp2.dept_id FROM emp INNER JOIN emp2 ON (emp.dept_id) = \
             (emp2.dept_id) ORDER BY dept_id ASC NULLS FIRST"
        );
        assert!(
            !sql.contains("__td_"),
            "a unique-name ordinal ORDER BY key must still take the merge \
             shortcut, no wrap; got: {sql}"
        );
    }

    #[test]
    fn d7_homonym_alias_rejects_merge_and_wrap_binds_uniquified_name() {
        let _g = tap_guard();
        // D7 / rule (i): `emp` and `emp2` are BOTH user-aliased "m"
        // (`AliasedRelation` drops the table name, so `m` is each side's
        // ONLY analyzer-scope alias entry) — a homonym-alias hazard the
        // per-ref rule's condition (i) must catch (two distinct `RelScope`
        // aliases entries sharing the same name), even though the ordinal
        // itself unambiguously falls within ONE side's span. The ancestor
        // filter's merge attempt must be REJECTED (not silently bind through
        // the wrong "m"), falling back to the reprojected wrap, where the
        // duplicate-name output binds the filter positionally instead.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "m")),
            right: Box::new(aliased_scan("dept", "d")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 1)),
                right: Box::new(pidcol("dept_id", 2)),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(inner_join),
            right: Box::new(aliased_scan("emp2", "m")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 2)),
                right: Box::new(pidcol("dept_id", 3)),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![3],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(outer_join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 3)),
                right: Box::new(int_lit(20)),
            }),
        });
        let bt = base_types_emp_dept_emp2(&filter);
        let typed = analyze(filter, &bt).expect("analyze d7 homonym shape");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("__td_sub"),
            "the homonym alias hazard must reject the merge, falling back to \
             the reprojected wrap; got: {sql}"
        );
        assert!(
            sql.ends_with("WHERE (dept_id_2) = (20)"),
            "the filter must bind through the wrap's uniquified name for \
             emp2.dept_id, positionally, never through either ambiguous `m`; \
             got: {sql}"
        );
    }

    #[test]
    fn internally_duplicate_span_rejects_merge_rule_iii() {
        let _g = tap_guard();
        // Rule (iii): `dup_tbl` has TWO fields both named `dept_id` within
        // its OWN span — even though it is the sole, uniquely-exposed
        // covering alias for the ancestor's ordinal (rules (i)/(ii) both
        // hold), an internally-duplicate span means "the" position within
        // it is not well-defined by name alone, so the per-ref rule must
        // still reject the merge (leftmost-binding the wrong physical
        // column would otherwise be silent and undetectable).
        let dup_schema = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("dept_id", DataType::Integer),
        ]);
        let other_schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let bt = BaseTypes::build_from_plan(
            &CommonAst::new(CommonOp::Join {
                left: Box::new(scan("dup_tbl")),
                right: Box::new(scan("other")),
                join_type: JoinType::Inner,
                condition: Some(Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(pidcol("id", 1)),
                    right: Box::new(pidcol("id", 2)),
                })),
                using_columns: vec![],
                natural: false,
                lateral: false,
                left_plan_ids: vec![1],
                right_plan_ids: vec![2],
            }),
            |name| match name {
                "dup_tbl" => Some(dup_schema.clone()),
                "other" => Some(other_schema.clone()),
                _ => None,
            },
        );
        let join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("dup_tbl")),
            right: Box::new(scan("other")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("id", 1)),
                right: Box::new(pidcol("id", 2)),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![1],
            right_plan_ids: vec![2],
        });
        let filter = CommonAst::new(CommonOp::Filter {
            input: Box::new(join),
            condition: Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(pidcol("dept_id", 1)),
                right: Box::new(int_lit(5)),
            }),
        });
        let typed = analyze(filter, &bt).expect("analyze internally-dup-span shape");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("__td_sub"),
            "an internally-duplicate covering span must reject the merge and \
             fall back to the reprojected wrap; got: {sql}"
        );
        assert!(
            !sql.contains("dup_tbl.dept_id"),
            "must never bind qualified through a span where the name isn't \
             unique — that would silently pick the leftmost occurrence; got: \
             {sql}"
        );
    }

    #[test]
    fn reproject_qualifiers_ordinal_arm_binds_bare_dup_by_id() {
        let _g = tap_guard();
        // Direct unit pin for the `reproject_qualifiers` bare-dup else-arm
        // (N10-lite): a bare duplicate-name ref rewrites to `uniquified[k]`
        // where `k` is found by the reference's `expr_id` — fixtures stamp
        // REAL ids read off the fixture schema (not `expr_id: None`). A bare
        // UNIQUE-name ref and a bare ref with `expr_id: None` (deferred
        // resolution) are both left untouched regardless.
        let schema = Schema::minted(StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("name", DataType::String),
        ]));
        let id_col_id = schema.fields[0].expr_id;
        let dept_id_second = schema.fields[2].expr_id;
        let name_col_id = schema.fields[3].expr_id;
        let uniquified = vec![
            "id".to_owned(),
            "dept_id".to_owned(),
            "dept_id_1".to_owned(),
            "name".to_owned(),
        ];
        let dup = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Integer),
            nullable: Some(true),
            expr_id: Some(dept_id_second),
        });
        let unique_no_rewrite = Expression::ColumnReference(ColumnReference {
            name: "id".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Long),
            nullable: Some(false),
            expr_id: Some(id_col_id),
        });
        let deferred_no_ordinal = Expression::ColumnReference(ColumnReference {
            name: "name".to_owned(),
            qualifier: None,
            data_type: Some(DataType::String),
            nullable: Some(true),
            expr_id: Some(name_col_id),
        });
        let input = TypedAst::new(
            TypedOp::TableScan {
                table: "dup_tbl".to_owned(),
                alias: None,
            },
            schema.clone(),
        );
        let rewritten_dup = reproject_qualifiers(&dup, &input, &uniquified);
        match rewritten_dup {
            Expression::ColumnReference(c) => {
                assert_eq!(c.qualifier, None);
                assert_eq!(c.name, "dept_id_1");
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
        let rewritten_unique = reproject_qualifiers(&unique_no_rewrite, &input, &uniquified);
        match rewritten_unique {
            Expression::ColumnReference(c) => {
                assert_eq!(c.qualifier, None);
                assert_eq!(c.name, "id", "a unique-name ref is untouched");
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
        let rewritten_deferred = reproject_qualifiers(&deferred_no_ordinal, &input, &uniquified);
        match rewritten_deferred {
            Expression::ColumnReference(c) => {
                assert_eq!(c.qualifier, None);
                assert_eq!(c.name, "name", "a bare unique-name ref is untouched");
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn reproject_qualifiers_same_id_two_slots_binds_first_occurrence() {
        let _g = tap_guard();
        // agg-026 shape: a grouping key restated in the aggregate list folds
        // into TWO output slots that are the SAME projected-through column
        // — identical `expr_id` at both positions (a clone of one
        // attribute, not two fresh mints). N10-lite's id-keyed lookup uses
        // `Iterator::position`'s natural first-match semantics: the same id
        // in two slots binds the FIRST occurrence — value-correct because
        // one id means one per-row value within a single schema, regardless
        // of which of its slots is addressed.
        let dup_attr = Attribute::minted("dept_id", DataType::Integer, true);
        let schema = Schema::new(vec![
            Attribute::minted("id", DataType::Long, false),
            dup_attr.clone(),
            dup_attr.clone(),
            Attribute::minted("total_salary", DataType::Long, true),
        ]);
        let shared_id = schema.fields[1].expr_id;
        assert_eq!(
            schema.fields[2].expr_id, shared_id,
            "fixture precondition: both duplicate slots must share one id"
        );
        let uniquified = vec![
            "id".to_owned(),
            "dept_id".to_owned(),
            "dept_id_1".to_owned(),
            "total_salary".to_owned(),
        ];
        let bare_ref = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Integer),
            nullable: Some(true),
            expr_id: Some(shared_id),
        });
        let input = TypedAst::new(
            TypedOp::TableScan {
                table: "agg026".to_owned(),
                alias: None,
            },
            schema.clone(),
        );
        let rewritten = reproject_qualifiers(&bare_ref, &input, &uniquified);
        match rewritten {
            Expression::ColumnReference(c) => {
                assert_eq!(c.qualifier, None);
                assert_eq!(
                    c.name, "dept_id",
                    "the same id at two slots must bind the FIRST occurrence"
                );
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn reproject_qualifiers_id_absent_from_schema_stays_untouched() {
        let _g = tap_guard();
        // N10-lite: an `expr_id` that names no field in the boundary
        // schema (e.g. stamped against a DIFFERENT schema than the one
        // reached here) is left completely untouched — no silent
        // wrong-column rewrite; the unresolved reference surfaces as a
        // loud DuckDB binder error instead.
        //
        // D2: this is also the exact shape a correlated outer reference now
        // takes at emission — tier-(g) stamps it with the OUTER plan's
        // attribute id (`resolve_in_outer`, `analyzer.rs`), which by
        // construction never appears in THIS (local/inner) schema. The
        // `foreign_schema`/`foreign_id` fixture below models that: an id
        // minted from a schema this function never sees, standing in for
        // the outer plan's schema.
        let schema = Schema::minted(StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("name", DataType::String),
        ]));
        let foreign_schema = Schema::minted(StructType::new(vec![StructField::nullable(
            "dept_id",
            DataType::Integer,
        )]));
        let foreign_id = foreign_schema.fields[0].expr_id;
        let uniquified = vec![
            "id".to_owned(),
            "dept_id".to_owned(),
            "dept_id_1".to_owned(),
            "name".to_owned(),
        ];
        let orphaned = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Integer),
            nullable: Some(true),
            expr_id: Some(foreign_id),
        });
        let input = TypedAst::new(
            TypedOp::TableScan {
                table: "dup_tbl".to_owned(),
                alias: None,
            },
            schema.clone(),
        );
        let rewritten = reproject_qualifiers(&orphaned, &input, &uniquified);
        match rewritten {
            Expression::ColumnReference(c) => {
                assert_eq!(c.qualifier, None);
                assert_eq!(
                    c.name, "dept_id",
                    "an id absent from the schema must stay untouched"
                );
            }
            other => panic!("expected ColumnReference, got {other:?}"),
        }
    }

    #[test]
    fn plain_join_over_dup_name_side_renders_star() {
        let _g = tap_guard();
        // join-022 round-1 corruption pin: a single-alias side whose OWN
        // resolved_schema has duplicate names (`id`, `dept_id` both appear
        // twice in `emp JOIN emp2`) sits as the RIGHT side of a plain-ON
        // outer join — the right side never inlines, so it always wraps as
        // ONE alias, `__td_jr`, over the duplicate-name schema. Change 2
        // (non-USING joins never build a default slot list) must render bare
        // `*` here, never a name-qualified list that would double-bind
        // `__td_jr.id` / `__td_jr.dept_id`.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("emp2", "e2")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("e2", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("dept")),
            right: Box::new(inner_join),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("dept", "dept_id")),
                right: Box::new(qcol("e", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&outer_join);
        let typed = analyze(outer_join, &bt).expect("analyze plain-join-over-dup-name-side");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.starts_with("SELECT * FROM"),
            "non-USING join must render bare `*`, never a hoisted slot list; got: {sql}"
        );
        assert!(
            !sql.contains("__td_jr.id") && !sql.contains("__td_jr.dept_id"),
            "must not double-bind the duplicate-name side's fields; got: {sql}"
        );
    }

    #[test]
    fn using_join_over_dup_name_side_is_boundary_error() {
        let _g = tap_guard();
        // Change 3: a USING(dept_id) parent's RIGHT side is NEVER inlined
        // (`may_inline_nested_join` is hardcoded `false` for the right in
        // `build_join_side`), so a nested `emp JOIN emp2` there always wraps
        // as ONE alias (`__td_jr`) regardless of any analyzer contract —
        // `side_slot_quals`'s single-alias fast path then qualifies EVERY
        // field (including both `id`s) with that same alias. Neither a
        // per-field-qualified slot list (double-binds `__td_jr.id`) nor bare
        // `*` (breaks USING's key-first output order) is safe here — an
        // honest Thunderduck-boundary error is the correct interim.
        let inner_join = CommonAst::new(CommonOp::Join {
            left: Box::new(aliased_scan("emp", "e")),
            right: Box::new(aliased_scan("emp2", "e2")),
            join_type: JoinType::Inner,
            condition: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(qcol("e", "dept_id")),
                right: Box::new(qcol("e2", "dept_id")),
            })),
            using_columns: vec![],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let outer_join = CommonAst::new(CommonOp::Join {
            left: Box::new(scan("dept")),
            right: Box::new(inner_join),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec!["dept_id".to_owned()],
            natural: false,
            lateral: false,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let bt = base_types_emp_dept_emp2(&outer_join);
        let typed = analyze(outer_join, &bt).expect("analyze USING-over-dup-name-side");
        let err = dispatch_op(&typed.op, &typed.resolved_schema).expect_err("must bail");
        expect_unsupported(
            err,
            UnsupportedKind::Op,
            "Join",
            &["duplicate column names"],
        );
    }

    fn ucol(name: &str) -> Expression {
        Expression::UnresolvedColumn(crate::transpiler_v2::expression::UnresolvedColumn {
            name: name.to_owned(),
            qualifier: None,
            plan_id: None,
        })
    }

    fn count_star_gt_one() -> Expression {
        Expression::Binary(BinaryExpression {
            op: BinaryOp::Gt,
            left: Box::new(fexpr(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )),
            right: Box::new(int_lit(1)),
        })
    }

    #[test]
    fn render_aggregate_having_emits_group_by_having_not_outer_where() {
        let _g = tap_guard();
        // `SELECT dept_id, count(*) FROM emp GROUP BY dept_id HAVING count(*) > 1`
        // — the HAVING predicate must fold into the aggregate SELECT as
        // `GROUP BY … HAVING`, never an outer `WHERE` wrapper (which DuckDB
        // rejects for aggregate predicates).
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: Some(count_star_gt_one()),
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze aggregate with having");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("GROUP BY"), "expected GROUP BY, got: {sql}");
        assert!(sql.contains("HAVING"), "expected HAVING, got: {sql}");
        let group_pos = sql.find("GROUP BY").expect("GROUP BY present");
        let having_pos = sql.find("HAVING").expect("HAVING present");
        assert!(having_pos > group_pos, "HAVING must follow GROUP BY: {sql}");
        assert!(
            !sql.contains("__td_filter"),
            "no outer WHERE wrapper: {sql}"
        );
    }

    #[test]
    fn render_aggregate_grouping_expr_also_projected_no_prepended_slot() {
        let _g = tap_guard();
        // agg-007 shape: `SELECT dept_id >= 40 AS senior, avg(salary) AS s
        // FROM emp GROUP BY dept_id >= 40`. The grouping key structurally
        // equals the alias-stripped first aggregate → already folded → the
        // SELECT list must have exactly the 2 projected slots, with NO spurious
        // leading `(dept_id >= 40)` slot.
        let senior_expr = || {
            Expression::Binary(BinaryExpression {
                op: BinaryOp::GtEq,
                left: Box::new(ucol("dept_id")),
                right: Box::new(int_lit(40)),
            })
        };
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![senior_expr()],
            aggregates: vec![
                Expression::Alias(AliasExpression {
                    expr: Box::new(senior_expr()),
                    alias: "senior".to_owned(),
                }),
                Expression::Alias(AliasExpression {
                    expr: Box::new(fexpr("avg", vec![ucol("salary")])),
                    alias: "s".to_owned(),
                }),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze folded aggregate");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // The select list (between SELECT and FROM) must be the 2 projected
        // slots only: the grouping expr appears once in `senior` and once in
        // GROUP BY = twice total. A spurious prepended slot would make it 3.
        assert_eq!(
            sql.matches("(dept_id) >= (40)").count(),
            2,
            "expected the grouping expr exactly twice (senior slot + GROUP BY), got: {sql}"
        );
        assert_eq!(
            sql.matches(" AS senior").count(),
            1,
            "senior alias appears exactly once: {sql}"
        );
    }

    /// N7 (formerly the Design-035 `Folded`/`Grouped` pair, now collapsed to
    /// one test): `CommonOp::Aggregate` built directly (as the SQL front-end
    /// does) never auto-prepends the grouping key — `aggregates` IS the
    /// complete SELECT list by construction. `dept_id` appears once, from
    /// `GROUP BY` only. The DataFrame-shaped prepend behavior lives in the
    /// dedicated `grouped_aggregate` constructor, exercised separately (see
    /// the `grouped_aggregate_*` tests in `ast.rs`).
    #[test]
    fn aggregate_does_not_prepend_grouping_key() {
        let _g = tap_guard();
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![fexpr(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupBy,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze aggregate");
        assert_eq!(
            typed.resolved_schema.fields.len(),
            1,
            "must not prepend the grouping key to the resolved schema"
        );
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(
            sql.matches("dept_id").count(),
            1,
            "dept_id must appear exactly once (GROUP BY only), got: {sql}"
        );
    }

    #[test]
    fn render_aggregate_all_empty_grouping_sets_emits_group_by() {
        let _g = tap_guard();
        // `GROUP BY GROUPING SETS ((), ())`: the flat grouping list is empty
        // (no columns referenced) but `grouping_sets` holds two empty sets.
        // Each empty set is a distinct grand-total group, so Spark returns one
        // row per set. The GROUP BY must NOT be dropped (which would collapse
        // both sets into a single grand-total row — a silent wrong row-count).
        // DuckDB accepts `GROUP BY GROUPING SETS ((), ())`.
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![],
            aggregates: vec![fexpr(
                "count",
                vec![Expression::Star(StarExpression { qualifier: None })],
            )],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupingSets,
            grouping_sets: vec![vec![], vec![]],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze all-empty grouping sets");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("GROUP BY GROUPING SETS ((), ())"),
            "expected GROUP BY GROUPING SETS ((), ()), got: {sql}"
        );
    }

    #[test]
    fn render_aggregate_having_composes_with_rollup() {
        let _g = tap_guard();
        // ROLLUP grouping + HAVING → `GROUP BY ROLLUP(dept_id) HAVING …`.
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::Rollup,
            grouping_sets: vec![],
            having: Some(count_star_gt_one()),
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze rollup aggregate with having");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(sql.contains("ROLLUP("), "expected ROLLUP, got: {sql}");
        assert!(sql.contains("HAVING"), "expected HAVING, got: {sql}");
        let rollup_pos = sql.find("ROLLUP(").expect("ROLLUP present");
        let having_pos = sql.find("HAVING").expect("HAVING present");
        assert!(
            having_pos > rollup_pos,
            "HAVING must follow ROLLUP group clause: {sql}"
        );
    }

    #[test]
    fn render_aggregate_having_grouping_id_spliced_with_rollup() {
        let _g = tap_guard();
        // gx-012: `... GROUP BY ROLLUP(dept_id) HAVING grouping_id() = 0`.
        // The HAVING predicate carries a no-arg `grouping_id()` which DuckDB
        // cannot parse; emission must splice the ambient grouping column into
        // the call, mirroring the SELECT-slot rewrite.
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::Rollup,
            grouping_sets: vec![],
            having: Some(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(grouping_id_call()),
                right: Box::new(int_lit(0)),
            })),
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze rollup aggregate with grouping_id having");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("grouping_id(dept_id)"),
            "expected spliced grouping_id(dept_id) in HAVING, got: {sql}"
        );
        assert!(
            !sql.contains("grouping_id()"),
            "bare zero-arg grouping_id() must not reach DuckDB, got: {sql}"
        );
    }

    #[test]
    fn render_aggregate_grouping_sets_emits_per_set_group_clause() {
        let _g = tap_guard();
        // GROUPING SETS ((dept_id, name), (dept_id), ()) → flat grouping
        // [dept_id, name] with per-set membership [[0, 1], [0], []].
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id"), ucol("name")],
            aggregates: vec![
                ucol("dept_id"),
                ucol("name"),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupingSets,
            grouping_sets: vec![vec![0, 1], vec![0], vec![]],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze grouping-sets aggregate");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("GROUPING SETS ((dept_id, name), (dept_id), ())"),
            "expected per-set GROUP BY clause, got: {sql}"
        );
    }

    #[test]
    fn render_aggregate_grouping_sets_empty_metadata_preserves_boundary() {
        let _g = tap_guard();
        // DataFrame `groupingSets` path leaves `grouping_sets` empty — emission
        // must surface the preserved Thunderduck-boundary error (ADR-022).
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::GroupingSets,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze grouping-sets aggregate");
        let err = dispatch_op(&typed.op, &typed.resolved_schema).unwrap_err();
        expect_unsupported(err, UnsupportedKind::Op, "Aggregate[GroupingSets]", &[]);
    }

    // ── rewrite_grouping_id widening (finding 6) ────────────────────────────
    // `rewrite_grouping_id` splices no-arg `grouping_id()`/`grouping()` calls
    // with the ambient grouping columns anywhere in an aggregate slot — not
    // just the 4 originally-hand-enumerated containers (FunctionCall args /
    // Alias / Cast / CaseWhen). These pin the widened `children_mut` walk
    // directly against the pure function (no analyze/dispatch needed).

    fn grouping_id_call() -> Expression {
        fexpr("grouping_id", vec![])
    }

    #[test]
    fn rewrite_grouping_id_bare_call_spliced() {
        let mut e = grouping_id_call();
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(e, fexpr("grouping_id", vec![ucol("dept_id")]));
    }

    #[test]
    fn rewrite_grouping_id_binary_witness_spliced() {
        // `grouping_id() + 1` — the corpus-witness shape (Finding 6): pre-fix
        // this fell through `other => other.clone()` and reached DuckDB as a
        // literal zero-arg `grouping_id()`, a parse error.
        let mut e = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(grouping_id_call()),
            right: Box::new(int_lit(1)),
        });
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(
            e,
            Expression::Binary(BinaryExpression {
                op: BinaryOp::Add,
                left: Box::new(fexpr("grouping_id", vec![ucol("dept_id")])),
                right: Box::new(int_lit(1)),
            })
        );
    }

    #[test]
    fn rewrite_grouping_id_unary_and_nested_binary_spliced() {
        // `NOT (grouping_id() = 0)` — covers a `Unary` hop wrapping a
        // `Binary` hop wrapping the call, in one tree.
        let mut e = Expression::Unary(UnaryExpression {
            op: UnaryOp::Not,
            operand: Box::new(Expression::Binary(BinaryExpression {
                op: BinaryOp::Eq,
                left: Box::new(grouping_id_call()),
                right: Box::new(int_lit(0)),
            })),
        });
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(
            e,
            Expression::Unary(UnaryExpression {
                op: UnaryOp::Not,
                operand: Box::new(Expression::Binary(BinaryExpression {
                    op: BinaryOp::Eq,
                    left: Box::new(fexpr("grouping_id", vec![ucol("dept_id")])),
                    right: Box::new(int_lit(0)),
                })),
            })
        );
    }

    #[test]
    fn rewrite_grouping_id_in_list_container_spliced() {
        // `grouping_id() IN (0, 3)` — a container beyond the 4 originally
        // hand-enumerated arms (FunctionCall/Alias/Cast/CaseWhen), pinning
        // that the widened walk now reaches siblings, not just the witness.
        let mut e = Expression::InList(InListExpression {
            expr: Box::new(grouping_id_call()),
            list: vec![int_lit(0), int_lit(3)],
            negated: false,
        });
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(
            e,
            Expression::InList(InListExpression {
                expr: Box::new(fexpr("grouping_id", vec![ucol("dept_id")])),
                list: vec![int_lit(0), int_lit(3)],
                negated: false,
            })
        );
    }

    #[test]
    fn rewrite_grouping_id_alias_cast_previously_covered_arms_unchanged() {
        // Pin the 2 originally hand-enumerated arms still work identically
        // post-widening: `CAST(grouping_id() AS BIGINT) AS gid`.
        let mut e = Expression::Alias(AliasExpression {
            expr: Box::new(Expression::Cast(CastExpression {
                expr: Box::new(grouping_id_call()),
                to_type: DataType::Long,
                try_cast: false,
                implicit: false,
            })),
            alias: "gid".to_owned(),
        });
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(
            e,
            Expression::Alias(AliasExpression {
                expr: Box::new(Expression::Cast(CastExpression {
                    expr: Box::new(fexpr("grouping_id", vec![ucol("dept_id")])),
                    to_type: DataType::Long,
                    try_cast: false,
                    implicit: false,
                })),
                alias: "gid".to_owned(),
            })
        );
    }

    #[test]
    fn rewrite_grouping_id_call_with_args_unchanged() {
        // `grouping_id(dept_id)` (already has args) must not be touched.
        let mut e = fexpr("grouping_id", vec![ucol("dept_id")]);
        let before = e.clone();
        rewrite_grouping_id(&mut e, &[ucol("dept_id")]);
        assert_eq!(e, before);
    }

    #[test]
    fn rewrite_grouping_id_empty_grouping_is_noop() {
        // No GROUP BY columns to splice → leave the bare call as-is (still a
        // pre-existing, separate, un-witnessed DuckDB error path; out of
        // this finding's scope — see design doc §5).
        let mut e = grouping_id_call();
        let before = e.clone();
        rewrite_grouping_id(&mut e, &[]);
        assert_eq!(e, before);
    }

    #[test]
    fn render_aggregate_op_binary_witness_grouping_id_plus_one() {
        let _g = tap_guard();
        // Regression example 1 (SQL front-end shape): `grouping_id() + 1` in
        // a ROLLUP aggregate. Pre-fix this reached DuckDB as a literal
        // zero-arg `grouping_id()` → `Parser Error: syntax error at or near
        // ")"`. Post-fix it must render `grouping_id(dept_id) + 1`.
        let plan = CommonAst::new(CommonOp::Aggregate {
            input: Box::new(scan("emp")),
            grouping: vec![ucol("dept_id")],
            aggregates: vec![
                ucol("dept_id"),
                Expression::Alias(AliasExpression {
                    expr: Box::new(Expression::Binary(BinaryExpression {
                        op: BinaryOp::Add,
                        left: Box::new(grouping_id_call()),
                        right: Box::new(int_lit(1)),
                    })),
                    alias: "gid1".to_owned(),
                }),
                fexpr(
                    "count",
                    vec![Expression::Star(StarExpression { qualifier: None })],
                ),
            ],
            grouping_kind: crate::transpiler_v2::ast::GroupingKind::Rollup,
            grouping_sets: vec![],
            having: None,
        });
        let bt = base_types_with_emp();
        let typed = analyze(plan, &bt).expect("analyze rollup aggregate with grouping_id() + 1");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("grouping_id(dept_id)"),
            "expected the splice to carry the grouping column, got: {sql}"
        );
        assert!(
            !sql.contains("grouping_id()"),
            "must not leave a zero-arg grouping_id() call, got: {sql}"
        );
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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
            input: Box::new(scan("emp")),
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

    // ── 12b. render_data_type — Struct with duplicate field names ────────

    /// arr-012 boundary hygiene: `render_data_type` on
    /// `Struct<tags, tags>` MUST dedup the substrate field names because
    /// DuckDB's `CAST(x AS STRUCT("tags" VARCHAR, "tags" VARCHAR))` raises
    /// `Binder Error: Duplicate STRUCT type argument name`. The τ analyzer
    /// still records the Spark-visible duplicates in `resolved_schema` — the
    /// dedup only affects the DuckDB-substrate SQL, and the outbound
    /// Arrow-schema stamp in `connect-server` uses the same convention so
    /// the round-trip re-materialises the duplicates on the client.
    #[test]
    fn render_data_type_struct_dedups_duplicate_field_names_arr012_shape() {
        use crate::types::StructField as CoreStructField;
        let dup_struct = DataType::Struct(StructType::new(vec![
            CoreStructField::nullable("tags", DataType::String),
            CoreStructField::nullable("tags", DataType::String),
        ]));
        let sql = render_data_type(&dup_struct);
        // Substrate must not contain duplicate `tags` field names — DuckDB
        // rejects `Binder Error: Duplicate STRUCT type argument name`.
        assert!(
            sql.contains("tags_0") && sql.contains("tags_1"),
            "expected dedup'd names `tags_0`,`tags_1`; got: {sql}",
        );
        assert!(
            sql.starts_with("STRUCT("),
            "expected STRUCT(...); got: {sql}"
        );
    }

    /// Companion: unique names must NOT be renamed — the dedup is a no-op
    /// when there are no collisions. Pins the false-positive contract.
    #[test]
    fn render_data_type_struct_preserves_unique_field_names() {
        use crate::types::StructField as CoreStructField;
        let uniq_struct = DataType::Struct(StructType::new(vec![
            CoreStructField::nullable("a", DataType::Long),
            CoreStructField::nullable("b", DataType::String),
        ]));
        let sql = render_data_type(&uniq_struct);
        assert!(
            sql.contains("a ") || sql.contains("\"a\""),
            "expected unique field `a` unchanged; got: {sql}",
        );
        assert!(
            !sql.contains("a_0") && !sql.contains("b_0"),
            "unique names must NOT be suffixed; got: {sql}",
        );
    }

    /// Nested case: `Array<Struct<tags, tags>>` — the arr-012 wire shape.
    /// The inner struct's field names must be dedup'd; the outer array
    /// wrapper is untouched.
    #[test]
    fn render_data_type_array_of_struct_dedups_inner_names() {
        use crate::types::StructField as CoreStructField;
        let inner = DataType::Struct(StructType::new(vec![
            CoreStructField::nullable("tags", DataType::String),
            CoreStructField::nullable("tags", DataType::String),
        ]));
        let arr = DataType::Array(Box::new(inner), true);
        let sql = render_data_type(&arr);
        assert!(sql.ends_with("[]"), "expected trailing `[]`; got: {sql}");
        assert!(
            sql.contains("tags_0") && sql.contains("tags_1"),
            "nested struct field names must dedup; got: {sql}",
        );
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

    // ── build_file_reader_sql (shared helper) ───────────────────────────

    #[test]
    fn build_file_reader_sql_parquet_single_path() {
        let sql = build_file_reader_sql(
            FileFormat::Parquet,
            &["/data/orders.parquet".to_owned()],
            &[],
        )
        .expect("build");
        assert_eq!(sql, "read_parquet('/data/orders.parquet')");
    }

    #[test]
    fn build_file_reader_sql_parquet_multi_path() {
        let sql = build_file_reader_sql(
            FileFormat::Parquet,
            &[
                "/data/part1.parquet".to_owned(),
                "/data/part2.parquet".to_owned(),
            ],
            &[],
        )
        .expect("build");
        assert_eq!(
            sql,
            "read_parquet(['/data/part1.parquet', '/data/part2.parquet'])"
        );
    }

    #[test]
    fn build_file_reader_sql_parquet_with_options() {
        let sql = build_file_reader_sql(
            FileFormat::Parquet,
            &["/data/orders.parquet".to_owned()],
            &[("hive_partitioning".to_owned(), "true".to_owned())],
        )
        .expect("build");
        assert_eq!(
            sql,
            "read_parquet('/data/orders.parquet', hive_partitioning='true')"
        );
    }

    #[test]
    fn build_file_reader_sql_csv_single_path() {
        let sql = build_file_reader_sql(FileFormat::Csv, &["/data/test.csv".to_owned()], &[])
            .expect("build");
        assert_eq!(sql, "read_csv('/data/test.csv')");
    }

    #[test]
    fn build_file_reader_sql_delta_single_path() {
        let sql =
            build_file_reader_sql(FileFormat::Delta, &["/tmp/x".to_owned()], &[]).expect("build");
        assert_eq!(sql, "delta_scan('/tmp/x')");
    }

    #[test]
    fn build_file_reader_sql_delta_multi_path_errors() {
        let result = build_file_reader_sql(
            FileFormat::Delta,
            &["/tmp/a".to_owned(), "/tmp/b".to_owned()],
            &[],
        );
        assert!(result.is_err());
    }

    #[test]
    fn render_file_scan_delta_emits_select_star_from_delta_scan() {
        let _g = tap_guard();
        let schema = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
        let ast = CommonAst::new(CommonOp::FileScan {
            format: FileFormat::Delta,
            paths: vec!["/tmp/x".to_owned()],
            schema: Some(schema),
            options: vec![],
        });
        let typed = analyze(ast, &BaseTypes::empty()).expect("analyze");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert_eq!(sql, "SELECT * FROM delta_scan('/tmp/x')");
    }

    #[test]
    fn build_file_reader_sql_empty_paths_errors() {
        let result = build_file_reader_sql(FileFormat::Parquet, &[], &[]);
        assert!(result.is_err());
    }

    // ── 14-15. render_cast (§4.2 first item) ─────────────────────────────

    #[test]
    fn render_cast_emits_cast() {
        let expr = CastExpression {
            expr: Box::new(int_lit(1)),
            to_type: DataType::Long,
            try_cast: false,
            implicit: false,
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
            implicit: false,
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

    // ── ADR-006 ANSI divide/mod-by-zero guards (math-010/011) ────────────

    #[test]
    fn render_binary_div_column_divisor_guards_divide_by_zero() {
        let b = BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(int_lit(6)),
            right: Box::new(col_ref_expr("b")),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert!(
            sql.starts_with("CASE WHEN (b) = 0 THEN error('[DIVIDE_BY_ZERO]"),
            "got: {sql}"
        );
        assert!(sql.ends_with("ELSE (6) / (b) END"), "got: {sql}");
    }

    #[test]
    fn render_binary_mod_column_divisor_guards_remainder_by_zero() {
        let b = BinaryExpression {
            op: BinaryOp::Mod,
            left: Box::new(int_lit(6)),
            right: Box::new(col_ref_expr("b")),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert!(
            sql.starts_with("CASE WHEN (b) = 0 THEN error('[REMAINDER_BY_ZERO]"),
            "got: {sql}"
        );
        assert!(sql.ends_with("ELSE (6) % (b) END"), "got: {sql}");
    }

    #[test]
    fn render_binary_div_nonzero_literal_divisor_skips_guard() {
        let b = BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(int_lit(6)),
            right: Box::new(int_lit(2)),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert_eq!(sql, "(6) / (2)");
    }

    #[test]
    fn render_pmod_column_divisor_guards_remainder_by_zero() {
        let sql = render_fn("pmod", vec![col_ref_expr("a"), col_ref_expr("b")]);
        assert!(
            sql.starts_with("CASE WHEN (b) = 0 THEN error('[REMAINDER_BY_ZERO]"),
            "got: {sql}"
        );
        assert!(sql.ends_with("ELSE pmod(a, b) END"), "got: {sql}");
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

    // ── Pass 85 — defensive UnresolvedRegex arm ─────────────────────────

    #[test]
    fn render_expr_on_unresolved_regex_returns_unsupported_expression() {
        use crate::transpiler_v2::expression::UnresolvedRegexExpression;
        let expr = Expression::UnresolvedRegex(UnresolvedRegexExpression {
            pattern: ".*_id".to_owned(),
            plan_id: None,
        });
        let err = render_expr(&expr, &empty_schema()).unwrap_err();
        expect_unsupported(err, UnsupportedKind::Expression, "UnresolvedRegex", &[]);
    }

    // ── ExtractValue emission dispatches on child data_type (cx-001/002) ──

    fn extract_value(child: Expression, extraction: Expression) -> Expression {
        Expression::ExtractValue(ExtractValueExpression {
            child: Box::new(child),
            extraction: Box::new(extraction),
        })
    }

    #[test]
    fn extract_value_over_struct_child_emits_dot_field() {
        // Regression: struct getField stays on the `.field` path.
        let child = col_with_type(
            "address",
            DataType::Struct(StructType::new(vec![StructField::nullable(
                "city",
                DataType::String,
            )])),
        );
        let ev = extract_value(child, str_lit("city"));
        let sql = render_expr(&ev, &empty_schema()).unwrap();
        assert_eq!(sql, "(address).city");
    }

    #[test]
    fn extract_value_over_array_child_emits_ansi_throwing_list_extract() {
        // Spark `arr[0]` (0-indexed, ANSI): in-bounds returns the element via
        // DuckDB list_extract(.., idx+1); OOB/negative THROWS
        // `[INVALID_ARRAY_INDEX]` (GetArrayItem failOnError=true), NOT NULL.
        // A NULL array short-circuits to NULL.
        let child = col_with_type("arr", DataType::Array(Box::new(DataType::Integer), false));
        let ev = extract_value(child, int_lit(0));
        let sql = render_expr(&ev, &empty_schema()).unwrap();
        assert_eq!(
            sql,
            "CASE WHEN (arr) IS NULL THEN NULL \
             WHEN (0) < 0 OR (0) >= len((arr)) THEN \
             error('[INVALID_ARRAY_INDEX] The index ' || (0)::VARCHAR \
             || ' is out of bounds. The array has ' || len((arr))::VARCHAR \
             || ' elements. Use the SQL function `get()` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003') \
             ELSE list_extract((arr), (0) + 1) END"
        );
    }

    #[test]
    fn extract_value_over_array_child_throws_on_negative_and_oob() {
        // The OOB/negative branch must THROW `[INVALID_ARRAY_INDEX]` (never
        // NULL) — distinct from element_at's `_IN_ELEMENT_AT` class — while the
        // in-bounds ELSE branch still returns the element.
        let child = col_with_type("arr", DataType::Array(Box::new(DataType::Integer), false));
        let ev = extract_value(child, int_lit(-1));
        let sql = render_expr(&ev, &empty_schema()).unwrap();
        assert!(
            sql.contains("error('[INVALID_ARRAY_INDEX] The index '"),
            "missing INVALID_ARRAY_INDEX throw: {sql}"
        );
        assert!(
            !sql.contains("INVALID_ARRAY_INDEX_IN_ELEMENT_AT"),
            "must NOT use the element_at class: {sql}"
        );
        assert!(sql.contains("(-1) < 0"), "missing negative guard: {sql}");
        assert!(
            sql.contains("ELSE list_extract((arr), (-1) + 1) END"),
            "in-bounds branch must still return the element: {sql}"
        );
    }

    #[test]
    fn extract_value_over_map_child_emits_element_at() {
        // Spark `map[k]` → DuckDB element_at(map, k)[1] (scalar, NULL on miss).
        let child = col_with_type(
            "m",
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: true,
            },
        );
        let ev = extract_value(child, str_lit("a"));
        let sql = render_expr(&ev, &empty_schema()).unwrap();
        assert_eq!(sql, "element_at((m), ('a'))[1]");
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
    fn quote_ident_quotes_at_reserved_alias() {
        // Regression for jn-018: `at` is a DuckDB reserved word (used in
        // ASOF joins / `AT TIME ZONE`) and must be quoted when emitted as a
        // derived-table/column alias, even though it is a common Spark
        // alias name (e.g. from `.alias("at")`).
        let out = quote_ident("at");
        assert!(matches!(out, Cow::Owned(_)));
        assert_eq!(out, "\"at\"");

        // Case-insensitive: `AT` / `At` are reserved too.
        assert_eq!(quote_ident("AT"), "\"AT\"");
        assert_eq!(quote_ident("At"), "\"At\"");
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

    // ── 28. INV3 — no forbidden `use` inside emission.rs ─────────────────

    #[test]
    fn inv3_no_forbidden_use_in_emission() {
        // Only scan the non-test region of emission.rs; the tests themselves
        // legitimately name forbidden prefixes inside their assertion literals.
        let this_file = include_str!("emission.rs");
        // The `#[cfg(test)]` module below carries the offending literals; cut
        // at its start marker.
        let module_marker = "#[cfg(test)]\nmod tests {";
        let scan_slice = match this_file.find(module_marker) {
            Some(idx) => &this_file[..idx],
            None => this_file,
        };
        // Build needles at runtime so this test's source doesn't self-match.
        // The first four prefixes name retired v1 modules deleted 2026-07-05
        // (barrier prevents accidental re-introduction). `runtime` is active
        // but must not be imported into emission — that's an INV10 concern
        // enforced here for defence in depth.
        let forbidden_prefixes = ["generator", "functions", "logical", "parser", "runtime"];
        for base in forbidden_prefixes {
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
        // StructType}` — value-level types — or from the τ-owned
        // `bail_boundary_*!` boundary-error macros (Pass 7 / OPP-D). Both are
        // INV10-safe: the τ macros expand to `EmissionError::Unsupported*`
        // constructors, which is exactly what the manual `Err(EmissionError::
        // ...)` sites they replace did. The `#[cfg(test)]` tests below
        // legitimately import fixtures from `crate::transpiler_v2::…`.
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
            let is_typed_import = trimmed.starts_with("use crate::types::");
            let is_bail_macro_import = trimmed.starts_with("use crate::bail_boundary_")
                || trimmed.starts_with("use crate::{bail_boundary_");
            assert!(
                is_typed_import || is_bail_macro_import,
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
        let ast = scan("emp");
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
    fn extension_targets_is_empty_by_default() {
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
        // Sample WITH REPLACEMENT is a permanent Thunderduck-boundary error
        // (ADR-022 — DuckDB has no row-level sampling with replacement), so it
        // is a stable erroring dispatch that won't bit-rot as coverage grows.
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(scan("emp")),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: true,
            seed: Some(11),
        });
        let result = generate(&ast, &bt);
        assert!(
            result.is_err(),
            "sample-with-replacement must fail dispatch; if this becomes \
             supported, pick another Thunderduck-boundary op for this test",
        );
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
            kind: IntervalKind::Calendar,
        };
        let sql = render_interval(&i).expect("render");
        assert!(sql.starts_with("INTERVAL '"), "got: {sql}");
        assert!(sql.contains("1 months 2 days 3 microseconds"), "got: {sql}");
    }

    #[test]
    fn render_interval_is_kind_invisible() {
        // The semantic kind steers only `data_type()`; emission is kind-blind
        // (DuckDB has a single INTERVAL type). A YearMonth-kind carrier renders
        // the identical INTERVAL string as a Calendar-kind carrier.
        let ym = IntervalExpression {
            months: 14,
            days: 0,
            microseconds: 0,
            kind: IntervalKind::YearMonth,
        };
        let cal = IntervalExpression {
            months: 14,
            days: 0,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        };
        assert_eq!(
            render_interval(&ym).expect("render"),
            render_interval(&cal).expect("render"),
        );
    }

    #[test]
    fn render_column_reference_qualified() {
        let c = ColumnReference {
            name: "id".to_owned(),
            qualifier: Some("emp".to_owned()),
            data_type: Some(DataType::Long),
            nullable: Some(false),
            expr_id: None,
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

    /// §9 test 1 — struct-001 regression: `struct("name","age")` →
    /// `struct_pack(name := name, age := age)`.
    #[test]
    fn render_struct_two_column_refs() {
        let sql = render_fn("struct", vec![col_ref_expr("name"), col_ref_expr("age")]);
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
        let sql = render_fn("struct", vec![aliased]);
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
        let sql = render_fn("struct", vec![lit]);
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
        let sql = render_fn("struct", vec![computed, col_ref_expr("b")]);
        assert_eq!(sql, "struct_pack(col1 := (a) + (1), b := b)");
    }

    /// §9 test 5 — zero-arg `struct()` emits `struct_pack()`.
    #[test]
    fn render_struct_empty() {
        let sql = render_fn("struct", vec![]);
        assert_eq!(sql, "struct_pack()");
    }

    // ── JSON / CSV cluster (Pass 62) ────────────────────────────────────

    /// json-005 anchor: 1-arg `to_json(struct(...))` wraps DuckDB's native
    /// `to_json` with `json_strip_nulls` so Spark's default
    /// `ignoreNullFields=true` semantics are preserved (null-valued object
    /// keys dropped recursively; array `null`s and empty-object containers
    /// preserved). Corpus: `json-005`.
    #[test]
    fn render_to_json_wraps_with_json_strip_nulls() {
        let struct_arg = fexpr("struct", vec![col_ref_expr("name"), col_ref_expr("age")]);
        let sql = render_fn("to_json", vec![struct_arg]);
        assert_eq!(
            sql, "json_strip_nulls(to_json(struct_pack(name := name, age := age)))",
            "1-arg to_json wraps DuckDB's to_json with json_strip_nulls",
        );
    }

    /// Nested structs must not double-wrap: DuckDB's `json_strip_nulls` is
    /// already recursive, so τ emits the wrapper exactly once at the
    /// outermost `to_json` call. Guards against a future refactor
    /// accidentally re-wrapping inside `render_expr` recursion.
    #[test]
    fn render_to_json_of_nested_struct_still_wraps_once() {
        let inner_struct = fexpr("struct", vec![col_ref_expr("city"), col_ref_expr("zip")]);
        let outer_struct = fexpr("struct", vec![col_ref_expr("name"), inner_struct]);
        let sql = render_fn("to_json", vec![outer_struct]);
        assert_eq!(
            sql.matches("json_strip_nulls(").count(),
            1,
            "wrapper must appear exactly once (outermost); got: {sql}",
        );
        assert!(
            sql.starts_with("json_strip_nulls(to_json("),
            "wrapper must be the outermost call; got: {sql}",
        );
    }

    /// Explicit `to_json(x, {'ignoreNullFields': 'false'})` disables Spark's
    /// default null-key stripping — τ must emit the bare DuckDB `to_json`
    /// (no wrapper), letting DuckDB retain null-valued keys verbatim.
    #[test]
    fn render_to_json_with_ignore_null_fields_false_option_omits_wrapper() {
        let struct_arg = fexpr("struct", vec![col_ref_expr("name"), col_ref_expr("age")]);
        let options = Expression::MapLiteral(MapLiteralExpression {
            entries: vec![(
                Expression::Literal(Literal {
                    value: LiteralValue::String("ignoreNullFields".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("false".to_owned()),
                    data_type: DataType::String,
                }),
            )],
            key_type: DataType::String,
            value_type: DataType::String,
        });
        let sql = render_fn("to_json", vec![struct_arg, options]);
        assert_eq!(
            sql, "to_json(struct_pack(name := name, age := age))",
            "explicit ignoreNullFields=false must emit bare to_json (no wrapper)",
        );
    }

    /// Any options key other than `ignoreNullFields` is a
    /// Thunderduck-boundary error (ADR-022): τ does not silently drop
    /// unsupported JSON options. Guards the dead-arm rot on the options
    /// match.
    #[test]
    fn render_to_json_with_unsupported_option_is_boundary_error() {
        let struct_arg = fexpr("struct", vec![col_ref_expr("ts")]);
        let options = Expression::MapLiteral(MapLiteralExpression {
            entries: vec![(
                Expression::Literal(Literal {
                    value: LiteralValue::String("timestampFormat".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("yyyy-MM-dd".to_owned()),
                    data_type: DataType::String,
                }),
            )],
            key_type: DataType::String,
            value_type: DataType::String,
        });
        let f = fcall("to_json", vec![struct_arg, options]);
        let err = render_function_call(&f, &empty_schema())
            .expect_err("unsupported to_json option must be a boundary error");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "to_json",
            &["ignoreNullFields"],
        );
    }

    /// hash-001 anchor: `crc32(col)` is remapped to `spark_crc32(col)`
    /// (session macro registered by `DuckDbSession::spawn`; NOT the
    /// `thdck_spark_funcs` extension). Defends the dispatch-arm shape.
    #[test]
    fn render_crc32_remaps_to_spark_crc32() {
        let sql = render_fn("crc32", vec![col_ref_expr("name")]);
        assert_eq!(sql, "spark_crc32(name)");
    }

    /// json-006 anchor: `schema_of_json(...)` is remapped to
    /// `spark_schema_of_json(...)` (thdck_spark_funcs extension).
    #[test]
    fn render_schema_of_json_remaps_to_extension() {
        let lit = Expression::Literal(Literal {
            value: LiteralValue::String(r#"{"a":1,"b":"x"}"#.to_owned()),
            data_type: DataType::String,
        });
        let sql = render_fn("schema_of_json", vec![lit]);
        assert_eq!(sql, "spark_schema_of_json('{\"a\":1,\"b\":\"x\"}')");
    }

    /// `json_object_keys(...)` is remapped to DuckDB's native `json_keys(...)`
    /// — same `VARCHAR[]` shape, name rename only (no CAST).
    #[test]
    fn render_json_object_keys_remaps_to_json_keys() {
        let sql = render_fn("json_object_keys", vec![col_ref_expr("json_str")]);
        assert_eq!(sql, "json_keys(json_str)");
    }

    /// json-008 anchor: `to_csv(struct(a, b, c))` — DuckDB has no `to_csv`
    /// scalar; τ unpacks the struct fields and emits
    /// `concat_ws(',', CAST(a AS VARCHAR), CAST(b AS VARCHAR), CAST(c AS VARCHAR))`.
    #[test]
    fn render_to_csv_of_struct_emits_concat_ws() {
        let struct_arg = fexpr(
            "struct",
            vec![
                col_ref_expr("id"),
                col_ref_expr("name"),
                col_ref_expr("age"),
            ],
        );
        let sql = render_fn("to_csv", vec![struct_arg]);
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
        let named_struct = fexpr(
            "named_struct",
            vec![key1, col_ref_expr("id"), key2, col_ref_expr("name")],
        );
        let sql = render_fn("to_csv", vec![named_struct]);
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
        let f = fcall("to_csv", vec![col_ref_expr("some_struct_col")]);
        let err = render_function_call(&f, &empty_schema())
            .expect_err("to_csv on non-struct arg must boundary-error");
        expect_unsupported(err, UnsupportedKind::Function, "to_csv", &["struct"]);
    }

    // ── Math domain-guard wrappers (Pass 63) ────────────────────────────

    /// `math-005` anchor: `log(y)` with y=0 must return NULL under Spark
    /// non-ANSI semantics, not raise DuckDB "cannot take logarithm of zero".
    /// τ wraps the call in a CASE that guards `> 0`.
    #[test]
    fn render_log_wraps_in_null_safe_domain_guard() {
        let sql = render_fn("log", vec![col_ref_expr("y")]);
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN ln(y) ELSE NULL END");
    }

    /// Explicit `ln(y)` — identical guard, direct DuckDB name.
    #[test]
    fn render_ln_wraps_in_null_safe_domain_guard() {
        let sql = render_fn("ln", vec![col_ref_expr("y")]);
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN ln(y) ELSE NULL END");
    }

    /// `log10(y)` — same guard, DuckDB has native `log10`.
    #[test]
    fn render_log10_wraps_in_null_safe_domain_guard() {
        let sql = render_fn("log10", vec![col_ref_expr("y")]);
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log10(y) ELSE NULL END");
    }

    /// `log2(y)` — same guard, DuckDB has native `log2`.
    #[test]
    fn render_log2_wraps_in_null_safe_domain_guard() {
        let sql = render_fn("log2", vec![col_ref_expr("y")]);
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log2(y) ELSE NULL END");
    }

    /// Two-arg `log(base, x)` — guard is on the value arg (x), the base is
    /// passed through as DuckDB's `log(base, x)` positional form.
    #[test]
    fn render_log_two_arg_guards_value_only() {
        let sql = render_fn("log", vec![int_lit(10), col_ref_expr("y")]);
        assert_eq!(sql, "CASE WHEN (y) > 0 THEN log(10, y) ELSE NULL END");
    }

    /// `math-012` anchor: `shiftleft(a, 2)` where `a` may be negative must
    /// not raise DuckDB "Cannot left-shift negative number". τ emits as
    /// arithmetic multiplication `a * (1::BIGINT << n)` which accepts
    /// negative operands and preserves 2's-complement shift semantics.
    #[test]
    fn render_shiftleft_uses_arithmetic_form() {
        let sql = render_fn("shiftleft", vec![col_ref_expr("a"), int_lit(2)]);
        assert_eq!(sql, "(a * (1::BIGINT << (2)))");
    }

    /// Pass 73: `hypot(a, b)` — DuckDB has no `hypot` scalar; τ emits the
    /// inline form `sqrt(a*a + b*b)` with explicit DOUBLE casts.
    #[test]
    fn render_hypot_emits_inline_sqrt_form() {
        let sql = render_fn("hypot", vec![col_ref_expr("x"), col_ref_expr("y")]);
        assert!(sql.starts_with("sqrt("));
        assert!(sql.contains("CAST(x AS DOUBLE)"));
        assert!(sql.contains("CAST(y AS DOUBLE)"));
    }

    /// Pass 73: `format_string(fmt, args...)` remaps to DuckDB's `printf`.
    #[test]
    fn render_format_string_remaps_to_printf() {
        let sql = render_fn(
            "format_string",
            vec![
                Expression::Literal(Literal {
                    value: LiteralValue::String("%s=%d".to_owned()),
                    data_type: DataType::String,
                }),
                col_ref_expr("name"),
                col_ref_expr("age"),
            ],
        );
        assert!(sql.starts_with("printf("));
    }

    /// Pass 73: `bround(x, n)` — Spark's banker's rounding. Emit a
    /// half-even CASE around `round(x * 10^n)`.
    #[test]
    fn render_bround_emits_half_even_case() {
        let sql = render_fn("bround", vec![col_ref_expr("x"), int_lit(1)]);
        assert!(sql.contains("floor(") || sql.contains("round("));
        // The half-even branch must reference the even-parity check.
        assert!(sql.contains("% 2 = 0"));
    }

    /// `shiftright(a, n)` — DuckDB's `>>` is arithmetic on signed BIGINT
    /// and accepts negative operands, so τ passes it through directly.
    #[test]
    fn render_shiftright_uses_operator_form() {
        let sql = render_fn("shiftright", vec![col_ref_expr("a"), int_lit(2)]);
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
        let sql = render_fn(
            "nth_value",
            vec![col_ref_expr("salary"), int_lit(2), bool_lit],
        );
        assert_eq!(sql, "nth_value(salary, 2)");
    }

    /// Two-arg `nth_value` (no trailing bool) passes through unchanged.
    #[test]
    fn render_nth_value_two_args_passes_through() {
        let sql = render_fn("nth_value", vec![col_ref_expr("salary"), int_lit(2)]);
        assert_eq!(sql, "nth_value(salary, 2)");
    }

    /// A trailing non-boolean argument must NOT be silently dropped — the
    /// arm only triggers on a boolean literal in the trailing position.
    /// Verifies the safety-net check on the trim behavior.
    #[test]
    fn render_nth_value_with_non_bool_extra_arg_passes_through() {
        // Falls through to pass-through emission — DuckDB will still reject
        // the extra arg, but τ preserves it faithfully rather than silently
        // dropping a real value.
        let sql = render_fn(
            "nth_value",
            vec![col_ref_expr("salary"), int_lit(2), int_lit(99)],
        );
        assert_eq!(sql, "nth_value(salary, 2, 99)");
    }

    // ── Unpivot emission ────────────────────────────────────────────────

    /// grp-004 shape — emits conditional-aggregate SQL that matches Spark's
    /// PIVOT semantics (empty COUNT buckets → NULL, not 0). Pass 60 anchor.
    #[test]
    fn render_pivot_explicit_values_emits_conditional_aggregate_shape() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        // Build: emp.groupBy("dept_id").pivot("id", [1, 2]).agg(count(*) AS n)
        // Using existing emp cols to satisfy the analyzer.
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
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
                expr: Box::new(fexpr("count", vec![int_lit(1)])),
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
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
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
                    expr: Box::new(fexpr(
                        "sum",
                        vec![Expression::UnresolvedColumn(
                            crate::transpiler_v2::expression::UnresolvedColumn {
                                name: "salary".to_owned(),
                                qualifier: None,
                                plan_id: None,
                            },
                        )],
                    )),
                }),
                Expression::Alias(AliasExpression {
                    alias: "c".to_owned(),
                    expr: Box::new(fexpr("count", vec![int_lit(1)])),
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

    /// G3 (pass 107): an Alias-wrapped pivot value (SQL `IN (1 AS one)`) must
    /// have its alias stripped inside the CASE comparison — the alias only
    /// names the output column (`AS "one"`), it must not leak into the CASE.
    #[test]
    fn render_pivot_strips_alias_from_pivot_value_in_case() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Implicit,
            pivot_column: Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            ),
            pivot_values: vec![Expression::Alias(AliasExpression {
                alias: "one".to_owned(),
                expr: Box::new(int_lit(1)),
            })],
            aggregates: vec![Expression::Alias(AliasExpression {
                alias: "n".to_owned(),
                expr: Box::new(fexpr("count", vec![int_lit(1)])),
            })],
        });
        let sql = generate(&ast, &bt).expect("generate aliased-value pivot");
        // The CASE compares against the bare literal, not `1 AS one`.
        assert!(
            sql.contains("IS NOT DISTINCT FROM 1") && !sql.contains("IS NOT DISTINCT FROM 1 AS"),
            "alias must be stripped inside the CASE; got: {sql}"
        );
        // The alias names the output column (a bare identifier needs no quotes).
        assert!(sql.contains(" AS one "), "got: {sql}");
    }

    /// pv-002: `count(*)` inside a PIVOT conditional-aggregate must render
    /// as `THEN 1 END` (not `THEN * END`) — DuckDB rejects a bare `*`
    /// anywhere except as an expression root.
    #[test]
    fn render_pivot_count_star_rewrites_to_count_one() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        // Build: emp.groupBy("dept_id").pivot("id", [1, 2]).agg(count(*) AS n)
        // Mirrors the existing explicit-values test but uses Star instead of
        // int_lit(1) for the count arg — exercises the rewrite path.
        let ast = CommonAst::new(CommonOp::Pivot {
            input: Box::new(scan("emp")),
            grouping: PivotGrouping::Explicit(vec![Expression::UnresolvedColumn(
                crate::transpiler_v2::expression::UnresolvedColumn {
                    name: "dept_id".to_owned(),
                    qualifier: None,
                    plan_id: None,
                },
            )]),
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
                    args: vec![Expression::Star(StarExpression { qualifier: None })],
                    distinct: false,
                })),
            })],
        });
        let sql = generate(&ast, &bt).expect("generate pivot with count(*)");
        // Star must be rewritten to literal 1 inside the CASE body.
        assert!(
            sql.contains("THEN 1 END"),
            "count(*) should rewrite Star to 1 inside CASE; got: {sql}"
        );
        assert!(
            !sql.contains("THEN * END"),
            "bare * must not appear inside CASE; got: {sql}"
        );
        // NULLIF empty-bucket wrap must still be present.
        assert!(
            sql.contains("NULLIF(count("),
            "COUNT should be NULLIF-wrapped; got: {sql}"
        );
    }

    #[test]
    fn render_unpivot_emits_duckdb_unpivot_shape() {
        // Anchor: piv-004 shape — emits
        //   UNPIVOT (SELECT <ids>,<values> FROM (<child>) AS __td_unpivot_src)
        //     ON <values> INTO NAME "metric" VALUE "value"
        // per τ's UNPIVOT emission contract (see `gen_unpivot` above).
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Unpivot {
            input: Box::new(scan("emp")),
            ids: UnpivotIds::Explicit(vec!["id".to_owned()]),
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

    // ── Describe / Summary emission (Pass 80) ────────────────────────────

    #[test]
    fn render_describe_wraps_child_in_cte_and_emits_union_all_rows() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Describe {
            input: Box::new(scan("emp")),
            cols: vec!["dept_id".to_owned(), "salary".to_owned()],
        });
        let sql = generate(&ast, &bt).expect("generate describe");
        assert!(
            sql.starts_with("WITH __stats_input__ AS MATERIALIZED ("),
            "got: {sql}"
        );
        // Five stat rows joined by UNION ALL (count, mean, stddev, min, max).
        assert_eq!(sql.matches(" UNION ALL ").count(), 4, "got: {sql}");
        assert!(sql.contains("'count' AS summary"), "got: {sql}");
        assert!(sql.contains("'mean' AS summary"), "got: {sql}");
        assert!(sql.contains("'stddev' AS summary"), "got: {sql}");
        assert!(sql.contains("'min' AS summary"), "got: {sql}");
        assert!(sql.contains("'max' AS summary"), "got: {sql}");
        // Per-col aggregate uses TRY_CAST(... AS DOUBLE) for the mean row.
        assert!(
            sql.contains("CAST(AVG(TRY_CAST(dept_id AS DOUBLE)) AS VARCHAR) AS dept_id"),
            "got: {sql}"
        );
        assert!(sql.contains("FROM __stats_input__"), "got: {sql}");
    }

    #[test]
    fn render_summary_emits_percentile_via_quantile_disc_try_cast() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Summary {
            input: Box::new(scan("emp")),
            statistics: vec![
                "count".to_owned(),
                "min".to_owned(),
                "25%".to_owned(),
                "75%".to_owned(),
                "max".to_owned(),
            ],
        });
        let sql = generate(&ast, &bt).expect("generate summary");
        assert!(
            sql.starts_with("WITH __stats_input__ AS MATERIALIZED ("),
            "got: {sql}"
        );
        assert_eq!(sql.matches(" UNION ALL ").count(), 4, "got: {sql}");
        assert!(sql.contains("'25%' AS summary"), "got: {sql}");
        // Percentile stats emit quantile_disc(TRY_CAST(...)).
        assert!(
            sql.contains("quantile_disc(TRY_CAST(salary AS DOUBLE),"),
            "got: {sql}"
        );
        assert!(sql.contains("'75%' AS summary"), "got: {sql}");
    }

    // ── FreqItems emission (Pass 82) ─────────────────────────────────────

    #[test]
    fn render_freq_items_single_col_wraps_child_in_materialized_cte_with_list_having() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(scan("emp")),
            cols: vec!["dept_id".to_owned()],
            support: 0.3,
        });
        let sql = generate(&ast, &bt).expect("generate freqItems");
        assert!(
            sql.starts_with("WITH __freq_input__ AS MATERIALIZED ("),
            "got: {sql}"
        );
        // LIST(...) ORDER BY correlated subquery, output aliased with
        // {col}_freqItems (quote_ident leaves the safe identifier bare).
        assert!(
            sql.contains("(SELECT LIST(dept_id ORDER BY dept_id) FROM ("),
            "got: {sql}"
        );
        assert!(
            sql.contains("SELECT dept_id, COUNT(*) AS __cnt FROM __freq_input__"),
            "got: {sql}"
        );
        assert!(
            sql.contains("WHERE dept_id IS NOT NULL GROUP BY dept_id"),
            "got: {sql}"
        );
        assert!(
            sql.contains("HAVING COUNT(*) >= 0.3 * (SELECT COUNT(*) FROM __freq_input__)"),
            "got: {sql}"
        );
        assert!(sql.contains("AS dept_id_freqItems"), "got: {sql}");
    }

    #[test]
    fn render_freq_items_multi_col_emits_one_correlated_subquery_per_col() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::FreqItems {
            input: Box::new(scan("emp")),
            cols: vec!["dept_id".to_owned(), "salary".to_owned()],
            support: 0.01,
        });
        let sql = generate(&ast, &bt).expect("generate freqItems multi");
        // One "LIST(<col> ORDER BY <col>)" per input col.
        assert_eq!(sql.matches("LIST(dept_id ORDER BY dept_id)").count(), 1);
        assert_eq!(sql.matches("LIST(salary ORDER BY salary)").count(), 1);
        // Two output aliases joined by ", ".
        assert!(sql.contains("AS dept_id_freqItems, "), "got: {sql}");
        assert!(sql.contains("AS salary_freqItems"), "got: {sql}");
    }

    #[test]
    fn render_freq_items_empty_cols_returns_unsupported_op_defensive_guard() {
        // The analyzer path never yields empty cols (PySpark client rejects
        // client-side, and `materialise_stats_cols` expands empty to all
        // input columns). We call `render_freq_items` directly to exercise
        // the defensive guard.
        let typed_input = TypedAst::new(
            TypedOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            },
            Schema::empty(),
        );
        let err = super::render_freq_items(&typed_input, &[], 0.01).unwrap_err();
        expect_unsupported(err, UnsupportedKind::Op, "FreqItems", &[]);
    }

    // ── Sample / SampleBy emission (Pass 83) ─────────────────────────────

    #[test]
    fn render_sample_emits_tablesample_bernoulli_with_percent() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(scan("emp")),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: false,
            seed: None,
        });
        let sql = generate(&ast, &bt).expect("generate Sample");
        assert!(
            sql.contains("TABLESAMPLE BERNOULLI(50.0000 PERCENT)"),
            "got: {sql}"
        );
        assert!(
            !sql.contains("REPEATABLE"),
            "no seed → no REPEATABLE clause"
        );
    }

    #[test]
    fn render_sample_with_seed_emits_repeatable_clause() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::Sample {
            input: Box::new(scan("emp")),
            lower_bound: 0.0,
            upper_bound: 0.5,
            with_replacement: false,
            seed: Some(11),
        });
        let sql = generate(&ast, &bt).expect("generate Sample with seed");
        assert!(
            sql.contains("TABLESAMPLE BERNOULLI(50.0000 PERCENT) REPEATABLE(11)"),
            "got: {sql}"
        );
    }

    #[test]
    fn render_sample_with_replacement_returns_unsupported_op() {
        // ADR-022 Thunderduck-boundary: DuckDB has no row-level sampling
        // with replacement. Emission surfaces `UnsupportedOp`.
        let _g = tap_guard();
        let typed_input = TypedAst::new(
            TypedOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            },
            Schema::empty(),
        );
        let err = super::render_sample(&typed_input, 0.0, 0.5, true, Some(11)).unwrap_err();
        expect_unsupported(err, UnsupportedKind::Op, "Sample[with_replacement]", &[]);
    }

    #[test]
    fn render_sample_by_emits_per_stratum_or_chain_with_setseed_wrapper() {
        let _g = tap_guard();
        let bt = base_types_with_emp();
        let ast = CommonAst::new(CommonOp::SampleBy {
            input: Box::new(scan("emp")),
            col: Expression::UnresolvedColumn(crate::transpiler_v2::expression::UnresolvedColumn {
                name: "dept_id".to_owned(),
                qualifier: None,
                plan_id: None,
            }),
            fractions: vec![
                (
                    Literal {
                        value: LiteralValue::Int(10),
                        data_type: DataType::Integer,
                    },
                    0.5,
                ),
                (
                    Literal {
                        value: LiteralValue::Int(20),
                        data_type: DataType::Integer,
                    },
                    0.5,
                ),
                (
                    Literal {
                        value: LiteralValue::Int(30),
                        data_type: DataType::Integer,
                    },
                    1.0,
                ),
            ],
            seed: Some(11),
        });
        let sql = generate(&ast, &bt).expect("generate SampleBy");
        // Per-stratum OR chain.
        assert!(
            sql.contains("(dept_id = 10 AND RANDOM() < 0.5)"),
            "got: {sql}"
        );
        assert!(
            sql.contains("(dept_id = 20 AND RANDOM() < 0.5)"),
            "got: {sql}"
        );
        assert!(
            sql.contains("(dept_id = 30 AND RANDOM() < 1)"),
            "got: {sql}"
        );
        // Seed → setseed wrapper.
        assert!(sql.contains("(SELECT setseed("), "got: {sql}");
        // Sanity: __td_sample_by alias.
        assert!(sql.contains("__td_sample_by"), "got: {sql}");
    }

    #[test]
    fn render_sample_by_empty_fractions_emits_where_false() {
        let _g = tap_guard();
        let typed_input = TypedAst::new(
            TypedOp::TableScan {
                table: "emp".to_owned(),
                alias: None,
            },
            Schema::minted(emp_schema()),
        );
        let col_ref = Expression::ColumnReference(ColumnReference {
            name: "dept_id".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Integer),
            nullable: Some(true),
            expr_id: None,
        });
        let sql =
            super::render_sample_by(&typed_input, &col_ref, &[], None).expect("empty fractions ok");
        assert!(sql.contains("WHERE FALSE"), "got: {sql}");
        assert!(sql.contains("__td_sample_by"), "got: {sql}");
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
        col_with_type("address", address_struct_dt())
    }

    fn addr_schema() -> Schema {
        Schema::minted(StructType::new(vec![StructField::nullable(
            "address",
            address_struct_dt(),
        )]))
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
        let sql = render_fn(
            "to_date",
            vec![str_lit("15/01/2026"), str_lit("dd/MM/yyyy")],
        );
        assert!(sql.starts_with("CAST(strptime('15/01/2026', replace("));
        assert!(sql.contains("'dd/MM/yyyy'"));
        assert!(sql.ends_with(") AS DATE)"));
    }

    #[test]
    fn render_to_date_one_arg_stays_a_cast() {
        let sql = render_fn("to_date", vec![str_lit("2026-01-15")]);
        assert_eq!(sql, "CAST('2026-01-15' AS DATE)");
    }

    #[test]
    fn render_to_timestamp_two_arg_uses_strptime() {
        // dt-010 regression: `F.to_timestamp(F.lit("2026-01-15 10:00"),
        // "yyyy-MM-dd HH:mm")` must emit `strptime(..., translated_fmt)` —
        // NOT `to_timestamp(STRING, STRING)` which DuckDB rejects.
        let sql = render_fn(
            "to_timestamp",
            vec![str_lit("2026-01-15 10:00"), str_lit("yyyy-MM-dd HH:mm")],
        );
        assert!(sql.starts_with("strptime('2026-01-15 10:00', replace("));
        assert!(sql.contains("'yyyy-MM-dd HH:mm'"));
    }

    #[test]
    fn render_to_timestamp_one_arg_stays_a_cast() {
        let sql = render_fn("to_timestamp", vec![str_lit("2026-01-15 10:00:00")]);
        assert_eq!(sql, "CAST('2026-01-15 10:00:00' AS TIMESTAMP)");
    }

    #[test]
    fn render_unix_timestamp_one_arg_casts_epoch_to_bigint() {
        // dt-014 regression #1: `F.unix_timestamp("last_login")` must emit
        // `CAST(epoch(last_login) AS BIGINT)`. Pre-Pass-66 emission was just
        // `epoch(last_login)` which DuckDB accepts but with wrong Spark-parity
        // return type (Double vs Long) and TZ column shape mismatch.
        let sql = render_fn("unix_timestamp", vec![col_ref_expr("last_login")]);
        assert_eq!(sql, "CAST(epoch(last_login) AS BIGINT)");
    }

    #[test]
    fn render_unix_timestamp_two_arg_wraps_strptime() {
        let sql = render_fn(
            "unix_timestamp",
            vec![col_ref_expr("ts_str"), str_lit("yyyy-MM-dd HH:mm:ss")],
        );
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
        let sql = render_fn(
            "unix_timestamp",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "last_login".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::Timestamp),
                    nullable: Some(true),
                    expr_id: None,
                }),
                str_lit("yyyy-MM-dd HH:mm:ss"),
            ],
        );
        assert_eq!(sql, "CAST(epoch(last_login) AS BIGINT)");
    }

    #[test]
    fn render_from_unixtime_one_arg_returns_default_format_string() {
        // dt-014 regression #2: `F.from_unixtime(F.lit(1700000000))` must
        // emit `strftime(to_timestamp(CAST(<lit> AS DOUBLE)),
        // '%Y-%m-%d %H:%M:%S')`. Spark returns String, not Timestamp.
        // Note: Long literal renders as `CAST(1700000000 AS BIGINT)` — the
        // outer `CAST(.. AS DOUBLE)` wraps it, which DuckDB folds fine.
        let sql = render_fn("from_unixtime", vec![long_lit(1_700_000_000)]);
        assert!(sql.starts_with("strftime(to_timestamp(CAST("));
        assert!(sql.contains("1700000000"));
        assert!(sql.ends_with(" AS DOUBLE)), '%Y-%m-%d %H:%M:%S')"));
    }

    #[test]
    fn render_from_unixtime_two_arg_translates_format() {
        let sql = render_fn(
            "from_unixtime",
            vec![long_lit(1_700_000_000), str_lit("yyyy/MM/dd")],
        );
        assert!(sql.starts_with("strftime(to_timestamp(CAST("));
        assert!(sql.contains("1700000000"));
        assert!(sql.contains(" AS DOUBLE)), replace("));
        assert!(sql.contains("'yyyy/MM/dd'"));
    }

    /// `test_to_char` (test_string_collection_differential): the corpus
    /// witness is `to_char(dt, 'yyyy-MM-dd')` on a DATE column. DuckDB has
    /// no native `to_char`; τ mirrors `date_format`'s `strftime` +
    /// Spark→DuckDB token translation.
    #[test]
    fn render_to_char_date_form_translates_to_strftime() {
        let sql = render_fn(
            "to_char",
            vec![col_with_type("dt", DataType::Date), str_lit("yyyy-MM-dd")],
        );
        assert!(sql.starts_with("strftime(dt, "), "sql = {sql}");
        assert!(sql.contains("'yyyy'"));
        assert!(sql.contains("'%Y'"));
    }

    #[test]
    fn render_btrim_one_arg_renames_to_trim() {
        let sql = render_fn("btrim", vec![col_ref_expr("s")]);
        assert_eq!(sql, "trim(s)");
    }

    #[test]
    fn render_btrim_two_arg_renames_to_trim() {
        let sql = render_fn("btrim", vec![col_ref_expr("s"), str_lit("xy")]);
        assert_eq!(sql, "trim(s, 'xy')");
    }

    /// `test_substring_index` (test_string_collection_differential):
    /// positive `count` keeps the first `count` delimited pieces from the
    /// left. Empirically verified against live Spark 4.1.1.
    #[test]
    fn render_substring_index_positive_count_takes_from_left() {
        let sql = render_fn(
            "substring_index",
            vec![col_ref_expr("domain"), str_lit("."), int_lit(2)],
        );
        assert_eq!(
            sql,
            "CASE WHEN (2) >= 0 \
             THEN array_to_string(list_slice(string_split(domain, '.'), 1, (2)), '.') \
             ELSE array_to_string(list_slice(string_split(domain, '.'), (2), -1), '.') \
             END"
        );
    }

    /// Negative `count` keeps the last `|count|` pieces from the right.
    #[test]
    fn render_substring_index_negative_count_takes_from_right() {
        let sql = render_fn(
            "substring_index",
            vec![col_ref_expr("domain"), str_lit("."), int_lit(-2)],
        );
        assert!(sql.contains("(-2) >= 0"), "sql = {sql}");
        assert!(
            sql.contains("list_slice(string_split(domain, '.'), (-2), -1)"),
            "sql = {sql}"
        );
    }

    /// `count == 0` renders through the `>= 0` (left) branch with an empty
    /// slice bound (`list_slice(..., 1, 0)`), which DuckDB clamps to an
    /// empty list — `array_to_string` of an empty list is `''`, matching
    /// Spark's `substring_index(s, delim, 0) == ''` (verified live).
    #[test]
    fn render_substring_index_zero_count_renders_empty_slice() {
        let sql = render_fn(
            "substring_index",
            vec![col_ref_expr("domain"), str_lit("."), int_lit(0)],
        );
        assert!(sql.contains("(0) >= 0"), "sql = {sql}");
        assert!(
            sql.contains("list_slice(string_split(domain, '.'), 1, (0))"),
            "sql = {sql}"
        );
    }

    #[test]
    fn render_dayname_passes_through_native() {
        let sql = render_fn("dayname", vec![col_ref_expr("d")]);
        assert_eq!(sql, "dayname(d)");
    }

    #[test]
    fn render_monthname_passes_through_native() {
        let sql = render_fn("monthname", vec![col_ref_expr("d")]);
        assert_eq!(sql, "monthname(d)");
    }

    /// Non-struct base is a Spark-emulated error (Spark itself rejects
    /// `withField` on scalar types).
    #[test]
    fn render_update_fields_non_struct_base_is_error() {
        let schema = Schema::minted(StructType::new(vec![StructField::nullable(
            "name",
            DataType::String,
        )]));
        let expr = Expression::UpdateFields(UpdateFieldsExpression {
            struct_expr: Box::new(Expression::ColumnReference(ColumnReference {
                name: "name".to_owned(),
                qualifier: None,
                data_type: Some(DataType::String),
                nullable: Some(true),
                expr_id: None,
            })),
            updates: vec![("x".to_owned(), None)],
        });
        let err = render_expr(&expr, &schema).expect_err("must error on non-struct base");
        expect_unsupported(err, UnsupportedKind::Expression, "UpdateFields", &[]);
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
            expr_id: None,
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
        let sql = render_fn("exists", vec![arr, lambda]);
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
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x_7".to_owned()],
            body: Box::new(Expression::Binary(BinaryExpression {
                op: BinaryOp::Gt,
                left: Box::new(fexpr(
                    "length",
                    vec![Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x_7".to_owned(),
                    })],
                )),
                right: Box::new(Expression::Literal(Literal {
                    value: LiteralValue::Long(0),
                    data_type: DataType::Long,
                })),
            })),
        });
        let sql = render_fn("forall", vec![arr, lambda]);
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
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x".to_owned(), "i".to_owned()],
            body: Box::new(fexpr(
                "concat",
                vec![
                    Expression::Cast(CastExpression {
                        expr: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "i".to_owned(),
                        })),
                        to_type: DataType::String,
                        try_cast: false,
                        implicit: false,
                    }),
                    Expression::Literal(Literal {
                        value: LiteralValue::String(":".to_owned()),
                        data_type: DataType::String,
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x".to_owned(),
                    }),
                ],
            )),
        });
        let sql = render_fn("transform", vec![arr, lambda]);
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
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x".to_owned()],
            body: Box::new(fexpr(
                "upper",
                vec![Expression::LambdaVariable(LambdaVariableExpression {
                    name: "x".to_owned(),
                })],
            )),
        });
        let sql = render_fn("transform", vec![arr, lambda]);
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
            right: Box::new(fexpr(
                "list_transform",
                vec![
                    Expression::ColumnReference(ColumnReference {
                        name: "arr".to_owned(),
                        qualifier: None,
                        data_type: Some(DataType::Array(Box::new(DataType::Long), true)),
                        nullable: Some(true),
                        expr_id: None,
                    }),
                    Expression::Lambda(LambdaExpression {
                        params: vec!["i".to_owned()],
                        body: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "i".to_owned(),
                        })),
                    }),
                ],
            )),
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
        col_with_type("tags", DataType::Array(Box::new(DataType::String), true))
    }

    fn tags2_col() -> Expression {
        col_with_type("tags2", DataType::Array(Box::new(DataType::String), true))
    }

    #[test]
    fn render_explode_emits_unnest() {
        let sql = render_fn("explode", vec![tags_col()]);
        assert_eq!(sql, "UNNEST(tags)");
    }

    #[test]
    fn render_explode_arity_error() {
        let f = fcall("explode", vec![tags_col(), tags_col()]);
        let err =
            render_function_call(&f, &empty_schema()).expect_err("explode with 2 args must error");
        expect_unsupported(err, UnsupportedKind::Function, "explode", &[]);
    }

    #[test]
    fn render_explode_outer_wraps_empty_and_null_arrays() {
        let sql = render_fn("explode_outer", vec![tags_col()]);
        // Explode_outer must emit a one-NULL-row fallback for both NULL and
        // empty arrays so the outer semantics hold.
        assert_eq!(
            sql,
            "UNNEST(CASE WHEN tags IS NULL OR len(tags) = 0 THEN [NULL] ELSE tags END)"
        );
    }

    #[test]
    fn render_posexplode_pos_emits_zero_indexed_subscripts() {
        let sql = render_fn("posexplode_pos", vec![tags_col()]);
        // DuckDB `generate_subscripts` is 1-indexed; subtract 1 to align
        // with Spark's 0-indexed posexplode.
        assert_eq!(sql, "(generate_subscripts(tags, 1) - 1)");
    }

    #[test]
    fn render_posexplode_val_emits_unnest() {
        let sql = render_fn("posexplode_val", vec![tags_col()]);
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
        let k_sql = render_fn("map_explode_key", vec![m.clone()]);
        let v_sql = render_fn("map_explode_val", vec![m]);
        assert_eq!(k_sql, "UNNEST(map_keys(attrs))");
        assert_eq!(v_sql, "UNNEST(map_values(attrs))");
    }

    /// τ remaps `arrays_overlap(a, b)` → DuckDB's `list_has_any(a, b)`;
    /// DuckDB has no `arrays_overlap` function. Corpus: `arr-011`.
    #[test]
    fn render_arrays_overlap_emits_list_has_any() {
        let sql = render_fn(
            "arrays_overlap",
            vec![
                tags_col(),
                Expression::ColumnReference(ColumnReference {
                    name: "tags2".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::Array(Box::new(DataType::String), true)),
                    nullable: Some(true),
                    expr_id: None,
                }),
            ],
        );
        assert_eq!(sql, "list_has_any(tags, tags2)");
    }

    /// τ 2-arg `array_join(arr, sep)` filters NULLs before joining to match
    /// Spark's default null-skip semantics. Corpus: `arr-010`.
    #[test]
    fn render_array_join_two_arg_filters_nulls() {
        let sql = render_fn(
            "array_join",
            vec![
                tags_col(),
                Expression::Literal(Literal {
                    value: LiteralValue::String(",".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
        assert_eq!(
            sql,
            "array_to_string(list_filter(tags, x -> x IS NOT NULL), ',')"
        );
    }

    /// τ 3-arg `array_join(arr, sep, null_repl)` replaces NULLs with the
    /// replacement string per Spark's semantics. Corpus: `arr-010`.
    #[test]
    fn render_array_join_three_arg_uses_coalesce() {
        let sql = render_fn(
            "array_join",
            vec![
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
        );
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
    /// `list_transform(range(0, least(len(a), len(b))), i -> body_at_i)`, where
    /// `a[i]` / `b[i]` render through the 0-based GetArrayItem ExtractValue arm.
    /// Corpus: `hof-006`.
    #[test]
    fn render_zip_with_emits_index_iteration() {
        let a = tags_col();
        let b = Expression::ColumnReference(ColumnReference {
            name: "tags2".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["x_1".to_owned(), "y_2".to_owned()],
            body: Box::new(fexpr(
                "concat",
                vec![
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "x_1".to_owned(),
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "y_2".to_owned(),
                    }),
                ],
            )),
        });
        let sql = render_fn("zip_with", vec![a, b, lambda]);
        assert!(
            sql.contains("list_transform(range(0, least("),
            "range shape: {sql}"
        );
        assert!(sql.contains("__zw_i"), "fresh index var used: {sql}");
        // a[i] / b[i] render through the 0-based GetArrayItem ExtractValue arm:
        // guarded `list_extract(child, i + 1)`.
        assert!(
            sql.contains("list_extract((tags), (__zw_i) + 1)"),
            "a[i] substitution: {sql}"
        );
        assert!(
            sql.contains("list_extract((tags2), (__zw_i) + 1)"),
            "b[i] substitution: {sql}"
        );
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
            expr_id: None,
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
        let sql = render_fn("map_filter", vec![m, lambda]);
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
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["k".to_owned(), "v".to_owned()],
            body: Box::new(fexpr(
                "upper",
                vec![Expression::LambdaVariable(LambdaVariableExpression {
                    name: "v".to_owned(),
                })],
            )),
        });
        let sql = render_fn("transform_values", vec![m, lambda]);
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
            expr_id: None,
        });
        let lambda = Expression::Lambda(LambdaExpression {
            params: vec!["k".to_owned(), "v".to_owned()],
            body: Box::new(fexpr(
                "concat",
                vec![
                    Expression::Literal(Literal {
                        value: LiteralValue::String("attr_".to_owned()),
                        data_type: DataType::String,
                    }),
                    Expression::LambdaVariable(LambdaVariableExpression {
                        name: "k".to_owned(),
                    }),
                ],
            )),
        });
        let sql = render_fn("transform_keys", vec![m, lambda]);
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
        let sql = render_fn("arrays_zip", vec![tags_col(), tags_col()]);
        assert!(
            sql.contains("list_transform(range(1, least("),
            "range: {sql}"
        );
        // Duplicate `tags` → positional 0, 1 names.
        assert!(sql.contains("\"0\" := (tags)[__az_i]"), "field 0: {sql}");
        assert!(sql.contains("\"1\" := (tags)[__az_i]"), "field 1: {sql}");
    }

    /// τ `array_distinct(a)` dedups `a` while preserving its
    /// first-occurrence order (DuckDB's `list_distinct` reorders by hash,
    /// breaking Spark parity — see `arr-005`, `test_array_distinct`).
    #[test]
    fn render_array_distinct_preserves_first_occurrence_order() {
        let sql = render_fn("array_distinct", vec![tags_col()]);
        assert_eq!(
            sql,
            "list_filter(tags, (x, i) -> list_position(tags, x) = i)"
        );
    }

    /// τ `array_union(a, b)` == `array_distinct(list_concat(a, b))`:
    /// `a`'s own duplicates collapse to their first occurrence (since `a`
    /// comes first in the concat) and `b`'s elements collapse to their
    /// first occurrence within `b`, dropping anything already seen in `a`
    /// — matching Spark's single linked-hash-set scan over `a` then `b`.
    /// Propagates NULL if either arg is NULL (`list_concat` treats a NULL
    /// list as empty rather than propagating). Corpus: `arr-011`,
    /// `test_array_union`.
    #[test]
    fn render_array_union_preserves_order_and_propagates_null() {
        let sql = render_fn("array_union", vec![tags_col(), tags2_col()]);
        assert!(
            sql.contains("CASE WHEN (tags) IS NULL OR (tags2) IS NULL THEN NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains(
                "list_filter(list_concat(tags, tags2), (x, i) -> list_position(list_concat(tags, tags2), x) = i)"
            ),
            "order-preserving distinct over the concat: {sql}"
        );
    }

    /// τ `array_except(a, b)` dedups `a` while preserving `a`'s first-
    /// occurrence order, drops elements present in `b` via a null-safe
    /// membership check (`list_contains` returns NULL — not FALSE — for a
    /// NULL needle even when `b` holds a NULL element; `list_position(b,
    /// x) IS NULL` is null-safe), and propagates NULL if either argument
    /// is NULL. Binder Error surfaced: `list_filter(INTEGER[],
    /// INTEGER[])` (the prior `list_filter` rename passed `b` directly as
    /// a lambda). Corpus: `arr2-005`, `test_array_except`.
    #[test]
    fn render_array_except_dedups_preserves_order_and_propagates_null() {
        let sql = render_fn("array_except", vec![tags_col(), tags2_col()]);
        assert!(
            sql.contains("CASE WHEN (tags) IS NULL OR (tags2) IS NULL THEN NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains(
                "list_filter(tags, (x, i) -> list_position(tags, x) = i AND list_position(tags2, x) IS NULL)"
            ),
            "dedup-by-first-occurrence + null-safe set-diff filter: {sql}"
        );
    }

    /// τ `array_intersect(a, b)` mirrors `array_except`'s shape with the
    /// membership test inverted: distinct elements of `a`, in `a`'s
    /// first-occurrence order, that are also present in `b` via a
    /// null-safe membership check (`list_position(b, x) IS NOT NULL`,
    /// which — unlike `list_contains` — correctly counts a NULL common to
    /// both arrays as "contains"). Propagates NULL if either argument is
    /// NULL. DuckDB's `list_intersect` reorders (verified directly:
    /// `list_intersect([3,1,2,1],[2,1])` → `[1, 2]`, not `a`'s order).
    /// Corpus: `arr2-005`, `test_array_intersect`.
    #[test]
    fn render_array_intersect_preserves_order_and_is_null_safe() {
        let sql = render_fn("array_intersect", vec![tags_col(), tags2_col()]);
        assert!(
            sql.contains("CASE WHEN (tags) IS NULL OR (tags2) IS NULL THEN NULL"),
            "null propagation: {sql}"
        );
        assert!(
            sql.contains(
                "list_filter(tags, (x, i) -> list_position(tags, x) = i AND list_position(tags2, x) IS NOT NULL)"
            ),
            "dedup-by-first-occurrence + null-safe membership filter: {sql}"
        );
    }

    /// τ `reverse` is type-dependent: on a STRING, DuckDB's native
    /// `reverse` already matches Spark and is left untouched; on an
    /// ARRAY, DuckDB's `reverse(VARCHAR)` rejects list arguments (Binder
    /// Error surfaced: `reverse(INTEGER[])`) — dispatch to `list_reverse`.
    /// Corpus: `arr-XXX` / `test_reverse_array`.
    #[test]
    fn render_reverse_array_uses_list_reverse() {
        let sql = render_fn("reverse", vec![tags_col()]);
        assert_eq!(sql, "list_reverse(tags)");
    }

    #[test]
    fn render_reverse_string_stays_native() {
        let sql = render_fn("reverse", vec![col_ref_expr("name")]);
        assert_eq!(sql, "reverse(name)");
    }

    /// τ `size`/`cardinality` is type-dependent: DuckDB's `len` rejects
    /// MAP (`len(VARCHAR|BIT|ANY[])` only — Binder Error surfaced:
    /// `len(MAP(VARCHAR, INTEGER))`), so a MAP-typed argument dispatches
    /// to DuckDB's MAP-only `cardinality`, cast down from DuckDB's native
    /// UBIGINT to BIGINT (Arrow UInt64 is rejected outright by PySpark's
    /// Arrow→Spark type conversion). Array (and other) args keep the
    /// existing `len` rename. Corpus: `test_size_map`.
    #[test]
    fn render_size_map_uses_cardinality() {
        let map_col = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::Integer),
                value_nullable: true,
            }),
            nullable: Some(true),
            expr_id: None,
        });
        let sql = render_fn("size", vec![map_col]);
        assert_eq!(sql, "CAST(cardinality(attrs) AS BIGINT)");
    }

    #[test]
    fn render_size_array_unchanged() {
        let sql = render_fn("size", vec![tags_col()]);
        assert_eq!(sql, "len(tags)");
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
            expr_id: None,
        });
        let sql = render_fn("flatten", vec![outer]);
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
        let sql = render_fn(
            "array_position",
            vec![
                tags_col(),
                Expression::Literal(Literal {
                    value: LiteralValue::String("rust".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
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
            right: Box::new(fexpr(
                "list_transform",
                vec![
                    Expression::ColumnReference(ColumnReference {
                        name: "arr".to_owned(),
                        qualifier: None,
                        data_type: Some(DataType::Array(Box::new(DataType::Long), true)),
                        nullable: Some(true),
                        expr_id: None,
                    }),
                    Expression::Lambda(LambdaExpression {
                        params: vec!["k".to_owned()],
                        body: Box::new(Expression::LambdaVariable(LambdaVariableExpression {
                            name: "k".to_owned(),
                        })),
                    }),
                ],
            )),
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
        let sql = render_fn("ceil", vec![col_ref_expr("x")]);
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(ceil(x) AS BIGINT) END"
        );
    }

    #[test]
    fn render_floor_uses_case_for_nan_safety() {
        let sql = render_fn("floor", vec![col_ref_expr("x")]);
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(floor(x) AS BIGINT) END"
        );
    }

    #[test]
    fn render_ceiling_alias_uses_case_for_nan_safety() {
        let sql = render_fn("ceiling", vec![col_ref_expr("x")]);
        assert_eq!(
            sql,
            "CASE WHEN (x) IS NULL THEN NULL \
             WHEN isnan(CAST((x) AS DOUBLE)) THEN CAST(0 AS BIGINT) \
             ELSE CAST(ceil(x) AS BIGINT) END"
        );
    }

    /// `test_positive` (test_math_bitwise_date_differential): Spark's
    /// `positive(x)` (`UnaryPositive`) is the identity — DuckDB has no
    /// native `positive` scalar, so τ emits the argument unchanged.
    #[test]
    fn render_positive_is_identity() {
        let sql = render_fn("positive", vec![double_lit(10.5)]);
        assert_eq!(sql, "(CAST(10.5 AS DOUBLE))");
    }

    /// `test_bit_get` (test_math_bitwise_date_differential): Spark's
    /// `bit_get(x, pos)` returns the bit at 0-indexed `pos` (from the LSB)
    /// of integral `x` as a Byte (TINYINT). DuckDB has no integral
    /// `bit_get`; τ composes shift + mask + cast.
    #[test]
    fn render_bit_get_shifts_masks_and_casts_to_tinyint() {
        let sql = render_fn("bit_get", vec![col_ref_expr("id"), int_lit(1)]);
        assert_eq!(sql, "CAST(((id >> 1) & 1) AS TINYINT)");
    }

    #[test]
    fn render_getbit_alias_matches_bit_get() {
        let sql = render_fn("getbit", vec![col_ref_expr("id"), int_lit(2)]);
        assert_eq!(sql, "CAST(((id >> 2) & 1) AS TINYINT)");
    }

    /// `intv-003` regression: `make_dt_interval(1, 2, 30, 0)` — DuckDB has no
    /// `make_dt_interval` scalar. τ emits a sum of INTERVAL fragments.
    #[test]
    fn render_make_dt_interval_four_args() {
        let sql = render_fn(
            "make_dt_interval",
            vec![int_lit(1), int_lit(2), int_lit(30), int_lit(0)],
        );
        assert_eq!(
            sql,
            "(INTERVAL (1) DAY + INTERVAL (2) HOUR + INTERVAL (30) MINUTE \
             + INTERVAL (CAST((0) * 1000000 AS BIGINT)) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_zero_args_defaults_all_zero() {
        let sql = render_fn("make_dt_interval", vec![]);
        assert_eq!(
            sql,
            "(INTERVAL (0) DAY + INTERVAL (0) HOUR + INTERVAL (0) MINUTE \
             + INTERVAL (0) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_one_arg_days_only() {
        let sql = render_fn("make_dt_interval", vec![int_lit(7)]);
        assert_eq!(
            sql,
            "(INTERVAL (7) DAY + INTERVAL (0) HOUR + INTERVAL (0) MINUTE \
             + INTERVAL (0) MICROSECOND)"
        );
    }

    #[test]
    fn render_make_dt_interval_too_many_args_is_boundary_error() {
        let f = fcall("make_dt_interval", vec![int_lit(1); 5]);
        let err = render_function_call(&f, &empty_schema()).expect_err("too many args");
        expect_unsupported(err, UnsupportedKind::Function, "make_dt_interval", &[]);
    }

    #[test]
    fn render_make_ym_interval_two_args() {
        let sql = render_fn("make_ym_interval", vec![int_lit(1), int_lit(6)]);
        assert_eq!(sql, "(INTERVAL (1) YEAR + INTERVAL (6) MONTH)");
    }

    // ── `F.window(ts, "N unit")` — tumbling time-window (win2-002) ──────

    /// Duration parser — accepts every {second,minute,hour,day,week} form.
    #[test]
    fn parse_window_duration_literal_accepts_standard_units() {
        let cases: &[(&str, u64, &str)] = &[
            ("1 second", 1, "second"),
            ("2 seconds", 2, "second"),
            ("3 minute", 3, "minute"),
            ("4 minutes", 4, "minute"),
            ("5 hour", 5, "hour"),
            ("6 hours", 6, "hour"),
            ("7 day", 7, "day"),
            ("8 days", 8, "day"),
            ("9 week", 9, "week"),
            ("10 weeks", 10, "week"),
            ("1 DAY", 1, "day"),
            ("1 Day", 1, "day"),
            ("  1   day  ", 1, "day"),
        ];
        for (input, n, unit) in cases {
            let got =
                parse_window_duration_literal(input).unwrap_or_else(|| panic!("failed on {input}"));
            assert_eq!(got, (*n, *unit), "input {input}");
        }
    }

    /// Duration parser rejects month/year (Spark accepts them, but
    /// variable-length buckets diverge from `time_bucket` — boundary reject
    /// per ADR-015 / ADR-022).
    #[test]
    fn parse_window_duration_literal_rejects_month_year() {
        for s in &["1 month", "1 months", "1 year", "1 years", "12 months"] {
            assert!(
                parse_window_duration_literal(s).is_none(),
                "expected reject for {s}"
            );
        }
    }

    /// Duration parser rejects compound / fractional / signed / empty /
    /// unknown-unit / bare-number / trailing-garbage forms.
    #[test]
    fn parse_window_duration_literal_rejects_malformed() {
        for s in &[
            "1 day 3 hours",
            "0.5 day",
            "-1 day",
            "+1 day",
            "",
            "day",
            "1",
            "1 fortnight",
            "1 millisecond",
            "1 microsecond",
            "1 day extra",
        ] {
            assert!(
                parse_window_duration_literal(s).is_none(),
                "expected reject for {s}"
            );
        }
    }

    /// `win2-002` core: `window(last_login, "1 day")` emits struct_pack over
    /// `time_bucket` with a quoted `"end"` field name (reserved keyword).
    #[test]
    fn render_window_emits_struct_pack_time_bucket_1_day() {
        let sql = render_fn("window", vec![ts_col_ref("last_login"), str_lit("1 day")]);
        assert_eq!(
            sql,
            "struct_pack(start := time_bucket(INTERVAL '1 day', last_login), \
             \"end\" := time_bucket(INTERVAL '1 day', last_login) + INTERVAL '1 day')"
        );
    }

    /// Hour + week variants — confirms canonical-unit passthrough and
    /// N carries through unchanged.
    #[test]
    fn render_window_emits_correct_sql_for_hour_and_week_units() {
        for (dur, n, unit) in &[
            ("1 hour", 1, "hour"),
            ("2 hours", 2, "hour"),
            ("3 weeks", 3, "week"),
        ] {
            let sql = render_fn("window", vec![ts_col_ref("ts"), str_lit(dur)]);
            let expected = format!(
                "struct_pack(start := time_bucket(INTERVAL '{n} {unit}', ts), \
                 \"end\" := time_bucket(INTERVAL '{n} {unit}', ts) + INTERVAL '{n} {unit}')"
            );
            assert_eq!(sql, expected, "dur={dur}");
        }
    }

    /// 3-arg (sliding) form → boundary reject per ADR-022.
    #[test]
    fn render_window_boundary_rejects_three_arg_form() {
        let f = fcall(
            "window",
            vec![
                ts_col_ref("last_login"),
                str_lit("1 day"),
                str_lit("30 minutes"),
            ],
        );
        let err = render_function_call(&f, &empty_schema()).expect_err("three-arg reject");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "window",
            &["[TDCK-BOUNDARY]", "tumbling"],
        );
    }

    /// Compound / month duration → boundary reject.
    #[test]
    fn render_window_boundary_rejects_compound_duration() {
        for dur in &["1 day 3 hours", "1 month"] {
            let f = fcall("window", vec![ts_col_ref("ts"), str_lit(dur)]);
            let err = render_function_call(&f, &empty_schema()).expect_err("compound reject");
            expect_unsupported(
                err,
                UnsupportedKind::Function,
                "window",
                &["[TDCK-BOUNDARY]"],
            );
        }
    }

    /// Non-literal `args[1]` (e.g. column reference) → boundary reject —
    /// Spark's `F.window` accepts a compile-time string, not a runtime
    /// expression, and τ can only translate literals.
    #[test]
    fn render_window_boundary_rejects_non_literal_duration() {
        let f = fcall(
            "window",
            vec![ts_col_ref("last_login"), col_ref_expr("dur_col")],
        );
        let err = render_function_call(&f, &empty_schema()).expect_err("non-literal reject");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "window",
            &["[TDCK-BOUNDARY]", "string literal"],
        );
    }

    /// Empirical smoke: the emitted SQL must actually parse and execute
    /// against DuckDB (verifies `struct_pack("end" := ...)` accepts the
    /// quoted reserved keyword field name AND that GROUP BY on the struct
    /// value folds correctly). Uses a fresh in-memory connection (no
    /// extension dependency).
    #[test]
    fn render_window_sql_parses_and_groups_correctly_in_duckdb() {
        let conn = duckdb::Connection::open_in_memory().expect("in-memory conn");
        let ddl = "CREATE TABLE emp(ts TIMESTAMP);";
        conn.execute_batch(ddl).expect("ddl");
        let insert = "INSERT INTO emp VALUES \
                      (TIMESTAMP '2024-01-15 10:30:00'), \
                      (TIMESTAMP '2024-01-15 22:00:00'), \
                      (TIMESTAMP '2024-01-16 05:00:00');";
        conn.execute_batch(insert).expect("insert");
        // Emit the same SQL our arm produces, verbatim.
        let sql = "SELECT struct_pack(start := time_bucket(INTERVAL '1 day', ts), \
                   \"end\" := time_bucket(INTERVAL '1 day', ts) + INTERVAL '1 day')::VARCHAR AS w, \
                   COUNT(*) AS n FROM emp \
                   GROUP BY struct_pack(start := time_bucket(INTERVAL '1 day', ts), \
                   \"end\" := time_bucket(INTERVAL '1 day', ts) + INTERVAL '1 day') \
                   ORDER BY 1";
        let mut stmt = conn.prepare(sql).expect("prepare");
        let rows: Vec<(String, i64)> = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .expect("query_map")
            .map(|r| r.expect("row"))
            .collect();
        assert_eq!(rows.len(), 2, "expected two day-buckets, got {rows:?}");
        assert_eq!(rows[0].1, 2, "Jan 15 bucket count");
        assert_eq!(rows[1].1, 1, "Jan 16 bucket count");
        // The stringified struct must include both field names.
        assert!(rows[0].0.contains("start"), "struct repr: {}", rows[0].0);
        assert!(rows[0].0.contains("end"), "struct repr: {}", rows[0].0);
    }

    /// `dt-017` regression: `to_utc_timestamp(ts, 'CET')` — DuckDB has no
    /// `to_utc_timestamp` scalar. τ normalises the input to TIMESTAMPTZ,
    /// extracts naive UTC wall-clock, reinterprets in `tz`, extracts UTC
    /// naive wall-clock again. The `CAST(... AS TIMESTAMPTZ)` normalises
    /// TIMESTAMP-vs-TIMESTAMPTZ inputs uniformly.
    #[test]
    fn render_to_utc_timestamp_uses_timezone_composition() {
        let sql = render_fn(
            "to_utc_timestamp",
            vec![col_ref_expr("last_login"), str_lit("CET")],
        );
        assert_eq!(
            sql,
            "timezone('UTC', timezone('CET', timezone('UTC', CAST(last_login AS TIMESTAMPTZ))))"
        );
    }

    #[test]
    fn render_from_utc_timestamp_uses_timezone_composition() {
        let sql = render_fn(
            "from_utc_timestamp",
            vec![col_ref_expr("last_login"), str_lit("CET")],
        );
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
            expr_id: None,
        });
        let sql = render_fn("element_at", vec![map_col, str_lit("team")]);
        assert_eq!(sql, "element_at(attrs, 'team')[1]");
    }

    /// Pass 95 (`arr-008` ANSI throw) — `element_at(ARRAY, i)` wraps the
    /// underlying `list_extract` in a CASE that raises Spark's
    /// `INVALID_ARRAY_INDEX_IN_ELEMENT_AT` on OOB / index-0. The message
    /// must be byte-identical to Spark 4.1's runtime template (with
    /// runtime-interpolated `idx` and `len(arr)`).
    #[test]
    fn render_element_at_array_wraps_with_ansi_oob_guard() {
        let arr_col = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
            expr_id: None,
        });
        let sql = render_fn(
            "element_at",
            vec![
                arr_col,
                Expression::Literal(super::super::expression::Literal {
                    value: super::super::expression::LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
        );
        // Spark class token — the runtime classifier keys on this.
        assert!(
            sql.contains("[INVALID_ARRAY_INDEX_IN_ELEMENT_AT]"),
            "expected Spark class token, got: {sql}"
        );
        // Underlying extractor still routes to DuckDB's list_extract.
        assert!(
            sql.contains("list_extract((tags), (1))"),
            "expected list_extract fall-through in ELSE, got: {sql}"
        );
        // Verbatim message fragments (bracket the runtime substitutions).
        use super::super::spark_errors::{
            INVALID_ARRAY_INDEX_MSG_HEAD, INVALID_ARRAY_INDEX_MSG_MID, INVALID_ARRAY_INDEX_MSG_TAIL,
        };
        assert!(
            sql.contains(INVALID_ARRAY_INDEX_MSG_HEAD),
            "expected HEAD fragment, got: {sql}"
        );
        assert!(
            sql.contains(INVALID_ARRAY_INDEX_MSG_MID),
            "expected MID fragment, got: {sql}"
        );
        assert!(
            sql.contains(INVALID_ARRAY_INDEX_MSG_TAIL),
            "expected TAIL fragment, got: {sql}"
        );
        // Full guard shape (NULL short-circuit + idx=0/OOB throw + ELSE fall-through).
        assert_eq!(
            sql,
            "CASE WHEN (tags) IS NULL THEN NULL WHEN (1) = 0 OR abs((1)) > len((tags)) \
             THEN error('[INVALID_ARRAY_INDEX_IN_ELEMENT_AT] The index ' || (1)::VARCHAR \
             || ' is out of bounds. The array has ' || len((tags))::VARCHAR \
             || ' elements. Use `try_element_at` to tolerate accessing element at invalid index and return NULL instead. SQLSTATE: 22003') \
             ELSE list_extract((tags), (1)) END"
        );
    }

    /// Pass 95 — a positive integer literal is NOT provably in-bounds
    /// because the array length is only known at runtime (`tags` is a
    /// column, not an ArrayLiteral). The guard must still fire; do not
    /// short-circuit on `is_nonzero_literal`-style predicates.
    #[test]
    fn render_element_at_positive_literal_still_guarded_since_len_unknown() {
        let arr_col = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
            expr_id: None,
        });
        let sql = render_fn(
            "element_at",
            vec![
                arr_col,
                Expression::Literal(super::super::expression::Literal {
                    value: super::super::expression::LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
        );
        assert!(
            sql.starts_with("CASE WHEN"),
            "expected CASE guard even for positive literal, got: {sql}"
        );
        assert!(
            sql.contains("error("),
            "expected error() call inside guard, got: {sql}"
        );
    }

    /// Pass 95 — `try_element_at(arr, k)` is the never-throw alias; it
    /// emits bare `list_extract` without the ANSI guard so DuckDB's
    /// silent-NULL semantics for OOB propagate to the caller.
    #[test]
    fn render_try_element_at_omits_guard() {
        let arr_col = Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
            expr_id: None,
        });
        let sql = render_fn(
            "try_element_at",
            vec![
                arr_col,
                Expression::Literal(super::super::expression::Literal {
                    value: super::super::expression::LiteralValue::Int(1),
                    data_type: DataType::Integer,
                }),
            ],
        );
        assert_eq!(sql, "list_extract(tags, 1)");
        assert!(
            !sql.contains("error("),
            "try_element_at must NOT emit error() guard, got: {sql}"
        );
    }

    /// Pass 95 — the Map arm of `element_at` is untouched: it still emits
    /// the singleton-unwrap `element_at(MAP, key)[1]`. Spark does not throw
    /// on missing map keys (returns NULL); the ANSI OOB guard applies only
    /// to Array collections.
    #[test]
    fn render_element_at_map_unchanged() {
        let map_col = Expression::ColumnReference(ColumnReference {
            name: "attrs".to_owned(),
            qualifier: None,
            data_type: Some(DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            }),
            nullable: Some(true),
            expr_id: None,
        });
        let sql = render_fn("element_at", vec![map_col, str_lit("missing")]);
        assert_eq!(sql, "element_at(attrs, 'missing')[1]");
        assert!(
            !sql.contains("INVALID_ARRAY_INDEX_IN_ELEMENT_AT"),
            "Map arm must not carry Array ANSI guard, got: {sql}"
        );
    }

    /// `meta-003` regression — `typeof(x)` wraps in `lower(...)` for
    /// Spark-lowercase parity.
    #[test]
    fn render_typeof_wraps_in_lower_for_spark_case() {
        let sql = render_fn("typeof", vec![col_ref_expr("salary")]);
        assert_eq!(sql, "lower(typeof(salary))");
    }

    /// `map-006` regression — `map_concat` guards NULL on every arg so a
    /// NULL input propagates to a NULL result (Spark semantics).
    #[test]
    fn render_map_concat_propagates_null_across_all_args() {
        let sql = render_fn("map_concat", vec![col_ref_expr("m1"), col_ref_expr("m2")]);
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
        let sql = render_fn("array_append", vec![col_ref_expr("tags"), str_lit("new")]);
        assert_eq!(
            sql,
            "CASE WHEN (tags) IS NULL THEN NULL ELSE array_append(tags, 'new') END"
        );
    }

    /// `map-006` regression — `create_map` (wire name `"map"`) splits into
    /// `map(list_value(keys...), list_value(values...))`.
    #[test]
    fn render_create_map_splits_pairs_into_two_lists() {
        let sql = render_fn(
            "map",
            vec![str_lit("a"), str_lit("1"), str_lit("b"), str_lit("2")],
        );
        assert_eq!(sql, "map(list_value('a', 'b'), list_value('1', '2'))");
    }

    /// Pass 74 (`parse-005`) — Spark's `find_in_set(needle, csv)` returns
    /// the 1-based position of `needle` in `csv`, or 0 if not found.
    /// DuckDB has no `find_in_set`; emit
    /// `COALESCE(list_position(string_split(csv, ','), needle), 0)`.
    #[test]
    fn render_find_in_set_uses_list_position_over_split() {
        let sql = render_fn("find_in_set", vec![str_lit("rust"), col_ref_expr("tags")]);
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
        let sql = render_fn(
            "elt",
            vec![int_lit(2), str_lit("a"), str_lit("b"), str_lit("c")],
        );
        assert_eq!(sql, "(['a', 'b', 'c'])[2]");
    }

    /// Pass 74 (`cond-010`) — Spark's `isnan(x)` schema is BOOLEAN
    /// non-nullable; DuckDB's `isnan(NULL)` returns NULL. Wrap in
    /// `COALESCE(..., FALSE)` to preserve the non-null semantics.
    #[test]
    fn render_isnan_wraps_in_coalesce_false() {
        let sql = render_fn("isnan", vec![col_ref_expr("score")]);
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
            expr_id: None,
        });
        let sql = render_fn("concat_ws", vec![str_lit(","), arr_col]);
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
        let sql = render_fn("concat", vec![col_ref_expr("name"), null_lit]);
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
        let sql = render_fn("parse_url", vec![url, part]);
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
        let sql = render_fn("parse_url", vec![url, part, key]);
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
            value: LiteralValue::Double(12.75),
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
        let schema = Schema::minted(StructType::new(vec![
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
        ]));
        let expr = Expression::Binary(BinaryExpression {
            left: Box::new(Expression::ColumnReference(ColumnReference {
                name: "d1".to_owned(),
                qualifier: None,
                data_type: Some(DataType::Decimal {
                    precision: 10,
                    scale: 2,
                }),
                nullable: Some(true),
                expr_id: None,
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
                expr_id: None,
            })),
        });
        let sql = render_expr(&expr, &schema).expect("render decimal div");
        assert!(
            sql.contains("spark_decimal_div"),
            "expected spark_decimal_div, got: {sql}"
        );
    }

    /// Pass 12 — `spark_decimal_div` operands must be CAST to their declared
    /// `DECIMAL(p,s)`: the analyzer types both operands Decimal, but a
    /// DuckDB-native aggregate (e.g. windowed `avg` over DECIMAL) emits
    /// DOUBLE at runtime, and the extension rejects DOUBLE args. Corpus:
    /// tpcds-q047/q053/q057/q063/q089.
    #[test]
    fn decimal_div_casts_operands_to_declared_decimal() {
        let l = decimal_lit("1.23", 10, 2);
        let r = decimal_lit("4.56", 6, 3);
        let b = BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(l),
            right: Box::new(r),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert_eq!(
            sql,
            "spark_decimal_div(CAST((CAST('1.23' AS DECIMAL(10, 2))) AS DECIMAL(10, 2)), \
             CAST((CAST('4.56' AS DECIMAL(6, 3))) AS DECIMAL(6, 3)))"
        );
    }

    /// tpcds-q066 — a `Div` with one Decimal operand and one plain integral
    /// (BIGINT-typed) column operand (`sum(decimal_expr) / w_warehouse_sq_ft`)
    /// must ALSO route to `spark_decimal_div`. DuckDB's native `DECIMAL /
    /// BIGINT` yields DOUBLE (unlike `+`/`-`/`*`, which stay DECIMAL),
    /// diverging from the analyzer's declared Decimal result type (Spark
    /// widens the integral operand to Decimal before dividing). N4 moved
    /// this widening from a `render_binary`-local re-derivation into the
    /// analyzer's `materialize_binary_coercions` pass, which inserts the
    /// implicit widening CAST onto the tree itself — mirror that here by
    /// materializing before rendering (`render_binary`/`render_expr` no
    /// longer re-derive the widening; they only read operand `data_type`).
    /// Corpus: tpcds-q066.
    ///
    /// Also pins the no-double-cast rule: the widened side is ALREADY a
    /// materialized `Cast` to its exact declared DECIMAL(p,s), so
    /// `render_binary`'s decimal-Div routing renders it bare (no outer
    /// re-wrap) — the only expectation delta from the pre-N4 string is the
    /// dropped inner parens around the (no-longer-double-cast) identifier.
    #[test]
    fn decimal_div_by_integral_column_routes_through_spark_decimal_div() {
        let schema = Schema::minted(StructType::new(vec![
            StructField::nullable(
                "jan_sales",
                DataType::Decimal {
                    precision: 38,
                    scale: 2,
                },
            ),
            StructField::nullable("w_warehouse_sq_ft", DataType::Long),
        ]));
        let l = col_with_type(
            "jan_sales",
            DataType::Decimal {
                precision: 38,
                scale: 2,
            },
        );
        let r = col_with_type("w_warehouse_sq_ft", DataType::Long);
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(l),
            right: Box::new(r),
        });
        let materialized = materialize_binary_coercions(expr, &schema);
        let Expression::Binary(mb) = &materialized else {
            panic!("expected Binary, got {materialized:?}");
        };
        assert!(
            matches!(mb.right.as_ref(), Expression::Cast(c) if c.implicit),
            "the integral side must be widened via an implicit N4 Cast: {mb:?}"
        );
        let sql = render_expr(&materialized, &schema).expect("render");
        assert_eq!(
            sql,
            "spark_decimal_div(CAST((jan_sales) AS DECIMAL(38, 2)), \
             CAST(w_warehouse_sq_ft AS DECIMAL(20, 0)))",
            "the already-materialized side renders bare (no double CAST), \
             the native side keeps its pre-N4 parenthesization"
        );
    }

    /// Pass 12 — a `Div` whose operands are not both analyzer-Decimal must
    /// NOT be routed to `spark_decimal_div`; it renders the plain operator
    /// (with the existing ANSI divide-by-zero guard, skipped here since the
    /// divisor is a nonzero literal). Guards against over-routing.
    #[test]
    fn div_non_decimal_operands_render_plain_slash() {
        let b = BinaryExpression {
            op: BinaryOp::Div,
            left: Box::new(double_lit(6.0)),
            right: Box::new(double_lit(2.0)),
        };
        let sql = render_binary(&b, &empty_schema()).expect("render");
        assert!(
            !sql.contains("spark_decimal_div"),
            "expected plain division, got: {sql}"
        );
        assert_eq!(sql, "(CAST(6.0 AS DOUBLE)) / (CAST(2.0 AS DOUBLE))");
    }

    // ── Date ± Interval arithmetic returns DATE (root cause: 026) ──────────
    //
    // DuckDB promotes `DATE ± INTERVAL` to TIMESTAMP; Spark's Date ± Interval
    // stays DATE (`binary_data_type` in expression.rs preserves the
    // date-like side). N4 moved the corrective CAST from `render_binary`
    // itself into the analyzer's `materialize_binary_coercions` pass, which
    // wraps the WHOLE node in an implicit `Cast` rendered by `render_cast` —
    // these tests now route the raw `Binary` through the materializer before
    // rendering (via `render_expr`, the top-level dispatcher), mirroring the
    // real `resolve_and_stamp` pipeline. Only ever fires when the inferred
    // type is actually Date, never for a Timestamp-base interval add (which
    // correctly infers Timestamp).

    #[test]
    fn render_binary_date_plus_interval_casts_to_date() {
        let i = IntervalExpression {
            months: 0,
            days: 5,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        };
        let b = BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(col_with_type("d", DataType::Date)),
            right: Box::new(Expression::Interval(i)),
        };
        let materialized = materialize_binary_coercions(Expression::Binary(b), &empty_schema());
        let sql = render_expr(&materialized, &empty_schema()).expect("render");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected a DATE cast wrapping the addition, got: {sql}"
        );
    }

    #[test]
    fn render_binary_interval_plus_date_casts_to_date() {
        // Same rule with the operands reversed (`INTERVAL + DATE`).
        let i = IntervalExpression {
            months: 1,
            days: 0,
            microseconds: 0,
            kind: IntervalKind::YearMonth,
        };
        let b = BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(Expression::Interval(i)),
            right: Box::new(col_with_type("d", DataType::Date)),
        };
        let materialized = materialize_binary_coercions(Expression::Binary(b), &empty_schema());
        let sql = render_expr(&materialized, &empty_schema()).expect("render");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected a DATE cast wrapping the addition, got: {sql}"
        );
    }

    #[test]
    fn render_binary_date_minus_interval_casts_to_date() {
        let i = IntervalExpression {
            months: 0,
            days: 3,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        };
        let b = BinaryExpression {
            op: BinaryOp::Sub,
            left: Box::new(col_with_type("d", DataType::Date)),
            right: Box::new(Expression::Interval(i)),
        };
        let materialized = materialize_binary_coercions(Expression::Binary(b), &empty_schema());
        let sql = render_expr(&materialized, &empty_schema()).expect("render");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected a DATE cast wrapping the subtraction, got: {sql}"
        );
    }

    /// No-regression guard: a Timestamp-base interval add must NOT get the
    /// DATE cast — `binary_data_type` infers `Timestamp` here (not `Date`),
    /// so the currently-GREEN `test_day_time_interval_*` / timestamp-base
    /// interval corpus cases stay untouched. Routed through the materializer
    /// too, for parity with the Date-shape tests above — it must be a no-op.
    #[test]
    fn render_binary_timestamp_plus_interval_is_not_cast_to_date() {
        let i = IntervalExpression {
            months: 0,
            days: 5,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        };
        let b = BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(ts_col_ref("ts")),
            right: Box::new(Expression::Interval(i)),
        };
        let before = Expression::Binary(b.clone());
        let materialized = materialize_binary_coercions(Expression::Binary(b), &empty_schema());
        assert_eq!(
            materialized, before,
            "a Timestamp-base interval add must not be touched by N4's Date rule"
        );
        let sql = render_expr(&materialized, &empty_schema()).expect("render");
        assert!(
            !sql.contains("AS DATE"),
            "Timestamp ± Interval must not be cast to DATE, got: {sql}"
        );
    }

    /// N4 byte-identity pin: routing the DATE-cast correction through the
    /// analyzer's `materialize_binary_coercions` + `render_cast` must
    /// produce the EXACT same SQL string the old `render_binary`-local guard
    /// used to build directly (`format!("CAST({inner} AS DATE)")`) — proves
    /// the migration didn't perturb wire output byte-for-byte.
    #[test]
    fn render_binary_date_plus_interval_n4_pipeline_is_byte_identical_to_legacy_guard() {
        let i = IntervalExpression {
            months: 0,
            days: 5,
            microseconds: 0,
            kind: IntervalKind::Calendar,
        };
        let b = BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(col_with_type("d", DataType::Date)),
            right: Box::new(Expression::Interval(i)),
        };
        let inner = render_binary(&b, &empty_schema()).expect("render inner");
        let materialized = materialize_binary_coercions(Expression::Binary(b), &empty_schema());
        let sql = render_expr(&materialized, &empty_schema()).expect("render materialized");
        assert_eq!(sql, format!("CAST({inner} AS DATE)"));
    }

    /// N3 ∘ N4 composition: `date_add(d, 1) + INTERVAL '1' DAY` nests an
    /// N3-typed function (`date_add`, whose own `render_function_call` wrap
    /// already supplies its OWN corrective `CAST(.. AS DATE)` via
    /// `needs_date_return_cast`) under an N4 `Binary` (whose LEFT side is
    /// therefore Date-typed, so the outer Add ALSO needs the Date-preserving
    /// correction). Both layers' casts are legitimate and independent — one
    /// per level, not a redundant double-wrap of either node.
    #[test]
    fn render_binary_date_add_function_plus_interval_composes_n3_and_n4_single_casts_each() {
        let schema = Schema::minted(StructType::new(vec![StructField::nullable(
            "d",
            DataType::Date,
        )]));
        let date_add_call = fexpr(
            "date_add",
            vec![col_with_type("d", DataType::Date), int_lit(1)],
        );
        let outer = Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(date_add_call),
            right: Box::new(Expression::Interval(IntervalExpression {
                months: 0,
                days: 1,
                microseconds: 0,
                kind: IntervalKind::Calendar,
            })),
        });
        let materialized = materialize_binary_coercions(outer, &schema);
        let Expression::Cast(outer_cast) = &materialized else {
            panic!("expected N4 to wrap the outer Add in an implicit Cast, got {materialized:?}");
        };
        assert!(outer_cast.implicit);
        assert_eq!(outer_cast.to_type, DataType::Date);
        // N4 must not have touched the inner `date_add(...)` FunctionCall —
        // it stays exactly as built, opaque to the Binary-only materializer.
        let Expression::Binary(inner_binary) = outer_cast.expr.as_ref() else {
            panic!("expected the Cast to wrap the original Binary unchanged");
        };
        assert!(matches!(
            inner_binary.left.as_ref(),
            Expression::FunctionCall(f) if f.name == "date_add"
        ));
        let sql = render_expr(&materialized, &schema).expect("render");
        assert_eq!(
            sql.matches("AS DATE)").count(),
            2,
            "expected exactly two Date corrections (N3's on date_add, N4's on the outer Add), \
             got: {sql}"
        );
        assert!(
            !sql.contains("AS DATE) AS DATE)"),
            "the two corrections must not stack as an adjacent redundant double-cast, got: {sql}"
        );
    }

    // ── add_months / date_add / date_sub return DATE (root cause: 026) ─────
    //
    // DuckDB's `DATE + INTERVAL n MONTH|DAY` promotes to TIMESTAMP; Spark's
    // `add_months`/`date_add`/`date_sub` always return DATE. Corpus:
    // test_date_add, test_date_sub, test_add_months.

    #[test]
    fn add_months_casts_result_to_date() {
        let f = fcall(
            "add_months",
            vec![col_with_type("d", DataType::Date), int_lit(1)],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render add_months");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected add_months to CAST its result to DATE, got: {sql}"
        );
        assert!(sql.contains("INTERVAL"), "got: {sql}");
    }

    #[test]
    fn date_add_casts_result_to_date() {
        let f = fcall(
            "date_add",
            vec![col_with_type("d", DataType::Date), int_lit(5)],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render date_add");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected date_add to CAST its result to DATE, got: {sql}"
        );
    }

    #[test]
    fn date_sub_casts_result_to_date() {
        let f = fcall(
            "date_sub",
            vec![col_with_type("d", DataType::Date), int_lit(5)],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render date_sub");
        assert!(
            sql.starts_with("CAST(") && sql.ends_with("AS DATE)"),
            "expected date_sub to CAST its result to DATE, got: {sql}"
        );
    }

    // ── trunc(date, fmt) returns DATE (Pass N3) ─────────────────────────
    //
    // DuckDB's `date_trunc(fmt, date)` natively returns TIMESTAMP; Spark's
    // 2-arg `trunc(date, fmt)` always returns DATE. The
    // `render_function_call` wrapper's `needs_date_return_cast` roster
    // supplies the corrective CAST — the arm body itself is unchanged.

    #[test]
    fn trunc_date_casts_result_to_date() {
        let f = fcall(
            "trunc",
            vec![col_with_type("d", DataType::Date), str_lit("month")],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render trunc");
        assert_eq!(sql, "CAST(date_trunc('month', d) AS DATE)");
    }

    #[test]
    fn trunc_one_arg_not_cast() {
        // 1-arg `trunc` is not the date-truncation form (`needs_date_return_cast`
        // gates on `f.args.len() == 2`) — must not get the DATE cast.
        let f = fcall("trunc", vec![col_with_type("d", DataType::Date)]);
        let sql = render_function_call(&f, &empty_schema()).expect("render trunc");
        assert!(
            !sql.contains("AS DATE"),
            "1-arg trunc must not get the DATE cast, got: {sql}"
        );
    }

    /// `last_day` is deliberately excluded from `needs_date_return_cast`'s
    /// roster: DuckDB's native `last_day(DATE)` already returns DATE (no
    /// TIMESTAMP promotion to correct for — verified directly against
    /// DuckDB). Pin the exclusion so a future roster edit doesn't
    /// double-cast an already-correct result.
    #[test]
    fn last_day_passes_through_uncast() {
        let f = fcall("last_day", vec![col_with_type("d", DataType::Date)]);
        let sql = render_function_call(&f, &empty_schema()).expect("render last_day");
        assert_eq!(sql, "last_day(d)");
    }

    /// The DATE cast supplied by the `render_function_call` wrapper must
    /// survive when the call is nested inside another expression (not just
    /// when rendered standalone) — pins the wrapper against a future
    /// refactor that only casts top-level calls.
    #[test]
    fn date_add_nested_in_comparison_keeps_cast() {
        let date_add_call = fexpr(
            "date_add",
            vec![col_with_type("d", DataType::Date), int_lit(5)],
        );
        let date_literal = Expression::Literal(Literal {
            value: LiteralValue::Date(20103),
            data_type: DataType::Date,
        });
        let expr = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(date_add_call),
            right: Box::new(date_literal),
        });
        let sql = render_expr(&expr, &empty_schema()).expect("render");
        assert!(
            sql.contains("CAST((d + INTERVAL (5) DAY) AS DATE)"),
            "date_add's DATE cast must survive nested inside a comparison, got: {sql}"
        );
    }

    // ── DATE_RETURNING_FNS mechanical audit (Pass N3) ────────────────────
    //
    // `needs_date_return_cast`'s divergence roster and
    // `type_inference::DATE_RETURNING_FNS` (the Date-typed function roster)
    // must agree in substance: every function Spark types as Date must
    // ALSO render to SQL that DuckDB itself types as DATE — whether via τ's
    // own corrective CAST, or because the DuckDB-native substrate form
    // already returns DATE. This test executes the rendered SQL against a
    // real DuckDB connection (rather than pattern-matching strings), so it
    // mechanically catches any future roster/emission drift.

    /// One test sample per `DATE_RETURNING_FNS` entry.
    enum DateReturnSample {
        /// A scalar `FunctionCall` with literal (self-contained) args —
        /// render via `render_function_call` and execute
        /// `SELECT typeof(<sql>)` against a fresh in-memory connection.
        Expr(FunctionCall),
        /// A session-registered macro (`runtime::session::NEXT_DAY_MACRO_SQL`)
        /// that is not visible to a bare `duckdb::Connection` — its
        /// DATE-return contract is pinned instead by
        /// `runtime::session::tests::next_day_returns_date_not_timestamp`
        /// (via `conn_with_next_day`).
        SessionMacro,
    }

    fn date_return_sample_for(name: &str) -> Option<DateReturnSample> {
        let date_lit = || {
            Expression::Literal(Literal {
                value: LiteralValue::Date(20103),
                data_type: DataType::Date,
            })
        };
        match name {
            "add_months" => Some(DateReturnSample::Expr(fcall(
                "add_months",
                vec![date_lit(), int_lit(1)],
            ))),
            "current_date" => Some(DateReturnSample::Expr(fcall("current_date", vec![]))),
            "date_add" => Some(DateReturnSample::Expr(fcall(
                "date_add",
                vec![date_lit(), int_lit(5)],
            ))),
            "date_sub" => Some(DateReturnSample::Expr(fcall(
                "date_sub",
                vec![date_lit(), int_lit(5)],
            ))),
            "last_day" => Some(DateReturnSample::Expr(fcall("last_day", vec![date_lit()]))),
            "make_date" => Some(DateReturnSample::Expr(fcall(
                "make_date",
                vec![int_lit(2024), int_lit(1), int_lit(1)],
            ))),
            "next_day" => Some(DateReturnSample::SessionMacro),
            "to_date" => Some(DateReturnSample::Expr(fcall(
                "to_date",
                vec![str_lit("2024-01-01")],
            ))),
            "trunc" => Some(DateReturnSample::Expr(fcall(
                "trunc",
                vec![date_lit(), str_lit("month")],
            ))),
            _ => None,
        }
    }

    #[test]
    fn date_typed_functions_return_date_in_duckdb() {
        let conn = duckdb::Connection::open_in_memory().expect("in-memory conn");
        for &name in crate::transpiler_v2::type_inference::DATE_RETURNING_FNS {
            // Completeness: every roster entry must have a test sample.
            let sample = date_return_sample_for(name).unwrap_or_else(|| {
                panic!(
                    "DATE_RETURNING_FNS entry {name:?} has no test sample — add one in \
                     `date_return_sample_for` (emission.rs)"
                )
            });
            let f = match sample {
                DateReturnSample::SessionMacro => continue,
                DateReturnSample::Expr(f) => f,
            };
            let sql = render_function_call(&f, &empty_schema()).expect("render");
            let query = format!("SELECT typeof({sql})");
            let type_name: String = conn
                .query_row(&query, [], |row| row.get(0))
                .unwrap_or_else(|e| panic!("query `{query}` failed for {name:?}: {e}"));
            assert!(
                type_name.eq_ignore_ascii_case("date"),
                "{name} rendered SQL `{sql}` must produce DATE in DuckDB, got {type_name}"
            );
        }
    }

    // ── split(str, pattern[, limit]) limit semantics ────────────────────
    //
    // Spark's 3-arg `split` caps the result at `limit` elements (limit > 0)
    // or behaves unlimited (limit <= 0). Verified live against Spark 4.1.1
    // and DuckDB (`split`/`list_slice`/`array_to_string`). Corpus:
    // test_split_with_limit, test_split_with_limit_dataframe_api.

    #[test]
    fn split_2arg_unchanged() {
        let f = fcall(
            "split",
            vec![col_with_type("val", DataType::String), str_lit("-")],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render split");
        assert_eq!(sql, "split(val, '-')");
    }

    #[test]
    fn split_3arg_positive_limit_caps_and_rejoins_remainder() {
        let f = fcall(
            "split",
            vec![
                col_with_type("val", DataType::String),
                str_lit("-"),
                int_lit(2),
            ],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render split");
        // Unlimited split is capped at `limit` elements; the tail is the
        // delimiter-rejoined remainder (not re-split).
        assert!(
            sql.contains("list_slice(split(val, '-'), 1, (2) - 1)"),
            "got: {sql}"
        );
        assert!(
            sql.contains(
                "array_to_string(list_slice(split(val, '-'), (2), len(split(val, '-'))), '-')"
            ),
            "got: {sql}"
        );
        // Below the limit, falls through to the plain unlimited split.
        assert!(
            sql.contains("len(split(val, '-')) <= (2) THEN split(val, '-')"),
            "got: {sql}"
        );
    }

    #[test]
    fn split_3arg_nonpositive_limit_is_unlimited() {
        let f = fcall(
            "split",
            vec![
                col_with_type("val", DataType::String),
                str_lit("-"),
                int_lit(-1),
            ],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render split");
        assert!(sql.contains("(-1) <= 0 OR"), "got: {sql}");
    }

    #[test]
    fn split_3arg_null_args_propagate_null() {
        let f = fcall(
            "split",
            vec![
                col_with_type("val", DataType::String),
                str_lit("-"),
                int_lit(2),
            ],
        );
        let sql = render_function_call(&f, &empty_schema()).expect("render split");
        assert!(
            sql.starts_with("CASE WHEN (val) IS NULL OR ('-') IS NULL OR (2) IS NULL THEN NULL"),
            "got: {sql}"
        );
    }

    fn decimal_col(name: &str, precision: u8, scale: u8) -> Expression {
        col_with_type(name, DataType::Decimal { precision, scale })
    }

    /// `max_by(name, val)` renders to DuckDB's native `arg_max(name, val)` —
    /// same 2-arg (value, ordering) shape, name rename only.
    #[test]
    fn max_by_renders_to_arg_max() {
        let f = fcall(
            "max_by",
            vec![
                col_with_type("name", DataType::String),
                col_with_type("val", DataType::Integer),
            ],
        );
        let sql = render_aggregate(&f, &empty_schema()).expect("render max_by");
        assert_eq!(sql, "arg_max(name, val)");
    }

    /// `min_by(name, val)` renders to DuckDB's native `arg_min(name, val)`.
    #[test]
    fn min_by_renders_to_arg_min() {
        let f = fcall(
            "min_by",
            vec![
                col_with_type("name", DataType::String),
                col_with_type("val", DataType::Integer),
            ],
        );
        let sql = render_aggregate(&f, &empty_schema()).expect("render min_by");
        assert_eq!(sql, "arg_min(name, val)");
    }

    /// `test_count_distinct_multiple_columns`: Spark's
    /// `count(DISTINCT a, b)` counts distinct (a, b) tuples, skipping any
    /// row where either argument is NULL. DuckDB's `count` rejects >1
    /// non-ROW argument outright (Binder error), and a bare
    /// `count(DISTINCT (a, b))` ROW-tuple would over-count NULL-bearing
    /// rows (DuckDB ROWs are non-NULL even with NULL fields). τ must emit
    /// the NULL-guarded CASE form. Verified against live Spark 4.1.1 and
    /// the DuckDB binary for both all-non-null and mixed-null inputs.
    #[test]
    fn count_distinct_multi_arg_renders_null_guarded_tuple() {
        let f = FunctionCall {
            name: "count".to_owned(),
            args: vec![
                col_with_type("name", DataType::String),
                col_with_type("value", DataType::Integer),
            ],
            distinct: true,
        };
        let sql = render_aggregate(&f, &empty_schema()).expect("render count(DISTINCT a, b)");
        assert_eq!(
            sql,
            "count(DISTINCT CASE WHEN name IS NULL OR value IS NULL THEN NULL ELSE (name, value) END)"
        );
    }

    /// Single-arg `count(DISTINCT x)` is untouched by the multi-arg guard —
    /// DuckDB's own NULL-skip on a scalar argument already matches Spark.
    #[test]
    fn count_distinct_single_arg_unaffected_by_tuple_guard() {
        let f = FunctionCall {
            name: "count".to_owned(),
            args: vec![col_with_type("value", DataType::Integer)],
            distinct: true,
        };
        let sql = render_aggregate(&f, &empty_schema()).expect("render count(DISTINCT x)");
        assert_eq!(sql, "count(DISTINCT value)");
    }

    /// Pass 13 — `avg`/`mean` over a DECIMAL argument routes through the
    /// ext6 extension's `spark_avg` (native DECIMAL) instead of DuckDB's
    /// native `avg` (widens DECIMAL to DOUBLE), wrapped in the Spark-parity
    /// outer CAST to the analyzer-declared `AvgLike` type — DECIMAL(9,2) →
    /// DECIMAL(13,6). Corpus: tpcds-q047/q053/q057/q063/q089, agg-024.
    #[test]
    fn avg_of_decimal_routes_through_spark_avg() {
        let f = fcall("avg", vec![decimal_col("bonus", 9, 2)]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render avg(decimal)");
        assert!(
            sql.contains("spark_avg("),
            "expected spark_avg(, got: {sql}"
        );
        assert!(
            sql.contains("AS DECIMAL(13, 6)"),
            "expected AS DECIMAL(13, 6), got: {sql}"
        );
    }

    /// Pass 13 — `mean` is Spark's alias for `avg`; same decimal routing.
    #[test]
    fn mean_of_decimal_routes_through_spark_avg() {
        let f = fcall("mean", vec![decimal_col("bonus", 9, 2)]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render mean(decimal)");
        assert!(
            sql.contains("spark_avg("),
            "expected spark_avg(, got: {sql}"
        );
        assert!(
            sql.contains("AS DECIMAL(13, 6)"),
            "expected AS DECIMAL(13, 6), got: {sql}"
        );
    }

    /// Pass 13 — windowed decimal `avg` must wrap the WHOLE `spark_avg(...)
    /// OVER (...)` expression in the outer CAST (`CAST(...) OVER (...)` is
    /// invalid SQL), unlike the generic window path which appends OVER
    /// after an already-rendered function.
    #[test]
    fn windowed_decimal_avg_wraps_spark_avg_over() {
        use crate::transpiler_v2::expression::WindowFunction;
        let w = WindowFunction {
            func: Box::new(Expression::FunctionCall(fcall(
                "avg",
                vec![decimal_col("bonus", 9, 2)],
            ))),
            partition_by: vec![col_with_type("k", DataType::Integer)],
            order_by: vec![],
            frame: None,
        };
        let sql = render_window(&w, &empty_schema()).expect("render windowed avg(decimal)");
        assert_eq!(
            sql,
            "CAST(spark_avg(bonus) OVER (PARTITION BY k) AS DECIMAL(13, 6))"
        );
    }

    /// Pass 13 — `avg(DISTINCT d)` must propagate `DISTINCT` into
    /// `spark_avg(DISTINCT ...)`, not drop it.
    #[test]
    fn distinct_decimal_avg_propagates_distinct() {
        let mut f = fcall("avg", vec![decimal_col("bonus", 9, 2)]);
        f.distinct = true;
        let sql = render_aggregate(&f, &empty_schema()).expect("render avg(distinct decimal)");
        assert!(
            sql.contains("spark_avg(DISTINCT "),
            "expected spark_avg(DISTINCT , got: {sql}"
        );
    }

    /// Pass 13 (negative) — `avg` over a non-DECIMAL (DOUBLE) argument must
    /// stay on DuckDB's native `avg`, guarding the decimal-only routing
    /// predicate against over-firing on integer/float `avg`.
    #[test]
    fn avg_of_double_stays_native() {
        let f = fcall("avg", vec![col_with_type("salary", DataType::Double)]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render avg(double)");
        assert!(
            !sql.contains("spark_avg"),
            "avg(DOUBLE) must stay native, got: {sql}"
        );
        assert_eq!(sql, "avg(salary)");
    }

    /// Pass 13 (negative) — `avg` over an INTEGER argument must also stay on
    /// DuckDB's native `avg` (Spark's AvgLike over a non-decimal is DOUBLE,
    /// which DuckDB `avg` already yields): the decimal-only predicate must not
    /// fire on integer `avg`.
    #[test]
    fn avg_of_integer_stays_native() {
        let f = fcall("avg", vec![col_with_type("cnt", DataType::Integer)]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render avg(int)");
        assert!(
            !sql.contains("spark_avg"),
            "avg(INT) must stay native, got: {sql}"
        );
        assert_eq!(sql, "avg(cnt)");
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
        let f = fcall("percentile_approx", vec![col_ref_expr("salary"), q_lit]);
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

    /// Pass 124 — Spark's `percentile(col, p)` is the exact CONTINUOUS
    /// (linear-interpolation) quantile → DuckDB `quantile_cont`, distinct
    /// from `percentile_approx` (discrete `quantile_disc`). Corpus: agg-019.
    #[test]
    fn render_percentile_uses_quantile_cont() {
        let q_lit = Expression::Literal(Literal {
            value: LiteralValue::Double(0.5),
            data_type: DataType::Double,
        });
        let f = fcall("percentile", vec![col_ref_expr("salary"), q_lit]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render percentile");
        assert!(
            sql.contains("quantile_cont"),
            "expected quantile_cont, got: {sql}"
        );
        assert!(
            sql.contains("CAST(") && sql.contains("AS DOUBLE"),
            "expected CAST(... AS DOUBLE), got: {sql}"
        );
        assert!(
            !sql.contains("quantile_disc"),
            "must not use quantile_disc (that is percentile_approx), got: {sql}"
        );
    }

    /// Pass 124 — `collect_list` passes through verbatim to the session
    /// macro (`LIST(x) FILTER (WHERE x IS NOT NULL)`). Corpus: agg-018.
    #[test]
    fn render_collect_list_passes_through() {
        let f = fcall("collect_list", vec![col_ref_expr("name")]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render collect_list");
        assert!(
            sql.contains("collect_list("),
            "expected verbatim collect_list(, got: {sql}"
        );
    }

    /// Pass 124 — `collect_set` passes through verbatim; the DISTINCT lives
    /// inside the session macro, so the emitted SQL carries no DISTINCT
    /// token itself. Corpus: agg-018.
    #[test]
    fn render_collect_set_passes_through() {
        let f = fcall("collect_set", vec![col_ref_expr("name")]);
        let sql = render_aggregate(&f, &empty_schema()).expect("render collect_set");
        assert!(
            sql.contains("collect_set("),
            "expected verbatim collect_set(, got: {sql}"
        );
        assert!(
            !sql.to_ascii_uppercase().contains("DISTINCT"),
            "collect_set macro owns the DISTINCT; emission must not add it, got: {sql}"
        );
    }

    /// Pass 76 — Spark's `url_encode(s)` uses form-urlencoded (spaces → `+`),
    /// but DuckDB's `url_encode(s)` emits `%20`. τ post-substitutes so the
    /// bytes match Spark. Corpus witness: `parse-002`.
    #[test]
    fn render_url_encode_form_urlencoded_substitutes_space() {
        let sql = render_fn(
            "url_encode",
            vec![Expression::Literal(Literal {
                value: LiteralValue::String("a b&c".to_owned()),
                data_type: DataType::String,
            })],
        );
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
        let sql = render_fn(
            "url_decode",
            vec![Expression::Literal(Literal {
                value: LiteralValue::String("a+b%26c".to_owned()),
                data_type: DataType::String,
            })],
        );
        assert!(sql.contains("url_decode(replace("), "got: {sql}");
        assert!(sql.contains("'+'") && sql.contains("'%20'"), "got: {sql}");
    }

    /// Pass 76 — `try_to_number(str, '999.99')` derives `DECIMAL(5, 2)` from
    /// the literal format template and emits `try_cast(... AS DECIMAL(5, 2))`.
    /// Corpus witness: `parse-004`.
    #[test]
    fn render_try_to_number_emits_try_cast_decimal() {
        let sql = render_fn(
            "try_to_number",
            vec![
                col_ref_expr("num_str"),
                Expression::Literal(Literal {
                    value: LiteralValue::String("999.99".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
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

    /// Pass 87 — `from_csv(csv_str, "qty INT, label STRING, price DOUBLE")`
    /// emits a per-field `split_part` synthesis wrapped in a NULL guard so
    /// a NULL input yields a NULL struct (not a struct-of-NULLs).
    /// Corpus witness: `json-007`.
    #[test]
    fn render_from_csv_emits_split_part_struct_pack_with_null_guard() {
        let sql = render_fn(
            "from_csv",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "csv_str".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::String),
                    nullable: Some(true),
                    expr_id: None,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("qty INT, label STRING, price DOUBLE".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
        // NULL guard on the entire input.
        assert!(
            sql.starts_with("CASE WHEN (csv_str) IS NULL THEN NULL ELSE struct_pack("),
            "got: {sql}"
        );
        // Per-field synthesis: numerics get try_cast + nullif; strings get nullif only.
        assert!(
            sql.contains("qty := try_cast(nullif(split_part(csv_str, ',', 1), '') AS INTEGER)"),
            "got: {sql}"
        );
        assert!(
            sql.contains("label := nullif(split_part(csv_str, ',', 2), '')"),
            "got: {sql}"
        );
        assert!(
            sql.contains("price := try_cast(nullif(split_part(csv_str, ',', 3), '') AS DOUBLE)"),
            "got: {sql}"
        );
    }

    /// Pass 87 — DDL parsed into a flat `StructType` for τ's projection
    /// schema inference. Nested composite types are rejected (Spark's own
    /// `from_csv` accepts only flat primitives).
    #[test]
    fn from_csv_ddl_resolves_to_flat_struct_type() {
        let st = from_csv_ddl_to_struct("qty INT, label STRING, price DOUBLE").unwrap();
        assert_eq!(st.fields.len(), 3);
        assert_eq!(st.fields[0].name, "qty");
        assert_eq!(st.fields[0].data_type, DataType::Integer);
        assert_eq!(st.fields[1].name, "label");
        assert_eq!(st.fields[1].data_type, DataType::String);
        assert_eq!(st.fields[2].name, "price");
        assert_eq!(st.fields[2].data_type, DataType::Double);
        // Nested composite types → None (Spark's from_csv rejects them).
        assert!(from_csv_ddl_to_struct("a STRUCT<b:INT>").is_none());
        assert!(from_csv_ddl_to_struct("a ARRAY<INT>").is_none());
    }

    /// Pass 87 review M2 — Spark's `from_csv(csv_str, schema_ddl, options_map)`
    /// three-arg options form is a Thunderduck-boundary error. Prior to the
    /// fix, the arm's `if f.args.len() == 2` guard silently declined to match
    /// and DuckDB got literal `from_csv(...)` back, producing an opaque
    /// scalar-not-found error. Now the `!= 2` arm emits a τ-boundary
    /// `UnsupportedFunction` upfront.
    #[test]
    fn render_from_csv_three_arg_is_boundary_error() {
        let f = fcall(
            "from_csv",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "csv_str".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::String),
                    nullable: Some(true),
                    expr_id: None,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("qty INT, label STRING".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("sep=,".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
        let err = render_function_call(&f, &empty_schema()).expect_err("expected boundary error");
        expect_unsupported(err, UnsupportedKind::Function, "from_csv", &["options-map"]);
    }

    /// Pass 87 review M2 — Spark's `from_json(json_str, schema_ddl, options_map)`
    /// three-arg options form is likewise a Thunderduck-boundary error. The
    /// `!= 2` arm intercepts before the fallthrough could hand DuckDB a
    /// literal `from_json(...)` with an unrecognized options arg.
    #[test]
    fn render_from_json_three_arg_is_boundary_error() {
        let f = fcall(
            "from_json",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "json_str".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::String),
                    nullable: Some(true),
                    expr_id: None,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("a INT, b STRING".to_owned()),
                    data_type: DataType::String,
                }),
                Expression::Literal(Literal {
                    value: LiteralValue::String("mode=PERMISSIVE".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
        let err = render_function_call(&f, &empty_schema()).expect_err("expected boundary error");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "from_json",
            &["options-map"],
        );
    }

    /// Pass 87 — a non-literal schema argument is a Thunderduck-boundary
    /// error, mirroring `from_json`'s behavior.
    #[test]
    fn render_from_csv_non_literal_schema_is_boundary_error() {
        let f = fcall(
            "from_csv",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "csv_str".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::String),
                    nullable: Some(true),
                    expr_id: None,
                }),
                Expression::ColumnReference(ColumnReference {
                    name: "schema_col".to_owned(),
                    qualifier: None,
                    data_type: Some(DataType::String),
                    nullable: Some(true),
                    expr_id: None,
                }),
            ],
        );
        let err = render_function_call(&f, &empty_schema()).expect_err("expected boundary error");
        expect_unsupported(err, UnsupportedKind::Function, "from_csv", &[]);
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

    // ── Pass 90 — inline_field / inline_outer_field emission ────────────

    /// Schema with an `arr : Array<Struct<name STRING?, dept_id INT?, salary DOUBLE?>>`
    /// column — enough surface to test both plain and sentinel-wrapped forms.
    fn arr_of_struct_schema() -> Schema {
        let element = DataType::Struct(StructType::new(vec![
            StructField::nullable("name", DataType::String),
            StructField::nullable("dept_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ]));
        Schema::minted(StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("arr", DataType::Array(Box::new(element), true)),
        ]))
    }

    fn inline_field_args(field: &str) -> Vec<Expression> {
        vec![
            Expression::ColumnReference(ColumnReference {
                name: "arr".to_owned(),
                qualifier: None,
                data_type: None,
                nullable: None,
                expr_id: None,
            }),
            str_lit(field),
        ]
    }

    /// Plain `inline_field(arr, "name")` renders as `UNNEST(arr).name`.
    #[test]
    fn render_inline_field_emits_unnest_dot_field() {
        let sql = render_fn_on(
            &arr_of_struct_schema(),
            "inline_field",
            inline_field_args("name"),
        );
        assert_eq!(sql, "UNNEST(arr).name");
    }

    /// `inline_outer_field(arr, "dept_id")` renders with the struct-typed
    /// NULL sentinel guard so a NULL / empty array yields one all-NULL row.
    /// Snapshot pins the exact sentinel shape.
    #[test]
    fn render_inline_outer_field_emits_case_guard_with_typed_null() {
        let sql = render_fn_on(
            &arr_of_struct_schema(),
            "inline_outer_field",
            inline_field_args("dept_id"),
        );
        assert_eq!(
            sql,
            "UNNEST(CASE WHEN arr IS NULL OR len(arr) = 0 \
             THEN [struct_pack(\
             name := CAST(NULL AS VARCHAR), \
             dept_id := CAST(NULL AS INTEGER), \
             salary := CAST(NULL AS DOUBLE))] \
             ELSE arr END).dept_id",
        );
    }

    /// Wrong arity → `UnsupportedFunction` (internal-corruption signal — the
    /// analyzer's contract is 2 args, so this should never fire in practice).
    #[test]
    fn render_inline_field_rejects_wrong_arity() {
        let schema = arr_of_struct_schema();
        let f = fcall(
            "inline_field",
            vec![Expression::ColumnReference(ColumnReference {
                name: "arr".to_owned(),
                qualifier: None,
                data_type: None,
                nullable: None,
                expr_id: None,
            })],
        );
        let err = render_function_call(&f, &schema).expect_err("must reject arity != 2");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "inline_field",
            &["2 arguments"],
        );
    }

    // ── Pass 91 — json_tuple_field emission ─────────────────────────────

    fn json_str_schema() -> Schema {
        Schema::minted(StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("json_str", DataType::String),
        ]))
    }

    /// `json_tuple_field(json_str, "a")` renders as
    /// `json_extract_string(json_str, '$.a')` — same substrate as the
    /// `get_json_object` session macro (session.rs:344).
    #[test]
    fn render_json_tuple_field_emits_json_extract_string() {
        let sql = render_fn_on(
            &json_str_schema(),
            "json_tuple_field",
            vec![
                Expression::ColumnReference(ColumnReference {
                    name: "json_str".to_owned(),
                    qualifier: None,
                    data_type: None,
                    nullable: None,
                    expr_id: None,
                }),
                str_lit("a"),
            ],
        );
        assert_eq!(sql, "json_extract_string(json_str, '$.a')");
    }

    /// Wrong arity gets a graceful boundary error, NOT a panic: the name is
    /// user-invokable directly (τ forwards unknown function names with no
    /// allowlist), so the `expand_json_tuple_projections` choke point covers
    /// only the calls it synthesizes — the emission guard is load-bearing.
    #[test]
    fn render_json_tuple_field_rejects_wrong_arity() {
        let schema = json_str_schema();
        let f = fcall(
            "json_tuple_field",
            vec![Expression::ColumnReference(ColumnReference {
                name: "json_str".to_owned(),
                qualifier: None,
                data_type: None,
                nullable: None,
                expr_id: None,
            })],
        );
        let err = render_function_call(&f, &schema).expect_err("must reject arity != 2");
        expect_unsupported(
            err,
            UnsupportedKind::Function,
            "json_tuple_field",
            &["2 arguments"],
        );
    }

    /// Pass 76 — `parse_number_format` recognizes digit templates.
    #[test]
    fn parse_number_format_digit_template() {
        assert_eq!(parse_number_format("999.99"), Some((5, 2)));
        assert_eq!(parse_number_format("9999"), Some((4, 0)));
        assert_eq!(parse_number_format("0.00"), Some((3, 2)));
        // Grouping separator `,` is accepted in the integer part
        // (contributes no digit slot). Corpus witness: `parse-003`.
        assert_eq!(parse_number_format("9,999.99"), Some((6, 2)));
        // Sign / currency / other markers → None (τ boundary).
        assert_eq!(parse_number_format("S999.99"), None);
        // Empty / all-zero-precision → None.
        assert_eq!(parse_number_format(""), None);
    }

    /// `to_number(col, '9,999.99')` on non-parseable input emits a
    /// `CASE WHEN try_cast(...) IS NULL AND input IS NOT NULL THEN error(...)`
    /// branch carrying the Spark `[INVALID_FORMAT.MISMATCH_INPUT]` class
    /// token and the format literal `9,999.99`. Corpus witness: `parse-003`.
    #[test]
    fn render_to_number_emits_ansi_throw_on_mismatch() {
        let sql = render_fn(
            "to_number",
            vec![
                col_ref_expr("num_str"),
                Expression::Literal(Literal {
                    value: LiteralValue::String("9,999.99".to_owned()),
                    data_type: DataType::String,
                }),
            ],
        );
        // The DECIMAL(6, 2) precision/scale derives from the format
        // `'9,999.99'` (comma is grouping, no digit slot; four `9`s pre-dot
        // → precision 6, scale 2).
        assert!(sql.contains("DECIMAL(6, 2)"), "got: {sql}");
        assert!(sql.contains("try_cast("), "got: {sql}");
        // ANSI throw branch is emitted with the Spark class + format text.
        assert!(
            sql.contains("[INVALID_FORMAT.MISMATCH_INPUT]"),
            "got: {sql}"
        );
        assert!(
            sql.contains("The format is invalid: 9,999.99."),
            "got: {sql}"
        );
        // NULL input pass-through: the guard checks `IS NOT NULL` on the
        // input before raising, matching Spark's `nullSafeEval` semantics.
        assert!(sql.contains("IS NOT NULL"), "got: {sql}");
    }

    // ── render_na_fill — chain-002 regression ─────────────────────────────
    //
    // `.na.fill(0)` (single value, no subset) sends `NAFill.cols=[]` from the
    // PySpark client. Spark's `DataFrameNaFunctions.fillValue` silently skips
    // columns whose type does not match the fill value's type; τ must too, or
    // DuckDB rejects `COALESCE(varchar, bigint)`. This test locks the emission
    // shape: only numeric columns are COALESCEd; non-numeric pass through
    // bare.
    #[test]
    fn render_na_fill_empty_cols_int_value_only_coalesces_numeric_columns() {
        let _g = tap_guard();
        let mixed_schema = StructType::new(vec![
            StructField::nullable("s", DataType::String),
            StructField::nullable("l", DataType::Long),
            StructField::nullable("d", DataType::Double),
            StructField::nullable("b", DataType::Boolean),
        ]);
        let plan = scan("t");
        let bt = BaseTypes::build_from_plan(&plan, |n| {
            if n == "t" {
                Some(mixed_schema.clone())
            } else {
                None
            }
        });
        let ast = CommonAst::new(CommonOp::NaFill {
            input: Box::new(plan),
            cols: vec![],
            values: vec![int_lit(0)],
        });
        let sql = generate(&ast, &bt).expect("generate NaFill");
        // Numeric columns get COALESCEd (quote_ident's fast path leaves
        // simple identifiers unquoted).
        assert!(
            sql.contains("COALESCE(l, 0) AS l"),
            "expected long col COALESCE, got: {sql}"
        );
        assert!(
            sql.contains("COALESCE(d, 0) AS d"),
            "expected double col COALESCE, got: {sql}"
        );
        // Non-numeric columns pass through bare — no COALESCE against them.
        assert!(
            !sql.contains("COALESCE(s"),
            "String col must not be COALESCEd (Spark parity), got: {sql}"
        );
        assert!(
            !sql.contains("COALESCE(b"),
            "Boolean col must not be COALESCEd (Spark parity), got: {sql}"
        );
        // The bare column names still appear in the projection.
        assert!(sql.contains("SELECT s,"), "got: {sql}");
        assert!(sql.contains(", b FROM"), "got: {sql}");
    }

    // ── LATERAL VIEW emission tests (cx-007/cx-008/cx-009) ──────────────

    /// Build a `TypedOp::LateralView` with a TableScan("emp") aliased "e"
    /// and an `ARRAY<STRING>` tags column. Returns the TypedAst for the
    /// LateralView operator ready for dispatch_op / render tests.
    fn emp_tags_schema() -> StructType {
        StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("tags", DataType::Array(Box::new(DataType::String), true)),
        ])
    }

    fn typed_table_scan(table: &str, alias: Option<&str>, schema: StructType) -> TypedAst {
        TypedAst::new(
            TypedOp::TableScan {
                table: table.to_owned(),
                alias: alias.map(|s| s.to_owned()),
            },
            Schema::minted(schema),
        )
    }

    fn tags_col_ref() -> Expression {
        Expression::ColumnReference(ColumnReference {
            name: "tags".to_owned(),
            qualifier: Some("e".to_owned()),
            data_type: Some(DataType::Array(Box::new(DataType::String), true)),
            nullable: Some(true),
            expr_id: None,
        })
    }

    fn lateral_view_typed(columns: Vec<(String, Expression)>, input: TypedAst) -> TypedAst {
        // Freshly generated LateralView output columns — brand-new logical
        // columns that did not exist before this point: MINT.
        let gen_fields: Vec<Attribute> = columns
            .iter()
            .map(|(alias, expr)| {
                Attribute::minted(
                    alias.clone(),
                    expr.data_type(&input.resolved_schema),
                    expr.nullable(&input.resolved_schema),
                )
            })
            .collect();
        let resolved_schema = Schema::merge(&input.resolved_schema, &Schema::new(gen_fields));
        TypedAst::new(
            TypedOp::LateralView {
                input: Box::new(input),
                table_alias: "t".to_owned(),
                columns,
            },
            resolved_schema,
        )
    }

    #[test]
    fn render_lateral_view_plain_explode() {
        let _g = tap_guard();
        let input = typed_table_scan("emp", Some("e"), emp_tags_schema());
        let lv = lateral_view_typed(
            vec![("tag".to_owned(), fexpr("explode", vec![tags_col_ref()]))],
            input,
        );
        let sql = dispatch_op(&lv.op, &lv.resolved_schema).expect("render");
        // cx-007 shape: SELECT * FROM emp AS e, LATERAL (SELECT UNNEST(e.tags) AS tag) AS t
        assert!(
            sql.contains("LATERAL (SELECT"),
            "must contain LATERAL(SELECT), got: {sql}"
        );
        assert!(
            sql.contains("UNNEST(e.tags)"),
            "must contain UNNEST(e.tags), got: {sql}"
        );
        assert!(sql.contains("AS tag"), "must alias as tag, got: {sql}");
        assert!(
            sql.contains("AS t"),
            "must alias lateral table as t, got: {sql}"
        );
        assert!(
            !sql.contains("__td_proj"),
            "must not contain __td_proj, got: {sql}"
        );
    }

    #[test]
    fn render_lateral_view_outer_explode() {
        let _g = tap_guard();
        let input = typed_table_scan("emp", Some("e"), emp_tags_schema());
        let lv = lateral_view_typed(
            vec![(
                "tag".to_owned(),
                fexpr("explode_outer", vec![tags_col_ref()]),
            )],
            input,
        );
        let sql = dispatch_op(&lv.op, &lv.resolved_schema).expect("render");
        // cx-008 shape: the CASE-wrapped UNNEST should appear inside the
        // LATERAL(SELECT...) wrapper.
        assert!(
            sql.contains("LATERAL (SELECT"),
            "must contain LATERAL(SELECT), got: {sql}"
        );
        assert!(
            sql.contains("CASE WHEN"),
            "OUTER must use CASE rewrite, got: {sql}"
        );
        assert!(
            sql.contains("UNNEST(CASE WHEN e.tags"),
            "must contain UNNEST(CASE WHEN e.tags...), got: {sql}"
        );
    }

    #[test]
    fn render_lateral_view_posexplode_single_inner_select() {
        let _g = tap_guard();
        let input = typed_table_scan("emp", Some("e"), emp_tags_schema());
        let lv = lateral_view_typed(
            vec![
                (
                    "pos".to_owned(),
                    fexpr("posexplode_pos", vec![tags_col_ref()]),
                ),
                (
                    "tag".to_owned(),
                    fexpr("posexplode_val", vec![tags_col_ref()]),
                ),
            ],
            input,
        );
        let sql = dispatch_op(&lv.op, &lv.resolved_schema).expect("render");
        // cx-009 shape: both columns in ONE inner SELECT.
        let lateral_count = sql.matches("LATERAL (SELECT").count();
        assert_eq!(
            lateral_count, 1,
            "posexplode must produce exactly one LATERAL(SELECT), got {lateral_count} in: {sql}"
        );
        assert!(
            sql.contains("generate_subscripts"),
            "pos column must use generate_subscripts, got: {sql}"
        );
        assert!(sql.contains("AS pos"), "must alias pos, got: {sql}");
        assert!(sql.contains("AS tag"), "must alias tag, got: {sql}");
        assert!(
            !sql.contains("__td_proj"),
            "must not contain __td_proj, got: {sql}"
        );
    }

    #[test]
    fn render_project_over_lateral_view_no_td_proj() {
        let _g = tap_guard();
        let input = typed_table_scan("emp", Some("e"), emp_tags_schema());
        let lv = lateral_view_typed(
            vec![("tag".to_owned(), fexpr("explode", vec![tags_col_ref()]))],
            input,
        );
        // Project[e.id, t.tag] over LateralView
        let id_ref = Expression::ColumnReference(ColumnReference {
            name: "id".to_owned(),
            qualifier: Some("e".to_owned()),
            data_type: Some(DataType::Long),
            nullable: Some(false),
            expr_id: None,
        });
        let tag_ref = Expression::ColumnReference(ColumnReference {
            name: "tag".to_owned(),
            qualifier: Some("t".to_owned()),
            data_type: Some(DataType::String),
            nullable: Some(true),
            expr_id: None,
        });
        let proj = TypedAst::new(
            TypedOp::Project {
                input: Box::new(lv),
                projections: vec![id_ref, tag_ref],
            },
            Schema::minted(StructType::new(vec![
                StructField::not_null("id", DataType::Long),
                StructField::nullable("tag", DataType::String),
            ])),
        );
        let sql = dispatch_op(&proj.op, &proj.resolved_schema).expect("render");
        // The output must NOT wrap in __td_proj — the alias-transparent-from
        // arm must inline the LATERAL FROM body.
        assert!(
            !sql.contains("__td_proj"),
            "Project-over-LateralView must not contain __td_proj, got: {sql}"
        );
        assert!(sql.contains("e.id"), "must reference e.id, got: {sql}");
        assert!(sql.contains("t.tag"), "must reference t.tag, got: {sql}");
        assert!(
            sql.contains("LATERAL (SELECT"),
            "must contain LATERAL(SELECT), got: {sql}"
        );
    }

    // ── Plan 006 F3: LateralView default-slot widening pins ─────────────

    #[test]
    fn lateral_view_over_range_appends_generated_column() {
        let _g = tap_guard();
        // F3 regression pin (review findings #3): `range(3)`'s FROM-item
        // leaf carries a default `id` bind (tbl-006); a merged LateralView
        // must widen that default list to include its own generated column
        // too, or a downstream bare-star consumer would never see it.
        let range_input = TypedAst::new(
            TypedOp::TableFunction {
                name: "range".to_owned(),
                args: vec![int_lit(3)],
                with_ordinality: false,
            },
            Schema::minted(StructType::new(vec![StructField::not_null(
                "id",
                DataType::Long,
            )])),
        );
        let lv = lateral_view_typed(
            vec![(
                "c".to_owned(),
                fexpr(
                    "explode",
                    vec![fexpr("array", vec![int_lit(1), int_lit(2)])],
                ),
            )],
            range_input,
        );
        let sql = dispatch_op(&lv.op, &lv.resolved_schema).expect("render");
        assert!(
            sql.starts_with("SELECT id, t.c FROM range("),
            "default projection must widen to include the generated column; got: {sql}"
        );
        assert!(
            sql.contains("AS __td_range(id)"),
            "range's own id bind must survive; got: {sql}"
        );
        assert!(
            sql.contains("LATERAL (SELECT"),
            "must contain LATERAL(SELECT), got: {sql}"
        );
    }

    #[test]
    fn lateral_view_over_table_scan_still_renders_star() {
        let _g = tap_guard();
        // F3 no-op guard: a plain table-scan child has no default
        // projections (`None`), so extending must stay a no-op — the merged
        // block keeps rendering `SELECT *` (protects the cx-007..009 shape).
        let input = typed_table_scan("emp", Some("e"), emp_tags_schema());
        let lv = lateral_view_typed(
            vec![("tag".to_owned(), fexpr("explode", vec![tags_col_ref()]))],
            input,
        );
        let sql = dispatch_op(&lv.op, &lv.resolved_schema).expect("render");
        assert!(
            sql.starts_with("SELECT * FROM"),
            "LateralView over a plain scan must still render SELECT *; got: {sql}"
        );
    }

    // ── Pass-17: LATERAL derived-table join emission ────────────────────

    /// E2E analyze + emit of the tbl-005 shape: the SQL must contain
    /// `CROSS JOIN LATERAL` and NOT contain `__td_jl`.
    #[test]
    fn render_lateral_join_cross_emits_lateral_keyword_no_td_jl() {
        let _g = tap_guard();
        // Build: `SELECT e.name, t.dept_avg FROM emp e
        //   CROSS JOIN LATERAL (SELECT avg(e2.salary) AS dept_avg
        //     FROM emp e2 WHERE e2.dept_id = e.dept_id) t`
        // Simplified to avoid aggregate complexity: the right side just
        // projects a correlated column from the left.
        let left = aliased_scan("emp", "e");
        let right_inner = CommonAst::new(CommonOp::Project {
            input: Box::new(scan("dept")),
            projections: vec![Expression::Alias(
                crate::transpiler_v2::expression::AliasExpression {
                    expr: Box::new(qcol("e", "name")),
                    alias: "dept_avg".to_owned(),
                },
            )],
        });
        let right = aliased_scan_from(right_inner, "t");
        let lateral_join = CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_join),
            projections: vec![qcol("e", "name"), qcol("t", "dept_avg")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze lateral join");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("CROSS JOIN LATERAL"),
            "must contain CROSS JOIN LATERAL, got: {sql}"
        );
        assert!(
            !sql.contains("__td_jl"),
            "lateral join must not use __td_jl wrapper, got: {sql}"
        );
        assert!(
            sql.contains("AS e CROSS JOIN LATERAL"),
            "left alias must be hoisted as e before LATERAL, got: {sql}"
        );
    }

    /// Helper: wrap a plan in an AliasedRelation.
    fn aliased_scan_from(inner: CommonAst, alias: &str) -> CommonAst {
        CommonAst::new(CommonOp::AliasedRelation {
            input: Box::new(inner),
            alias: alias.to_owned(),
        })
    }

    /// Lateral join with ON clause: must emit `INNER JOIN LATERAL ... ON`.
    #[test]
    fn render_lateral_join_with_on_emits_inner_join_lateral_on() {
        let _g = tap_guard();
        let left = aliased_scan("emp", "e");
        let right_inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![Expression::Alias(
                crate::transpiler_v2::expression::AliasExpression {
                    expr: Box::new(int_lit(1)),
                    alias: "x".to_owned(),
                },
            )],
        });
        let right = aliased_scan_from(right_inner, "t");
        let condition = Expression::Binary(BinaryExpression {
            op: BinaryOp::Eq,
            left: Box::new(qcol("t", "x")),
            right: Box::new(qcol("e", "id")),
        });
        let lateral_join = CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: Some(condition),
            using_columns: vec![],
            natural: false,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_join),
            projections: vec![qcol("e", "name")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze lateral-with-ON");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("INNER JOIN LATERAL"),
            "must contain INNER JOIN LATERAL, got: {sql}"
        );
        assert!(sql.contains(" ON "), "must contain ON clause, got: {sql}");
    }

    /// A nested LATERAL join must stay isolated under its synthetic wrapper
    /// when it is the side of an enclosing join (its correlation must not be
    /// spliced into a shared FROM scope), while an equivalent non-lateral
    /// nested chain inlines flat.
    #[test]
    fn nested_lateral_join_side_never_inlines_into_outer_from() {
        let _g = tap_guard();
        let nested = |lateral: bool| {
            CommonAst::new(CommonOp::Join {
                left: Box::new(aliased_scan("emp", "e")),
                right: Box::new(aliased_scan("dept", "d")),
                join_type: JoinType::Cross,
                condition: None,
                using_columns: vec![],
                natural: false,
                lateral,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })
        };
        let outer = |inner: CommonAst| {
            CommonAst::new(CommonOp::Join {
                left: Box::new(inner),
                right: Box::new(aliased_scan("bonus", "b")),
                join_type: JoinType::Cross,
                condition: None,
                using_columns: vec![],
                natural: false,
                lateral: false,
                left_plan_ids: vec![],
                right_plan_ids: vec![],
            })
        };

        let plan = outer(nested(true));
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema()),
            "dept" => Some(dept_schema()),
            "bonus" => Some(dept_schema()),
            _ => None,
        });
        let typed = analyze(plan, &bt).expect("analyze outer-over-lateral");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            sql.contains("AS __td_jl"),
            "lateral nested join must stay wrapped under its synthetic alias, got: {sql}"
        );

        let plan = outer(nested(false));
        let typed = analyze(plan, &bt).expect("analyze outer-over-plain");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        assert!(
            !sql.contains("__td_jl"),
            "non-lateral nested chain should inline without a synthetic wrapper, got: {sql}"
        );
    }

    /// Regression: bare unaliased TableScan left with a lateral join must
    /// expose the table name as the FROM alias (not __td_jl), so that
    /// table-name-qualified correlated references inside the subquery resolve.
    #[test]
    fn render_lateral_join_bare_table_scan_left_exposes_table_name() {
        let _g = tap_guard();
        // `FROM emp JOIN LATERAL (SELECT emp.name AS x) t` — no alias on emp.
        let left = scan("emp");
        let right_inner = CommonAst::new(CommonOp::Project {
            input: Box::new(CommonAst::new(CommonOp::SingleRow)),
            projections: vec![Expression::Alias(
                crate::transpiler_v2::expression::AliasExpression {
                    expr: Box::new(qcol("emp", "name")),
                    alias: "x".to_owned(),
                },
            )],
        });
        let right = aliased_scan_from(right_inner, "t");
        let lateral_join = CommonAst::new(CommonOp::Join {
            left: Box::new(left),
            right: Box::new(right),
            join_type: JoinType::Inner,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: true,
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        });
        let plan = CommonAst::new(CommonOp::Project {
            input: Box::new(lateral_join),
            projections: vec![qcol("emp", "name"), qcol("t", "x")],
        });
        let bt = base_types_emp_dept(&plan);
        let typed = analyze(plan, &bt).expect("analyze lateral with bare TableScan left");
        let sql = dispatch_op(&typed.op, &typed.resolved_schema).expect("dispatch");
        // The emitted SQL must expose "emp" as the left alias, not __td_jl.
        assert!(
            !sql.contains("__td_jl"),
            "bare TableScan left must not use __td_jl, got: {sql}"
        );
        assert!(
            sql.contains("emp CROSS JOIN LATERAL"),
            "bare TableScan left must be aliased as emp, got: {sql}"
        );
        assert!(
            sql.contains("LATERAL"),
            "must contain LATERAL keyword, got: {sql}"
        );
    }

    // ── Pass 18: RecursiveCte emission tests ──────────────────────────────

    /// Full pipeline (lower→analyze→dispatch) for cte-009.
    #[test]
    fn render_recursive_cte_009_full_pipeline() {
        use crate::parser_v2::SparkSqlParserV2;
        let sql_input = "WITH RECURSIVE seq(n) AS (\
            SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 5\
        ) SELECT * FROM seq";
        let ast = SparkSqlParserV2::parse(sql_input).expect("parse cte-009");
        // The self-reference `seq` in the recursive term is a TableScan that
        // needs a BaseTypes entry — but here it resolves via the injected entry
        // inside analyze_recursive_cte, so empty base_types suffices.
        let bt = BaseTypes::empty();
        let sql = generate(&ast, &bt).expect("generate cte-009");
        // The outer query wraps RecursiveCte in AliasedRelation:
        // `SELECT * FROM (WITH RECURSIVE seq(n) AS (...) SELECT * FROM seq) AS seq`
        assert!(
            sql.contains("WITH RECURSIVE seq(n) AS ("),
            "must contain WITH RECURSIVE seq(n) AS (, got: {sql}"
        );
        assert!(
            sql.contains("UNION ALL"),
            "must contain UNION ALL, got: {sql}"
        );
        // The inner `SELECT * FROM seq` terminates the WITH RECURSIVE CTE.
        assert!(
            sql.contains("SELECT * FROM seq"),
            "must contain SELECT * FROM seq, got: {sql}"
        );
    }

    /// Full pipeline (lower→analyze→dispatch) for cte-010.
    #[test]
    fn render_recursive_cte_010_full_pipeline() {
        use crate::parser_v2::SparkSqlParserV2;
        let sql_input = "WITH RECURSIVE chain(id, name, manager_id, lvl) AS (\
            SELECT id, name, manager_id, 0 FROM emp WHERE manager_id IS NULL \
            UNION ALL \
            SELECT e.id, e.name, e.manager_id, c.lvl + 1 \
            FROM emp e JOIN chain c ON e.manager_id = c.id\
        ) SELECT * FROM chain";
        let ast = SparkSqlParserV2::parse(sql_input).expect("parse cte-010");
        // emp schema needs `manager_id` column for cte-010.
        let emp_schema_m = StructType::new(vec![
            StructField::not_null("id", DataType::Long),
            StructField::nullable("name", DataType::String),
            StructField::nullable("manager_id", DataType::Integer),
            StructField::nullable("salary", DataType::Double),
        ]);
        let plan = scan("emp");
        let bt = BaseTypes::build_from_plan(&plan, |name| match name {
            "emp" => Some(emp_schema_m.clone()),
            _ => None,
        });
        let sql = generate(&ast, &bt).expect("generate cte-010");
        // Assert the WITH RECURSIVE template shape.
        assert!(
            sql.contains("WITH RECURSIVE chain(id, name, manager_id, lvl) AS ("),
            "must contain WITH RECURSIVE chain(id, name, manager_id, lvl) AS (, got: {sql}"
        );
        assert!(
            sql.contains("UNION ALL"),
            "must contain UNION ALL, got: {sql}"
        );
        assert!(
            sql.contains("SELECT * FROM chain"),
            "must contain SELECT * FROM chain, got: {sql}"
        );
        // The join-form self-reference: `chain` appears in the recursive SQL
        // as a table reference (not inlined).
        let after_union = sql.split("UNION ALL").nth(1).expect("text after UNION ALL");
        assert!(
            after_union.contains("chain"),
            "recursive term must reference `chain`, got after UNION ALL: {after_union}"
        );
    }
}
