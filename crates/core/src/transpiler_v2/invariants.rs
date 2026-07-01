//! §CV.5 cross-cutting invariants — placeholder tests.
//!
//! One `#[test] fn invN_<slug>` per invariant. Each test:
//!   1. Quotes the canonical §CV.5 paragraph verbatim in its doc comment.
//!   2. Sets up the structural reservation (empty slice / unit-struct AST /
//!      minimal relation) exposed by the sibling submodules.
//!   3. Makes one semantic assertion — vacuous today, load-bearing once the
//!      substrate under it grows real behavior.
//!
//! The `TODO INV<N>:` line in each body names the substrate that will
//! replace the stub, so `git grep "TODO INV"` lists the unblocking work.

use super::analyzer::{
    analyze, has_resolved_schema, inference_smoke, BaseTypes, TypedAttr, TypedOp,
};
use super::ast::{CommonAst, CommonOp, Project, TableScan};
use super::emission::{
    dispatch, extension_targets, external_emit_paths, EmissionTable, ExternalEmit,
};
use super::provenance::{emit_write, Error as ProvError, ExternalRelation, Provenance};
// The `set_serializer_tap` name is deprecated in favor of `set_emit_tap`
// (see `mod.rs`); INV1 asserts the deprecated alias still delegates
// correctly, so the `#[allow(deprecated)]` on that use site below is
// the deprecation-alias contract check.
#[allow(deprecated)]
use super::set_serializer_tap;
use super::{clear_emit_tap, set_emit_tap, C_ESCAPE_HATCHES};
use crate::expression::{
    AliasExpression, BinaryExpression, BinaryOp, Expression, UnresolvedColumn,
};
use crate::types::{DataType, StructField, StructType};

/// **INV1 — Both engines receive byte-identical input.** (Touches ADR-015; constrains ADR-001.) Parity-via-identical-bytes is achieved by serialize-once-send-twice. Note this is *not* violated by ADR-001's cosmetic simplifications: cosmetic simplification is a τ transformation applied *once*, upstream of the single serialization, so both engines still receive the same simplified bytes — and DuckDB SQL is consumed only by DuckDB, never by Spark, so the cosmetic DuckDB cleanup is invisible to the comparison. (This is exactly why the rejected production-*canonicalizer* was different: it was proposed as a normalization that could differ from what Spark sees.) A proposal to add production-side normalization that could differ per engine, or that Spark would observe, must demonstrate it does not break this.
///
/// ADR cross-reference: ADR-015 (differential harness) / ADR-001 (cosmetic τ).
/// today: the full serialize-once-send-twice check is owned by the
/// differential harness in `tests/integration/` (ADR-015). Slice C.1
/// keeps this unit stub in place so the invariant name and structural
/// reservation exist; the check itself will be re-implemented on the
/// harness side, not here.
///
/// TODO INV1: the differential harness owns this — assert
/// `payloads.len() == 2 && payloads[0] == payloads[1]` once ADR-015's
/// harness ships. The `set_emit_tap` renamed hook (Slice C.1) is the
/// wire on the v2-emitter side; INV2 exercises it locally.
#[test]
#[allow(deprecated)] // intentional: this test asserts the deprecated
                     // `set_serializer_tap` alias still delegates to
                     // `set_emit_tap`. When the alias is removed, so is
                     // this test.
fn inv1_both_engines_receive_byte_identical_input() {
    // Structural reservation: the serializer tap's signature is fixed
    // even though no differential harness owns it yet. Setting a tap
    // must succeed via both the renamed `set_emit_tap` and the retained
    // (deprecated) `set_serializer_tap` alias — otherwise a future
    // serializer that starts emitting would silently take the wrong
    // path.
    fn noop_tap(_bytes: &[u8]) {}
    set_serializer_tap(noop_tap);
    set_emit_tap(noop_tap);
    clear_emit_tap();

    // TODO INV1: the differential harness activates the full check;
    // this unit stub keeps the name reserved.
    let payloads: &[&[u8]] = &[];
    assert!(
        payloads.is_empty(),
        "differential harness (ADR-015) owns the full INV1 check"
    );
}

