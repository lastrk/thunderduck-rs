mod arrow_ipc;
mod converter;
mod error;
mod service;

use std::net::SocketAddr;
use std::sync::Arc;

use clap::Parser;
use thunderduck_core::runtime::{RuntimeCompatMode, SessionManager, StreamingConfig};
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
#[command(name = "thunderduck-connect-server", about = "Thunderduck Spark Connect Server")]
struct Args {
    /// Bind address (host:port)
    #[arg(long, default_value = "0.0.0.0:15002")]
    bind: SocketAddr,

    /// Enable strict Spark compatibility mode (requires thdck_spark_funcs extension)
    #[arg(long, conflicts_with = "relaxed")]
    strict: bool,

    /// Enable relaxed mode (vanilla DuckDB functions, ~85% Spark parity)
    #[arg(long, conflicts_with = "strict")]
    relaxed: bool,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    let mode = if args.strict {
        RuntimeCompatMode::Strict
    } else if args.relaxed {
        RuntimeCompatMode::Relaxed
    } else {
        RuntimeCompatMode::from_env()
    };

    eprintln!("Starting Thunderduck Connect Server on {} (mode: {:?})", args.bind, mode);

    let mgr = Arc::new(SessionManager::new(mode, StreamingConfig::default()));
    let svc = ThunderduckService::new(mgr, mode);

    Server::builder()
        .add_service(SparkConnectServiceServer::new(svc))
        .serve(args.bind)
        .await?;

    Ok(())
}
