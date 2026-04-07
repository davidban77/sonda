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
use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use cli::{Cli, Commands, Verbosity};
use sonda_core::PreparedEntry;

fn main() {
    if let Err(err) = run() {
        let style = owo_colors::Style::new().bold().red();
        eprintln!(
            "{} {err:#}",
            "error:".if_supports_color(Stderr, |t| t.style(style))
        );
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
            let entry = sonda_core::ScenarioEntry::Metrics(config);

            // prepare_entries handles expansion, validation, and phase offsets.
            let prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            if prepared.len() == 1 {
                // Single scenario — original code path.
                let p = prepared.into_iter().next().expect("len checked above");
                status::print_start(&p.entry, verbosity);
                let mut handle = sonda_core::launch_scenario(
                    "cli-metrics".to_string(),
                    p.entry,
                    Arc::clone(&running),
                    p.start_delay,
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
                launch_and_join_prepared("cli-metrics", prepared, &running, verbosity)?;
            }
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args)?;
            let entry = sonda_core::ScenarioEntry::Logs(config);

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            status::print_start(&p.entry, verbosity);
            let mut handle = sonda_core::launch_scenario(
                "cli-logs".to_string(),
                p.entry,
                Arc::clone(&running),
                p.start_delay,
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

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            status::print_start(&p.entry, verbosity);
            let mut handle = sonda_core::launch_scenario(
                "cli-histogram".to_string(),
                p.entry,
                Arc::clone(&running),
                p.start_delay,
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

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            status::print_start(&p.entry, verbosity);
            let mut handle = sonda_core::launch_scenario(
                "cli-summary".to_string(),
                p.entry,
                Arc::clone(&running),
                p.start_delay,
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

            let prepared = sonda_core::prepare_entries(config.scenarios)
                .map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            launch_and_join_prepared("cli-run", prepared, &running, verbosity)?;
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

            if list_args.json {
                // JSON array output to stdout.
                let entries: Vec<serde_json::Value> = items
                    .iter()
                    .map(|s| {
                        serde_json::json!({
                            "name": s.name,
                            "category": s.category,
                            "signal_type": s.signal_type,
                            "description": s.description,
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entries)
                        .expect("JSON serialization of builtin scenarios cannot fail")
                );
            } else {
                // Print a formatted table to stdout with a bold header row.
                // Pad plain text first, then apply bold — ANSI escape bytes
                // would be counted as visible characters by `format!`, breaking
                // column alignment.
                use owo_colors::Stream::Stdout;

                let header_name = format!("{:<28}", "NAME");
                let header_name = header_name.if_supports_color(Stdout, |t| t.bold());
                let header_cat = format!("{:<18}", "CATEGORY");
                let header_cat = header_cat.if_supports_color(Stdout, |t| t.bold());
                let header_sig = format!("{:<12}", "SIGNAL");
                let header_sig = header_sig.if_supports_color(Stdout, |t| t.bold());
                let header_desc = "DESCRIPTION".if_supports_color(Stdout, |t| t.bold());
                println!("{header_name} {header_cat} {header_sig} {header_desc}");
                for s in &items {
                    println!(
                        "{:<28} {:<18} {:<12} {}",
                        s.name, s.category, s.signal_type, s.description
                    );
                }
            }
        }
        ScenariosAction::Show(ref show_args) => {
            let scenario = scenarios::get(&show_args.name).ok_or_else(|| {
                let names = scenarios::available_names();
                anyhow::anyhow!(
                    "unknown scenario {:?}; available scenarios: {}",
                    show_args.name,
                    names.join(", ")
                )
            })?;
            status::print_show_header(scenario.name, scenario.category, scenario.signal_type);
            let yaml = scenarios::get_yaml(&show_args.name)
                .expect("scenario must exist after get() succeeded");
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

    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity, cli_opts.dry_run) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        status::print_start(&p.entry, verbosity);
        let mut handle = sonda_core::launch_scenario(
            format!("builtin-{}", args.name),
            p.entry,
            Arc::clone(running),
            p.start_delay,
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
        launch_and_join_prepared(
            &format!("builtin-{}", args.name),
            prepared,
            running,
            verbosity,
        )?;
    }

    Ok(())
}

/// Handle verbose version display, dry-run config printing, and verbose config display.
///
/// Returns `true` if dry-run mode was active (caller should return early).
fn handle_pre_launch(prepared: &[PreparedEntry], verbosity: Verbosity, dry_run: bool) -> bool {
    if verbosity == Verbosity::Verbose {
        status::print_version();
    }

    if dry_run {
        for p in prepared {
            status::print_config(&p.entry);
        }
        status::print_dry_run_ok(prepared.len());
        return true;
    }

    if verbosity == Verbosity::Verbose {
        for p in prepared {
            status::print_config(&p.entry);
        }
    }

    false
}

/// Launch multiple prepared scenarios concurrently, join them, print
/// per-scenario stop banners, print an aggregate summary, and return an error
/// if any failed.
///
/// Each entry is launched via `sonda_core::launch_scenario` with its
/// pre-resolved `start_delay` from [`PreparedEntry`].
fn launch_and_join_prepared(
    id_prefix: &str,
    prepared: Vec<PreparedEntry>,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    let run_start = Instant::now();
    let mut handles = Vec::with_capacity(prepared.len());

    for (i, p) in prepared.into_iter().enumerate() {
        status::print_start(&p.entry, verbosity);
        let id = format!("{id_prefix}-{i}");
        let handle = sonda_core::launch_scenario(id, p.entry, Arc::clone(running), p.start_delay)
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
