//! τ's analyzer fixtures for INV4 and INV5.
//!
//! **INV10:** this module imports ONLY `crate::types::{DataType, StructField,
//! StructType}` and intra-τ modules.
//!
//! The input-relation schemas mirror the differential corpus's analyzer
//! inputs, reduced to τ's types.
//!
//! [`all_fixtures`] yields Ok-path fixtures only — used by
//! `invariants::inv4_inference_validated_in_isolation` and
//! `invariants::inv5_schema_everywhere`. Error-path fixtures are exercised
//! directly inside `analyzer.rs::tests`.

use super::analyzer::flip_all_nullable;
use super::ast::{CommonAst, CommonOp, JoinType, SetOpKind};
use super::base_types::BaseTypes;
use super::expression::{
    BinaryExpression, BinaryOp, Expression, ExtractValueExpression, Literal, LiteralValue,
    StarExpression, UnresolvedColumn,
};
use super::identifier::Qualifier;
use super::schema::ResolvedSchema;
use crate::types::{DataType, StructField, StructType};

/// Schema for the `emp` input relation.
pub(crate) fn emp_schema() -> StructType {
    StructType::new(vec![
        StructField::not_null("id", DataType::Long),
        StructField::nullable("name", DataType::String),
        StructField::nullable("dept_id", DataType::Integer),
        StructField::nullable("manager_id", DataType::Long),
        StructField::nullable("age", DataType::Integer),
        StructField::nullable("salary", DataType::Double),
        StructField::nullable(
            "bonus",
            DataType::Decimal {
                precision: 9,
                scale: 2,
            },
        ),
        StructField::nullable("hire_date", DataType::Date),
        StructField::nullable("last_login", DataType::Timestamp),
        StructField::nullable("active", DataType::Boolean),
        StructField::nullable("score", DataType::Double),
        StructField::nullable("tags", DataType::Array(Box::new(DataType::String), true)),
        StructField::nullable(
            "attrs",
            DataType::Map {
                key: Box::new(DataType::String),
                value: Box::new(DataType::String),
                value_nullable: true,
            },
        ),
        StructField::nullable(
            "address",
            DataType::Struct(StructType::new(vec![
                StructField::nullable("city", DataType::String),
                StructField::nullable("zip", DataType::String),
                StructField::nullable(
                    "geo",
                    DataType::Struct(StructType::new(vec![
                        StructField::nullable("lat", DataType::Double),
                        StructField::nullable("lng", DataType::Double),
                    ])),
                ),
            ])),
        ),
    ])
}

/// Schema for the `dept` input relation.
pub(crate) fn dept_schema() -> StructType {
    StructType::new(vec![
        StructField::not_null("dept_id", DataType::Integer),
        StructField::nullable("dept_name", DataType::String),
        StructField::nullable(
            "budget",
            DataType::Decimal {
                precision: 12,
                scale: 2,
            },
        ),
        StructField::nullable("location", DataType::String),
        StructField::nullable("country", DataType::String),
    ])
}

/// Schema for the `emp2` input relation (union-compatible with a subset of `emp`).
pub(crate) fn emp2_schema() -> StructType {
    StructType::new(vec![
        StructField::not_null("id", DataType::Long),
        StructField::nullable("name", DataType::String),
        StructField::nullable("dept_id", DataType::Integer),
        StructField::nullable("age", DataType::Integer),
        StructField::nullable("salary", DataType::Double),
        StructField::nullable("country", DataType::String),
    ])
}

/// Schema for the `nums` input relation.
pub(crate) fn nums_schema() -> StructType {
    StructType::new(vec![
        StructField::nullable("a", DataType::Integer),
        StructField::nullable("b", DataType::Integer),
        StructField::nullable("x", DataType::Double),
        StructField::nullable("y", DataType::Double),
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
        StructField::nullable("lng", DataType::Long),
    ])
}

