//! τ invariant stubs (INV1–INV10, INV7 omitted per ADR-022).
//!
//! Marker convention (§CV.5.1 of the rearchitect ADRs):
//! - active markers name the current slice; deletion is the completion signal.
//! - deferred markers name the owning slice; not tripped by the current gate.
//!
//! At Slice A.1 only INV10 is active. All other INVs are deferred to their
//! owning slice per the readiness map. INV7 is intentionally OMITTED (deleted
//! per ADR-022 §CV.5); do not add an INV7 stub.

// ── INV1 (Slice I — differential harness) ────────────────────────────────────

/// DEFER INV1 → Slice I: byte-identical-input principle validation.
///
/// ADR-015: differential tests must feed the same input to legacy and τ paths
/// and assert byte-identical SQL output when both paths handle the plan.
#[test]
#[ignore]
fn inv1_byte_identical_input() {
    todo!("INV1 activation is Slice I's deliverable (differential harness)")
}

// ── INV2 (Slice C.1 dispatch + Slice J escape-hatch) ─────────────────────────

/// DEFER INV2 → Slice C.1 (dispatch-is-only-writer) + Slice J (escape-hatch dimension):
/// dispatch is the ONLY writer of SQL text; every emission arm must route
/// through the dispatch table. Slice C.1 introduces `EMIT_TAP` + `EMIT_TAP_MUTEX`
/// to instrument this at runtime.
#[test]
#[ignore]
fn inv2_dispatch_is_only_sql_writer() {
    todo!("INV2 activation requires EMIT_TAP + EMIT_TAP_MUTEX (Slice C.1)")
}

// ── INV3 (Slice C.1 — emission table single source of truth) ─────────────────

/// DEFER INV3 → Slice C.1: the emission table is the SINGLE source of truth
/// for function → DuckDB mapping; a grep barrier over `crates/core/src/` must
/// find zero non-table sources of function → SQL name mappings.
#[test]
#[ignore]
fn inv3_emission_table_single_source_of_truth() {
    todo!("INV3 activation is Slice C.1's grep-barrier deliverable")
}

// ── INV4 (Slice B — analyzer isolation) ──────────────────────────────────────

/// DEFER INV4 → Slice B: inference is validated in isolation from emission —
/// the analyzer's schema/nullability results are verifiable without running
/// any SQL through DuckDB.
#[test]
#[ignore]
fn inv4_inference_validated_in_isolation() {
    todo!("INV4 activation is Slice B's analyzer deliverable")
}

// ── INV5 (Slice B — schema everywhere) ───────────────────────────────────────

/// DEFER INV5 → Slice B: every plan node carries a resolved schema after
/// analysis; grep barrier over `crates/core/src/transpiler_v2/` finds zero
/// `Schema::empty()` fallthroughs post-analyzer.
#[test]
#[ignore]
fn inv5_schema_everywhere() {
    todo!("INV5 activation is Slice B's analyzer deliverable")
}

// ── INV6 (Slice D — extension targets exist) ─────────────────────────────────

/// DEFER INV6 → Slice D: every entry in `extension_targets()` MUST resolve
/// against `duckdb_functions()` in a loaded ext6 session. Slice D's Phase 2
/// activation opens a session, loads the extension, and asserts the allow-list
/// is a subset of the loaded function catalog.
#[test]
#[ignore]
fn inv6_extension_targets_exist() {
    todo!("INV6 activation requires extension_targets() + duckdb_functions() check (Slice D)")
}

// ── INV7 — OMITTED per ADR-022 §CV.5 ─────────────────────────────────────────

// INV7 was deleted from the invariant set. Do not add an INV7 stub.

// ── INV8 (Slice H — external access delegation) ──────────────────────────────

/// DEFER INV8 → Slice H: any read/write against external storage is delegated
/// to a substrate adapter and NEVER inlined into emission arms.
#[test]
#[ignore]
fn inv8_external_access_delegated() {
    todo!("INV8 activation is Slice H's writes deliverable")
}

// ── INV9 (Slice H — writes require attached provenance) ──────────────────────

/// DEFER INV9 → Slice H: writable plans must carry attached provenance
/// (source-of-writes) before emission; no writes emitted from unattached plans.
#[test]
#[ignore]
fn inv9_writable_requires_attached_provenance() {
    todo!("INV9 activation is Slice H's writes deliverable")
}

// ── INV10 (ACTIVE — Slice A.2) ───────────────────────────────────────────────

/// A τ walk root — a directory and an optional file filter.
///
/// The filter exists so the connect-server crate can be walked without
/// pulling legacy converter files (`relation_converter.rs`, etc.) which
/// legitimately import from `crate::logical` / `crate::expression`. When
/// `files == Some(names)`, only files whose basename appears in `names`
/// contribute to the walk; when `files == None`, every `.rs` file under
/// `dir` is walked.
#[cfg(test)]
struct WalkRoot {
    dir: &'static str,
    files: Option<&'static [&'static str]>,
}

