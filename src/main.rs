mod watcher;

use anyhow::Result;
use axum::{Router, routing::get};
use std::net::SocketAddr;
use tokio::task::spawn_blocking;

#[tokio::main]
async fn main() -> Result<()> {
    let watcher = spawn_blocking(watcher::run_service);
    let app = Router::new().route("/", get(|| async { "Rust Job Watcher is running" }));
    let address = SocketAddr::from(([0, 0, 0, 0], 3004));
    let listener = tokio::net::TcpListener::bind(address).await?;
    println!("Job Watcher HTTP server listening on {address}");

    tokio::select! {
        result = axum::serve(listener, app) => result?,
        result = watcher => result??,
    }
    Ok(())
}
