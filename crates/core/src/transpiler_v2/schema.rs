//! τ's attribute-identity substrate — [`ExprId`] / [`Attribute`] /
//! [`ResolvedSchema`].
//!
//! N9 INCREMENT 1: pure carriage. Every production `TypedAst::new` site mints
//! or copies an [`ExprId`] per output column, but NOTHING downstream consumes
//! the ids yet (no resolver rewrite, no emission read) — this increment is
//! behavior-frozen: zero emitted-SQL change, zero test-string churn.
//!
//! **Convention:** construct [`Attribute`] ONLY via the constructors below
//! (`minted` / `from_field`) or by `.clone()`-ing an existing one. Do **not**
//! write `Attribute { .. }` struct literals outside this module — an
//! out-of-module literal is exactly the "implicit minting at a passthrough"
//! landmine this increment exists to avoid: every fresh id must come from a
//! visible `::minted` call so a reviewer (and a future increment's grep) can
//! find every place identity is created versus carried.

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::types::{DataType, StructField, StructType};

/// A process-unique identifier minted once per logical output column and
/// carried (cloned) through every operator that merely passes a column
/// through, so downstream increments can tell "the same column, threaded
/// through N operators" from "a new column that happens to share a name".
///
/// Precedent: Spark's `NamedExpression.newExprId` / `curId` (an atomic
/// counter scoped to the JVM). Like Spark's `ExprId`, this type promises
/// **uniqueness only** — no determinism, no stability across runs, no
/// meaning attached to the numeric value itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExprId(u64);

static NEXT_EXPR_ID: AtomicU64 = AtomicU64::new(0);

impl ExprId {
    /// Mint a fresh, process-unique id.
    pub fn fresh() -> Self {
        Self(NEXT_EXPR_ID.fetch_add(1, Ordering::Relaxed))
    }
}

/// A single resolved output column: a [`StructField`]'s `(name, data_type,
/// nullable)` triple plus the [`ExprId`] identifying which logical column
/// this is, across the operators it flows through.
///
/// Field names deliberately mirror [`StructField`] so existing
/// `.fields[k].name` / `.data_type` / `.nullable` read sites keep compiling
/// unchanged when a `Vec<StructField>` becomes a `Vec<Attribute>`.
#[derive(Debug, Clone)]
pub struct Attribute {
    pub name: String,
    pub data_type: DataType,
    pub nullable: bool,
    pub expr_id: ExprId,
    /// ADR-023 tier-3 source-qualifier lineage (Spark attribute lineage) —
    /// which relation qualifiers (table names / user aliases) this column
    /// inherits, now intrinsic to `Attribute` rather than a parallel
    /// per-node `RelScope` vector (N9 increment 3). Empty for a genuinely
    /// CREATED column (an `Alias` or a computed expression) — see
    /// [`Attribute::minted`]. Seeded at leaf origination points (`TableScan`,
    /// `AliasedRelation`) via [`Attribute::with_quals`]; carried through
    /// every passthrough `.clone()` otherwise.
    pub source_quals: BTreeSet<String>,
}

impl Attribute {
    /// Construct a brand-new attribute with a freshly minted id and EMPTY
    /// lineage — use for any column that did not exist before this point (a
    /// computed expression, an `Alias`, a generated/synthesized column, ...).
    pub fn minted(name: impl Into<String>, data_type: DataType, nullable: bool) -> Self {
        Self {
            name: name.into(),
            data_type,
            nullable,
            expr_id: ExprId::fresh(),
            source_quals: BTreeSet::new(),
        }
    }

    /// Construct a brand-new attribute from a [`StructField`], minting a
    /// fresh id. Use only at a genuine origination point (leaf schemas); NOT
    /// a general `StructField -> Attribute` bridge — passthroughs must clone
    /// an existing `Attribute` instead, never re-derive one from a field.
    pub fn from_field(field: &StructField) -> Self {
        Self::minted(field.name.clone(), field.data_type.clone(), field.nullable)
    }

    /// Chainable: overwrite `source_quals` — for LEAF mint sites
    /// (`TableScan`, `AliasedRelation`) that seed lineage at origination.
    /// Every OTHER production site inherits `source_quals` by cloning an
    /// existing `Attribute` (never re-derive it) — see the module doc's
    /// CONVENTION.
    pub fn with_quals(mut self, quals: BTreeSet<String>) -> Self {
        self.source_quals = quals;
        self
    }

    /// Project back down to a plain [`StructField`], dropping the id AND the
    /// source-qualifier lineage. This is a one-way door — there is no
    /// `From<StructField>` and no implicit conversion, precisely so every
    /// re-mint is a visible `::minted` call.
    pub fn to_field(&self) -> StructField {
        StructField::new(self.name.clone(), self.data_type.clone(), self.nullable)
    }
}

