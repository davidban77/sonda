//! sonda — CLI entrypoint.
//!
//! Parses arguments, loads config, validates it, then delegates to the
//! `sonda-core` scenario runner. All signal-generation logic lives in
//! `sonda-core`; this file is pure orchestration.

mod cli;
mod config;
mod status;

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
            status::print_start(&entry, cli.quiet);
            let mut handle = sonda_core::launch_scenario(
                "cli-metrics".to_string(),
                entry,
                Arc::clone(&running),
                None,
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;
            let join_result = handle.join(None);
            status::print_stop(
                &handle.name,
                handle.elapsed(),
                &handle.stats_snapshot(),
                cli.quiet,
            );
            join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args)?;
            let entry = sonda_core::ScenarioEntry::Logs(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;
            status::print_start(&entry, cli.quiet);
            let mut handle = sonda_core::launch_scenario(
                "cli-logs".to_string(),
                entry,
                Arc::clone(&running),
                None,
            )
            .map_err(|e| anyhow::anyhow!("{}", e))?;
            let join_result = handle.join(None);
            status::print_stop(
                &handle.name,
                handle.elapsed(),
                &handle.stats_snapshot(),
                cli.quiet,
            );
            join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Run(ref args) => {
            let config = config::load_multi_config(args)?;

            // Validate each scenario entry before launching any of them.
            for (i, entry) in config.scenarios.iter().enumerate() {
                sonda_core::validate_entry(entry)
                    .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
            }

            // Launch all scenarios and collect handles, respecting phase_offset.
            let mut handles = Vec::with_capacity(config.scenarios.len());
            for (i, entry) in config.scenarios.into_iter().enumerate() {
                // Parse the optional phase_offset into a Duration.
                let start_delay = match entry.phase_offset() {
                    Some(offset) => sonda_core::config::validate::parse_phase_offset(offset)
                        .map_err(|e| anyhow::anyhow!("scenario[{}] phase_offset: {}", i, e))?,
                    None => None,
                };

                status::print_start(&entry, cli.quiet);
                let id = format!("cli-run-{i}");
                let handle =
                    sonda_core::launch_scenario(id, entry, Arc::clone(&running), start_delay)
                        .map_err(|e| anyhow::anyhow!("{}", e))?;
                handles.push(handle);
            }

            // Wait for all scenarios to finish, collecting errors.
            let mut errors: Vec<String> = Vec::new();
            for mut handle in handles {
                if let Err(e) = handle.join(None) {
                    errors.push(e.to_string());
                }
                status::print_stop(
                    &handle.name,
                    handle.elapsed(),
                    &handle.stats_snapshot(),
                    cli.quiet,
                );
            }

            if !errors.is_empty() {
                return Err(anyhow::anyhow!("{}", errors.join("; ")));
            }
        }
    }

    Ok(())
}