/// Schema for the `raw` text-payload input relation.
pub(crate) fn raw_schema() -> StructType {
    StructType::new(vec![
        StructField::not_null("id", DataType::Long),
        StructField::nullable("json_str", DataType::String),
        StructField::nullable("csv_str", DataType::String),
        StructField::nullable("url", DataType::String),
        StructField::nullable("num_str", DataType::String),
    ])
}

/// Construct a `BaseTypes` overlay pre-populated with the five input relations.
pub(crate) fn base_types_all_inputs() -> BaseTypes {
    // Build a walker plan that references every input so `BaseTypes` picks
    // them up. We pin one TableScan per input under a synthetic Join tree.
    let plan = table_scan_chain(&["emp", "dept", "emp2", "nums", "raw"]);
    BaseTypes::build_from_plan(&plan, |name| match name {
        "emp" => Some(emp_schema()),
        "dept" => Some(dept_schema()),
        "emp2" => Some(emp2_schema()),
        "nums" => Some(nums_schema()),
        "raw" => Some(raw_schema()),
        _ => None,
    })
}

fn table_scan(name: &str) -> CommonAst {
    CommonAst::new(CommonOp::TableScan {
        table: Qualifier::single(name),
    })
}

fn table_scan_chain(names: &[&str]) -> CommonAst {
    let mut iter = names.iter();
    let first = table_scan(iter.next().expect("at least one name"));
    iter.fold(first, |acc, name| {
        CommonAst::new(CommonOp::Join {
            left: Box::new(acc),
            right: Box::new(table_scan(name)),
            join_type: JoinType::Cross,
            condition: None,
            using_columns: vec![],
            natural: false,
            lateral: false,
        })
    })
}

