//! Input-relation schema fixtures for the v2 analyzer smoke tests
//! (INV4 §CV.5).
//!
//! These schemas mirror
//! [`tests/integration/differential/dataframe_corpus.py::build_inputs`]
//! (`emp`, `dept`, `emp2`, `nums`, `raw`) field-by-field, including
//! nullability. They are the ground truth for
//! [`crate::transpiler_v2::analyzer::inference_smoke`].
//!
//! Five representative mini-fixtures ([`smoke_type_001`], [`smoke_cond_003`],
//! [`smoke_agg_013`], [`smoke_type_011`], [`smoke_type_019`]) drive the
//! smoke matrix. If any produced schema disagrees with the expected literal
//! Spark schema, [`run_all`] panics with a rich diff naming the fixture,
//! field, and mismatch — never a silent swallowed diff.

use crate::expression::Expression;
use crate::expression::{
    AliasExpression, BinaryExpression, BinaryOp, CaseWhenExpression, CastExpression, FunctionCall,
    UnresolvedColumn,
};
use crate::transpiler_v2::analyzer::{analyze, BaseTypes, TypedAst};
use crate::transpiler_v2::ast::{
    Aggregate, AggregateCall, CommonAst, CommonOp, Join, JoinKind, Project, TableScan, Union,
};
use crate::types::{DataType, StructField, StructType};

// ── Input-relation fixtures ───────────────────────────────────────────────────

/// The 14 fields of `emp`, matching `build_inputs`'s `emp_schema`.
pub fn fixture_emp() -> StructType {
    let geo = StructType::new(vec![
        StructField::nullable("lat", DataType::Double),
        StructField::nullable("lng", DataType::Double),
    ]);
    let address = StructType::new(vec![
        StructField::nullable("city", DataType::String),
        StructField::nullable("zip", DataType::String),
        StructField::nullable("geo", DataType::Struct(geo)),
    ]);
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
        StructField::nullable("address", DataType::Struct(address)),
    ])
}

