mod watcher;

use anyhow::{Context, Result};
use axum::{
    Json, Router,
    http::StatusCode,
    routing::{get, post},
};
use serde_json::Value;
use std::{net::SocketAddr, path::Path};
use tokio::task::spawn_blocking;

#[tokio::main]
async fn main() -> Result<()> {
    load_environment()?;
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

    tokio::select! {
        result = axum::serve(listener, app) => result?,
        result = watcher => result??,
    }
    Ok(())
}

fn load_environment() -> Result<()> {
    let system_config = Path::new("/etc/job-watcher/job-watcher.env");
    if system_config.is_file() {
        dotenvy::from_path(system_config)
            .context("failed to load /etc/job-watcher/job-watcher.env")?;
    } else {
        dotenvy::dotenv().ok();
    }
    Ok(())
}

async fn webhook(Json(payload): Json<Value>) -> StatusCode {
    println!("Received LINE webhook event: {payload}");
    StatusCode::OK
}
