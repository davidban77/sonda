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
use std::time::Instant;

use clap::Parser;

use cli::{Cli, Commands, Verbosity};
use sonda_core::ScenarioEntry;

fn main() {
    if let Err(err) = run() {
        eprintln!("error: {err:#}");
        process::exit(1);
    }
}

/// Top-level orchestration: parse -> load -> validate -> run.
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
    let verbosity = Verbosity::from_flags(cli.quiet, cli.verbose);

    match cli.command {
        Commands::Metrics(ref args) => {
            let config = config::load_config(args)?;
            // Expand multi-column csv_replay into N independent scenarios.
            let expanded =
                sonda_core::expand_scenario(config).map_err(|e| anyhow::anyhow!("{}", e))?;
            let entries: Vec<sonda_core::ScenarioEntry> = expanded
                .into_iter()
                .map(sonda_core::ScenarioEntry::Metrics)
                .collect();

            for (i, entry) in entries.iter().enumerate() {
                sonda_core::validate_entry(entry)
                    .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
            }

            if cli.dry_run {
                for entry in &entries {
                    status::print_config(entry);
                }
                status::print_dry_run_ok();
                return Ok(());
            }

            if verbosity == Verbosity::Verbose {
                for entry in &entries {
                    status::print_config(entry);
                }
            }

            if entries.len() == 1 {
                // Single scenario — original code path.
                let entry = entries.into_iter().next().expect("len checked above");
                status::print_start(&entry, verbosity);
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
                    verbosity,
                );
                join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
            } else {
                // Multi-column expansion — launch all scenarios concurrently.
                launch_and_join_scenarios("cli-metrics", entries, &running, verbosity, |_, _| {
                    Ok(None)
                })?;
            }
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args)?;
            let entry = sonda_core::ScenarioEntry::Logs(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;

            if cli.dry_run {
                status::print_config(&entry);
                status::print_dry_run_ok();
                return Ok(());
            }

            if verbosity == Verbosity::Verbose {
                status::print_config(&entry);
            }

            status::print_start(&entry, verbosity);
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
                verbosity,
            );
            join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Histogram(ref args) => {
            let config = config::load_histogram_config(args)?;
            let entry = sonda_core::ScenarioEntry::Histogram(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;

            if cli.dry_run {
                status::print_config(&entry);
                status::print_dry_run_ok();
                return Ok(());
            }

            if verbosity == Verbosity::Verbose {
                status::print_config(&entry);
            }

            status::print_start(&entry, verbosity);
            let mut handle = sonda_core::launch_scenario(
                "cli-histogram".to_string(),
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
                verbosity,
            );
            join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Summary(ref args) => {
            let config = config::load_summary_config(args)?;
            let entry = sonda_core::ScenarioEntry::Summary(config);
            sonda_core::validate_entry(&entry).map_err(|e| anyhow::anyhow!("{}", e))?;

            if cli.dry_run {
                status::print_config(&entry);
                status::print_dry_run_ok();
                return Ok(());
            }

            if verbosity == Verbosity::Verbose {
                status::print_config(&entry);
            }

            status::print_start(&entry, verbosity);
            let mut handle = sonda_core::launch_scenario(
                "cli-summary".to_string(),
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
                verbosity,
            );
            join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        Commands::Run(ref args) => {
            let config = config::load_multi_config(args)?;

            // Expand multi-column csv_replay entries into N independent scenarios.
            let mut expanded_scenarios: Vec<sonda_core::ScenarioEntry> = Vec::new();
            for entry in config.scenarios {
                let entries =
                    sonda_core::expand_entry(entry).map_err(|e| anyhow::anyhow!("{}", e))?;
                expanded_scenarios.extend(entries);
            }
            let config = sonda_core::MultiScenarioConfig {
                scenarios: expanded_scenarios,
            };

            // Validate each scenario entry before launching any of them.
            for (i, entry) in config.scenarios.iter().enumerate() {
                sonda_core::validate_entry(entry)
                    .map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
            }

            if cli.dry_run {
                for entry in &config.scenarios {
                    status::print_config(entry);
                }
                status::print_dry_run_ok();
                return Ok(());
            }

            if verbosity == Verbosity::Verbose {
                for entry in &config.scenarios {
                    status::print_config(entry);
                }
            }

            launch_and_join_scenarios(
                "cli-run",
                config.scenarios,
                &running,
                verbosity,
                |i, entry| match entry.phase_offset() {
                    Some(offset) => sonda_core::config::validate::parse_phase_offset(offset)
                        .map_err(|e| anyhow::anyhow!("scenario[{}] phase_offset: {}", i, e)),
                    None => Ok(None),
                },
            )?;
        }
        Commands::Scenarios(ref args) => {
            run_scenarios_command(args, &cli, verbosity, &running)?;
        }
    }

    Ok(())
}

/// Handle the `scenarios` subcommand (list, show, run).
fn run_scenarios_command(
    args: &cli::ScenariosArgs,
    cli_opts: &Cli,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use cli::ScenariosAction;
    use sonda_core::scenarios;

    match args.action {
        ScenariosAction::List(ref list_args) => {
            let items: Vec<&sonda_core::BuiltinScenario> = match list_args.category {
                Some(ref cat) => scenarios::list_by_category(cat),
                None => scenarios::list().iter().collect(),
            };

            if items.is_empty() {
                if let Some(ref cat) = list_args.category {
                    eprintln!("no scenarios found in category {:?}", cat);
                } else {
                    eprintln!("no built-in scenarios available");
                }
                return Ok(());
            }

            // Print a formatted table to stdout.
            let header_name = "NAME";
            let header_cat = "CATEGORY";
            let header_sig = "SIGNAL";
            let header_desc = "DESCRIPTION";
            println!(
                "{:<28} {:<18} {:<12} {}",
                header_name, header_cat, header_sig, header_desc
            );
            for s in &items {
                println!(
                    "{:<28} {:<18} {:<12} {}",
                    s.name, s.category, s.signal_type, s.description
                );
            }
        }
        ScenariosAction::Show(ref show_args) => {
            let yaml = scenarios::get_yaml(&show_args.name).ok_or_else(|| {
                let names = scenarios::available_names();
                anyhow::anyhow!(
                    "unknown scenario {:?}; available scenarios: {}",
                    show_args.name,
                    names.join(", ")
                )
            })?;
            print!("{yaml}");
        }
        ScenariosAction::Run(ref run_args) => {
            run_builtin_scenario(run_args, cli_opts, verbosity, running)?;
        }
    }

    Ok(())
}

/// Execute a built-in scenario, applying optional overrides.
fn run_builtin_scenario(
    args: &cli::ScenariosRunArgs,
    cli_opts: &Cli,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    use sonda_core::scenarios;

    let scenario = scenarios::get(&args.name).ok_or_else(|| {
        let names = scenarios::available_names();
        anyhow::anyhow!(
            "unknown scenario {:?}; available scenarios: {}",
            args.name,
            names.join(", ")
        )
    })?;

    let entries = config::parse_builtin_scenario(scenario, args)?;

    for (i, entry) in entries.iter().enumerate() {
        sonda_core::validate_entry(entry).map_err(|e| anyhow::anyhow!("scenario[{}]: {}", i, e))?;
    }

    if cli_opts.dry_run {
        for entry in &entries {
            status::print_config(entry);
        }
        status::print_dry_run_ok();
        return Ok(());
    }

    if verbosity == Verbosity::Verbose {
        for entry in &entries {
            status::print_config(entry);
        }
    }

    if entries.len() == 1 {
        let entry = entries.into_iter().next().expect("len checked above");
        status::print_start(&entry, verbosity);
        let mut handle = sonda_core::launch_scenario(
            format!("builtin-{}", args.name),
            entry,
            Arc::clone(running),
            None,
        )
        .map_err(|e| anyhow::anyhow!("{}", e))?;
        let join_result = handle.join(None);
        status::print_stop(
            &handle.name,
            handle.elapsed(),
            &handle.stats_snapshot(),
            verbosity,
        );
        join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
    } else {
        launch_and_join_scenarios(
            &format!("builtin-{}", args.name),
            entries,
            running,
            verbosity,
            |i, entry| match entry.phase_offset() {
                Some(offset) => sonda_core::config::validate::parse_phase_offset(offset)
                    .map_err(|e| anyhow::anyhow!("scenario[{}] phase_offset: {}", i, e)),
                None => Ok(None),
            },
        )?;
    }

    Ok(())
}

/// Launch multiple scenarios concurrently, join them, print per-scenario stop
/// banners, print an aggregate summary, and return an error if any failed.
///
/// Each entry is launched via `sonda_core::launch_scenario`. The optional
/// `start_delay_fn` computes a per-entry start delay (used by the `run`
/// subcommand for `phase_offset`); return `Ok(None)` for no delay.
fn launch_and_join_scenarios(
    id_prefix: &str,
    entries: Vec<ScenarioEntry>,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
    start_delay_fn: impl Fn(usize, &ScenarioEntry) -> anyhow::Result<Option<std::time::Duration>>,
) -> anyhow::Result<()> {
    let run_start = Instant::now();
    let mut handles = Vec::with_capacity(entries.len());

    for (i, entry) in entries.into_iter().enumerate() {
        let start_delay = start_delay_fn(i, &entry)?;
        status::print_start(&entry, verbosity);
        let id = format!("{id_prefix}-{i}");
        let handle = sonda_core::launch_scenario(id, entry, Arc::clone(running), start_delay)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        handles.push(handle);
    }

    let mut errors: Vec<String> = Vec::new();
    let mut total_events: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_errors: u64 = 0;
    let scenario_count = handles.len();

    for mut handle in handles {
        if let Err(e) = handle.join(None) {
            errors.push(e.to_string());
        }
        let stats = handle.stats_snapshot();
        status::print_stop(&handle.name, handle.elapsed(), &stats, verbosity);
        total_events += stats.total_events;
        total_bytes += stats.bytes_emitted;
        total_errors += stats.errors;
    }

    let total_elapsed = run_start.elapsed();
    let agg = status::AggregateStats {
        scenario_count,
        total_events,
        total_bytes,
        total_errors,
    };
    status::print_summary(&agg, total_elapsed, verbosity);

    if !errors.is_empty() {
        return Err(anyhow::anyhow!("{}", errors.join("; ")));
    }

    Ok(())
}