/// Hand-written `PartialEq` EXCLUDING `expr_id` and `source_quals`: neither
/// is part of the column's logical value — `expr_id` is derived identity
/// bookkeeping and `source_quals` is derived lineage bookkeeping (ADR-023
/// tier-3) — mirrors `ColumnReference`'s hand-written `PartialEq` excluding
/// `ordinal` (`expression.rs`, `ColumnReference`) and `TypedAst`/`RelScope`'s
/// derived-data exclusions elsewhere in this module tree. Keeps every
/// pre-existing schema-equality test (written against `StructField`/
/// `StructType` semantics) passing unchanged once schemas carry ids.
impl PartialEq for Attribute {
    fn eq(&self, other: &Self) -> bool {
        self.name == other.name
            && self.data_type == other.data_type
            && self.nullable == other.nullable
    }
}

/// Cross-type equality against a plain [`StructField`] — name/type/nullable
/// only, case-SENSITIVE names, exactly like `StructField`'s derived `PartialEq`.
impl PartialEq<StructField> for Attribute {
    fn eq(&self, other: &StructField) -> bool {
        self.name == other.name
            && self.data_type == other.data_type
            && self.nullable == other.nullable
    }
}

impl PartialEq<Attribute> for StructField {
    fn eq(&self, other: &Attribute) -> bool {
        other == self
    }
}

/// τ's resolved-schema type: an ordered list of [`Attribute`]s — a
/// `StructType` whose columns additionally carry stable identity.
///
/// Deliberately has NO `Eq` / `Hash` derive: `Attribute` excludes `expr_id`
/// from `PartialEq`, so `ResolvedSchema`'s `PartialEq` is likewise
/// value-only (not full equivalence-relation-safe for hashing purposes in
/// spirit, and there is no production need to hash a schema). If a compile
/// error ever surfaces a site that wants to hash a `ResolvedSchema`, convert
/// via `to_struct_type()` at that site and hash the `StructType` instead —
/// do NOT add a derived/manual `Hash` here.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResolvedSchema {
    pub fields: Vec<Attribute>,
}

impl ResolvedSchema {
    pub fn new(fields: Vec<Attribute>) -> Self {
        Self { fields }
    }

    /// An empty schema (used as a sentinel for unresolvable plans).
    pub fn empty() -> Self {
        Self { fields: Vec::new() }
    }

    /// THE ONE production `StructType -> ResolvedSchema` door: mints a fresh
    /// [`ExprId`] per column. Every other production site must receive an
    /// already-`ResolvedSchema` (or `Attribute`) value and clone/mutate it —
    /// never re-derive one from a `StructType` (that would silently mint a
    /// *new* identity for a column that already had one upstream). Leaf
    /// schema arms (`TableScan`, `Values`, `LocalRelation`, `FileScan`, ...)
    /// are exactly where this call belongs.
    pub fn minted(st: StructType) -> Self {
        Self {
            fields: st.fields.iter().map(Attribute::from_field).collect(),
        }
    }

    /// Project back down to a plain [`StructType`], dropping every id. There
    /// is intentionally no blanket `Into<StructType>` — the two production
    /// callers (`generate_with_schema` / `analyze_schema` in `mod.rs`) call
    /// this explicitly; any OTHER production call site is id-laundering and
    /// should stop and get reviewed rather than silently added.
    pub fn to_struct_type(&self) -> StructType {
        StructType {
            fields: self.fields.iter().map(Attribute::to_field).collect(),
        }
    }

    /// Lookup a field by name (case-insensitive, matches Spark behaviour;
    /// mirrors `StructType::field_by_name`).
    pub fn field_by_name(&self, name: &str) -> Option<&Attribute> {
        self.fields
            .iter()
            .find(|f| f.name.eq_ignore_ascii_case(name))
    }

    /// All field names in order.
    pub fn field_names(&self) -> Vec<&str> {
        self.fields.iter().map(|f| f.name.as_str()).collect()
    }

