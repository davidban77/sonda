//! sonda — CLI entrypoint.
//!
//! Parses arguments, loads config, validates it, then delegates to the
//! `sonda-core` scenario runner. All signal-generation logic lives in
//! `sonda-core`; this file is pure orchestration.

mod cli;
mod config;
mod import;
mod init;
mod packs;
mod progress;
mod scenarios;
mod status;
mod yaml_helpers;

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

    // Build the pack catalog once for the entire invocation.
    let pack_search_path = packs::build_search_path(cli.pack_path.as_deref());
    let pack_catalog = packs::PackCatalog::discover(&pack_search_path);

    // Build the scenario catalog once for the entire invocation.
    let scenario_search_path = scenarios::build_search_path(cli.scenario_path.as_deref());
    let scenario_catalog = scenarios::ScenarioCatalog::discover(&scenario_search_path);

    match cli.command {
        Commands::Metrics(ref args) => {
            let config = config::load_config(args, &scenario_catalog)?;
            let entry = sonda_core::ScenarioEntry::Metrics(config);

            // prepare_entries handles expansion, validation, and phase offsets.
            let prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            if prepared.len() == 1 {
                let p = prepared.into_iter().next().expect("len checked above");
                run_single_scenario("cli-metrics".to_string(), p, &running, verbosity)?;
            } else {
                // Multi-column expansion — launch all scenarios concurrently.
                launch_and_join_prepared("cli-metrics", prepared, &running, verbosity)?;
            }
        }
        Commands::Logs(ref args) => {
            let config = config::load_log_config(args, &scenario_catalog)?;
            let entry = sonda_core::ScenarioEntry::Logs(config);

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            run_single_scenario("cli-logs".to_string(), p, &running, verbosity)?;
        }
        Commands::Histogram(ref args) => {
            let config = config::load_histogram_config(args, &scenario_catalog)?;
            let entry = sonda_core::ScenarioEntry::Histogram(config);

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            run_single_scenario("cli-histogram".to_string(), p, &running, verbosity)?;
        }
        Commands::Summary(ref args) => {
            let config = config::load_summary_config(args, &scenario_catalog)?;
            let entry = sonda_core::ScenarioEntry::Summary(config);

            let mut prepared =
                sonda_core::prepare_entries(vec![entry]).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            let p = prepared.remove(0);
            run_single_scenario("cli-summary".to_string(), p, &running, verbosity)?;
        }
        Commands::Run(ref args) => {
            // Read the YAML first to detect pack configs before parsing.
            let yaml = config::resolve_scenario_source(&args.scenario, &scenario_catalog)?;

            let entries = if config::is_pack_config(&yaml) {
                config::load_pack_from_yaml(&yaml, &pack_catalog)?
            } else {
                let multi = config::load_multi_config(args, &scenario_catalog)?;
                multi.scenarios
            };

            let prepared =
                sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

            if handle_pre_launch(&prepared, verbosity, cli.dry_run) {
                return Ok(());
            }

            launch_and_join_prepared("cli-run", prepared, &running, verbosity)?;
        }
        Commands::Scenarios(ref args) => {
            run_scenarios_command(args, &cli, verbosity, &running, &scenario_catalog)?;
        }
        Commands::Packs(ref args) => {
            run_packs_command(args, &cli, verbosity, &running, &pack_catalog)?;
        }
        Commands::Import(ref args) => {
            run_import_command(args, &cli, verbosity, &running)?;
        }
        Commands::Init(ref _args) => {
            let result = init::run_init(&pack_catalog)?;
            if result.run_now {
                run_init_scenario(result.yaml, verbosity, &running)?;
            }
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
    catalog: &scenarios::ScenarioCatalog,
) -> anyhow::Result<()> {
    use cli::ScenariosAction;

    match args.action {
        ScenariosAction::List(ref list_args) => {
            let items: Vec<&sonda_core::BuiltinScenario> = match list_args.category {
                Some(ref cat) => catalog.list_by_category(cat),
                None => catalog.list().iter().collect(),
            };

            if items.is_empty() {
                if let Some(ref cat) = list_args.category {
                    eprintln!("no scenarios found in category {:?}", cat);
                } else {
                    eprintln!("no scenarios found (search path has no scenario YAML files)");
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
                            "source": s.source_path.display().to_string(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entries)
                        .expect("JSON serialization of scenario entries cannot fail")
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
                    let cat_padded = format!("{:<18}", s.category);
                    let cat_styled = cat_padded.if_supports_color(Stdout, |t| t.dimmed());
                    let sig_padded = format!("{:<12}", s.signal_type);
                    let sig_styled = sig_padded.if_supports_color(Stdout, |t| t.cyan());
                    println!("{:<28} {cat_styled} {sig_styled} {}", s.name, s.description);
                }
                // Footer: scenario count.
                let count = items.len();
                let noun = if count == 1 { "scenario" } else { "scenarios" };
                let footer = match list_args.category {
                    Some(ref cat) => format!("{count} {noun} in category \"{cat}\""),
                    None => format!("{count} {noun}"),
                };
                let footer = footer.if_supports_color(Stdout, |t| t.dimmed());
                println!("{footer}");
            }
        }
        ScenariosAction::Show(ref show_args) => {
            let scenario = catalog.find(&show_args.name).ok_or_else(|| {
                let names = catalog.available_names();
                let suggestion = find_closest_name(&show_args.name, &names);
                let base_msg = format!(
                    "unknown scenario {:?}; available scenarios: {}",
                    show_args.name,
                    names.join(", ")
                );
                if let Some(closest) = suggestion {
                    anyhow::anyhow!("{base_msg}\n\n  hint: did you mean `{closest}`?")
                } else {
                    anyhow::anyhow!("{}", base_msg)
                }
            })?;
            status::print_show_header(&scenario.name, &scenario.category, &scenario.signal_type);
            let yaml = catalog
                .read_yaml(&show_args.name)
                .expect("scenario must exist after find() succeeded")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to read scenario file {}: {}",
                        scenario.source_path.display(),
                        e
                    )
                })?;
            print!("{yaml}");
        }
        ScenariosAction::Run(ref run_args) => {
            run_builtin_scenario(run_args, cli_opts, verbosity, running, catalog)?;
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
    catalog: &scenarios::ScenarioCatalog,
) -> anyhow::Result<()> {
    let scenario = catalog.find(&args.name).ok_or_else(|| {
        let names = catalog.available_names();
        let suggestion = find_closest_name(&args.name, &names);
        let base_msg = format!(
            "unknown scenario {:?}; available scenarios: {}",
            args.name,
            names.join(", ")
        );
        if let Some(closest) = suggestion {
            anyhow::anyhow!("{base_msg}\n\n  hint: did you mean `{closest}`?")
        } else {
            anyhow::anyhow!("{}", base_msg)
        }
    })?;

    let entries = config::parse_builtin_scenario(scenario, args)?;

    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity, cli_opts.dry_run) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        run_single_scenario(format!("builtin-{}", args.name), p, running, verbosity)?;
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