/// A ready-to-analyze fixture record.
pub(crate) type Fixture = (&'static str, CommonAst, BaseTypes, StructType);

/// Yield every Ok-path τ's analyzer fixture.
///
/// Error-path fixtures are exercised directly in `analyzer.rs::tests`.
pub(crate) fn all_fixtures() -> impl Iterator<Item = Fixture> {
    vec![
        input_relation_emp(),
        input_relation_dept(),
        input_relation_emp2(),
        input_relation_nums(),
        input_relation_raw(),
        union_widens_int_and_decimal(),
        intersect_widens_int_and_double(),
        except_widens_short_and_long(),
        left_outer_join_flips_right_nullability(),
        right_outer_join_flips_left_nullability(),
        full_outer_join_flips_both_sides(),
        nested_struct_field_access(),
        plan_id_disambiguates_self_join(),
        star_expansion_in_project(),
        sparksql_no_plan_id_resolves_by_qualifier(),
        cross_join_dept_dept_id_binds_right_not_first_match(),
        cross_join_emp_dept_id_left_twin_unharmed(),
        inner_join_dept_dept_id_binds_right_not_cross_specific(),
        left_join_dept_dept_id_flips_through_range(),
        right_join_emp_id_flips_left_through_range(),
        full_join_dept_dept_id_flips_through_range(),
        duplicate_name_wrong_type_binds_by_range(),
        project_over_filter_over_join_binds_alias_through_passthrough(),
        nested_left_join_inner_join_ranges_beat_side_schemas(),
        semi_join_left_qualifier_resolves_from_left_side(),
        semi_join_child_sibling_offset_alignment(),
        project_over_deduplicate_over_join_binds_alias_through_passthrough(),
    ]
    .into_iter()
}

fn input_relation_emp() -> Fixture {
    (
        "emp",
        table_scan("emp"),
        base_types_all_inputs(),
        emp_schema(),
    )
}

fn input_relation_dept() -> Fixture {
    (
        "dept",
        table_scan("dept"),
        base_types_all_inputs(),
        dept_schema(),
    )
}

fn input_relation_emp2() -> Fixture {
    (
        "emp2",
        table_scan("emp2"),
        base_types_all_inputs(),
        emp2_schema(),
    )
}

fn input_relation_nums() -> Fixture {
    (
        "nums",
        table_scan("nums"),
        base_types_all_inputs(),
        nums_schema(),
    )
}

fn input_relation_raw() -> Fixture {
    (
        "raw",
        table_scan("raw"),
        base_types_all_inputs(),
        raw_schema(),
    )
}

fn values_row(name: &str, lit: LiteralValue, dt: DataType) -> CommonAst {
    CommonAst::new(CommonOp::Values {
        rows: vec![vec![Expression::Literal(Literal {
            value: lit,
            data_type: dt,
        })]],
        column_names: vec![name.to_owned()],
    })
}

fn union_widens_int_and_decimal() -> Fixture {
    let child_int = values_row("x", LiteralValue::Int(1), DataType::Integer);
    let child_dec = values_row(
        "x",
        LiteralValue::Decimal {
            value: "1.00".to_owned(),
            precision: 10,
            scale: 2,
        },
        DataType::Decimal {
            precision: 10,
            scale: 2,
        },
    );
    let ast = CommonAst::new(CommonOp::SetOp {
        kind: SetOpKind::Union,
        all: true,
        by_name: false,
        allow_missing_columns: false,
        children: vec![child_int, child_dec],
    });
    // Integer × Decimal(10,2) → Decimal(unify): Integer → Decimal(10,0),
    // then unify_decimal(10,0, 10,2) → precision=int_digits(10) + scale(2) = 12
    // but bounded at 38. Result: Decimal(12, 2).
    // Both literal children are non-null, so widened nullable = false.
    let expected = StructType::new(vec![StructField::not_null(
        "x",
        DataType::Decimal {
            precision: 12,
            scale: 2,
        },
    )]);
    (
        "union_widens_int_and_decimal",
        ast,
        BaseTypes::empty(),
        expected,
    )
}

fn intersect_widens_int_and_double() -> Fixture {
    let child_int = values_row("x", LiteralValue::Int(1), DataType::Integer);
    let child_dbl = values_row("x", LiteralValue::Double(1.5), DataType::Double);
    let ast = CommonAst::new(CommonOp::SetOp {
        kind: SetOpKind::Intersect,
        all: false,
        by_name: false,
        allow_missing_columns: false,
        children: vec![child_int, child_dbl],
    });
    let expected = StructType::new(vec![StructField::not_null("x", DataType::Double)]);
    (
        "intersect_widens_int_and_double",
        ast,
        BaseTypes::empty(),
        expected,
    )
}

fn except_widens_short_and_long() -> Fixture {
    let child_short = values_row("x", LiteralValue::Short(1), DataType::Short);
    let child_long = values_row("x", LiteralValue::Long(1), DataType::Long);
    let ast = CommonAst::new(CommonOp::SetOp {
        kind: SetOpKind::Except,
        all: false,
        by_name: false,
        allow_missing_columns: false,
        children: vec![child_short, child_long],
    });
    let expected = StructType::new(vec![StructField::not_null("x", DataType::Long)]);
    (
        "except_widens_short_and_long",
        ast,
        BaseTypes::empty(),
        expected,
    )
}

/// Test-only bridge: `flip_all_nullable` operates on `ResolvedSchema`, but
/// these fixtures build their `expected` schema as a plain `StructType`
/// (per `Fixture`'s shape) — mint, flip, then drop back to `StructType`.
fn flip_all_nullable_struct(schema: StructType) -> StructType {
    flip_all_nullable(&ResolvedSchema::minted(schema)).to_struct_type()
}

/// Positional concatenation — a join's expected output shape. Production
/// widening is `ResolvedSchema::merge`; fixtures need the id-free form.
fn merge(left: &StructType, right: &StructType) -> StructType {
    let mut fields = left.fields.clone();
    fields.extend(right.fields.clone());
    StructType { fields }
}

fn left_outer_join_flips_right_nullability() -> Fixture {
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(table_scan("emp")),
        right: Box::new(table_scan("dept")),
        join_type: JoinType::Left,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    // LEFT: left preserved, right flipped nullable.
    let expected = merge(&emp_schema(), &flip_all_nullable_struct(dept_schema()));
    (
        "left_outer_join_flips_right_nullability",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn right_outer_join_flips_left_nullability() -> Fixture {
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(table_scan("emp")),
        right: Box::new(table_scan("dept")),
        join_type: JoinType::Right,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let expected = merge(&flip_all_nullable_struct(emp_schema()), &dept_schema());
    (
        "right_outer_join_flips_left_nullability",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn full_outer_join_flips_both_sides() -> Fixture {
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(table_scan("emp")),
        right: Box::new(table_scan("dept")),
        join_type: JoinType::Full,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let expected = merge(
        &flip_all_nullable_struct(emp_schema()),
        &flip_all_nullable_struct(dept_schema()),
    );
    (
        "full_outer_join_flips_both_sides",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn nested_struct_field_access() -> Fixture {
    // Project `emp.address.geo.lat` — the analyzer should resolve nested
    // nullability via ExtractValue chaining. Since ExtractValue is
    // structural (child + extraction), we can express it as two nested
    // ExtractValue expressions over `address`.
    let address_expr = Expression::UnresolvedColumn(UnresolvedColumn {
        name_parts: vec!["address".to_owned()],
        plan_id: None,
        is_metadata_column: false,
    });
    let geo_expr = Expression::ExtractValue(ExtractValueExpression {
        child: Box::new(address_expr),
        extraction: Box::new(Expression::Literal(Literal {
            value: LiteralValue::String("geo".to_owned()),
            data_type: DataType::String,
        })),
    });
    let lat_expr = Expression::ExtractValue(ExtractValueExpression {
        child: Box::new(geo_expr),
        extraction: Box::new(Expression::Literal(Literal {
            value: LiteralValue::String("lat".to_owned()),
            data_type: DataType::String,
        })),
    });
    let ast = CommonAst::new(CommonOp::Project {
        input: Box::new(table_scan("emp")),
        projections: vec![lat_expr],
    });
    let expected = StructType::new(vec![StructField::nullable("lat", DataType::Double)]);
    (
        "nested_struct_field_access",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn plan_id_disambiguates_self_join() -> Fixture {
    let cond = Expression::Binary(BinaryExpression {
        op: BinaryOp::Eq,
        left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec!["id".to_owned()],
            plan_id: Some(1),
            is_metadata_column: false,
        })),
        right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec!["manager_id".to_owned()],
            plan_id: Some(2),
            is_metadata_column: false,
        })),
    });
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("emp", "e1").with_plan_id(1)),
        right: Box::new(aliased_scan("emp", "e2").with_plan_id(2)),
        join_type: JoinType::Inner,
        condition: Some(cond),
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    // Inner join over emp × emp: both sides preserve full schema.
    let expected = merge(&emp_schema(), &emp_schema());
    (
        "plan_id_disambiguates_self_join",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn star_expansion_in_project() -> Fixture {
    let ast = CommonAst::new(CommonOp::Project {
        input: Box::new(table_scan("dept")),
        projections: vec![Expression::Star(StarExpression::Unqualified)],
    });
    let expected = dept_schema();
    (
        "star_expansion_in_project",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn sparksql_no_plan_id_resolves_by_qualifier() -> Fixture {
    let ast = CommonAst::new(CommonOp::Project {
        input: Box::new(table_scan("emp")),
        projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec!["emp".to_owned(), "id".to_owned()],
            plan_id: None,
            is_metadata_column: false,
        })],
    });
    let expected = StructType::new(vec![StructField::not_null("id", DataType::Long)]);
    (
        "sparksql_no_plan_id_resolves_by_qualifier",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn aliased_scan(table: &str, alias: &str) -> CommonAst {
    CommonAst::new(CommonOp::AliasedRelation {
        input: Box::new(CommonAst::new(CommonOp::TableScan {
            table: Qualifier::single(table),
        })),
        alias: alias.to_owned(),
    })
}

fn emp_e_dept_d_join(join_type: JoinType) -> CommonAst {
    CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("emp", "e")),
        right: Box::new(aliased_scan("dept", "d")),
        join_type,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    })
}

fn project_qualified_ref(input: CommonAst, qualifier: &str, name: &str) -> CommonAst {
    CommonAst::new(CommonOp::Project {
        input: Box::new(input),
        projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
            name_parts: vec![qualifier.to_owned(), name.to_owned()],
            plan_id: None,
            is_metadata_column: false,
        })],
    })
}

