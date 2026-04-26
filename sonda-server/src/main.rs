//! sonda-server — HTTP control plane for the Sonda telemetry generator.
//!
//! Exposes a REST API that allows scenarios to be started, inspected, and
//! stopped over HTTP. All scenario lifecycle logic is delegated to sonda-core.

mod auth;
mod routes;
mod state;

use std::env;
use std::net::SocketAddr;
use std::os::unix::process::CommandExt;
use std::process::{exit, Command};
use std::time::Duration;

use anyhow::Context;
use clap::Parser;
use tracing::{info, warn};

use crate::state::AppState;

/// Sonda CLI subcommands recognised by the dispatch shim. Kept in sync with
/// the `sonda` binary's clap definition; if a subcommand is added there, add
/// it here so `docker run image <subcommand> ...` reaches the CLI directly.
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
    // Dispatch to the sibling sonda CLI binary when the first argument is one
    // of its subcommands. Lets `docker run image metrics ...` work without an
    // entrypoint override. Fires before clap parsing so unknown-subcommand
    // errors from clap don't shadow the dispatch.
    maybe_dispatch_to_sonda_cli();

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

/// If the first CLI argument is a sonda subcommand, exec the sibling `sonda`
/// binary in place of this process and never return. Otherwise return so the
/// caller can continue with sonda-server's own arg parsing.
///
/// The sibling binary is resolved relative to the current executable so that
/// dev runs (`cargo run -p sonda-server -- metrics ...`) dispatch to the
/// `sonda` built into the same `target/<profile>/` directory rather than a
/// hardcoded `/sonda`.
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

    // CommandExt::exec replaces the current process image — on success it
    // never returns. The error path is reached only when exec itself fails
    // (e.g., the sibling binary is missing or not executable).
    let err = Command::new(&sibling).arg(&first).args(args).exec();
    eprintln!(
        "sonda-server: failed to exec sibling sonda binary at {}: {err}",
        sibling.display()
    );
    exit(127);
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

    /// The dispatch list must include every public `sonda` subcommand. If a new
    /// subcommand is added to the CLI without being mirrored here, dispatch
    /// silently falls through to clap and surfaces an unhelpful error.
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

    /// Sonda-server's own flags (--port, --bind, --api-key) and clap built-ins
    /// (--help, --version) must NOT be matched as sonda subcommands, otherwise
    /// the dispatch shim would hijack them and break server startup.
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

    /// The dispatch list contains no duplicates — duplicates are a smell that
    /// the source-of-truth `sonda` clap definition has drifted.
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