/// Handle the `packs` subcommand (list, show, run).
fn run_packs_command(
    args: &cli::PacksArgs,
    cli_opts: &Cli,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
    catalog: &packs::PackCatalog,
) -> anyhow::Result<()> {
    use cli::PacksAction;

    match args.action {
        PacksAction::List(ref list_args) => {
            let items: Vec<&packs::PackEntry> = match list_args.category {
                Some(ref cat) => catalog.list_by_category(cat),
                None => catalog.list().iter().collect(),
            };

            if items.is_empty() {
                if let Some(ref cat) = list_args.category {
                    eprintln!("no packs found in category {:?}", cat);
                } else {
                    eprintln!("no packs found (search path has no pack YAML files)");
                }
                return Ok(());
            }

            if list_args.json {
                let entries: Vec<serde_json::Value> = items
                    .iter()
                    .map(|p| {
                        serde_json::json!({
                            "name": p.name,
                            "category": p.category,
                            "metric_count": p.metric_count,
                            "description": p.description,
                            "source": p.source_path.display().to_string(),
                        })
                    })
                    .collect();
                println!(
                    "{}",
                    serde_json::to_string_pretty(&entries)
                        .expect("JSON serialization of pack entries cannot fail")
                );
            } else {
                use owo_colors::Stream::Stdout;

                let header_name = format!("{:<32}", "NAME");
                let header_name = header_name.if_supports_color(Stdout, |t| t.bold());
                let header_cat = format!("{:<18}", "CATEGORY");
                let header_cat = header_cat.if_supports_color(Stdout, |t| t.bold());
                let header_count = format!("{:<10}", "METRICS");
                let header_count = header_count.if_supports_color(Stdout, |t| t.bold());
                let header_desc = format!("{:<40}", "DESCRIPTION");
                let header_desc = header_desc.if_supports_color(Stdout, |t| t.bold());
                let header_source = "SOURCE".if_supports_color(Stdout, |t| t.bold());
                println!("{header_name} {header_cat} {header_count} {header_desc} {header_source}");
                for p in &items {
                    let cat_padded = format!("{:<18}", p.category);
                    let cat_styled = cat_padded.if_supports_color(Stdout, |t| t.dimmed());
                    let count_padded = format!("{:<10}", p.metric_count);
                    let count_styled = count_padded.if_supports_color(Stdout, |t| t.cyan());
                    let desc_padded = format!("{:<40}", p.description);
                    let source = format!("{}", p.source_path.display());
                    let source_styled = source.if_supports_color(Stdout, |t| t.dimmed());
                    println!(
                        "{:<32} {cat_styled} {count_styled} {desc_padded} {source_styled}",
                        p.name
                    );
                }
                let count = items.len();
                let noun = if count == 1 { "pack" } else { "packs" };
                let footer = match list_args.category {
                    Some(ref cat) => format!("{count} {noun} in category \"{cat}\""),
                    None => format!("{count} {noun}"),
                };
                let footer = footer.if_supports_color(Stdout, |t| t.dimmed());
                println!("{footer}");
            }
        }
        PacksAction::Show(ref show_args) => {
            let entry = catalog.find(&show_args.name).ok_or_else(|| {
                let names = catalog.available_names();
                let suggestion = find_closest_name(&show_args.name, &names);
                let base_msg = format!(
                    "unknown pack {:?}; available packs: {}",
                    show_args.name,
                    names.join(", ")
                );
                if let Some(closest) = suggestion {
                    anyhow::anyhow!("{base_msg}\n\n  hint: did you mean `{closest}`?")
                } else {
                    anyhow::anyhow!("{}", base_msg)
                }
            })?;
            status::print_show_header(&entry.name, &entry.category, "pack");
            let yaml = catalog
                .read_yaml(&show_args.name)
                .expect("pack must exist after find() succeeded")
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to read pack file {}: {}",
                        entry.source_path.display(),
                        e
                    )
                })?;
            print!("{yaml}");
        }
        PacksAction::Run(ref run_args) => {
            run_pack(run_args, cli_opts, verbosity, running, catalog)?;
        }
    }

    Ok(())
}

