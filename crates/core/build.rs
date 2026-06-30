fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    download_extension();
}

fn download_extension() {
    // The `ext4` release packs binaries for multiple DuckDB versions under a
    // single tag, with the DuckDB version embedded in each filename. We pick
    // the one matching the duckdb crate (currently 1.10501.0 → DuckDB 1.5.1).
    const RELEASE_TAG: &str = "ext4";
    const EXT_DUCKDB_VERSION: &str = "v1.5.1";
    const BASE_URL: &str =
        "https://github.com/nubank/thunderduck-duckdb-extension/releases/download";

    let target = std::env::var("TARGET").expect("TARGET not set by Cargo");
    let platform = detect_platform(&target);

    let filename = format!("thdck_spark_funcs-{EXT_DUCKDB_VERSION}-{platform}.duckdb_extension");

    // Persistent cache: {workspace_root}/extensions/{tag}/{filename}
    // Lives outside Cargo's target/ so it survives `cargo clean`.
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").unwrap();
    let workspace_root = std::path::Path::new(&manifest_dir)
        .parent()
        .unwrap()
        .parent()
        .unwrap();
    let cache_dir = workspace_root.join("extensions").join(RELEASE_TAG);
    let cache_path = cache_dir.join(&filename);

    if !cache_path.exists() {
        std::fs::create_dir_all(&cache_dir).expect("failed to create extensions cache directory");

        let url = format!("{BASE_URL}/{RELEASE_TAG}/{filename}");
        println!("cargo:warning=Downloading extension from {url}");

        let status = std::process::Command::new("curl")
            .args(["-fL", "--retry", "3", "-o"])
            .arg(&cache_path)
            .arg(&url)
            .status()
            .expect("failed to run curl — is curl installed?");

        if !status.success() {
            panic!(
                "Failed to download extension binary from {url}\n\
                 Check your internet connection or download it manually to:\n  {}",
                cache_path.display()
            );
        }
    }

    // Copy to OUT_DIR so include_bytes! has a stable, cargo-managed path.
    let out_dir = std::env::var("OUT_DIR").unwrap();
    let dest = std::path::Path::new(&out_dir).join("thdck_spark_funcs.duckdb_extension");
    std::fs::copy(&cache_path, &dest).expect("failed to copy extension to OUT_DIR");

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
            "Unsupported target for bundled-extension: {target}\n\
             Supported targets: x86_64-linux, aarch64-linux, x86_64-apple, aarch64-apple"
        )
    }
}
