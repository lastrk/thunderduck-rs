fn main() -> Result<(), Box<dyn std::error::Error>> {
    add_duckdb_runtime_rpath();

    // Use vendored protoc so well-known types (google/protobuf/*.proto) are always
    // available regardless of what protoc version is installed on the host system.
    // This fixes macOS builds where prost-build 0.13 no longer bundles these itself.
    if let Ok(protoc) = protoc_bin_vendored::protoc_bin_path() {
        std::env::set_var("PROTOC", protoc);
    }

    let proto_files = [
        "proto/spark/connect/base.proto",
        "proto/spark/connect/relations.proto",
        "proto/spark/connect/expressions.proto",
        "proto/spark/connect/types.proto",
        "proto/spark/connect/commands.proto",
        "proto/spark/connect/common.proto",
        "proto/spark/connect/catalog.proto",
        "proto/spark/connect/ml.proto",
        "proto/spark/connect/ml_common.proto",
        "proto/spark/connect/pipelines.proto",
        "proto/spark/connect/example_plugins.proto",
    ];

    tonic_build::configure()
        .build_server(true)
        .build_client(false)
        .compile_protos(&proto_files, &["proto"])?;

    Ok(())
}

/// Find the version-matched dynamic DuckDB library that `libduckdb-sys`
/// copies beside Cargo-built executables in `deps/`.
///
/// The download build path emits its own rpath, but that linker option does not
/// propagate through a Rust library dependency to this final binary. Keep the
/// path relative to the executable so release builds work without
/// `LD_LIBRARY_PATH` / `DYLD_LIBRARY_PATH`.
fn add_duckdb_runtime_rpath() {
    let target = std::env::var("TARGET").expect("Cargo always sets TARGET for build scripts");

    if target.contains("linux") {
        println!("cargo:rustc-link-arg-bin=thunderduck-connect-server=-Wl,-rpath,$ORIGIN/deps");
    } else if target.contains("apple") {
        println!(
            "cargo:rustc-link-arg-bin=thunderduck-connect-server=-Wl,-rpath,@loader_path/deps"
        );
    }
}