/// Execute a metric pack from the catalog, applying optional CLI overrides.
fn run_pack(
    args: &cli::PacksRunArgs,
    cli_opts: &Cli,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
    catalog: &packs::PackCatalog,
) -> anyhow::Result<()> {
    let entries = config::load_pack_from_catalog(args, catalog)?;

    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity, cli_opts.dry_run) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        run_single_scenario(format!("pack-{}", args.name), p, running, verbosity)?;
    } else {
        launch_and_join_prepared(&format!("pack-{}", args.name), prepared, running, verbosity)?;
    }

    Ok(())
}

/// Handle the `import` subcommand: analyze, generate, or run from CSV.
fn run_import_command(
    args: &cli::ImportArgs,
    cli_opts: &Cli,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let columns = import::parse_column_list(args.columns.as_deref())?;
    let column_slice = columns.as_deref();

    if args.analyze {
        // Read-only analysis: print detected patterns, no side effects.
        import::run_analyze(&args.file, column_slice)?;
    } else if let Some(ref output) = args.output {
        // Generate scenario YAML and write to file.
        import::run_generate(&args.file, output, column_slice, args.rate, &args.duration)?;
    } else if args.run {
        // Generate scenario YAML and immediately execute it.
        let yaml =
            import::run_generate_and_execute(&args.file, column_slice, args.rate, &args.duration)?;

        // Parse the generated YAML as a scenario and run it.
        let entries: Vec<sonda_core::ScenarioEntry> = if yaml.contains("scenarios:") {
            let multi: sonda_core::MultiScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .map_err(|e| anyhow::anyhow!("generated YAML is invalid: {e}"))?;
            multi.scenarios
        } else {
            let config: sonda_core::config::ScenarioConfig = serde_yaml_ng::from_str(&yaml)
                .map_err(|e| anyhow::anyhow!("generated YAML is invalid: {e}"))?;
            vec![sonda_core::ScenarioEntry::Metrics(config)]
        };

        let prepared =
            sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

        if handle_pre_launch(&prepared, verbosity, cli_opts.dry_run) {
            return Ok(());
        }

        if prepared.len() == 1 {
            let p = prepared.into_iter().next().expect("len checked above");
            run_single_scenario("csv-import".to_string(), p, running, verbosity)?;
        } else {
            launch_and_join_prepared("csv-import", prepared, running, verbosity)?;
        }
    } else {
        // No mode specified — this is a user error.
        anyhow::bail!(
            "specify one of --analyze, -o <output.yaml>, or --run.\n\
             Use `sonda import --help` for usage."
        );
    }

    Ok(())
}