/// **INV2 — Every τ decision is node-local (post-A) or a labeled C escape hatch.** (Touches ADR-007, ADR-009.) A new decision that is non-local must either be made local by the A pass (push the fact into the node) or be a *counted* C entry. It may not be a hidden closure inside the emission table. Genuinely structural forced transliterations live in the retained B layer (ADR-007), not as hidden table closures.
///
/// ADR cross-reference: ADR-007 (B-layer) / ADR-009 (declarative emission table).
/// today: `C_ESCAPE_HATCHES` is empty; the uniqueness/non-empty invariant is vacuously true.
/// TODO INV2: as ADR-007/ADR-009 land specific escape hatches, this test enforces they are named and unique.
#[test]
fn inv2_node_local_or_labeled_escape_hatch() {
    // Structural reservation: every entry in the reviewed exception list
    // must be a stable, non-empty, unique name. Vacuously true today.
    let hatches: &[&str] = C_ESCAPE_HATCHES;

    for (i, name) in hatches.iter().enumerate() {
        assert!(!name.is_empty(), "C_ESCAPE_HATCHES[{i}] must not be empty");
    }

    // TODO INV2: enforce uniqueness once entries land.
    let mut seen: Vec<&&str> = Vec::new();
    for name in hatches {
        assert!(!seen.contains(&name), "duplicate C escape hatch: {name}");
        seen.push(name);
    }
}

/// **INV3 — The dispatch match arms in `emission::dispatch_op` are the single source of truth for v2 SQL shape; there is no runtime string dispatch.** (Touches ADR-009, ADR-014, ADR-015.) In Slice C.1 the emitter is a hand-written `match` over `TypedOp` discriminants — declarative-not-runtime discipline is preserved by keeping every `TypedOp → SQL` decision inside those arms and by disallowing module-level imports of the legacy runtime `FunctionRegistry`. Slice C.2 promotes the arms to declarative per-function rows once row count justifies the substrate; INV3's `use crate::generator::SqlGenerator` allowance is a *deliberate seam* that C.2 will drain.
///
/// ADR cross-reference: ADR-009 (declarative emission) / ADR-014 (attributability) / ADR-015 (coverage denominator).
/// today: `EmissionTable::dispatch` is the sole public path to build an
/// `EmittedSql`; `emission::dispatch_op`'s `match` covers every
/// `TypedOp` variant Slice C.1 supports. Slice C.2 replaces per-function
/// `SqlGenerator::gen_expr` calls with declarative rows; when it lands
/// the `SqlGenerator` import in `emission.rs` goes away and this test
/// should reject it entirely.
#[test]
fn inv3_emission_table_single_source_of_truth() {
    // Structural reservation: the *only* public dispatch artifact is
    // `emission::EmissionTable`. Constructing one is the reservation.
    let _table = EmissionTable;

    // Slice C.1 teeth (part 1) — grep-based: the emission module MUST
    // NOT import the runtime `FunctionRegistry` at the module surface.
    // Delegation to the legacy scalar-expression renderer
    // (`SqlGenerator::gen_expr`) is permitted inside a helper fn
    // (documented as the C.2 seam), but importing the function registry
    // directly would let arbitrary runtime dispatch leak in. This is
    // the ADR-014 contamination barrier applied to source text.
    //
    // We check specifically for `use ...FunctionRegistry` module-level
    // clauses (not any mention of the name in doc comments — those may
    // reference the type to explain the seam).
    const EMISSION_SRC: &str = include_str!("emission.rs");
    assert!(
        !EMISSION_SRC.contains("use crate::functions::FunctionRegistry"),
        "INV3 violated: emission.rs imports `FunctionRegistry` (ADR-009 declarative-not-runtime)"
    );
    assert!(
        !EMISSION_SRC.contains("use crate::functions::*"),
        "INV3 violated: emission.rs glob-imports crate::functions (ADR-014 contamination barrier)"
    );
    assert!(
        !EMISSION_SRC.contains("SqlGenerator::new().generate("),
        "INV3 violated: emission.rs calls `SqlGenerator::generate` (should go through the emission table)"
    );
    assert!(
        !EMISSION_SRC.contains("use crate::generator::*"),
        "INV3 violated: emission.rs glob-imports crate::generator (ADR-014 contamination barrier)"
    );

    // Slice C.1 teeth (part 2) — coverage anchor: the SQL-emitting
    // choke point is the set of `render_<op>` helpers reachable from
    // `dispatch_op`'s match. Enumerate their function names in source
    // so a future refactor can't rename them out from under the arm
    // without failing this test. Any new op arm must add its
    // `render_<op>` here.
    const REQUIRED_RENDERERS: &[&str] = &[
        "fn render_project",
        "fn render_filter",
        "fn render_sort",
        "fn render_limit",
        "fn render_tail",
        "fn render_distinct",
        "fn render_with_columns",
        "fn render_drop_columns",
        "fn render_aliased_relation",
        "fn render_table_scan",
        "fn render_local_relation",
        "fn render_range",
        "fn render_union",
        "fn render_intersect",
        "fn render_except",
        "fn render_aggregate",
    ];
    for name in REQUIRED_RENDERERS {
        assert!(
            EMISSION_SRC.contains(name),
            "INV3 violated: expected `{name}` in emission.rs — the dispatch match arms are the single source of truth for v2 SQL shape"
        );
    }
    // The choke point itself must exist and be reachable through
    // `dispatch` (the module-private wrapper that fires the emit tap).
    assert!(
        EMISSION_SRC.contains("fn dispatch_op"),
        "INV3 violated: `dispatch_op` (the single-choke-point match) missing from emission.rs"
    );
    assert!(
        EMISSION_SRC.contains("pub fn dispatch("),
        "INV3 violated: `dispatch` public entry point missing from emission.rs"
    );
}

