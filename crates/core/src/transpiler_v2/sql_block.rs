//! The SELECT-block builder — emission's structural layer.
//!
//! Operators merge their clause into the child's open [`SelectBlock`] when
//! the clause ordinal and alias-visibility preconditions hold, and wrap the
//! child as a derived table (a fresh block) only on slot-occupancy conflict.
//! This provides one uniform mechanism for clause placement and derived-table
//! wrapping (ADR-001 permits the resulting cosmetic simplification).
//!
//! A block stores **already-rendered SQL fragments** (projection slot lists,
//! predicates, sort keys) produced by `emission`'s expression layer; this
//! module never renders expressions itself. Alias visibility is tracked via
//! [`SelectBlock::exposes`] (backed by [`FromItem::exposed`]): the set of
//! FROM-scope aliases the block actually emits, which merging operators
//! check their analyzer-stamped qualifiers against (the emission-side
//! counterpart of the analyzer's `RelScope`).

use super::identifier::{quote_ident, Qualifier};

/// Clause ordinals in SQL's logical evaluation order. An operator may merge
/// into a block only "downstream" of everything already occupied:
/// strictly greater than [`SelectBlock::max_clause`], except `Where`, where
/// equality is allowed (WHERE conjuncts compose by conjunction).
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub(crate) enum Clause {
    /// FROM — every fresh block starts here.
    From,
    /// WHERE — conjunct-composable (`Where` accepts `Where`).
    Where,
    /// GROUP BY (occupied together with `Select` by Aggregate).
    GroupBy,
    /// HAVING (analyzer-populated; only Aggregate fills it).
    Having,
    /// The SELECT list.
    Select,
    /// DISTINCT / DISTINCT ON.
    Distinct,
    /// ORDER BY.
    OrderBy,
    /// LIMIT / OFFSET.
    LimitOffset,
}

/// A rendered relational unit: either an open, merge-accepting SELECT block
/// or an opaque SQL string (set-op chains, `WITH RECURSIVE`, and operators
/// that require string rendering). Nothing merges into `Raw`;
/// parents wrap it.
#[derive(Debug)]
pub(crate) enum SqlUnit {
    /// An open SELECT block parents may merge into (boxed — a block is an
    /// order of magnitude larger than the `Raw` string variant).
    Select(Box<SelectBlock>),
    /// Fully-rendered SQL; parents wrap it as a derived table.
    Raw(String),
}

impl From<SelectBlock> for SqlUnit {
    fn from(block: SelectBlock) -> Self {
        SqlUnit::Select(Box::new(block))
    }
}

impl SqlUnit {
    /// Render to the final SQL string.
    pub(crate) fn to_sql(&self) -> String {
        match self {
            SqlUnit::Select(block) => block.to_sql(),
            SqlUnit::Raw(sql) => sql.clone(),
        }
    }
}

/// One item in a block's FROM clause.
#[derive(Debug)]
pub(crate) enum FromItem {
    /// `FROM <base> [AS <alias>]` — a bare table.
    Relation {
        /// Parsed table name.
        base: Qualifier,
        /// Optional user alias (raw).
        alias: Option<String>,
    },
    /// `FROM (<unit>) AS <alias>` — a derived table.
    Derived {
        /// The wrapped unit.
        unit: Box<SqlUnit>,
        /// Derived-table alias (raw).
        alias: String,
    },
    /// A join living flat in ONE FROM scope:
    /// `<left> <kind> [LATERAL ]<right><clause>`.
    Join {
        /// Left side.
        left: Box<FromItem>,
        /// Right side.
        right: Box<FromItem>,
        /// Join keyword (`join_kind_sql` output).
        kind: &'static str,
        /// Rendered ` ON …` / ` USING (…)` clause (leading space) or empty.
        clause: String,
        /// Render `LATERAL` before the right side.
        lateral: bool,
    },
    /// A verbatim FROM body and the aliases it exposes.
    Raw {
        /// The FROM-body SQL.
        sql: String,
        /// Qualifiers this body exposes.
        exposed: Vec<Qualifier>,
    },
}