/// Execute a scenario from YAML generated by `sonda init`.
///
/// Parses the YAML and runs it using the same approach as `sonda import --run`.
/// Supports single-scenario configs, multi-scenario configs, and pack configs.
fn run_init_scenario(
    yaml: String,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    eprintln!(
        "  {}",
        "Running scenario...".if_supports_color(Stderr, |t| t.dimmed())
    );

    // Detect what kind of YAML we generated and parse accordingly.
    let entries: Vec<sonda_core::ScenarioEntry> = if yaml.contains("pack:") {
        // Pack-based scenario — expand via the pack catalog.
        let pack_search_path = crate::packs::build_search_path(None);
        let pack_catalog = crate::packs::PackCatalog::discover(&pack_search_path);

        crate::config::load_pack_from_yaml(&yaml, &pack_catalog)?
    } else if yaml.contains("scenarios:") {
        let multi: sonda_core::MultiScenarioConfig = serde_yaml_ng::from_str(&yaml)
            .map_err(|e| anyhow::anyhow!("generated YAML is invalid: {e}"))?;
        multi.scenarios
    } else if yaml.contains("signal_type: logs") {
        let config: sonda_core::config::LogScenarioConfig = serde_yaml_ng::from_str(&yaml)
            .map_err(|e| anyhow::anyhow!("generated YAML is invalid: {e}"))?;
        vec![sonda_core::ScenarioEntry::Logs(config)]
    } else {
        let config: sonda_core::config::ScenarioConfig = serde_yaml_ng::from_str(&yaml)
            .map_err(|e| anyhow::anyhow!("generated YAML is invalid: {e}"))?;
        vec![sonda_core::ScenarioEntry::Metrics(config)]
    };

    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity, false) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        run_single_scenario("init".to_string(), p, running, verbosity)?;
    } else {
        launch_and_join_prepared("init", prepared, running, verbosity)?;
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

    let total = prepared.len();

    if dry_run {
        for (i, p) in prepared.iter().enumerate() {
            status::print_config(&p.entry, i + 1, total);
        }
        status::print_dry_run_ok(total);
        return true;
    }

    if verbosity == Verbosity::Verbose {
        for (i, p) in prepared.iter().enumerate() {
            status::print_config(&p.entry, i + 1, total);
        }
    }

    false
}

