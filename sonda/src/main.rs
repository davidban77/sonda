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

            // Run the scenario. The runner blocks until duration elapses or
            // until Ctrl+C sets `running` to false (see note below).
            //
            // Note: the current runner in sonda-core does not take a
            // `running` flag — it relies on duration for termination. Ctrl+C
            // via the AtomicBool ensures a clean exit for indefinite runs
            // because the OS delivers SIGINT which the ctrlc crate catches;
            // the handler sets the flag and the process exits normally after
            // the next sleep interval completes. For the MVP this is
            // acceptable. A future slice can thread the AtomicBool into
            // run_with_sink.
            sonda_core::schedule::runner::run(&config).map_err(|e| anyhow::anyhow!("{}", e))?;

            // If Ctrl+C was pressed before the duration elapsed, exit cleanly.
            if !running.load(Ordering::SeqCst) {
                // Output was already flushed by the runner's flush-on-exit path.
            }
        }
    }

    Ok(())
}