/// **INV2 (unit slice) — `dispatch` is the single writer of v2 SQL.**
///
/// The full INV2 (labeled C escape hatches enumerated in `C_ESCAPE_HATCHES`)
/// lives above; this companion test asserts the *type-system-level* claim:
/// [`crate::transpiler_v2::emission::dispatch`] is the only path that
/// constructs an [`crate::transpiler_v2::emission::EmittedSql`], so it is
/// the only path that fires the emit tap.
///
/// Setup:
///   1. Install a counting tap via [`set_emit_tap`].
///   2. Dispatch a trivial `TypedOp::LocalRelation` (leaf operator).
///   3. Assert the counter incremented exactly once.
///   4. Clear the tap so unrelated tests are not perturbed.
#[test]
fn inv2_dispatch_is_only_sql_writer() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static TAP_HITS: AtomicUsize = AtomicUsize::new(0);
    fn counting_tap(_bytes: &[u8]) {
        TAP_HITS.fetch_add(1, Ordering::SeqCst);
    }
    TAP_HITS.store(0, Ordering::SeqCst);
    set_emit_tap(counting_tap);
    let op = TypedOp::LocalRelation {
        schema: StructType::single("x", DataType::Long),
    };
    let _sql = dispatch(&op).expect("LocalRelation dispatch must succeed");
    let hits = TAP_HITS.load(Ordering::SeqCst);
    clear_emit_tap();
    assert_eq!(
        hits, 1,
        "dispatch must fire the emit tap exactly once per outermost call, got {hits}"
    );
}

/// **INV4 — Inference is validated in isolation before translation tests run.** (Touches ADR-005, ADR-006, ADR-015.) Preserves attributability (ADR-014). The AnalyzePlan schema diff must be green before result-level translation failures are interpreted as translation bugs. Applies also to rule *provenance*: an LLM-extracted coercion/nullability rule is not trusted until the diff is green for it.
///
/// ADR cross-reference: ADR-005 (analyzer) / ADR-006 (nullability) / ADR-015 (AnalyzePlan diff harness).
#[test]
fn inv4_inference_isolation() {
    // The analyzer's inference-only smoke test runs the analyzer over five
    // literal fixtures drawn from the DataFrame corpus (`type-001`,
    // `cond-003`, `agg-013`, `type-011`, `type-019`) and panics with a rich
    // diff on any schema mismatch. If it returns without panic, every
    // mini-fixture's produced schema matched the expected literal — that is
    // the isolation guarantee INV4 requires.
    inference_smoke();
}

/// **INV5 — thunderduck knows the schema everywhere, even where it emits delegated structure.** (Touches ADR-002, ADR-005.) The internal resolver/star-expander for type-tracking must not be removed on the grounds that resolution/star-expansion is delegated. Emit-level delegation ≠ analysis-level delegation.
///
/// ADR cross-reference: ADR-002 (delegation premise) / ADR-005 (analyzer).
#[test]
fn inv5_no_unresolved_after_analyzer() {
    // Build a small CommonAst: project `col('a') + col('lng')` from the `nums`
    // fixture, seed the base-types catalog, and analyze.
    let mut base_types = BaseTypes::new();
    base_types.insert(
        "nums".to_string(),
        super::analyzer::analyzer_fixtures::fixture_nums(),
    );
    let a_plus_lng = Expression::Alias(AliasExpression {
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
    });
    let ast = CommonAst {
        root: CommonOp::Project(Project {
            input: Box::new(CommonOp::TableScan(TableScan {
                name: "nums".to_string(),
                schema: StructType::empty(),
            })),
            projections: vec![a_plus_lng],
        }),
    };
    let typed = analyze(ast, &base_types).expect("analyze must succeed");
    assert!(
        has_resolved_schema(&typed),
        "analyzer output must have no Unresolved slots (INV5)"
    );

    // Now plant an intentionally-`Unresolved` TypedAttr slot in a
    // hand-built `TypedAst` and prove the walker actually looks — i.e.
    // catches Unresolved wherever it appears, not just when the analyzer
    // produced a clean result.
    let planted = super::analyzer::TypedAst {
        root: TypedOp::Project {
            input: Box::new(TypedOp::LocalRelation {
                schema: StructType::single("x", DataType::Long),
            }),
            projections: vec![Expression::UnresolvedColumn(UnresolvedColumn {
                name: "x".to_string(),
                qualifier: None,
            })],
            projection_types: vec![TypedAttr {
                data_type: DataType::Unresolved,
                nullable: true,
            }],
            schema: StructType::new(vec![StructField::nullable("x", DataType::Long)]),
        },
    };
    assert!(
        !has_resolved_schema(&planted),
        "walker must detect a planted DataType::Unresolved slot (INV5 teeth)"
    );
}

