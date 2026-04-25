//! sonda-server — HTTP control plane for the Sonda telemetry generator.
//!
//! Exposes a REST API that allows scenarios to be started, inspected, and
//! stopped over HTTP. All scenario lifecycle logic is delegated to sonda-core.

mod auth;
mod routes;
mod state;

use std::env;
use std::io::Write;
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::process::{exit, Command};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use crate::state::AppState;

/// Subcommands the dispatch shim forwards to the sibling `sonda` binary.
/// Mirror of `sonda`'s clap definition.
const SONDA_SUBCOMMANDS: &[&str] = &[
    "metrics",
    "logs",
    "histogram",
    "summary",
    "run",
    "catalog",
    "scenarios",
    "packs",
    "import",
    "init",
];

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
    maybe_dispatch_to_sonda_cli();

    // Initialise structured logging. Respects RUST_LOG env var. Writes to
    // stderr so stdout is reserved for the bound-port announce contract.
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .with_writer(std::io::stderr)
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

    // Announce the actual bound port on stdout so parents (test harnesses,
    // tooling) get a typed, parseable signal once the OS has assigned a port
    // (necessary when `--port 0` is used) and the listener is ready to accept.
    let bound_addr = listener
        .local_addr()
        .context("failed to read local address from bound listener")?;
    announce_bound_port(bound_addr.port())?;

    info!(addr = %bound_addr, "sonda-server listening");

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(state))
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

/// Write the bound-port announce line to stdout and flush.
///
/// Contract: a single JSON line `{"sonda_server":{"port":N}}\n` is the first
/// (and only) thing written to stdout. The namespaced envelope leaves room for
/// future fields without colliding with anything else a tool might print.
fn announce_bound_port(port: u16) -> anyhow::Result<()> {
    let line = serde_json::json!({ "sonda_server": { "port": port } });
    let stdout = std::io::stdout();
    let mut handle = stdout.lock();
    writeln!(handle, "{line}").context("failed to write stdout announce")?;
    handle.flush().context("failed to flush stdout announce")?;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dispatch_list_covers_all_known_subcommands() {
        let expected = [
            "metrics",
            "logs",
            "histogram",
            "summary",
            "run",
            "catalog",
            "scenarios",
            "packs",
            "import",
            "init",
        ];
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
}
