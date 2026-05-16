//! sonda CLI entrypoint.

mod catalog_dir;
mod cli;
mod config;
mod dry_run;
mod new;
mod progress;
mod scenario_loader;
mod sink_format;
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

fn run() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_target(false)
        .without_time()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

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
    let catalog = cli.catalog.as_deref();

    match cli.command {
        Commands::Run(ref args) => run_scenario(args, &cli, catalog, verbosity, &running)?,
        Commands::List(ref args) => list_catalog(args, catalog)?,
        Commands::Show(ref args) => show_entry(args, catalog)?,
        Commands::New(ref args) => new::run(args)?,
    }

    Ok(())
}

fn run_scenario(
    args: &cli::RunArgs,
    cli_opts: &Cli,
    catalog: Option<&std::path::Path>,
    verbosity: Verbosity,
    running: &Arc<AtomicBool>,
) -> anyhow::Result<()> {
    let mut compiled = scenario_loader::load_scenario_compiled(&args.scenario, catalog)?;
    config::apply_run_overrides_compiled(&mut compiled, args)?;
    let has_gates = scenario_loader::has_while_clause(&compiled);

    if cli_opts.dry_run {
        let format = dry_run::parse_format(cli_opts.format.as_deref())?;
        dry_run::print_dry_run_compiled(&args.scenario, &compiled, format)?;
        return Ok(());
    }

    if has_gates {
        if verbosity == Verbosity::Verbose {
            status::print_version();
        }
        run_compiled_with_progress(compiled, running, verbosity)?;
        return Ok(());
    }

    let entries =
        sonda_core::compiler::prepare::prepare(compiled).map_err(|e| anyhow::anyhow!("{}", e))?;
    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity, cli_opts.dry_run) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        run_single_scenario("cli-run".to_string(), p, running, verbosity)?;
    } else {
        launch_and_join_prepared("cli-run", prepared, running, verbosity)?;
    }
    Ok(())
}

fn list_catalog(args: &cli::ListArgs, catalog: Option<&std::path::Path>) -> anyhow::Result<()> {
    let dir =
        catalog.ok_or_else(|| anyhow::anyhow!("--catalog <dir> is required for `sonda list`"))?;
    let kind_filter = match args.kind.as_deref() {
        None => None,
        Some("runnable") => Some(catalog_dir::EntryKind::Runnable),
        Some("composable") => Some(catalog_dir::EntryKind::Composable),
        Some(other) => {
            anyhow::bail!("unknown --kind {other:?}: expected 'runnable' or 'composable'")
        }
    };

    let mut entries = catalog_dir::enumerate(dir)?;
    if let Some(k) = kind_filter {
        entries.retain(|e| e.kind == k);
    }
    if let Some(ref tag) = args.tag {
        entries.retain(|e| e.tags.iter().any(|t| t == tag));
    }

    if args.json {
        let dto: Vec<serde_json::Value> = entries
            .iter()
            .map(|e| {
                serde_json::json!({
                    "name": e.name,
                    "kind": e.kind.as_str(),
                    "description": e.description,
                    "tags": e.tags,
                    "source": e.source_path.display().to_string(),
                })
            })
            .collect();
        let out = serde_json::to_string_pretty(&dto)
            .expect("JSON serialization of catalog entries cannot fail");
        println!("{out}");
    } else {
        println!("KIND\tNAME\tTAGS\tDESCRIPTION");
        for e in &entries {
            let tags = e.tags.join(",");
            println!(
                "{}\t{}\t{}\t{}",
                e.kind.as_str(),
                e.name,
                tags,
                e.description
            );
        }
    }
    Ok(())
}

fn show_entry(args: &cli::ShowArgs, catalog: Option<&std::path::Path>) -> anyhow::Result<()> {
    let name = args.name.strip_prefix('@').unwrap_or(args.name.as_str());
    let dir =
        catalog.ok_or_else(|| anyhow::anyhow!("--catalog <dir> is required for `sonda show`"))?;
    let entries = catalog_dir::enumerate(dir)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown catalog entry {:?}", name))?;
    let raw = std::fs::read_to_string(&entry.source_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", entry.source_path.display()))?;
    print!("{raw}");
    Ok(())
}

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
        Arc::clone(&handle.alive),
    )]))
}

fn maybe_start_progress_multi(
    handles: &[sonda_core::ScenarioHandle],
    verbosity: Verbosity,
) -> Option<progress::ProgressDisplay> {
    if verbosity == Verbosity::Quiet {
        return None;
    }
    let scenarios: Vec<_> = handles
        .iter()
        .map(|h| {
            (
                h.name.clone(),
                Arc::clone(&h.stats),
                h.target_rate,
                Arc::clone(&h.alive),
            )
        })
        .collect();
    Some(progress::ProgressDisplay::start(scenarios))
}

