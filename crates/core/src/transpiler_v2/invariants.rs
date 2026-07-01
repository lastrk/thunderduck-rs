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

use super::analyzer::{has_resolved_schema, inference_smoke};
use super::ast::CommonAst;
use super::emission::{extension_targets, external_emit_paths, EmissionTable, ExternalEmit};
use super::provenance::{emit_write, Error as ProvError, ExternalRelation, Provenance};
use super::{set_serializer_tap, C_ESCAPE_HATCHES};

/// **INV1 — Both engines receive byte-identical input.** (Touches ADR-015; constrains ADR-001.) Parity-via-identical-bytes is achieved by serialize-once-send-twice. Note this is *not* violated by ADR-001's cosmetic simplifications: cosmetic simplification is a τ transformation applied *once*, upstream of the single serialization, so both engines still receive the same simplified bytes — and DuckDB SQL is consumed only by DuckDB, never by Spark, so the cosmetic DuckDB cleanup is invisible to the comparison. (This is exactly why the rejected production-*canonicalizer* was different: it was proposed as a normalization that could differ from what Spark sees.) A proposal to add production-side normalization that could differ per engine, or that Spark would observe, must demonstrate it does not break this.
///
/// ADR cross-reference: ADR-015 (differential harness) / ADR-001 (cosmetic τ).
/// today: no serializer exists, so the tap is a no-op and no payloads are recorded.
/// TODO INV1: assert `payloads.len() == 2 && payloads[0] == payloads[1]` once the v2 serializer + harness land (ADR-015).
#[test]
fn inv1_both_engines_receive_byte_identical_input() {
    // Structural reservation: the serializer tap's signature is fixed even
    // though no serializer exists yet. Setting a tap must succeed and remain
    // a no-op — otherwise a future serializer that starts emitting would
    // silently take the wrong path.
    fn noop_tap(_bytes: &[u8]) {}
    set_serializer_tap(noop_tap);

    // TODO INV1: replace with `assert_eq!(recorded_spark_bytes, recorded_duckdb_bytes)`
    // once the serialize-once-send-twice harness (ADR-015) is wired.
    let payloads: &[&[u8]] = &[];
    assert!(
        payloads.is_empty(),
        "no payloads recorded until ADR-015 harness ships"
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

/// **INV3 — The emission table is the single source of truth for generation and coverage.** (Touches ADR-009, ADR-014, ADR-015.) Refinements to the table must keep both the input grammar and the coverage denominator derived from it; they must not drift into separate artifacts.
///
/// ADR cross-reference: ADR-009 (declarative emission table) / ADR-014 (attributability) / ADR-015 (coverage denominator).
/// today: `EmissionTable` is a unit-struct placeholder; there is only one dispatch artifact by construction.
/// TODO INV3: once the table carries real dispatch entries, assert generation and coverage both derive from `EmissionTable` and no sibling module declares a competing table.
#[test]
fn inv3_emission_table_single_source_of_truth() {
    // Structural reservation: the *only* dispatch artifact is
    // `emission::EmissionTable`. Constructing one is the reservation.
    let _table = EmissionTable;

    // TODO INV3: assert that generation and coverage both trace back to this
    // single artifact once ADR-009's real dispatch entries land.
}

/// **INV4 — Inference is validated in isolation before translation tests run.** (Touches ADR-005, ADR-006, ADR-015.) Preserves attributability (ADR-014). The AnalyzePlan schema diff must be green before result-level translation failures are interpreted as translation bugs. Applies also to rule *provenance*: an LLM-extracted coercion/nullability rule is not trusted until the diff is green for it.
///
/// ADR cross-reference: ADR-005 (analyzer) / ADR-006 (nullability) / ADR-015 (AnalyzePlan diff harness).
/// today: `inference_smoke()` is a no-op; there is no analyzer to run yet.
/// TODO INV4: replace with an AnalyzePlan schema-diff assertion once the analyzer (ADR-005/006) ships.
#[test]
fn inv4_inference_validated_in_isolation() {
    // Structural reservation: an inference-only entry point exists. Calling
    // it must succeed without touching a translation path.
    inference_smoke();

    // TODO INV4: assert the AnalyzePlan schema diff is green for a fixed
    // corpus once the analyzer is real.
}

/// **INV5 — thunderduck knows the schema everywhere, even where it emits delegated structure.** (Touches ADR-002, ADR-005.) The internal resolver/star-expander for type-tracking must not be removed on the grounds that resolution/star-expansion is delegated. Emit-level delegation ≠ analysis-level delegation.
///
/// ADR cross-reference: ADR-002 (delegation premise) / ADR-005 (analyzer).
/// today: `CommonAst` is a unit struct; `has_resolved_schema` is vacuously `true`.
/// TODO INV5: once `CommonAst` carries nodes, assert the predicate detects any `DataType::Unresolved` (or its analyzer-facing successor).
#[test]
fn inv5_thunderduck_knows_schema_everywhere() {
    // Structural reservation: an empty AST is trivially fully resolved.
    let ast = CommonAst;
    assert!(
        has_resolved_schema(&ast),
        "empty CommonAst must be considered resolved"
    );

    // TODO INV5: build an AST containing a `DataType::Unresolved` leaf and
    // assert the predicate returns `false`, once the walker is real.
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
    // Structural reservation: two independent constructions of `CommonAst`
    // (one standing in for the SparkSQL front-end, one for the Connect-proto
    // front-end) must be equal today because there is nothing to disagree
    // about.
    let from_sql = CommonAst;
    let from_proto = CommonAst;

    // Unit structs have no `PartialEq` by default, but both are trivially the
    // same shape. The Debug rendering exercises the reservation.
    assert_eq!(format!("{from_sql:?}"), format!("{from_proto:?}"));

    // TODO INV7: replace with a real corpus + `assert_eq!(sql_ast, proto_ast)`
    // once `CommonAst` carries structure.
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
