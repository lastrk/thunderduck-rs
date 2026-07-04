//! Slice B fixtures — 5 input relations + 11 mini-fixtures for INV4 / INV5.
//!
//! **INV10:** this module imports ONLY `crate::types::{DataType, StructField,
//! StructType}` and intra-τ modules.
//!
//! The five input-relation schemas mirror
//! `tests/integration/differential/dataframe_corpus.py::build_inputs`,
//! reduced to Slice B types (arrays / maps / structs kept for `emp.address`).
//!
//! [`all_fixtures`] yields Ok-path fixtures only — used by
//! `invariants::inv4_inference_validated_in_isolation` and
//! `invariants::inv5_schema_everywhere`. Error-path fixtures (e.g. ambiguous
//! column) are exercised directly inside `analyzer.rs::tests`.

use super::ast::{CommonAst, CommonOp, JoinType, SetOpKind};
use super::base_types::BaseTypes;
use super::expression::{
    BinaryExpression, BinaryOp, Expression, ExtractValueExpression, Literal, LiteralValue,
    StarExpression, UnresolvedColumn,
};
use crate::types::{DataType, StructField, StructType};

// ── Input-relation schemas ──────────────────────────────────────────────────

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
        table: name.to_owned(),
        alias: None,
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
            left_plan_ids: vec![],
            right_plan_ids: vec![],
        })
    })
}

// ── Mini-fixture builders ───────────────────────────────────────────────────

/// A ready-to-analyze fixture record.
pub(crate) type Fixture = (&'static str, CommonAst, BaseTypes, StructType);

/// Yield every Ok-path Slice B fixture.
///
/// Ordering matches plan §7 §11 — error-path fixtures (`AmbiguousColumn`) are
/// exercised directly in `analyzer.rs::tests`, not here.
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
    ]
    .into_iter()
}

// ── Input-relation fixtures ────────────────────────────────────────────────

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

// ── Set-op widening fixtures ────────────────────────────────────────────────

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

// ── Outer-join nullability fixtures ─────────────────────────────────────────

fn left_outer_join_flips_right_nullability() -> Fixture {
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(table_scan("emp")),
        right: Box::new(table_scan("dept")),
        join_type: JoinType::Left,
        condition: None,
        using_columns: vec![],
        left_plan_ids: vec![],
        right_plan_ids: vec![],
    });
    // LEFT: left preserved, right flipped nullable.
    let expected = StructType::merge(&emp_schema(), &flip_all_nullable(&dept_schema()));
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
        left_plan_ids: vec![],
        right_plan_ids: vec![],
    });
    let expected = StructType::merge(&flip_all_nullable(&emp_schema()), &dept_schema());
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
        left_plan_ids: vec![],
        right_plan_ids: vec![],
    });
    let expected = StructType::merge(
        &flip_all_nullable(&emp_schema()),
        &flip_all_nullable(&dept_schema()),
    );
    (
        "full_outer_join_flips_both_sides",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

fn flip_all_nullable(schema: &StructType) -> StructType {
    let fields = schema
        .fields
        .iter()
        .map(|f| StructField::new(f.name.clone(), f.data_type.clone(), true))
        .collect();
    StructType::new(fields)
}

// ── Nested-struct field access ──────────────────────────────────────────────

fn nested_struct_field_access() -> Fixture {
    // Project `emp.address.geo.lat` — the analyzer should resolve nested
    // nullability via ExtractValue chaining. Since ExtractValue is
    // structural (child + extraction), we can express it as two nested
    // ExtractValue expressions over `address`.
    let address_expr = Expression::UnresolvedColumn(UnresolvedColumn {
        name: "address".to_owned(),
        qualifier: None,
        plan_id: None,
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
    let expected = StructType::new(vec![StructField::nullable("expr", DataType::Double)]);
    (
        "nested_struct_field_access",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

// ── plan_id disambiguates self-join ─────────────────────────────────────────

fn plan_id_disambiguates_self_join() -> Fixture {
    // `emp AS e1 JOIN emp AS e2 ON e1.id = e2.manager_id` — we use aliases
    // so the join condition columns can be resolved to distinct sides via
    // qualifier. plan_ids are attached to signal Slice E's downstream use.
    let cond = Expression::Binary(BinaryExpression {
        op: BinaryOp::Eq,
        left: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
            name: "id".to_owned(),
            qualifier: Some("e1".to_owned()),
            plan_id: Some(1),
        })),
        right: Box::new(Expression::UnresolvedColumn(UnresolvedColumn {
            name: "manager_id".to_owned(),
            qualifier: Some("e2".to_owned()),
            plan_id: Some(2),
        })),
    });
    let ast = CommonAst::new(CommonOp::Join {
        left: Box::new(CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: Some("e1".to_owned()),
        })),
        right: Box::new(CommonAst::new(CommonOp::TableScan {
            table: "emp".to_owned(),
            alias: Some("e2".to_owned()),
        })),
        join_type: JoinType::Inner,
        condition: Some(cond),
        using_columns: vec![],
        left_plan_ids: vec![1],
        right_plan_ids: vec![2],
    });
    // Inner join over emp × emp: both sides preserve full schema.
    let expected = StructType::merge(&emp_schema(), &emp_schema());
    (
        "plan_id_disambiguates_self_join",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

// ── Star expansion ──────────────────────────────────────────────────────────

fn star_expansion_in_project() -> Fixture {
    let ast = CommonAst::new(CommonOp::Project {
        input: Box::new(table_scan("dept")),
        projections: vec![Expression::Star(StarExpression { qualifier: None })],
    });
    let expected = dept_schema();
    (
        "star_expansion_in_project",
        ast,
        base_types_all_inputs(),
        expected,
    )
}

// ── SparkSQL plan_id = None resolves via qualifier (Open Decision 12) ───────

fn sparksql_no_plan_id_resolves_by_qualifier() -> Fixture {
    // Reference `emp.id` with no plan_id — the analyzer must resolve via
    // the qualifier (a struct-field lookup falls through to the top-level
    // column if the qualifier matches an operand alias / column).
    // Slice B's `resolve_column` uses `qualified_column_type` which
    // returns Long for `id` from `emp`.
    let ast = CommonAst::new(CommonOp::Project {
        input: Box::new(table_scan("emp")),
        projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
            name: "id".to_owned(),
            qualifier: Some("emp".to_owned()),
            plan_id: None,
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
