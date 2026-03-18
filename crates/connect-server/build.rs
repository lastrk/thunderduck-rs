fn main() -> Result<(), Box<dyn std::error::Error>> {
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
