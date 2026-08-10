//! τ invariant stubs (INV1–INV10, INV7 omitted per ADR-022).
//!
//! Marker convention (§CV.5.1 of the rearchitect ADRs):
//! - active markers name the current phase; deletion is the completion signal.
//! - deferred markers name the owning phase; not tripped by the current gate.
//!
//! At τ only INV10 is active. All other INVs are deferred to their
//! owning phase. INV7 is intentionally OMITTED (deleted
//! per ADR-022 §CV.5); do not add an INV7 stub.

// ── INV1 (deferred — differential harness) ────────────────────────────────────

/// DEFER INV1: byte-identical-input principle validation.
///
/// ADR-015: differential tests must feed the same input to both engines
/// (Spark reference + τ) and assert Spark-parity output.
#[test]
#[ignore]
fn inv1_byte_identical_input() {
    todo!("INV1 — activation deferred; not yet implemented")
}

// ── INV3 (ACTIVE) ────────────────────────────────────────────────

/// INV3: the emission table is the SINGLE source of truth for function →
/// DuckDB mapping. Grep-barrier form: `emission.rs` MUST NOT import from
/// `crate::generator::` or `crate::functions::` (retired v1 sources of
/// function-name mappings; the modules were deleted on 2026-07-05 and the
/// barrier prevents accidental re-introduction). INV10's walker already
/// checks intra-τ imports at the file level; this test asserts the specific
/// emission-file constraint.
#[test]
fn inv3_emission_table_single_source_of_truth() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    let emission = root.join("crates/core/src/transpiler_v2/emission.rs");
    let contents = std::fs::read_to_string(&emission)
        .unwrap_or_else(|_| panic!("cannot read {}", emission.display()));
    // Only scan the non-test region — the `#[cfg(test)]` module in
    // emission.rs legitimately names forbidden prefixes inside its
    // assertion literals.
    let module_marker = "#[cfg(test)]\nmod tests {";
    let scan_slice = match contents.find(module_marker) {
        Some(idx) => &contents[..idx],
        None => contents.as_str(),
    };
    // Build needles at runtime so this test's source doesn't self-match.
    for base in ["generator", "functions"] {
        let use_form = format!("use crate::{base}::");
        let path_form = format!("crate::{base}::");
        assert!(
            !scan_slice.contains(&use_form),
            "INV3 violation — emission.rs contains `{use_form}`",
        );
        assert!(
            !scan_slice.contains(&path_form),
            "INV3 violation — emission.rs contains `{path_form}`",
        );
    }
}

// ── INV4 (ACTIVE) ──────────────────────────────────────────────────

/// INV4: inference is validated in isolation from emission — the analyzer's
/// schema/nullability results are verifiable without running any SQL through
/// DuckDB.
///
/// Iterates the τ's analyzer fixture registry and, for each Ok-path fixture,
/// asserts the analyzed `resolved_schema` field-by-field matches the
/// expected schema recorded in the fixture.
#[test]
fn inv4_inference_validated_in_isolation() {
    use super::analyze;
    use super::analyzer_fixtures;
    for (name, ast, base_types, expected_schema) in analyzer_fixtures::all_fixtures() {
        let typed = analyze(ast, &base_types)
            .unwrap_or_else(|e| panic!("fixture `{name}` failed to analyze: {e}"));
        assert_eq!(
            typed.resolved_schema.fields.len(),
            expected_schema.fields.len(),
            "fixture `{name}` field count mismatch: got {} fields, expected {}",
            typed.resolved_schema.fields.len(),
            expected_schema.fields.len(),
        );
        for (idx, (actual, expected)) in typed
            .resolved_schema
            .fields
            .iter()
            .zip(expected_schema.fields.iter())
            .enumerate()
        {
            assert_eq!(
                actual, expected,
                "fixture `{name}` field #{idx} mismatch: got {actual:?}, expected {expected:?}",
            );
        }
    }
}

// ── INV5 (ACTIVE) ──────────────────────────────────────────────────