/// Run a single prepared scenario with progress tracking.
///
/// Encapsulates the full single-scenario lifecycle: print start banner, launch
/// the scenario, show live progress, join the thread, stop progress, print the
/// stop banner, and propagate any runner error.
fn run_single_scenario(
    name: String,
    prepared: PreparedEntry,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    status::print_start(&prepared.entry, verbosity, None);
    let mut handle = sonda_core::launch_scenario(
        name,
        prepared.entry,
        Arc::clone(running),
        prepared.start_delay,
    )
    .map_err(|e| anyhow::anyhow!("{}", e))?;
    let progress = maybe_start_progress(&handle, verbosity);
    let join_result = handle.join(None);
    if let Some(p) = progress {
        p.stop();
    }
    status::print_stop(
        &handle.name,
        handle.elapsed(),
        &handle.stats_snapshot(),
        verbosity,
        None,
    );
    join_result.map_err(|e| anyhow::anyhow!("{}", e))?;
    Ok(())
}

/// Start a progress display for a single running scenario handle.
///
/// Returns `None` when verbosity is [`Verbosity::Quiet`], so progress output
/// is suppressed entirely in quiet mode.
fn maybe_start_progress(
    handle: &sonda_core::ScenarioHandle,
    verbosity: Verbosity,
) -> Option<progress::ProgressDisplay> {
    if verbosity == Verbosity::Quiet {
        return None;
    }
    Some(progress::ProgressDisplay::start(vec![(
        handle.name.clone(),
        Arc::clone(&handle.stats),
        handle.target_rate,
    )]))
}

/// Start a progress display for multiple running scenario handles.
///
/// Returns `None` when verbosity is [`Verbosity::Quiet`].
fn maybe_start_progress_multi(
    handles: &[sonda_core::ScenarioHandle],
    verbosity: Verbosity,
) -> Option<progress::ProgressDisplay> {
    if verbosity == Verbosity::Quiet {
        return None;
    }
    let scenarios: Vec<_> = handles
        .iter()
        .map(|h| (h.name.clone(), Arc::clone(&h.stats), h.target_rate))
        .collect();
    Some(progress::ProgressDisplay::start(scenarios))
}

/// Launch multiple prepared scenarios concurrently, join them, print
/// per-scenario stop banners in launch order, print an aggregate summary,
/// and return an error if any failed.
///
/// Each entry is launched via `sonda_core::launch_scenario` with its
/// pre-resolved `start_delay` from [`PreparedEntry`]. Stop banners are
/// collected and printed in launch order after all scenarios complete.
fn launch_and_join_prepared(
    id_prefix: &str,
    prepared: Vec<PreparedEntry>,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    let run_start = Instant::now();
    let scenario_count = prepared.len();
    let mut handles = Vec::with_capacity(scenario_count);

    for (i, p) in prepared.into_iter().enumerate() {
        let position = Some((i + 1, scenario_count));
        status::print_start(&p.entry, verbosity, position);
        let id = format!("{id_prefix}-{i}");
        let handle = sonda_core::launch_scenario(id, p.entry, Arc::clone(running), p.start_delay)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        handles.push(handle);
    }

    // Start progress display for all launched scenarios.
    let progress = maybe_start_progress_multi(&handles, verbosity);

    // Collect results from all handles first, preserving launch order.
    let mut errors: Vec<String> = Vec::new();
    let mut total_events: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_errors: u64 = 0;

    struct StopInfo {
        name: String,
        elapsed: std::time::Duration,
        stats: sonda_core::schedule::stats::ScenarioStats,
    }

    let mut stop_infos: Vec<StopInfo> = Vec::with_capacity(scenario_count);

    for mut handle in handles {
        if let Err(e) = handle.join(None) {
            errors.push(e.to_string());
        }
        let stats = handle.stats_snapshot();
        let info = StopInfo {
            name: handle.name.clone(),
            elapsed: handle.elapsed(),
            stats,
        };
        total_events += info.stats.total_events;
        total_bytes += info.stats.bytes_emitted;
        total_errors += info.stats.errors;
        stop_infos.push(info);
    }

    // Stop progress display before printing stop banners.
    if let Some(p) = progress {
        p.stop();
    }

    // Print stop banners in launch order.
    for (i, info) in stop_infos.iter().enumerate() {
        let position = Some((i + 1, scenario_count));
        status::print_stop(&info.name, info.elapsed, &info.stats, verbosity, position);
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

/// Find the closest matching name from a list of candidates.
///
/// Uses simple Levenshtein edit distance. Returns `Some(name)` if the best
/// match has an edit distance of 3 or less, or if the query is a substring
/// of a candidate (or vice versa). Returns `None` if no close match is found.
fn find_closest_name<'a>(query: &str, candidates: &[&'a str]) -> Option<&'a str> {
    let query_lower = query.to_lowercase();

    // First, try substring matching (skip very short queries to avoid false positives).
    if query_lower.len() >= 3 {
        for &name in candidates {
            let name_lower = name.to_lowercase();
            if name_lower.contains(&query_lower) || query_lower.contains(&name_lower) {
                return Some(name);
            }
        }
    }

    // Fall back to edit distance.
    let mut best: Option<(&str, usize)> = None;
    for &name in candidates {
        let dist = edit_distance(&query_lower, &name.to_lowercase());
        match best {
            Some((_, best_dist)) if dist < best_dist => best = Some((name, dist)),
            None => best = Some((name, dist)),
            _ => {}
        }
    }

    best.filter(|(_, dist)| *dist <= 3).map(|(name, _)| name)
}

