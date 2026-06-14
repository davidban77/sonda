//! sonda-server — HTTP control plane for the Sonda telemetry generator.
//!
//! Exposes a REST API that allows scenarios to be started, inspected, and
//! stopped over HTTP. All scenario lifecycle logic is delegated to sonda-core.

mod auth;
mod gate_registry;
mod middleware;
mod routes;
mod state;

use std::collections::HashMap;
use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::{exit, Command};
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use anyhow::Context;
use clap::Parser;
use tokio::sync::Semaphore;
use tracing::{info, warn};

use crate::routes::RouterConfig;
use crate::state::AppState;

/// Subcommands the dispatch shim forwards to the sibling `sonda` binary.
/// Mirror of `sonda`'s clap definition.
const SONDA_SUBCOMMANDS: &[&str] = &["run", "list", "show", "new"];

/// Command-line arguments for sonda-server.
///
/// Uses a manual `Debug` implementation to redact `api_key`, preventing
/// accidental exposure of the secret in log output.
#[derive(Parser)]
#[command(name = "sonda-server", version, about = "HTTP control plane for Sonda")]
struct Args {
    /// Port to listen on.
    #[arg(long, default_value_t = 8080)]
    port: u16,

    /// Address to bind to.
    #[arg(long, default_value = "0.0.0.0")]
    bind: String,

    /// API key for bearer-token authentication on `/scenarios/*`, `/events`, and `/metrics` endpoints.
    ///
    /// When set, requests to these endpoints must include an
    /// `Authorization: Bearer <key>` header. The `/health` endpoint remains
    /// public regardless of this setting.
    ///
    /// Can also be set via the `SONDA_API_KEY` environment variable.
    #[arg(long, env = "SONDA_API_KEY")]
    api_key: Option<String>,

    /// Directory of scenario and pack YAML files for resolving `pack: <name>`
    /// references in posted scenario bodies.
    ///
    /// Can also be set via the `SONDA_CATALOG` environment variable.
    #[arg(long, env = "SONDA_CATALOG")]
    catalog: Option<PathBuf>,

    /// Tokio worker thread count. Defaults to `std::thread::available_parallelism()`.
    #[arg(long, value_parser = clap::builder::RangedU64ValueParser::<u64>::new().range(1..))]
    workers: Option<u64>,

    /// Maximum concurrent scenario rows in `AppState`. `0` means unlimited.
    #[arg(long, default_value_t = 0)]
    max_scenarios: usize,

    /// Maximum concurrent in-flight control-plane HTTP requests. Defaults to `4 * workers`.
    #[arg(long)]
    max_inflight_requests: Option<usize>,

    /// Per-request timeout in seconds applied to control-plane routes. Returns 408 on expiry.
    #[arg(long, default_value_t = 30)]
    request_timeout: u64,

    /// Maximum request body size in bytes accepted by control-plane routes. Returns 413 when exceeded.
    #[arg(long, default_value_t = 1_048_576)]
    max_body_bytes: usize,
}

impl std::fmt::Debug for Args {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Args")
            .field("port", &self.port)
            .field("bind", &self.bind)
            .field("api_key", &self.api_key.as_ref().map(|_| "[REDACTED]"))
            .field("catalog", &self.catalog)
            .field("workers", &self.workers)
            .field("max_scenarios", &self.max_scenarios)
            .field("max_inflight_requests", &self.max_inflight_requests)
            .field("request_timeout", &self.request_timeout)
            .field("max_body_bytes", &self.max_body_bytes)
            .finish()
    }
}

fn main() -> anyhow::Result<()> {
    maybe_dispatch_to_sonda_cli();

    let args = Args::parse();
    let workers = args.workers.map(|n| n as usize).unwrap_or_else(|| {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1)
    });
    let max_inflight_requests = args.max_inflight_requests.unwrap_or(workers * 4);

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(workers)
        .enable_all()
        .build()
        .context("failed to build tokio runtime")?;

    runtime.block_on(async move { run(args, workers, max_inflight_requests).await })
}

async fn run(args: Args, workers: usize, max_inflight_requests: usize) -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
        .init();

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
        info!("API key authentication enabled for /scenarios/*, /events, and /metrics endpoints");
    } else {
        info!("API key authentication disabled — all endpoints are public");
    }

    if let Some(dir) = &args.catalog {
        if !dir.is_dir() {
            anyhow::bail!(
                "--catalog {}: does not exist or is not a directory",
                dir.display()
            );
        }
        info!(catalog = %dir.display(), "pack catalog enabled for POST /scenarios");
    }

    let permits = if args.max_scenarios == 0 {
        warn!("--max-scenarios 0 — scenario row cap disabled (unlimited)");
        Semaphore::new(Semaphore::MAX_PERMITS)
    } else {
        Semaphore::new(args.max_scenarios)
    };

    let state = AppState {
        scenarios: Arc::new(RwLock::new(HashMap::new())),
        api_key: api_key.map(Arc::new),
        catalog_dir: args.catalog.clone().map(Arc::new),
        gate_bus_registry: Arc::new(crate::gate_registry::GateBusRegistry::new()),
        scenario_permits: Arc::new(permits),
        started_at: Instant::now(),
        worker_threads: workers,
        max_scenarios: args.max_scenarios,
        request_counters: Arc::new(RwLock::new(
            HashMap::<crate::state::RouteKey, AtomicU64>::new(),
        )),
        request_histograms: Arc::new(RwLock::new(HashMap::new())),
    };

    let inflight_semaphore = Arc::new(Semaphore::new(max_inflight_requests));
    let router_cfg = RouterConfig {
        request_timeout: Duration::from_secs(args.request_timeout),
        max_body_bytes: args.max_body_bytes,
        inflight_semaphore,
    };
    let app = routes::router_with_config(state.clone(), router_cfg);

    let listener = tokio::net::TcpListener::bind(bind_addr)
        .await
        .with_context(|| format!("failed to bind to {bind_addr}"))?;

    let bound_addr = listener
        .local_addr()
        .context("failed to read local address from bound listener")?;

    #[cfg(unix)]
    let sigterm = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
        .context("failed to install SIGTERM handler")?;

    announce_bound_port(bound_addr.port())?;

    info!(addr = %bound_addr, workers, "sonda-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(
            state,
            #[cfg(unix)]
            sigterm,
        ))
        .await
        .context("server error")?;

    info!("sonda-server shut down cleanly");
    Ok(())
}