impl FromItem {
    fn to_sql(&self) -> String {
        match self {
            FromItem::Relation { base, alias } => {
                let name = base.to_sql();
                match alias {
                    Some(a) => format!("{name} AS {}", quote_ident(a)),
                    None => name,
                }
            }
            FromItem::Derived { unit, alias } => {
                format!("({}) AS {}", unit.to_sql(), quote_ident(alias))
            }
            FromItem::Join {
                left,
                right,
                kind,
                clause,
                lateral,
            } => {
                let lat = if *lateral { "LATERAL " } else { "" };
                format!("{} {kind} {lat}{}{clause}", left.to_sql(), right.to_sql())
            }
            FromItem::Raw { sql, .. } => sql.clone(),
        }
    }

    /// The alias names this item exposes to the enclosing block's clauses.
    pub(crate) fn exposed(&self) -> Vec<Qualifier> {
        match self {
            // An aliased relation is addressable ONLY by the alias; a bare
            // one by its table name.
            FromItem::Relation { base, alias } => {
                vec![alias
                    .as_ref()
                    .map_or_else(|| base.clone(), Qualifier::single)]
            }
            FromItem::Derived { alias, .. } => vec![Qualifier::single(alias)],
            FromItem::Join { left, right, .. } => {
                let mut names = left.exposed();
                names.extend(right.exposed());
                names
            }
            FromItem::Raw { exposed, .. } => exposed.clone(),
        }
    }
}

/// DISTINCT flavor on a block.
#[derive(Debug)]
pub(crate) enum DistinctKind {
    /// `SELECT DISTINCT …`.
    Distinct,
    /// `SELECT DISTINCT ON (<cols>) …` — `cols` is the rendered column list.
    DistinctOn(String),
}

/// The uniform derived-table alias used when an operator must wrap its child
/// instead of merging (the universal slot-conflict fallback). Emission-local;
/// nothing binds through it.
pub(crate) const WRAP_ALIAS: &str = "__td_sub";

/// One analyzer-named, rendered SELECT slot.
#[derive(Debug, Clone)]
pub(crate) struct DefaultSlot {
    /// The output column name, in analyzer casing.
    pub(crate) name: String,
    /// The rendered slot SQL, e.g. `e.salary` or `dept_id`.
    pub(crate) sql: String,
}

/// An open SELECT block being composed bottom-up. All expression-level
/// content is pre-rendered SQL text; the block only owns clause placement.
#[derive(Debug)]
pub(crate) struct SelectBlock {
    /// Rendered SELECT slot list. `None` = free (renders
    /// `default_projections`, else `*`).
    projections: Option<String>,
    /// Soft SELECT list a merging Project/Aggregate may overwrite — the join
    /// builder's hoisted slot list (resolved-schema column order), named so
    /// consumers can filter/extend it without parsing SQL text. Rendered
    /// only while `projections` is `None`; does NOT occupy the `Select`
    /// ordinal.
    default_projections: Option<Vec<DefaultSlot>>,
    distinct: Option<DistinctKind>,
    from: FromItem,
    /// AND-composed WHERE conjuncts.
    where_conjuncts: Vec<String>,
    /// Fully-rendered GROUP BY body (`a, b` / `ROLLUP(a, b)` /
    /// `GROUPING SETS ((…), …)`).
    group_by: Option<String>,
    /// Rendered HAVING predicate.
    having: Option<String>,
    /// Rendered `expr DIR NULLS …` sort keys.
    order_by: Vec<String>,
    limit: Option<i64>,
    offset: Option<i64>,
    /// Highest occupied clause ordinal (`From` for a fresh block).
    max_clause: Clause,
}

impl SelectBlock {
    /// A fresh block over `from`, with every other slot free.
    pub(crate) fn from_item(from: FromItem) -> Self {
        Self {
            projections: None,
            default_projections: None,
            distinct: None,
            from,
            where_conjuncts: Vec::new(),
            group_by: None,
            having: None,
            order_by: Vec::new(),
            limit: None,
            offset: None,
            max_clause: Clause::From,
        }
    }