/// The 5 fields of `dept`.
pub fn fixture_dept() -> StructType {
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

/// The 6 fields of `emp2`.
pub fn fixture_emp2() -> StructType {
    StructType::new(vec![
        StructField::not_null("id", DataType::Long),
        StructField::nullable("name", DataType::String),
        StructField::nullable("dept_id", DataType::Integer),
        StructField::nullable("age", DataType::Integer),
        StructField::nullable("salary", DataType::Double),
        StructField::nullable("country", DataType::String),
    ])
}

/// The 7 fields of `nums`.
pub fn fixture_nums() -> StructType {
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

/// The 5 fields of `raw`.
pub fn fixture_raw() -> StructType {
    StructType::new(vec![
        StructField::not_null("id", DataType::Long),
        StructField::nullable("json_str", DataType::String),
        StructField::nullable("csv_str", DataType::String),
        StructField::nullable("url", DataType::String),
        StructField::nullable("num_str", DataType::String),
    ])
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn base_types() -> BaseTypes {
    let mut m = BaseTypes::new();
    m.insert("emp".to_string(), fixture_emp());
    m.insert("dept".to_string(), fixture_dept());
    m.insert("emp2".to_string(), fixture_emp2());
    m.insert("nums".to_string(), fixture_nums());
    m.insert("raw".to_string(), fixture_raw());
    m
}

fn col(name: &str) -> Expression {
    Expression::UnresolvedColumn(UnresolvedColumn {
        name: name.to_string(),
        qualifier: None,
    })
}

fn table(name: &str) -> CommonOp {
    CommonOp::TableScan(TableScan {
        name: name.to_string(),
        schema: StructType::empty(),
    })
}

fn alias(expr: Expression, name: &str) -> Expression {
    Expression::Alias(AliasExpression {
        expr: Box::new(expr),
        alias: name.to_string(),
    })
}

/// Assert `actual` matches `expected` field-by-field. Panics with a diff that
/// names the mismatched field, expected/actual types, and expected/actual
/// nullability — every case is expressed literally.
fn assert_schema_eq(fixture: &str, actual: &StructType, expected: &StructType) {
    if actual.fields.len() != expected.fields.len() {
        panic!(
            "[{fixture}] schema field count mismatch: expected {} fields {:?}, got {} fields {:?}",
            expected.fields.len(),
            expected
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            actual.fields.len(),
            actual
                .fields
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
        );
    }
    for (i, (a, e)) in actual.fields.iter().zip(expected.fields.iter()).enumerate() {
        if a.name != e.name {
            panic!(
                "[{fixture}] field[{i}] name mismatch: expected `{}`, got `{}`",
                e.name, a.name,
            );
        }
        if a.data_type != e.data_type {
            panic!(
                "[{fixture}] field[{i}] `{}` type mismatch: expected `{}`, got `{}`",
                e.name, e.data_type, a.data_type,
            );
        }
        if a.nullable != e.nullable {
            panic!(
                "[{fixture}] field[{i}] `{}` nullability mismatch: expected nullable={}, got nullable={}",
                e.name, e.nullable, a.nullable,
            );
        }
    }
}

// ── Mini-fixtures ─────────────────────────────────────────────────────────────

/// `type-001`: `nums.select((col('a') + col('lng')).alias('r'))`.
/// Expected: `[r: Long, nullable=true]` — Int + Long promotes to Long,
/// and either operand nullable → result nullable.
pub fn smoke_type_001() -> (CommonAst, StructType) {
    let expr = alias(
        Expression::Binary(BinaryExpression {
            op: BinaryOp::Add,
            left: Box::new(col("a")),
            right: Box::new(col("lng")),
        }),
        "r",
    );
    let ast = CommonAst {
        root: CommonOp::Project(Project {
            input: Box::new(table("nums")),
            projections: vec![expr],
        }),
    };
    let expected = StructType::new(vec![StructField::nullable("r", DataType::Long)]);
    (ast, expected)
}

/// `cond-003`: `emp.select(when(col('active'), col('salary')).alias('maybe_sal'))`.
/// Expected: `[maybe_sal: Double, nullable=true]` — when-without-otherwise is
/// always nullable.
pub fn smoke_cond_003() -> (CommonAst, StructType) {
    let when_expr = Expression::CaseWhen(CaseWhenExpression {
        base: None,
        branches: vec![(col("active"), col("salary"))],
        else_expr: None,
    });
    let expr = alias(when_expr, "maybe_sal");
    let ast = CommonAst {
        root: CommonOp::Project(Project {
            input: Box::new(table("emp")),
            projections: vec![expr],
        }),
    };
    let expected = StructType::new(vec![StructField::nullable("maybe_sal", DataType::Double)]);
    (ast, expected)
}

/// `agg-013`: `emp.agg(percentile_approx('salary', 0.5), median('salary'))`.
/// Expected: two Double-nullable output columns with Spark's default names.
pub fn smoke_agg_013() -> (CommonAst, StructType) {
    // Spark aggregates typically produce `percentile_approx(salary, 0.5)` and
    // `median(salary)` as unaliased column names.  Our `spark_column_name`
    // helper renders literal `Double(0.5)` as `0.5`.
    let percentile = Expression::FunctionCall(FunctionCall {
        name: "percentile_approx".to_string(),
        args: vec![
            col("salary"),
            Expression::Literal(crate::expression::Literal {
                value: crate::expression::LiteralValue::Double(0.5),
                data_type: DataType::Double,
            }),
        ],
        distinct: false,
    });
    let median = Expression::FunctionCall(FunctionCall {
        name: "median".to_string(),
        args: vec![col("salary")],
        distinct: false,
    });
    let ast = CommonAst {
        root: CommonOp::Aggregate(Aggregate {
            input: Box::new(table("emp")),
            grouping: vec![],
            aggregates: vec![
                AggregateCall {
                    func: percentile,
                    is_distinct: false,
                    filter: None,
                },
                AggregateCall {
                    func: median,
                    is_distinct: false,
                    filter: None,
                },
            ],
            having: None,
            grouping_sets: None,
        }),
    };
    // The legacy `spark_column_name` renders the two calls as
    // `percentile_approx(salary, 0.5)` and `median(salary)`.
    // `TypeInferenceEngine::aggregate_return_type` returns Double for
    // both `percentile_approx` and `median` — since `median` is not in
    // its lookup table, the fallback returns the argument type (Double).
    let expected = StructType::new(vec![
        StructField::nullable("percentile_approx(salary, 0.5)", DataType::Double),
        StructField::nullable("median(salary)", DataType::Double),
    ]);
    (ast, expected)
}

/// `type-011`: `dept.join(emp.select('dept_id', col('id').alias('eid')),
/// on='dept_id', how='left')`.
/// Expected: right side's `eid` becomes `Long, nullable=true` (was NOT NULL
/// in `emp`) via Pass 3's outer-join nullability rewrite.
pub fn smoke_type_011() -> (CommonAst, StructType) {
    let right = CommonOp::Project(Project {
        input: Box::new(table("emp")),
        projections: vec![col("dept_id"), alias(col("id"), "eid")],
    });
    let ast = CommonAst {
        root: CommonOp::Join(Join {
            left: Box::new(table("dept")),
            right: Box::new(right),
            join_type: JoinKind::Left,
            on: None,
            using: vec!["dept_id".to_string()],
        }),
    };
    // Expected schema:
    //   dept_id (INT, NOT NULL — from the USING key on the left/inner side)
    //   dept_name, budget, location, country (from dept, all nullable)
    //   eid (LONG, nullable=true — outer join makes right side nullable)
    let expected = StructType::new(vec![
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
        StructField::nullable("eid", DataType::Long),
    ]);
    (ast, expected)
}

/// `type-019`: `nums.select(a.cast(dec(5,0))) unionByName nums.select(d1)`.
/// Expected widened schema: `[a: Decimal(10,2), nullable=true]` per Spark's
/// decimal widening (`unify_decimal(5,0, 10,2)` = scale=max(0,2)=2,
/// int_digits=max(5-0, 10-2)=8, precision=min(8+2,38)=10). Pass 2's downward
/// sub-sweep does the widening.
pub fn smoke_type_019() -> (CommonAst, StructType) {
    let left = CommonOp::Project(Project {
        input: Box::new(table("nums")),
        projections: vec![Expression::Cast(CastExpression {
            expr: Box::new(col("a")),
            to_type: DataType::Decimal {
                precision: 5,
                scale: 0,
            },
            try_cast: false,
        })],
    });
    let right = CommonOp::Project(Project {
        input: Box::new(table("nums")),
        projections: vec![col("d1")],
    });
    let ast = CommonAst {
        root: CommonOp::Union(Union {
            left: Box::new(left),
            right: Box::new(right),
            all: false,
        }),
    };
    // The left projection field is unnamed; `spark_column_name` renders
    // the CAST as `CAST(a AS DECIMAL(5,0))`.  Union takes the left name.
    let expected = StructType::new(vec![StructField::nullable(
        "CAST(a AS DECIMAL(5,0))",
        DataType::Decimal {
            precision: 10,
            scale: 2,
        },
    )]);
    (ast, expected)
}

// ── Driver ────────────────────────────────────────────────────────────────────

/// Run every mini-fixture; panic on the first mismatch with a rich diff.
///
/// [INV4] entry point. Called by
/// [`crate::transpiler_v2::analyzer::inference_smoke`] and by
/// [`crate::transpiler_v2::invariants::inv4_inference_validated_in_isolation`].
pub fn run_all() {
    for (name, builder) in fixtures() {
        let (ast, expected) = builder();
        let typed: TypedAst = match analyze(ast, &base_types()) {
            Ok(t) => t,
            Err(e) => panic!("[{name}] analyzer failed: {e}"),
        };
        assert_schema_eq(name, typed.root.schema(), &expected);
    }
}

type Builder = fn() -> (CommonAst, StructType);

fn fixtures() -> Vec<(&'static str, Builder)> {
    vec![
        ("smoke_type_001", smoke_type_001 as Builder),
        ("smoke_cond_003", smoke_cond_003 as Builder),
        ("smoke_agg_013", smoke_agg_013 as Builder),
        ("smoke_type_011", smoke_type_011 as Builder),
        ("smoke_type_019", smoke_type_019 as Builder),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixture_emp_has_14_fields() {
        assert_eq!(fixture_emp().fields.len(), 14);
    }

    #[test]
    fn fixture_dept_has_5_fields() {
        assert_eq!(fixture_dept().fields.len(), 5);
    }

    #[test]
    fn fixture_emp2_has_6_fields() {
        assert_eq!(fixture_emp2().fields.len(), 6);
    }

    #[test]
    fn fixture_nums_has_7_fields() {
        assert_eq!(fixture_nums().fields.len(), 7);
    }

    #[test]
    fn fixture_raw_has_5_fields() {
        assert_eq!(fixture_raw().fields.len(), 5);
    }

    #[test]
    fn smoke_matrix_runs() {
        run_all();
    }
}
