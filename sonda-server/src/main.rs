//! sonda-server — HTTP control plane for the Sonda telemetry generator.
//!
//! Exposes a REST API that allows scenarios to be started, inspected, and
//! stopped over HTTP. All scenario lifecycle logic is delegated to sonda-core.

mod auth;
mod routes;
mod state;

use std::net::SocketAddr;
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use crate::state::AppState;

/// Command-line arguments for sonda-server.
///
/// Uses a manual `Debug` implementation to redact `api_key`, preventing
/// accidental exposure of the secret in log output.
#[derive(Parser)]
#[command(name = "sonda-server", about = "HTTP control plane for Sonda")]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Address to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// API key for bearer-token authentication on `/scenarios/*` endpoints.
    ///
    /// When set, all requests to `/scenarios/*` must include an
    /// `Authorization: Bearer <key>` header. The `/health` endpoint remains
    /// public regardless of this setting.
    ///
    /// Can also be set via the `SONDA_API_KEY` environment variable.
    #[arg(long, env = "SONDA_API_KEY")]
    api_key: Option<String>,
}

impl std::fmt::Debug for Args {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Args")
            .field("port", &self.port)
            .field("bind", &self.bind)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .finish()
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Initialise structured logging. Respects RUST_LOG env var.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let args = Args::parse();

    let bind_addr: SocketAddr = format!("{}:{}", args.bind, args.port)
        .parse()
        .with_context(|| format!("invalid bind address: {}:{}", args.bind, args.port))?;

    // Normalise the API key: treat empty strings as None (disabled).
    let api_key = args.api_key.filter(|k| {
        if k.is_empty() {
            warn!("--api-key / SONDA_API_KEY is empty — authentication disabled");
            false
        } else {
            true
        }
    });

    if api_key.is_some() {
        info!("API key authentication enabled for /scenarios/* endpoints");
    } else {
        info!("API key authentication disabled — all endpoints are public");
    }

    let state = AppState::with_api_key(api_key);
    let app = routes::router(state.clone());

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind to {bind_addr}"))?;

    info!(addr = %bind_addr, "sonda-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
        .await
        .context("server error")?;

    info!("sonda-server shut down cleanly");
    Ok(())
}

/// Wait for Ctrl+C, then stop all running scenarios and signal shutdown.
async fn shutdown_signal(state: AppState) {
    tokio::signal::ctrl_c()
        .await
        .expect("failed to listen for ctrl_c signal");

    info!("shutdown signal received — stopping all running scenarios");

    // Stop every running scenario so their threads exit cleanly.
    if let Ok(scenarios) = state.scenarios.read() {
        for handle in scenarios.values() {
            handle.stop();
        }
    }

    // Join scenario threads with a timeout so sinks can flush before exit.
    // Requires a write lock because join() consumes the inner JoinHandle.
    if let Ok(mut scenarios) = state.scenarios.write() {
        for (id, handle) in scenarios.iter_mut() {
            match handle.join(Some(Duration::from_secs(5))) {
                Ok(_) => info!(scenario = %id, "scenario thread joined"),
                Err(e) => warn!(scenario = %id, error = %e, "scenario thread join failed"),
            }
        }
    }
}