/// Root paths INV10 walks. Slice A.3 covers four roots:
///
/// - `crates/core/src/transpiler_v2/` — τ's substrate (unfiltered).
/// - `crates/core/src/parser_v2/` — τ's SparkSQL front-end (unfiltered).
/// - `crates/connect-server/src/converter/v2_relation_converter.rs` — τ's
///   protobuf front-end (single-file filter — the sibling legacy converter
///   files legitimately import from `crate::logical` etc.).
/// - `crates/connect-server/src/service.rs` — the τ dispatch site (single-file
///   filter; sibling files `main.rs`, `arrow_ipc.rs`, `error.rs` legitimately
///   import from legacy paths and are excluded).
#[cfg(test)]
const WALK_ROOTS: &[WalkRoot] = &[
    WalkRoot {
        dir: "crates/core/src/transpiler_v2/",
        files: None,
    },
    WalkRoot {
        dir: "crates/core/src/parser_v2/",
        files: None,
    },
    WalkRoot {
        dir: "crates/connect-server/src/converter/",
        files: Some(&["v2_relation_converter.rs"]),
    },
    WalkRoot {
        dir: "crates/connect-server/src/",
        files: Some(&["service.rs"]),
    },
];

/// Import prefixes that are DISALLOWED anywhere under a τ-owned tree.
/// Each entry is a `starts_with()` prefix on a trimmed source line.
///
/// The `use crate::…` prefixes apply to `crates/core/src/{transpiler_v2,parser_v2}/`;
/// the `use thunderduck_core::…` prefixes apply to
/// `crates/connect-server/src/converter/v2_relation_converter.rs`, which
/// reaches into `thunderduck_core` from a different crate.
#[cfg(test)]
const DISALLOWED_IMPORT_PREFIXES: &[&str] = &[
    // Intra-core τ tree (transpiler_v2 + parser_v2).
    "use crate::logical::",
    "use crate::logical ",
    "use crate::logical;",
    "use crate::expression::",
    "use crate::expression ",
    "use crate::expression;",
    "use crate::generator::",
    "use crate::generator ",
    "use crate::generator;",
    "use crate::functions::",
    "use crate::functions ",
    "use crate::functions;",
    "use crate::parser::",
    "use crate::parser ",
    "use crate::parser;",
    "use crate::runtime::",
    "use crate::runtime ",
    "use crate::runtime;",
    "use crate::types::TypeInferenceEngine",
    // connect-server → thunderduck_core reach.
    "use thunderduck_core::logical::",
    "use thunderduck_core::expression::",
    "use thunderduck_core::generator::",
    "use thunderduck_core::functions::",
    "use thunderduck_core::parser::",
    "use thunderduck_core::runtime::",
    "use thunderduck_core::types::TypeInferenceEngine",
];

/// Recursively collect every `.rs` file under `dir`.
#[cfg(test)]
fn collect_rs_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

/// Collect the files INV10 walks for a single [`WalkRoot`], applying the
/// filter if one is present.
#[cfg(test)]
fn collect_files_for_root(root: &std::path::Path, walk: &WalkRoot) -> Vec<std::path::PathBuf> {
    let mut files = Vec::new();
    collect_rs_files(root, &mut files);
    match walk.files {
        None => files,
        Some(names) => files
            .into_iter()
            .filter(|p| {
                p.file_name()
                    .and_then(|s| s.to_str())
                    .map(|n| names.contains(&n))
                    .unwrap_or(false)
            })
            .collect(),
    }
}

/// Locate the workspace root by walking upward until a `Cargo.toml` with
/// `[workspace]` is found. Returns `None` on failure.
#[cfg(test)]
fn find_workspace_root() -> Option<std::path::PathBuf> {
    // Cargo sets CARGO_MANIFEST_DIR to the crate's Cargo.toml directory.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").ok()?;
    let mut dir = std::path::PathBuf::from(manifest_dir);
    for _ in 0..8 {
        let cargo_toml = dir.join("Cargo.toml");
        if cargo_toml.exists() {
            if let Ok(contents) = std::fs::read_to_string(&cargo_toml) {
                if contents.contains("[workspace]") {
                    return Some(dir);
                }
            }
        }
        if !dir.pop() {
            break;
        }
    }
    None
}