/// INV5: every plan node carries a resolved schema after analysis; no
/// `DataType::Unresolved` remains and no `ColumnReference` has `data_type`
/// or `nullable` unset.
#[test]
fn inv5_schema_everywhere() {
    use super::analyzer_fixtures;
    use super::{analyze, has_resolved_schema};
    for (name, ast, base_types, _expected_schema) in analyzer_fixtures::all_fixtures() {
        let typed = analyze(ast, &base_types)
            .unwrap_or_else(|e| panic!("fixture `{name}` failed to analyze: {e}"));
        assert!(
            has_resolved_schema(&typed),
            "fixture `{name}` post-analysis has an unresolved schema or column",
        );
    }
}

// ── INV6 (deferred — extension targets exist) ─────────────────────────────────

/// DEFER INV6: every entry in `extension_targets()` MUST resolve
/// against `duckdb_functions()` in a loaded ext6 session. τ's extension-target wiring's Phase 2
/// activation opens a session, loads the extension, and asserts the allow-list
/// is a subset of the loaded function catalog.
#[test]
#[ignore]
fn inv6_extension_targets_exist() {
    todo!("INV6 activation requires extension_targets() + duckdb_functions() check")
}

// ── INV7 — OMITTED per ADR-022 §CV.5 ─────────────────────────────────────────

// INV7 was deleted from the invariant set. Do not add an INV7 stub.

// ── INV8 (deferred — external access delegation) ──────────────────────────────

/// DEFER INV8: any read/write against external storage is delegated
/// to a substrate adapter and NEVER inlined into emission arms.
#[test]
#[ignore]
fn inv8_external_access_delegated() {
    todo!("INV8 — activation deferred; not yet implemented")
}

// ── INV9 (deferred — writes require attached provenance) ──────────────────────

/// DEFER INV9: writable plans must carry attached provenance
/// (source-of-writes) before emission; no writes emitted from unattached plans.
#[test]
#[ignore]
fn inv9_writable_requires_attached_provenance() {
    todo!("INV9 — activation deferred; not yet implemented")
}

// ── INV10 (ACTIVE) ───────────────────────────────────────────────

/// A τ walk root — a directory and an optional file filter.
///
/// The filter exists so the connect-server crate can be walked without
/// pulling files outside τ's converter boundary. When `files == Some(names)`,
/// only files whose basename appears in `names` contribute to the walk;
/// when `files == None`, every `.rs` file under `dir` is walked.
#[cfg(test)]
struct WalkRoot {
    dir: &'static str,
    files: Option<&'static [&'static str]>,
}

/// Root paths INV10 walks. The τ dispatch site covers four roots:
///
/// - `crates/core/src/transpiler_v2/` — τ's substrate (unfiltered).
/// - `crates/core/src/parser_v2/` — τ's SparkSQL front-end (unfiltered).
/// - `crates/connect-server/src/converter/v2_relation_converter.rs` — τ's
///   protobuf front-end (single-file filter).
/// - `crates/connect-server/src/service.rs` — the τ dispatch site (single-file
///   filter; sibling files `main.rs`, `arrow_ipc.rs`, `error.rs` are
///   excluded from the walk scope).
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
/// The trailing separator (`::`, ` `, `;`) is spelled out per entry: bare
/// `use crate::parser` would also match the LIVE `crate::parser_v2`.
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
/// τ-owned tree. Extended to also cover the connect-server
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

/// the τ dispatch site anti-regression: no file under `crates/connect-server/src/`
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

// ── Attribute/ResolvedSchema struct-literal ban (finding 13a) ─────────────
//
// `schema.rs`'s module doc bans a bare struct-literal construction of
// `Attribute`/`ResolvedSchema` anywhere else in τ: every production site must
// go through a constructor (`Attribute::minted` / `Attribute::from_field` /
// `ResolvedSchema::minted` / `ResolvedSchema::new`) or a `.clone()` of an
// existing value, never a hand-written literal that would silently mint
// identity-less or duplicate-identity data bypassing the constructors. The
// convention was doc-only before this check (zero violations today — this
// pins it mechanically, reusing INV10's [`WALK_ROOTS`]/walk machinery).