struct StopInfo {
    name: String,
    elapsed: std::time::Duration,
    stats: sonda_core::schedule::stats::ScenarioStats,
}

fn launch_and_join_prepared(
    id_prefix: &str,
    prepared: Vec<PreparedEntry>,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    let run_start = Instant::now();
    let scenario_count = prepared.len();
    let mut handles = Vec::with_capacity(scenario_count);
    let mut clock_groups: Vec<(Option<String>, Option<bool>)> = Vec::with_capacity(scenario_count);

    for (i, p) in prepared.into_iter().enumerate() {
        let position = Some((i + 1, scenario_count));
        status::print_start(&p.entry, verbosity, position);
        clock_groups.push((
            p.entry.clock_group().map(|s| s.to_string()),
            p.entry.clock_group_is_auto(),
        ));
        let id = format!("{id_prefix}-{i}");
        let handle = sonda_core::launch_scenario(id, p.entry, Arc::clone(running), p.start_delay)
            .map_err(|e| anyhow::anyhow!("{}", e))?;
        handles.push(handle);
    }

    let progress = maybe_start_progress_multi(&handles, verbosity);

    let mut errors: Vec<String> = Vec::new();
    let mut total_events: u64 = 0;
    let mut total_bytes: u64 = 0;
    let mut total_errors: u64 = 0;
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

    if let Some(p) = progress {
        p.stop();
    }

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

    let grouped = build_clock_group_stats(&clock_groups, &stop_infos_for_groups(&stop_infos));
    if distinct_group_count(&clock_groups) >= 2 {
        status::print_summary_by_clock_group(&grouped, &agg, total_elapsed, verbosity);
    } else {
        status::print_summary(&agg, total_elapsed, verbosity);
    }

    if !errors.is_empty() {
        return Err(anyhow::anyhow!("{}", errors.join("; ")));
    }
    Ok(())
}

fn run_compiled_with_progress(
    compiled: sonda_core::compiler::compile_after::CompiledFile,
    running: &Arc<AtomicBool>,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    let handles =
        sonda_core::schedule::multi_runner::launch_multi_compiled(compiled, Arc::clone(running))
            .map_err(|e| anyhow::anyhow!("{}", e))?;

    let progress = maybe_start_progress_multi(&handles, verbosity);

    let mut errors: Vec<String> = Vec::new();
    for mut handle in handles {
        if let Err(e) = handle.join(None) {
            errors.push(e.to_string());
        }
    }

    if let Some(p) = progress {
        p.stop();
    }

    if !errors.is_empty() {
        return Err(anyhow::anyhow!("{}", errors.join("; ")));
    }
    Ok(())
}

fn stop_infos_for_groups(
    infos: &[StopInfo],
) -> Vec<(&sonda_core::schedule::stats::ScenarioStats,)> {
    infos.iter().map(|i| (&i.stats,)).collect()
}

fn build_clock_group_stats(
    clock_groups: &[(Option<String>, Option<bool>)],
    stop_infos: &[(&sonda_core::schedule::stats::ScenarioStats,)],
) -> Vec<status::ClockGroupStats> {
    debug_assert_eq!(clock_groups.len(), stop_infos.len());

    let mut order: Vec<Option<String>> = Vec::new();
    let mut bins: std::collections::HashMap<Option<String>, status::ClockGroupStats> =
        std::collections::HashMap::new();

    for ((group, is_auto), (stats,)) in clock_groups.iter().zip(stop_infos.iter()) {
        let key = group.clone();
        let entry = bins
            .entry(key.clone())
            .or_insert_with(|| status::ClockGroupStats {
                group: key.clone(),
                group_is_auto: *is_auto,
                scenario_count: 0,
                total_events: 0,
                total_bytes: 0,
                total_errors: 0,
            });
        if entry.scenario_count == 0 {
            order.push(key);
        }
        entry.scenario_count += 1;
        entry.total_events += stats.total_events;
        entry.total_bytes += stats.bytes_emitted;
        entry.total_errors += stats.errors;
    }

    order
        .into_iter()
        .map(|k| bins.remove(&k).expect("bin exists for key in order list"))
        .collect()
}

fn distinct_group_count(groups: &[(Option<String>, Option<bool>)]) -> usize {
    let set: std::collections::BTreeSet<&Option<String>> = groups.iter().map(|(g, _)| g).collect();
    set.len()
}
