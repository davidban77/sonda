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
            let entry = sonda_core::ScenarioEntry::Metrics(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;
            let mut handle =
                sonda_core::launch_scenario("cli-metrics".to_string(), entry, Arc::clone(&running))
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
            handle.join(None).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args)?;
            let entry = sonda_core::ScenarioEntry::Logs(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;
            let mut handle =
                sonda_core::launch_scenario("cli-logs".to_string(), entry, Arc::clone(&running))
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
            handle.join(None).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Run(ref args) => {
            let config = config::load_multi_config(args)?;

            // Validate each scenario entry before launching any of them.
            for (i, entry) in config.scenarios.iter().enumerate() {
                sonda_core::validate_entry(entry)
                    .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
            }

            // Launch all scenarios and collect handles.
            let mut handles = Vec::with_capacity(config.scenarios.len());
            for (i, entry) in config.scenarios.into_iter().enumerate() {
                let id = format!("cli-run-{i}");
                let handle = sonda_core::launch_scenario(id, entry, Arc::clone(&running))
                    .map_err(|e| anyhow::anyhow!("{}", e))?;
                handles.push(handle);
            }

            // Wait for all scenarios to finish, collecting errors.
            let mut errors: Vec<String> = Vec::new();
            for mut handle in handles {
                if let Err(e) = handle.join(None) {
                    errors.push(e.to_string());
                }
            }

            if !errors.is_empty() {
                return Err(anyhow::anyhow!("{}", errors.join("; ")));
            }
        }
    }

    Ok(())
}
