use anyhow::{Context, Result};
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    routing::{get, post},
};
use job_watcher::{load_environment, watcher};
use serde_json::Value;
use std::net::SocketAddr;
use tokio::task::spawn_blocking;
use tracing::info;

#[tokio::main]
async fn main() -> Result<()> {
    load_environment()?;
    tracing_subscriber::fmt().with_target(false).init();
    let service = watcher::Service::new()?;
    let app = Router::new()
        .route("/", get(|| async { "Rust Job Watcher is running" }))
        .route("/webhook", post(webhook))
        .with_state(service.clone());
    let port = std::env::var("JOB_WATCHER_PORT")
        .unwrap_or_else(|_| "3004".to_owned())
        .parse::<u16>()
        .context("JOB_WATCHER_PORT must be a valid port number")?;
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    info!(%address, "starting Job Watcher HTTP server");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .with_context(|| format!("failed to bind Job Watcher HTTP server to {address}"))?;
    info!(%address, "Job Watcher HTTP server listening");

    let watcher = spawn_blocking(move || watcher::run_service(service));

    tokio::select! {
        result = axum::serve(listener, app) => result?,
        result = watcher => result??,
    }
    Ok(())
}

async fn webhook(
    State(service): State<watcher::Service>,
    Json(payload): Json<Value>,
) -> StatusCode {
    info!(
        event_count = payload
            .get("events")
            .and_then(|events| events.as_array())
            .map_or(0, Vec::len),
        "received LINE webhook event"
    );
    watcher::handle_webhook(payload, service);
    StatusCode::OK
}