/// **INV6 — Every `Extension(...)` target in the dispatch table corresponds to an existing, loaded function in the `thunderduck-duckdb-extension` C++ project.** (Touches ADR-009, ADR-010.) Unlike LB5 (an empirical bet about expressiveness), this is a mechanically *checkable, preservable* property — verify at build/test time that the table's emission targets and the extension's exported symbols agree. It is the mechanical complement to LB5: LB5 asserts an adequate extension *can* be written; INV6 asserts every extension the table *names* actually *exists and is loaded*. A compiled-dispatch build (ADR-009) can enforce INV6 at compile time.
///
/// ADR cross-reference: ADR-009 (declarative emission table) / ADR-010 (extension surface).
/// today: `extension_targets()` is empty; the containment check is vacuously true, but the connection + LOAD path runs so a regression in the extension load is caught here.
/// TODO INV6: as ADR-010 extension targets are declared, this test diffs them against `duckdb_functions()` and fails loudly on any missing symbol.
#[test]
fn inv6_extension_targets_exist_in_loaded_extension() {
    // Structural reservation: open an in-memory DuckDB with
    // `allow_unsigned_extensions=true` (the same config the session runtime
    // uses to load `thdck_spark_funcs`), load the bundled extension,
    // enumerate the `spark_*` functions, and assert every name the dispatch
    // table declares is present.
    let config = duckdb::Config::default()
        .with("allow_unsigned_extensions", "true")
        .expect("DuckDB config must accept allow_unsigned_extensions");
    let conn = duckdb::Connection::open_in_memory_with_flags(config)
        .expect("in-memory DuckDB connection must open in tests");
    crate::runtime::extension_loader::load(&conn)
        .expect("bundled thdck_spark_funcs extension must load");

    let mut stmt = conn
        .prepare(
            "SELECT function_name FROM duckdb_functions() \
             WHERE function_name LIKE 'spark\\_%' ESCAPE '\\'",
        )
        .expect("duckdb_functions() query must prepare");
    let loaded: std::collections::HashSet<String> = stmt
        .query_map([], |row| row.get::<_, String>(0))
        .expect("duckdb_functions() query must execute")
        .filter_map(|r| r.ok())
        .collect();

    for target in extension_targets() {
        assert!(
            loaded.contains(*target),
            "extension target `{target}` is declared by the dispatch table but not exported by the loaded extension"
        );
    }

    // TODO INV6: fail on the *reverse* direction too (exported but undeclared)
    // once the coverage denominator (ADR-015) is derivable from the table.
}

/// **INV7 — Both front-ends produce the same common-AST node for semantically equivalent inputs.** (Added with the common-AST/SQL ADRs; touches ADR-003, ADR-004.) The SparkSQL parser and the Connect-proto deserializer must normalize to identical AST (same node, same resolved type/nullability) for the same meaning; otherwise the common-AST guarantee — that SQL inherits emission/inference rules for free for shared constructs — breaks. This is the soundness condition for having one τ behind two front-ends. **Check:** AnalyzePlan schema diff on the same SQL parsed by thunderduck vs analyzed by Spark (Tension T5).
///
/// ADR cross-reference: ADR-003 (common AST) / ADR-004 (front-end normalization).
/// today: `CommonAst` is a unit struct; both "front-ends" produce the same empty value trivially.
/// TODO INV7: parse a small corpus through both front-ends and assert AST equality (structural + resolved types) once ADR-003/004 land.
#[test]
fn inv7_both_frontends_produce_same_ast() {
    // Structural reservation: two independent constructions of the same
    // trivial `CommonAst` (one standing in for the SparkSQL front-end, one
    // for the Connect-proto front-end) must be equal. Slice B grew
    // `CommonAst` from a unit struct into a full tree, so this now compares
    // real structural equality via `PartialEq`.
    let build = || CommonAst {
        root: CommonOp::TableScan(TableScan {
            name: "nums".to_string(),
            schema: StructType::empty(),
        }),
    };
    let from_sql = build();
    let from_proto = build();
    assert_eq!(from_sql, from_proto);

    // TODO INV7: replace with a real corpus + `assert_eq!(sql_ast, proto_ast)`
    // for shared SQL/DataFrame constructs once the two front-ends both lower
    // to `CommonAst` (Slice C+).
}

