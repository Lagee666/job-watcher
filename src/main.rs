mod watcher;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::Value;
use std::net::SocketAddr;
use tokio::task::spawn_blocking;

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();
    let app = Router::new()
        .route("/", get(|| async { "Rust Job Watcher is running" }))
        .route("/webhook", post(webhook));
    let port = std::env::var("JOB_WATCHER_PORT")
        .unwrap_or_else(|_| "3004".to_owned())
        .parse::<u16>()
        .context("JOB_WATCHER_PORT must be a valid port number")?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    println!("Starting Job Watcher HTTP server on {address}");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind Job Watcher HTTP server to {address}"))?;
    println!("Job Watcher HTTP server listening on {address}");

    let watcher = spawn_blocking(watcher::run_service);
    println!("Job Watcher HTTP server listening on {address}");

    tokio::select! {
        result = axum::serve(listener, app) => result?,
        result = watcher => result??,
    }
    Ok(())
}

async fn webhook(Json(payload): Json<Value>) -> StatusCode {
    println!("Received LINE webhook event: {payload}");
    StatusCode::OK
}