/// If `argv[1]` is a sonda subcommand, exec the sibling `sonda` binary and
/// never return. Otherwise no-op. Sibling resolved via `current_exe()` so dev
/// builds dispatch within the same `target/<profile>/`.
fn maybe_dispatch_to_sonda_cli() {
    let mut args = env::args_os();
    let _self_arg = args.next();
    let first = match args.next() {
        Some(arg) => arg,
        None => return,
    };

    let Some(first_str) = first.to_str() else {
        return;
    };

    if !SONDA_SUBCOMMANDS.contains(&first_str) {
        return;
    }

    let sibling = match env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("sonda")))
    {
        Some(path) => path,
        None => {
            eprintln!(
                "sonda-server: failed to resolve sibling `sonda` binary path; \
                 cannot dispatch `{first_str}` subcommand"
            );
            exit(127);
        }
    };

    // exec only returns on failure (replaces process image otherwise).
    let err = Command::new(&sibling).arg(&first).args(args).exec();
    eprintln!(
        "sonda-server: failed to exec sibling sonda binary at {}: {err}",
        sibling.display()
    );
    exit(127);
}

/// Write `{"sonda_server":{"port":N}}\n` to stdout and flush.
fn announce_bound_port(port: u16) -> anyhow::Result<()> {
    let line = serde_json::json!({ "sonda_server": { "port": port } });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{line}").context("failed to write stdout announce")?;
    handle.flush().context("failed to flush stdout announce")?;
    Ok(())
}

/// Wait for Ctrl+C or SIGTERM, then stop all running scenarios and signal
/// shutdown. SIGTERM coverage is what `docker stop` and Kubernetes pod eviction
/// rely on; without it the process is SIGKILLed after the grace period.
async fn shutdown_signal(state: AppState, #[cfg(unix)] mut sigterm: tokio::signal::unix::Signal) {
    let ctrl_c = async {
        tokio::signal::ctrl_c()
            .await
            .expect("failed to install ctrl_c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        sigterm.recv().await;
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {},
        _ = terminate => {},
    }

    info!("shutdown signal received — stopping all running scenarios");

    if let Ok(scenarios) = state.scenarios.read() {
        for handle in scenarios.values() {
            handle.stop();
        }
    }

    // Write lock: join consumes the inner JoinHandle.
    let mut ids_handles: Vec<(String, sonda_core::ScenarioHandle)> = Vec::new();
    if let Ok(mut scenarios) = state.scenarios.write() {
        for (id, handle) in scenarios.drain() {
            ids_handles.push((id, handle));
        }
    }
    for (id, mut handle) in ids_handles {
        match handle.join_async(Some(Duration::from_secs(5))).await {
            Ok(_) => info!(scenario = %id, "scenario task joined"),
            Err(e) => warn!(scenario = %id, error = %e, "scenario task join failed"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_list_covers_all_known_subcommands() {
        let expected = ["run", "list", "show", "new"];
        assert_eq!(SONDA_SUBCOMMANDS.len(), expected.len());
        for name in expected {
            assert!(
                SONDA_SUBCOMMANDS.contains(&name),
                "{name} must be in SONDA_SUBCOMMANDS"
            );
        }
    }

    #[test]
    fn server_flags_are_not_treated_as_subcommands() {
        for flag in [
            "--port",
            "--bind",
            "--api-key",
            "--help",
            "--version",
            "-h",
            "-V",
        ] {
            assert!(
                !SONDA_SUBCOMMANDS.contains(&flag),
                "{flag} must not be in SONDA_SUBCOMMANDS"
            );
        }
    }

    #[test]
    fn dispatch_list_has_no_duplicates() {
        let mut sorted: Vec<&str> = SONDA_SUBCOMMANDS.to_vec();
        sorted.sort_unstable();
        let len_before = sorted.len();
        sorted.dedup();
        assert_eq!(
            len_before,
            sorted.len(),
            "SONDA_SUBCOMMANDS contains duplicates"
        );
    }

    #[test]
    fn sonda_git_sha_env_is_injected_by_build_rs() {
        let sha = env!("SONDA_GIT_SHA");
        assert!(
            !sha.is_empty(),
            "SONDA_GIT_SHA must be injected (either a git rev or the 'unknown' fallback)"
        );
    }
}