    /// If nothing but the FROM slot is occupied, surrender the [`FromItem`]
    /// for inlining into an enclosing FROM scope (join-side hoisting);
    /// otherwise return the block unchanged. `default_projections` is
    /// dropped ONLY here, on a true inline: the enclosing FROM scope takes
    /// over binding (either re-deriving its own column list, or — for a
    /// flattened plain-join chain — rendering `*` in natural left-then-right
    /// order, which already matches the declared schema). A block that is
    /// NOT inlined (the caller wraps it instead) keeps its defaults intact —
    /// see [`SelectBlock::from_ref`] for the read-only peek that lets a
    /// caller decide eligibility before consuming the block.
    pub(crate) fn into_pure_from(self) -> Result<FromItem, Box<SelectBlock>> {
        if self.max_clause == Clause::From && self.projections.is_none() {
            Ok(self.from)
        } else {
            Err(Box::new(self))
        }
    }

    /// Read-only peek at the FROM item, for inline-eligibility checks that
    /// must run BEFORE consuming the block (a caller that decides not to
    /// inline needs the block, defaults and all, still intact to wrap).
    // `from_ref` names the `from` field being peeked, not a `From`-trait-style
    // conversion; clippy's from_* convention heuristic doesn't apply here.
    #[allow(clippy::wrong_self_convention)]
    pub(crate) fn from_ref(&self) -> &FromItem {
        &self.from
    }

    /// Append `suffix` to FROM and expose `extra_aliases`.
    pub(crate) fn extend_from(&mut self, suffix: &str, extra_aliases: Vec<Qualifier>) {
        debug_assert_eq!(self.max_clause, Clause::From);
        let sql = format!("{}{suffix}", self.from.to_sql());
        let mut exposed = self.from.exposed();
        exposed.extend(extra_aliases);
        self.from = FromItem::Raw { sql, exposed };
    }

    /// Install the soft (overridable) SELECT list — the join builder's
    /// hoisted slot list. Does not occupy the `Select` ordinal.
    pub(crate) fn set_default_projections(&mut self, slots: Vec<DefaultSlot>) {
        self.default_projections = Some(slots);
    }

    /// Read-only access to the soft SELECT slot list, for consumers that
    /// need to filter (`DropColumns`) or substitute (`Project`'s bare-star
    /// merge) individual slots by name rather than the joined SQL string.
    pub(crate) fn default_slots(&self) -> Option<&[DefaultSlot]> {
        self.default_projections.as_deref()
    }

    /// Add soft SELECT slots when a default list exists.
    pub(crate) fn extend_default_projections(&mut self, extra: Vec<DefaultSlot>) {
        if let Some(slots) = &mut self.default_projections {
            slots.extend(extra);
        }
    }

    /// Fill GROUP BY (pre-rendered body). Caller must have checked
    /// `can_accept(GroupBy)`.
    pub(crate) fn set_group_by(&mut self, body: String) {
        debug_assert!(self.can_accept(Clause::GroupBy));
        self.group_by = Some(body);
        self.bump(Clause::GroupBy);
    }

    /// Fill HAVING. Caller must have checked `can_accept(Having)`.
    pub(crate) fn set_having(&mut self, predicate: String) {
        debug_assert!(self.can_accept(Clause::Having));
        self.having = Some(predicate);
        self.bump(Clause::Having);
    }

    /// The universal conflict fallback: wrap `unit` as
    /// `(…) AS __td_sub` and open a fresh block over it.
    pub(crate) fn wrap(unit: SqlUnit) -> Self {
        Self::from_item(FromItem::Derived {
            unit: Box::new(unit),
            alias: WRAP_ALIAS.to_owned(),
        })
    }

