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

            // Build a sink from the config so we can pass it and the shutdown
            // flag directly to run_with_sink. This allows Ctrl+C to be checked
            // on every tick rather than waiting for a blocking sleep to wake up.
            let mut sink = sonda_core::sink::create_sink(&config.sink)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            sonda_core::schedule::runner::run_with_sink(
                &config,
                sink.as_mut(),
                Some(running.as_ref()),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args)?;
            sonda_core::config::validate::validate_log_config(&config)
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            // Build a sink from the config so we can pass it and the shutdown
            // flag directly to run_logs_with_sink.
            let mut sink = sonda_core::sink::create_sink(&config.sink)
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            sonda_core::schedule::log_runner::run_logs_with_sink(
                &config,
                sink.as_mut(),
                Some(running.as_ref()),
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Run(ref args) => {
            let config = config::load_multi_config(args)?;

            // Validate each scenario entry before handing off to the runner.
            for (i, entry) in config.scenarios.iter().enumerate() {
                match entry {
                    sonda_core::ScenarioEntry::Metrics(scenario_config) => {
                        sonda_core::config::validate::validate_config(scenario_config)
                            .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
                    }
                    sonda_core::ScenarioEntry::Logs(log_config) => {
                        sonda_core::config::validate::validate_log_config(log_config)
                            .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
                    }
                }
            }

            sonda_core::schedule::multi_runner::run_multi(config, Arc::clone(&running))
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        }
    }

    Ok(())
}
