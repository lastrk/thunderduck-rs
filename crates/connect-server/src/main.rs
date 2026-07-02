mod arrow_ipc;
mod converter;
mod error;
mod service;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use thunderduck_core::runtime::{SessionManager, StreamingConfig};
use tonic::transport::Server;

use crate::proto::spark::connect::spark_connect_service_server::SparkConnectServiceServer;
use crate::service::ThunderduckService;

// Include proto-generated code.
pub mod proto {
    pub mod spark {
        pub mod connect {
            tonic::include_proto!("spark.connect");
        }
    }
}

#[derive(Parser)]
#[command(
    name = "thunderduck-connect-server",
    about = "Thunderduck Spark Connect Server"
)]
struct Args {
    /// Bind address (host:port)
    #[arg(long, default_value = "0.0.0.0:15002")]
    bind: String,

    /// Port to listen on (overrides the port in --bind; host remains 0.0.0.0)
    #[arg(long)]
    port: Option<u16>,

    /// Deprecated no-op: strict mode is the only supported mode. Accepted so
    /// existing scripts keep working; logs a one-line deprecation warning.
    #[arg(long)]
    strict: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let bind: SocketAddr = if let Some(port) = args.port {
        format!("0.0.0.0:{port}").parse()?
    } else {
        args.bind.parse()?
    };

    if args.strict {
        tracing::warn!(
            "--strict is deprecated and has no effect; strict is the only mode \
             (see ADR-020 in docs/thunderduck-rearchitect-ADRs.md)"
        );
    }

    // Reject any THUNDERDUCK_COMPAT_MODE=relaxed at startup so users discover the
    // change loudly; "strict" and "auto" are accepted as deprecated no-ops.
    match std::env::var("THUNDERDUCK_COMPAT_MODE")
        .unwrap_or_default()
        .to_lowercase()
        .as_str()
    {
        "" => {}
        "strict" | "auto" => {
            tracing::warn!(
                "THUNDERDUCK_COMPAT_MODE is deprecated and ignored; strict is the only mode"
            );
        }
        "relaxed" => {
            return Err("THUNDERDUCK_COMPAT_MODE=relaxed is no longer supported \
                        (see ADR-020); strict is the only mode"
                .into());
        }
        other => {
            return Err(format!(
                "unrecognized THUNDERDUCK_COMPAT_MODE '{other}', expected 'strict' or unset"
            )
            .into());
        }
    }

    // v2 transpiler path removed pending Slice A of the restart track
    // (see tag `v2-morph-track-end` for the discarded implementation).
    // Legacy is the sole active path until Slice A relocates dispatch to
    // the protobuf boundary per ADR-021.
    if std::env::var("THUNDERDUCK_TRANSPILER").is_ok() {
        tracing::warn!(
            "THUNDERDUCK_TRANSPILER is currently ignored; the v2 transpiler was \
             discarded in favor of a fresh implementation (see \
             tasks/v2-adr-readiness-map.md Slice A)"
        );
    }

    tracing::info!("Starting Thunderduck Connect Server on {}", bind);

    let mgr = Arc::new(SessionManager::new(StreamingConfig::default()));
    let svc = ThunderduckService::new(mgr);

    Server::builder()
        .add_service(SparkConnectServiceServer::new(svc))
        .serve(bind)
        .await?;

    Ok(())
}
