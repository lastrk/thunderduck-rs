fn main() -> Result<(), Box<dyn std::error::Error>> {
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