    pub fn len(&self) -> usize {
        self.fields.len()
    }

    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Merge two schemas (used for JOIN output: left fields then right
    /// fields). Duplicate names are kept — callers must qualify with table
    /// aliases. Both sides' attributes (and their ids) ride through
    /// unchanged — this is a pure concatenation, never a re-mint.
    pub fn merge(left: &ResolvedSchema, right: &ResolvedSchema) -> ResolvedSchema {
        let mut fields = left.fields.clone();
        fields.extend(right.fields.iter().cloned());
        ResolvedSchema { fields }
    }
}

/// Cross-type equality against a plain [`StructType`] — delegates
/// field-by-field to `Attribute`'s `PartialEq<StructField>`.
impl PartialEq<StructType> for ResolvedSchema {
    fn eq(&self, other: &StructType) -> bool {
        self.fields.len() == other.fields.len()
            && self
                .fields
                .iter()
                .zip(other.fields.iter())
                .all(|(a, f)| a == f)
    }
}

impl PartialEq<ResolvedSchema> for StructType {
    fn eq(&self, other: &ResolvedSchema) -> bool {
        other == self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn field(name: &str, data_type: DataType, nullable: bool) -> StructField {
        StructField::new(name, data_type, nullable)
    }

    #[test]
    fn fresh_ids_are_unique() {
        let a = ExprId::fresh();
        let b = ExprId::fresh();
        assert_ne!(a, b);
    }

    #[test]
    fn minted_schema_has_distinct_ids_per_column() {
        let st = StructType::new(vec![
            field("a", DataType::Integer, false),
            field("b", DataType::String, true),
        ]);
        let rs = ResolvedSchema::minted(st);
        assert_ne!(rs.fields[0].expr_id, rs.fields[1].expr_id);
    }

    #[test]
    fn attribute_eq_excludes_expr_id() {
        let a = Attribute::minted("x", DataType::Integer, false);
        let b = Attribute::minted("x", DataType::Integer, false);
        assert_ne!(a.expr_id, b.expr_id);
        assert_eq!(a, b, "Attribute equality must ignore expr_id");
    }

    #[test]
    fn resolved_schema_eq_excludes_expr_id() {
        let st = StructType::new(vec![field("a", DataType::Integer, false)]);
        let rs1 = ResolvedSchema::minted(st.clone());
        let rs2 = ResolvedSchema::minted(st);
        assert_eq!(rs1, rs2, "ResolvedSchema equality must ignore expr_id");
    }

    #[test]
    fn cross_type_partial_eq_with_struct_type() {
        let st = StructType::new(vec![
            field("a", DataType::Integer, false),
            field("b", DataType::String, true),
        ]);
        let rs = ResolvedSchema::minted(st.clone());
        assert_eq!(rs, st);
        assert_eq!(st, rs);
    }

    #[test]
    fn to_struct_type_round_trips_values() {
        let st = StructType::new(vec![
            field("a", DataType::Integer, false),
            field("b", DataType::String, true),
        ]);
        let rs = ResolvedSchema::minted(st.clone());
        assert_eq!(rs.to_struct_type(), st);
    }

    #[test]
    fn clone_preserves_expr_id() {
        let attr = Attribute::minted("x", DataType::Integer, false);
        let id = attr.expr_id;
        let cloned = attr.clone();
        assert_eq!(cloned.expr_id, id);
    }

    #[test]
    fn merge_preserves_both_sides_ids() {
        let left = ResolvedSchema::new(vec![Attribute::minted("a", DataType::Integer, false)]);
        let right = ResolvedSchema::new(vec![Attribute::minted("b", DataType::String, true)]);
        let left_id = left.fields[0].expr_id;
        let right_id = right.fields[0].expr_id;
        let merged = ResolvedSchema::merge(&left, &right);
        assert_eq!(merged.len(), 2);
        assert_eq!(merged.fields[0].expr_id, left_id);
        assert_eq!(merged.fields[1].expr_id, right_id);
    }

    #[test]
    fn field_by_name_case_insensitive() {
        let st = StructType::new(vec![field("Name", DataType::String, true)]);
        let rs = ResolvedSchema::minted(st);
        assert!(rs.field_by_name("name").is_some());
        assert!(rs.field_by_name("NAME").is_some());
        assert!(rs.field_by_name("missing").is_none());
    }

    #[test]
    fn minted_has_empty_source_quals() {
        let a = Attribute::minted("x", DataType::Integer, false);
        assert!(a.source_quals.is_empty());
    }

    #[test]
    fn with_quals_overwrites_source_quals_and_keeps_expr_id() {
        let a = Attribute::minted("x", DataType::Integer, false);
        let id = a.expr_id;
        let quals: BTreeSet<String> = ["t".to_owned(), "alias".to_owned()].into_iter().collect();
        let a = a.with_quals(quals.clone());
        assert_eq!(a.source_quals, quals);
        assert_eq!(a.expr_id, id);
    }

    #[test]
    fn attribute_eq_excludes_source_quals() {
        let a = Attribute::minted("x", DataType::Integer, false);
        let b = Attribute::minted("x", DataType::Integer, false)
            .with_quals(["t".to_owned()].into_iter().collect());
        assert_eq!(a, b, "Attribute equality must ignore source_quals");
    }

    #[test]
    fn attribute_partial_eq_struct_field_ignores_case_sensitively() {
        let attr = Attribute::minted("Name", DataType::String, true);
        let same = StructField::new("Name", DataType::String, true);
        let different_case = StructField::new("name", DataType::String, true);
        assert_eq!(attr, same);
        assert_ne!(attr, different_case);
    }
}
