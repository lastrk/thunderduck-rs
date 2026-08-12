fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    link_external_duckdb_runtime();
    embed_vendored_extension();
}

/// When DuckDB is NOT compiled from source (the `bundled` feature is off), we
/// link a prebuilt `libduckdb` provided via `DUCKDB_LIB_DIR`. That static
/// archive is built from C++ and pulls in the C++ runtime plus libm, which the
/// non-bundled path of `libduckdb-sys` does not emit. Add them here so they
/// land after `duckdb` on the final link line and resolve its symbols. The
/// `bundled` path already links the C++ runtime itself, so skip it then.
fn link_external_duckdb_runtime() {
    println!("cargo:rerun-if-env-changed=DUCKDB_LIB_DIR");
    if std::env::var_os("CARGO_FEATURE_BUNDLED").is_some() {
        return; // bundled build handles the C++ runtime
    }
    // Apple toolchains (Xcode 15+) no longer ship `libstdc++.dylib`, only
    // `libc++.dylib`; Linux toolchains ship `libstdc++` and lack `libc++` by
    // default. Pick the one the platform actually has.
    let cxx_runtime = if std::env::var("CARGO_CFG_TARGET_VENDOR").as_deref() == Ok("apple") {
        "c++"
    } else {
        "stdc++"
    };
    println!("cargo:rustc-link-lib=dylib={cxx_runtime}");
    println!("cargo:rustc-link-lib=dylib=m");
    if std::env::var_os("DUCKDB_LIB_DIR").is_none() {
        println!(
            "cargo:warning=DuckDB `bundled` feature is off and DUCKDB_LIB_DIR is unset; \
             linking will look for a system libduckdb. For local dev run \
             scripts/dev/dev-cache-setup.sh, or build with `--features bundled`."
        );
    }
}

/// Embed the vendored `thdck_spark_funcs` extension binary into the build.
///
/// `extensions/vendored/` checks in all 4 platform binaries of exactly one
/// adopted release (see `scripts/dev/adopt-extension-release.sh` and
/// `extensions/vendored/MANIFEST.toml`); this just picks the one matching
/// `TARGET` and copies it to `OUT_DIR` so `include_bytes!` has a stable,
/// cargo-managed path. No network access — vendoring is a git-tracked,
/// once-per-adoption step, not a build-time fetch.
fn embed_vendored_extension() {
    println!("cargo:rerun-if-env-changed=THUNDERDUCK_EXT_PATH");

    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = std::path::Path::new(&out_dir).join("thdck_spark_funcs.duckdb_extension");

    // Build-time embed override for extension developers: point straight at a
    // locally built binary, bypassing the vendored set entirely. This is a
    // different phase from the runtime `THUNDERDUCK_DELTA_EXT_PATH`
    // (`crates/core/src/runtime/extension_loader.rs`), which `LOAD`s a second,
    // *additional* extension at session startup rather than replacing the
    // embedded bytes at compile time.
    if let Ok(path) = std::env::var("THUNDERDUCK_EXT_PATH") {
        let src = std::path::Path::new(&path);
        println!("cargo:rerun-if-changed={}", src.display());
        std::fs::copy(src, &dest).unwrap_or_else(|e| {
            panic!("THUNDERDUCK_EXT_PATH={} set but failed to copy: {e}", path)
        });
        println!("cargo:rustc-env=EXTENSION_BIN_PATH={}", dest.display());
        return;
    }

    // crates/core → workspace root → extensions/vendored. build.rs has no
    // workspace_root() helper, so derive it from CARGO_MANIFEST_DIR directly.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let vendored_dir = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .join("extensions")
        .join("vendored");

    let target = std::env::var("TARGET").expect("TARGET not set by Cargo");
    let platform = detect_platform(&target);
    let suffix = format!("-{platform}.duckdb_extension");

    let entries = std::fs::read_dir(&vendored_dir).unwrap_or_else(|e| {
        panic!(
            "Failed to read vendored extension directory {}: {e}\n\
             Run scripts/dev/adopt-extension-release.sh <release-tag> <duckdb-version> to \
             vendor a release (or `git lfs pull` if this tree has migrated to LFS).",
            vendored_dir.display()
        )
    });

    let mut matches: Vec<std::path::PathBuf> = entries
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter(|p| {
            p.file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("thdck_spark_funcs-") && n.ends_with(&suffix))
        })
        .collect();

    match matches.len() {
        0 => panic!(
            "No vendored thdck_spark_funcs extension found for platform {platform} under {}\n\
             Run scripts/dev/adopt-extension-release.sh <release-tag> <duckdb-version> to vendor \
             a release for this platform (or `git lfs pull` if this tree has migrated to LFS).",
            vendored_dir.display()
        ),
        1 => {}
        _ => panic!(
            "Multiple vendored thdck_spark_funcs extensions match platform {platform} under {}: \
             {matches:?}\n\
             Adoption must keep exactly one version vendored at a time — re-run \
             scripts/dev/adopt-extension-release.sh to reconcile.",
            vendored_dir.display()
        ),
    }

    let src = matches.remove(0);
    println!("cargo:rerun-if-changed={}", src.display());
    std::fs::copy(&src, &dest)
        .unwrap_or_else(|e| panic!("failed to copy {} to OUT_DIR: {e}", src.display()));

    println!("cargo:rustc-env=EXTENSION_BIN_PATH={}", dest.display());
}

fn detect_platform(target: &str) -> &'static str {
    if target.contains("x86_64") && target.contains("linux") {
        "linux_amd64"
    } else if target.contains("aarch64") && target.contains("linux") {
        "linux_arm64"
    } else if target.contains("x86_64") && target.contains("apple") {
        "osx_amd64"
    } else if target.contains("aarch64") && target.contains("apple") {
        "osx_arm64"
    } else {
        panic!(
            "Unsupported target for thdck_spark_funcs extension: {target}\n\
             Supported targets: x86_64-linux, aarch64-linux, x86_64-apple, aarch64-apple"
        )
    }
}