    /// Like [`wrap`], but the derived table's exposed
    /// columns are the caller-supplied UNIQUE names, positionally —
    /// `(…) AS __td_sub(c0, c1, …)`, the SQL-92 derived-table
    /// column-alias-list — instead of `unit`'s own (possibly duplicate)
    /// output names. Used exactly when `unit`'s declared output has a
    /// duplicate name: [`crate::types::pyspark_parity::uniquify`]'s result
    /// names every column distinctly, so the enclosing block can reference
    /// any of them by bare name with no ambiguity — closing the
    /// duplicate-output-name class that resolution-time bare-name dropping
    /// (ADR-023 tier 3e-ii) cannot disambiguate on its own. `unit`'s own
    /// SELECT list is untouched; only the derived table's exposed name list
    /// changes, confirmed by an empirical DuckDB smoke check that the
    /// alias-list syntax binds positionally over a duplicate-named inner
    /// projection.
    pub(crate) fn wrap_reprojected(unit: SqlUnit, uniquified: &[String]) -> Self {
        let cols = uniquified
            .iter()
            .map(|c| quote_ident(c).into_owned())
            .collect::<Vec<_>>()
            .join(", ");
        Self::from_item(FromItem::Raw {
            sql: format!("({}) AS {}({cols})", unit.to_sql(), quote_ident(WRAP_ALIAS)),
            exposed: vec![Qualifier::single(WRAP_ALIAS)],
        })
    }

    /// May `clause` still be filled? Strictly downstream of everything
    /// occupied, except WHERE-onto-WHERE (conjuncts compose).
    pub(crate) fn can_accept(&self, clause: Clause) -> bool {
        clause > self.max_clause || (clause == Clause::Where && self.max_clause == Clause::Where)
    }

    /// Whether the block's FROM scope exposes `qualifier`.
    pub(crate) fn exposes(&self, qualifier: &Qualifier) -> bool {
        self.from
            .exposed()
            .iter()
            .any(|exposed| exposed.matches_suffix(qualifier))
    }

    /// Is the SELECT list still free (renders `*`)?
    pub(crate) fn select_free(&self) -> bool {
        self.max_clause < Clause::Select
    }

    /// Is nothing but the FROM slot occupied?
    pub(crate) fn pure_from(&self) -> bool {
        self.max_clause == Clause::From && self.projections.is_none()
    }

    /// Is the DISTINCT slot compatible with adding an ORDER BY?
    /// (`DISTINCT ON` picks rows arbitrarily; sorting a `DISTINCT ON` block
    /// could change which representative row survives — never merge.)
    pub(crate) fn distinct_allows_order(&self) -> bool {
        !matches!(self.distinct, Some(DistinctKind::DistinctOn(_)))
    }

    fn bump(&mut self, clause: Clause) {
        if clause > self.max_clause {
            self.max_clause = clause;
        }
    }

    /// Fill the SELECT list. Caller must have checked `can_accept(Select)`.
    pub(crate) fn set_projections(&mut self, slots: String) {
        debug_assert!(self.can_accept(Clause::Select));
        self.projections = Some(slots);
        self.bump(Clause::Select);
    }

    /// Add a WHERE conjunct. Caller must have checked `can_accept(Where)`.
    pub(crate) fn push_where(&mut self, conjunct: String) {
        debug_assert!(self.can_accept(Clause::Where));
        self.where_conjuncts.push(conjunct);
        self.bump(Clause::Where);
    }

    /// Set DISTINCT / DISTINCT ON. Caller must have checked
    /// `can_accept(Distinct)` (and, for `DistinctOn`, that the SELECT list
    /// is free).
    pub(crate) fn set_distinct(&mut self, kind: DistinctKind) {
        debug_assert!(self.can_accept(Clause::Distinct));
        self.distinct = Some(kind);
        self.bump(Clause::Distinct);
    }

