//! sonda — CLI entrypoint.
//!
//! Parses arguments, loads config, validates it, then delegates to the
//! `sonda-core` scenario runner. All signal-generation logic lives in
//! `sonda-core`; this file is pure orchestration.

mod cli;
mod config;

use std::process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use clap::Parser;

use cli::{Cli, Commands};

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        process::exit(1);
    }
}

/// Top-level orchestration: parse → load → validate → run.
///
/// Separated from `main` so errors can be returned with `?` and printed
/// uniformly.
fn run() -> anyhow::Result<()> {
    // Register Ctrl+C handler. The runner loop checks `running` each tick so
    // it can exit gracefully instead of being killed mid-write.
    let running = Arc::new(AtomicBool::new(true));
    {
        let r = Arc::clone(&running);
        ctrlc::set_handler(move || {
            r.store(false, Ordering::SeqCst);
        })
        .expect("failed to register Ctrl+C handler");
    }

    let cli = Cli::parse();

    match cli.command {
        Commands::Metrics(ref args) => {
            let config = config::load_config(args)?;
            sonda_core::config::validate::validate_config(&config)
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            // Run the scenario. The runner blocks until the configured duration
            // elapses. For indefinite runs (no `duration`), the OS delivers
            // SIGINT on Ctrl+C, which the ctrlc crate catches; the handler
            // sets `running` to false and the process exits cleanly after the
            // signal is received (the next blocking sleep wakes up).
            //
            // TODO(slice-future): thread `Arc<AtomicBool>` into
            // `sonda_core::schedule::runner::run` so the tick loop checks
            // `running` on every iteration. This allows immediate, mid-tick
            // cancellation instead of waiting for the next sleep to complete.
            sonda_core::schedule::runner::run(&config).map_err(|e| anyhow::anyhow!("{}", e))?;

            // After the runner returns, check whether Ctrl+C was the cause.
            // Nothing to do here — ctrlc handler already set the flag and the
            // runner exited naturally; stdout was flushed by the runner itself.
            let _ = running.load(Ordering::SeqCst);
        }
    }

    Ok(())
}