/// jn-006 witness: `SELECT d.dept_id FROM emp e CROSS JOIN dept d` — must
/// bind to dept's non-null `dept_id`, not emp's nullable copy (the first
/// match by name in the merged schema).
fn cross_join_dept_dept_id_binds_right_not_first_match() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Cross), "d", "dept_id");
    let expected = StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
    (
        "cross_join_dept_dept_id_binds_right_not_first_match",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Left twin of the jn-006 witness: `e.dept_id` over the same CROSS JOIN
/// must still resolve to emp's (nullable) copy — the fix must not disturb
/// the correct, already-working side.
fn cross_join_emp_dept_id_left_twin_unharmed() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Cross), "e", "dept_id");
    let expected = StructType::new(vec![StructField::nullable("dept_id", DataType::Integer)]);
    (
        "cross_join_emp_dept_id_left_twin_unharmed",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// The bug is not CROSS-specific: INNER over the same duplicated-name shape
/// mis-stamps identically without the fix.
fn inner_join_dept_dept_id_binds_right_not_cross_specific() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Inner), "d", "dept_id");
    let expected = StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
    (
        "inner_join_dept_dept_id_binds_right_not_cross_specific",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// LEFT JOIN flips the right side nullable — `d.dept_id` (non-null at rest)
/// must come out nullable via the SAME range-based lookup, i.e. the flip
/// must be visible through the alias binding, not just the flat schema.
fn left_join_dept_dept_id_flips_through_range() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Left), "d", "dept_id");
    let expected = StructType::new(vec![StructField::nullable("dept_id", DataType::Integer)]);
    (
        "left_join_dept_dept_id_flips_through_range",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// RIGHT JOIN flips the left side nullable — `e.id` (non-null at rest) must
/// come out nullable.
fn right_join_emp_id_flips_left_through_range() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Right), "e", "id");
    let expected = StructType::new(vec![StructField::nullable("id", DataType::Long)]);
    (
        "right_join_emp_id_flips_left_through_range",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// FULL JOIN flips both sides nullable.
fn full_join_dept_dept_id_flips_through_range() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::Full), "d", "dept_id");
    let expected = StructType::new(vec![StructField::nullable("dept_id", DataType::Integer)]);
    (
        "full_join_dept_dept_id_flips_through_range",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Duplicated-name adjacency with DIFFERING TYPES (not just nullability):
/// `l.k` is `Integer`, `r.k` is `Long`. A name-only first match would stamp
/// `r.k` as `Integer` (wrong TYPE, not just wrong nullability).
fn duplicate_name_wrong_type_binds_by_range() -> Fixture {
    let l_schema = StructType::new(vec![StructField::not_null("k", DataType::Integer)]);
    let r_schema = StructType::new(vec![StructField::not_null("k", DataType::Long)]);
    let bt = BaseTypes::from_entries(
        [
            (Qualifier::single("l"), l_schema),
            (Qualifier::single("r"), r_schema),
        ]
        .into_iter()
        .collect(),
    );
    let join = CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("l", "lft")),
        right: Box::new(aliased_scan("r", "rgt")),
        join_type: JoinType::Inner,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let ast = project_qualified_ref(join, "rgt", "k");
    let expected = StructType::new(vec![StructField::not_null("k", DataType::Long)]);
    (
        "duplicate_name_wrong_type_binds_by_range",
        ast,
        bt,
        expected,
    )
}