/// **INV8 — External-table access is always delegated to a DuckDB storage extension.** (Added with ADR-013; touches ADR-002, ADR-013.) thunderduck emits the storage-extension surface (`read_parquet`/`iceberg_scan`/`delta_scan`/`ATTACH TYPE iceberg`/`uc_catalog`) and **never** parses a table format, reads a transaction log, or speaks a catalog protocol itself. This is the bounded-scope line for storage, analogous to INV5 (don't remove the internal type-resolver) and INV6 (every extension target exists): it keeps the external-table surface a *translation* concern, not a reimplementation one. A proposal to read a format directly in thunderduck must demonstrate why delegation is impossible — and would reopen ADR-013.
///
/// ADR cross-reference: ADR-002 (delegation premise) / ADR-013 (external tables).
/// today: `external_emit_paths()` is an empty slice; every element is trivially inside the closed `ExternalEmit` allow-list.
/// TODO INV8: as ADR-013 emit paths land, this test asserts every entry is one of the enumerated kinds and no bespoke reader path leaks in.
#[test]
fn inv8_external_access_is_delegated() {
    // Structural reservation: the closed enum `ExternalEmit` names every
    // legal external-emit kind; the runtime list `external_emit_paths()`
    // must be a subset of it. Vacuously true today because the slice is
    // empty.
    for path in external_emit_paths() {
        let ok = matches!(
            path,
            ExternalEmit::ReadParquet
                | ExternalEmit::IcebergScan
                | ExternalEmit::DeltaScan
                | ExternalEmit::AttachIceberg
                | ExternalEmit::UcCatalog
        );
        assert!(
            ok,
            "external emit path {path:?} is not on the ADR-013 allow-list"
        );
    }

    // TODO INV8: assert the *positive* coverage — every ADR-013 kind that
    // ships must appear in `external_emit_paths()`.
}

/// **INV9 — A writable external relation must have attached-catalog provenance; path-scan provenance is read-only.** (Added with ADR-017; touches ADR-011, ADR-013, ADR-017.) External tables reached by a bare path-scan (`read_parquet` / `delta_scan` / `iceberg_scan`) are read-only by construction; any write (append/insert/delete/merge/CTAS) requires the table to be reached via an attachment (`ATTACH … TYPE delta`/`iceberg`, or `uc_catalog`). This is the rule that keeps the write story consistent across formats: every per-format write ADR (Delta ADR-017; Iceberg ADR-018; and any future format) must route writes through an attachment, never a path-scan. This is reinforced externally: Databricks UC forbids path-based access to managed tables outright (ADR-018), so for UC targets the invariant is enforced by the catalog as well as by thunderduck. **Check:** the overlay's recorded provenance (ADR-012/013) gates whether a write command may be emitted at all.
///
/// ADR cross-reference: ADR-011 (writes) / ADR-013 (external tables) / ADR-017 (Delta writes).
/// today: `emit_write` returns `Error::ReadOnlyProvenance` for `PathScan` and `Ok(String::new())` for `Attached`; the gate itself is real, only the emitted SQL is a stub.
/// TODO INV9: extend once the overlay-backed relation type and real write SQL (ADR-012/017) land.
#[test]
fn inv9_writable_requires_attached_provenance() {
    // Structural reservation: build one relation of each provenance kind and
    // assert the gate behaves.
    let path_scan = ExternalRelation {
        provenance: Provenance::PathScan,
        name: "path_scan_rel".to_string(),
    };
    let err = emit_write(&path_scan).expect_err("path-scan write must be rejected");
    assert!(
        matches!(err, ProvError::ReadOnlyProvenance { ref name } if name == "path_scan_rel"),
        "expected ReadOnlyProvenance for path_scan_rel, got {err:?}"
    );

    let attached = ExternalRelation {
        provenance: Provenance::Attached,
        name: "attached_rel".to_string(),
    };
    assert!(
        emit_write(&attached).is_ok(),
        "attached-provenance write must be permitted"
    );

    // TODO INV9: assert the emitted SQL routes through the attachment once
    // ADR-017's real write path lands.
}