/// INV10: enforce τ substrate independence — no imports from
/// `crate::{logical,expression,generator,functions,parser,runtime}` and no
/// re-use of `crate::types::TypeInferenceEngine` inside any file under a
/// τ-owned tree. Extended in Slice A.2 to also cover the connect-server
/// `v2_relation_converter.rs` file (reaching into `thunderduck_core::*`).
///
/// Walks every `.rs` file matched by [`WALK_ROOTS`], splits each into lines,
/// trims leading whitespace, and asserts no line starts with any prefix in
/// [`DISALLOWED_IMPORT_PREFIXES`].
#[test]
fn inv10_no_disallowed_imports_from_transpiler_v2() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let mut offenders: Vec<String> = Vec::new();
    for walk in WALK_ROOTS {
        let dir = root.join(walk.dir);
        if !dir.exists() {
            continue;
        }
        for file in collect_files_for_root(&dir, walk) {
            let contents = match std::fs::read_to_string(&file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (lineno, line) in contents.lines().enumerate() {
                let trimmed = line.trim_start();
                for prefix in DISALLOWED_IMPORT_PREFIXES {
                    if trimmed.starts_with(prefix) {
                        offenders.push(format!("{}:{}: {}", file.display(), lineno + 1, trimmed));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "INV10 violation — disallowed imports inside τ tree:\n{}",
        offenders.join("\n")
    );
}

/// Sanity: the INV10 walker must find at least one `.rs` file. Otherwise
/// the invariant check would silently pass because every walk root is
/// empty.
#[test]
fn inv10_walks_at_least_one_file() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let mut total = 0usize;
    for walk in WALK_ROOTS {
        let dir = root.join(walk.dir);
        if !dir.exists() {
            continue;
        }
        total += collect_files_for_root(&dir, walk).len();
    }
    assert!(
        total > 0,
        "INV10 walker discovered zero files across walk roots",
    );
}

/// Sanity: every [`WalkRoot::dir`] must exist at test time (unless the
/// entire crate hasn't been created yet, in which case A.2's file plan is
/// broken).
#[test]
fn inv10_walk_roots_all_exist() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let missing: Vec<&'static str> = WALK_ROOTS
        .iter()
        .filter(|w| !root.join(w.dir).exists())
        .map(|w| w.dir)
        .collect();
    assert!(missing.is_empty(), "missing walk-root dirs: {missing:?}");
}

/// Slice A.3 sanity: `crates/connect-server/src/service.rs` must be in the
/// INV10 walk scope so τ-boundary discipline covers the dispatch site.
#[test]
fn inv10_service_rs_is_in_walk_scope() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let found = WALK_ROOTS.iter().any(|w| {
        let dir = root.join(w.dir);
        if !dir.exists() {
            return false;
        }
        collect_files_for_root(&dir, w).iter().any(|p| {
            p.file_name()
                .and_then(|s| s.to_str())
                .map(|n| n == "service.rs")
                .unwrap_or(false)
                && p.parent()
                    .and_then(|d| d.file_name())
                    .and_then(|s| s.to_str())
                    == Some("src")
                && p.components().any(|c| c.as_os_str() == "connect-server")
        })
    });
    assert!(
        found,
        "INV10 walk scope must include crates/connect-server/src/service.rs"
    );
}

/// Slice A.3 anti-regression: no file under `crates/connect-server/src/`
/// (any file, not just `service.rs`) may reference `THUNDERDUCK_TRANSPILER`
/// — the env-var no-op block was removed from `main.rs` at A.3.
#[test]
fn no_thunderduck_transpiler_references_in_connect_server() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let connect_src = root.join("crates/connect-server/src/");
    if !connect_src.exists() {
        panic!("crates/connect-server/src/ must exist");
    }
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    collect_rs_files(&connect_src, &mut files);
    let mut offenders: Vec<String> = Vec::new();
    for file in files {
        let contents = match std::fs::read_to_string(&file) {
            Ok(c) => c,
            Err(_) => continue,
        };
        for (lineno, line) in contents.lines().enumerate() {
            if line.contains("THUNDERDUCK_TRANSPILER") {
                offenders.push(format!("{}:{}: {}", file.display(), lineno + 1, line));
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "THUNDERDUCK_TRANSPILER references must be zero across crates/connect-server/src/:\n{}",
        offenders.join("\n")
    );
}

/// The filtered `converter/` root must only walk the files named in its
/// filter — a sanity check that would trip if the filter API broke.
#[test]
fn inv10_filtered_root_only_walks_named_files() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let filtered: &WalkRoot = WALK_ROOTS
        .iter()
        .find(|w| w.files.is_some())
        .expect("Slice A.2 must have at least one filtered walk root");
    let dir = root.join(filtered.dir);
    if !dir.exists() {
        // The filtered root's directory should exist per
        // `inv10_walk_roots_all_exist`; if it doesn't, that test will fail
        // and this one has nothing to prove.
        return;
    }
    let allowed: &[&str] = filtered.files.expect("filter set");
    let files = collect_files_for_root(&dir, filtered);
    for file in &files {
        let name = file
            .file_name()
            .and_then(|s| s.to_str())
            .expect("file has utf-8 name");
        assert!(
            allowed.contains(&name),
            "filtered walk pulled unexpected file: {}",
            file.display()
        );
    }
}