    /// Set ORDER BY, plus the LIMIT/OFFSET a `Sort` operator absorbed.
    /// Caller must have checked `can_accept(OrderBy)` (which implies the
    /// LIMIT slot is free) and `distinct_allows_order()`.
    pub(crate) fn set_order_by(
        &mut self,
        keys: Vec<String>,
        limit: Option<i64>,
        offset: Option<i64>,
    ) {
        debug_assert!(self.can_accept(Clause::OrderBy));
        debug_assert!(self.distinct_allows_order());
        self.order_by = keys;
        self.bump(Clause::OrderBy);
        if limit.is_some() || offset.is_some() {
            self.limit = limit;
            self.offset = offset;
            self.bump(Clause::LimitOffset);
        }
    }

    /// Set LIMIT/OFFSET. Caller must have checked `can_accept(LimitOffset)`.
    pub(crate) fn set_limit(&mut self, limit: i64, offset: Option<i64>) {
        debug_assert!(self.can_accept(Clause::LimitOffset));
        self.limit = Some(limit);
        self.offset = offset;
        self.bump(Clause::LimitOffset);
    }

    /// Render the block. Clause order is fixed:
    /// `SELECT [DISTINCT[ ON (…)]] <slots> FROM <from>[ WHERE …]
    /// [ ORDER BY …][ LIMIT n][ OFFSET m]`.
    pub(crate) fn to_sql(&self) -> String {
        let mut sql = String::from("SELECT ");
        match &self.distinct {
            Some(DistinctKind::Distinct) => sql.push_str("DISTINCT "),
            Some(DistinctKind::DistinctOn(cols)) => {
                sql.push_str("DISTINCT ON (");
                sql.push_str(cols);
                sql.push_str(") ");
            }
            None => {}
        }
        let default_list: Option<String> = self.default_projections.as_ref().map(|slots| {
            slots
                .iter()
                .map(|s| s.sql.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        });
        sql.push_str(
            self.projections
                .as_deref()
                .or(default_list.as_deref())
                .unwrap_or("*"),
        );
        sql.push_str(" FROM ");
        sql.push_str(&self.from.to_sql());
        match self.where_conjuncts.as_slice() {
            [] => {}
            // A single predicate renders bare; composed predicates parenthesize each
            // conjunct to keep operator precedence unambiguous.
            [only] => {
                sql.push_str(" WHERE ");
                sql.push_str(only);
            }
            many => {
                sql.push_str(" WHERE ");
                let joined = many
                    .iter()
                    .map(|c| format!("({c})"))
                    .collect::<Vec<_>>()
                    .join(" AND ");
                sql.push_str(&joined);
            }
        }
        if let Some(g) = &self.group_by {
            sql.push_str(" GROUP BY ");
            sql.push_str(g);
        }
        if let Some(h) = &self.having {
            sql.push_str(" HAVING ");
            sql.push_str(h);
        }
        if !self.order_by.is_empty() {
            sql.push_str(" ORDER BY ");
            sql.push_str(&self.order_by.join(", "));
        }
        if let Some(l) = self.limit {
            sql.push_str(&format!(" LIMIT {l}"));
        }
        if let Some(o) = self.offset {
            sql.push_str(&format!(" OFFSET {o}"));
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transpiler_v2::parse_sql_multipart_identifier;

    fn scan(base: &str, alias: Option<&str>) -> SelectBlock {
        SelectBlock::from_item(FromItem::Relation {
            base: Qualifier::from_parts(
                parse_sql_multipart_identifier(base).expect("valid test relation"),
            ),
            alias: alias.map(str::to_owned),
        })
    }

    #[test]
    fn fresh_block_renders_select_star() {
        assert_eq!(scan("emp", None).to_sql(), "SELECT * FROM emp");
        assert_eq!(scan("emp", Some("e")).to_sql(), "SELECT * FROM emp AS e");
    }

    #[test]
    fn relation_parts_and_literal_alias_render_distinctly() {
        let block = scan("catalog.`a.b`", Some("x.y"));
        assert_eq!(block.to_sql(), "SELECT * FROM catalog.\"a.b\" AS \"x.y\"");
        assert!(scan("catalog.`a.b`", None).exposes(&Qualifier::single("a.b")));
    }

    #[test]
    fn clause_ordinal_matrix() {
        let mut b = scan("emp", None);
        // Fresh block accepts everything.
        for c in [
            Clause::Where,
            Clause::GroupBy,
            Clause::Having,
            Clause::Select,
            Clause::Distinct,
            Clause::OrderBy,
            Clause::LimitOffset,
        ] {
            assert!(b.can_accept(c), "{c:?} should be free on a fresh block");
        }
        // WHERE composes with WHERE, but nothing upstream of it.
        b.push_where("a > 1".into());
        assert!(b.can_accept(Clause::Where));
        assert!(b.can_accept(Clause::Select));
        // SELECT occupies; WHERE and SELECT now conflict, ORDER BY is free.
        b.set_projections("a".into());
        assert!(!b.can_accept(Clause::Where));
        assert!(!b.can_accept(Clause::Select));
        assert!(b.can_accept(Clause::Distinct));
        assert!(b.can_accept(Clause::OrderBy));
        // ORDER BY occupies; only LIMIT remains.
        b.set_order_by(vec!["a ASC NULLS FIRST".into()], None, None);
        assert!(!b.can_accept(Clause::OrderBy));
        assert!(b.can_accept(Clause::LimitOffset));
        b.set_limit(5, Some(2));
        assert!(!b.can_accept(Clause::LimitOffset));
        assert_eq!(
            b.to_sql(),
            "SELECT a FROM emp WHERE a > 1 ORDER BY a ASC NULLS FIRST LIMIT 5 OFFSET 2"
        );
    }

    #[test]
    fn where_conjuncts_parenthesize_only_when_composed() {
        let mut b = scan("t", None);
        b.push_where("a > 1".into());
        assert_eq!(b.to_sql(), "SELECT * FROM t WHERE a > 1");
        b.push_where("b < 2".into());
        assert_eq!(b.to_sql(), "SELECT * FROM t WHERE (a > 1) AND (b < 2)");
    }

    #[test]
    fn wrap_uses_uniform_alias_and_scope() {
        let inner = scan("t", Some("x"));
        let b = SelectBlock::wrap(inner.into());
        assert!(b.exposes(&Qualifier::single("__td_sub")));
        assert!(!b.exposes(&Qualifier::single("x")));
        assert_eq!(
            b.to_sql(),
            "SELECT * FROM (SELECT * FROM t AS x) AS __td_sub"
        );
    }

    #[test]
    fn scope_matching_is_case_insensitive_and_alias_shadows_base() {
        let aliased = scan("Emp", Some("E"));
        assert!(aliased.exposes(&Qualifier::single("e")));
        assert!(
            !aliased.exposes(&Qualifier::single("emp")),
            "alias replaces table name in SQL scope"
        );
        let bare = scan("Emp", None);
        assert!(bare.exposes(&Qualifier::single("emp")));
    }

    #[test]
    fn distinct_on_blocks_order_merge() {
        let mut b = scan("t", None);
        b.set_distinct(DistinctKind::DistinctOn("a".into()));
        assert!(!b.distinct_allows_order());
        assert_eq!(b.to_sql(), "SELECT DISTINCT ON (a) * FROM t");
        let mut d = scan("t", None);
        d.set_projections("a, b".into());
        d.set_distinct(DistinctKind::Distinct);
        assert!(d.distinct_allows_order());
        assert_eq!(d.to_sql(), "SELECT DISTINCT a, b FROM t");
    }

    #[test]
    fn sort_absorbed_limit_occupies_limit_slot() {
        let mut b = scan("t", None);
        b.set_order_by(vec!["a ASC NULLS LAST".into()], Some(3), None);
        assert!(!b.can_accept(Clause::LimitOffset));
        assert_eq!(
            b.to_sql(),
            "SELECT * FROM t ORDER BY a ASC NULLS LAST LIMIT 3"
        );
    }

    #[test]
    fn quoting_applies_to_relation_and_derived_aliases() {
        let b = scan("select", Some("my alias"));
        assert_eq!(b.to_sql(), r#"SELECT * FROM "select" AS "my alias""#);
    }
}