/// A matched needle occurrence on `trimmed` is exempt (not a genuine
/// construction) when the line is one of three shapes that legitimately
/// place the type name directly before an opening brace without
/// constructing a value: a COMMENT or attribute line (doc prose may quote
/// the banned literal — e.g. this very ban's documentation), a
/// function/closure signature whose return type is immediately followed by
/// the body's opening brace (always carries a `->` earlier on the same
/// line — e.g. `fn f(..) -> ResolvedSchema{ .. }`), or a `struct`/`impl`
/// header (the type's own definition or an impl block on it). The `->`
/// exemption is deliberately line-wide and the header checks token-anchored
/// (`impl `/`impl<`) — a genuine bare construction co-occurring with an
/// arrow on one rustfmt'd line is implausible, and the residual
/// false-negative risk is preferred over false positives on signatures.
/// String literals are NOT skipped — none in the walked tree contains the
/// needle today, and a message quoting it should use backticks in a comment
/// instead.
#[cfg(test)]
fn is_struct_literal_exception(trimmed: &str) -> bool {
    trimmed.starts_with("//")
        || trimmed.starts_with('*')
        || trimmed.starts_with("#[")
        || trimmed.contains("->")
        || trimmed.starts_with("struct ")
        || trimmed.starts_with("pub struct ")
        || trimmed.starts_with("impl ")
        || trimmed.starts_with("impl<")
}

/// Whether `line` contains `needle` as a BARE token — i.e. not as the tail of
/// a longer identifier. Excludes, e.g., the proto `UnresolvedAttribute{`
/// literal (its character immediately before the match is `d`, a word
/// character) while still catching a genuine bare construction.
#[cfg(test)]
fn contains_bare_needle(line: &str, needle: &str) -> bool {
    let mut search_from = 0;
    while let Some(rel_idx) = line[search_from..].find(needle) {
        let idx = search_from + rel_idx;
        let is_bare = line[..idx]
            .chars()
            .next_back()
            .map(|c| !(c.is_alphanumeric() || c == '_'))
            .unwrap_or(true);
        if is_bare {
            return true;
        }
        search_from = idx + 1;
    }
    false
}

/// finding 13a: mechanically enforce the `Attribute`/`ResolvedSchema`
/// struct-literal ban outside `schema.rs`. Walks the same [`WALK_ROOTS`] as
/// INV10 (τ's substrate + front-ends + dispatch site), skipping `schema.rs`
/// itself (the ban's one legitimate constructor-and-test home).
#[test]
fn attribute_resolved_schema_literal_ban_outside_schema_module() {
    let root = find_workspace_root().expect("workspace root should be discoverable");
    // Built at runtime (two literals concatenated), NOT written as one
    // contiguous literal, so this test's own source text never contains the
    // substring it scans for (same self-match hazard `inv3_...` avoids by
    // building its needles via `format!`).
    let needles = [
        format!("{}{}", "Attribute", " {"),
        format!("{}{}", "ResolvedSchema", " {"),
    ];
    let mut offenders: Vec<String> = Vec::new();
    for walk in WALK_ROOTS {
        let dir = root.join(walk.dir);
        if !dir.exists() {
            continue;
        }
        for file in collect_files_for_root(&dir, walk) {
            if file.file_name().and_then(|s| s.to_str()) == Some("schema.rs") {
                // The ban's one legitimate home: constructors + own tests.
                continue;
            }
            let contents = match std::fs::read_to_string(&file) {
                Ok(c) => c,
                Err(_) => continue,
            };
            for (lineno, line) in contents.lines().enumerate() {
                let trimmed = line.trim_start();
                if is_struct_literal_exception(trimmed) {
                    continue;
                }
                for needle in &needles {
                    if contains_bare_needle(trimmed, needle) {
                        offenders.push(format!("{}:{}: {}", file.display(), lineno + 1, trimmed));
                    }
                }
            }
        }
    }
    assert!(
        offenders.is_empty(),
        "Attribute/ResolvedSchema struct-literal ban violated outside schema.rs:\n{}",
        offenders.join("\n"),
    );
}