/// Compute the Levenshtein edit distance between two strings.
fn edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // edit_distance: correctness
    // -----------------------------------------------------------------------

    #[test]
    fn edit_distance_identical_strings() {
        assert_eq!(edit_distance("abc", "abc"), 0);
    }

    #[test]
    fn edit_distance_empty_strings() {
        assert_eq!(edit_distance("", ""), 0);
    }

    #[test]
    fn edit_distance_one_empty() {
        assert_eq!(edit_distance("abc", ""), 3);
        assert_eq!(edit_distance("", "abc"), 3);
    }

    #[test]
    fn edit_distance_single_substitution() {
        assert_eq!(edit_distance("cat", "bat"), 1);
    }

    #[test]
    fn edit_distance_single_insertion() {
        assert_eq!(edit_distance("abc", "abcd"), 1);
    }

    #[test]
    fn edit_distance_single_deletion() {
        assert_eq!(edit_distance("abcd", "abc"), 1);
    }

    #[test]
    fn edit_distance_completely_different() {
        assert_eq!(edit_distance("abc", "xyz"), 3);
    }

    // -----------------------------------------------------------------------
    // find_closest_name: suggestions
    // -----------------------------------------------------------------------

    #[test]
    fn find_closest_name_exact_match() {
        let candidates = vec!["cpu-spike", "memory-leak", "disk-full"];
        assert_eq!(
            find_closest_name("cpu-spike", &candidates),
            Some("cpu-spike")
        );
    }

    #[test]
    fn find_closest_name_substring_match() {
        let candidates = vec!["cpu-spike", "memory-leak", "disk-full"];
        assert_eq!(find_closest_name("cpu", &candidates), Some("cpu-spike"));
    }

    #[test]
    fn find_closest_name_close_typo() {
        let candidates = vec!["cpu-spike", "memory-leak", "disk-full"];
        // "cpu-spke" is 1 edit away from "cpu-spike"
        assert_eq!(
            find_closest_name("cpu-spke", &candidates),
            Some("cpu-spike")
        );
    }

    #[test]
    fn find_closest_name_no_close_match() {
        let candidates = vec!["cpu-spike", "memory-leak"];
        // "zzzzzzzzz" is too far from anything
        assert_eq!(find_closest_name("zzzzzzzzz", &candidates), None);
    }

    #[test]
    fn find_closest_name_empty_candidates() {
        let candidates: Vec<&str> = vec![];
        assert_eq!(find_closest_name("cpu-spike", &candidates), None);
    }

    #[test]
    fn find_closest_name_case_insensitive_substring() {
        let candidates = vec!["cpu-spike", "memory-leak"];
        assert_eq!(find_closest_name("CPU", &candidates), Some("cpu-spike"));
    }
}