/// Passthrough descent: `Project(Filter(Join Cross))` — the qualifier
/// binding must be visible through the schema-verbatim `Filter` node so
/// `d.dept_id` still resolves correctly one level up.
fn project_over_filter_over_join_binds_alias_through_passthrough() -> Fixture {
    let filter_ast = CommonAst::new(CommonOp::Filter {
        input: Box::new(emp_e_dept_d_join(JoinType::Cross)),
        condition: Expression::Literal(Literal {
            value: LiteralValue::Boolean(true),
            data_type: DataType::Boolean,
        }),
    });
    let ast = project_qualified_ref(filter_ast, "d", "dept_id");
    let expected = StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
    (
        "project_over_filter_over_join_binds_alias_through_passthrough",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Nesting: `a LEFT JOIN (b INNER JOIN c)`. `c.id` (emp2's non-null `id`) is
/// untouched by the INNER join, but the OUTER LEFT join flips the entire
/// right subtree (including `c`) nullable. Ranges are read off the
/// fully-flipped outer schema, so this comes out nullable — a retained
/// pre-flip side-schema for the inner join would wrongly report non-null.
fn nested_left_join_inner_join_ranges_beat_side_schemas() -> Fixture {
    let inner = CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("dept", "b")),
        right: Box::new(aliased_scan("emp2", "c")),
        join_type: JoinType::Inner,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let outer = CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("emp", "a")),
        right: Box::new(inner),
        join_type: JoinType::Left,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let ast = project_qualified_ref(outer, "c", "id");
    let expected = StructType::new(vec![StructField::nullable("id", DataType::Long)]);
    (
        "nested_left_join_inner_join_ranges_beat_side_schemas",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Semi-join left-only recursion: `SELECT e.dept_id FROM emp e SEMI JOIN
/// dept d`. A SEMI join's output schema is the LEFT side only, so `e`
/// binds to the full range and `d` (the right side) contributes no
/// columns / no binding. `e.dept_id` must resolve to emp's nullable copy.
fn semi_join_left_qualifier_resolves_from_left_side() -> Fixture {
    let ast = project_qualified_ref(emp_e_dept_d_join(JoinType::LeftSemi), "e", "dept_id");
    let expected = StructType::new(vec![StructField::nullable("dept_id", DataType::Integer)]);
    (
        "semi_join_left_qualifier_resolves_from_left_side",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Sibling-offset alignment after a left-only semi recursion:
/// `(emp e SEMI JOIN dept d) INNER JOIN dept d2`. The semi join emits only
/// emp's 16 columns, so `collect_qualifier_bindings` must offset the outer
/// join's RIGHT child (`d2`) by 16 — the semi join's resolved-schema length
/// — not by 21 (emp+dept, as if it had recursed the semi's right side).
/// `d2.dept_id` (a name that ALSO exists, nullable, in emp's left range)
/// must resolve to dept's non-null copy at the correct offset.
fn semi_join_child_sibling_offset_alignment() -> Fixture {
    let semi = CommonAst::new(CommonOp::Join {
        left: Box::new(aliased_scan("emp", "e")),
        right: Box::new(aliased_scan("dept", "d")),
        join_type: JoinType::LeftSemi,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let outer = CommonAst::new(CommonOp::Join {
        left: Box::new(semi),
        right: Box::new(aliased_scan("dept", "d2")),
        join_type: JoinType::Inner,
        condition: None,
        using_columns: vec![],
        natural: false,
        lateral: false,
    });
    let ast = project_qualified_ref(outer, "d2", "dept_id");
    let expected = StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
    (
        "semi_join_child_sibling_offset_alignment",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

/// Passthrough descent through `Deduplicate`: `Project(Deduplicate(Join
/// Cross))`. `Deduplicate` clones the input schema verbatim, so the
/// qualifier binding must be visible through it — `d.dept_id` still
/// resolves to dept's non-null copy one level up.
fn project_over_deduplicate_over_join_binds_alias_through_passthrough() -> Fixture {
    let dedup = CommonAst::new(CommonOp::Deduplicate {
        input: Box::new(emp_e_dept_d_join(JoinType::Cross)),
        on_columns: vec![],
    });
    let ast = project_qualified_ref(dedup, "d", "dept_id");
    let expected = StructType::new(vec![StructField::not_null("dept_id", DataType::Integer)]);
    (
        "project_over_deduplicate_over_join_binds_alias_through_passthrough",
        ast,
        base_types_all_inputs(),
        expected,
    )
}
