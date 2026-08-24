//! sonda CLI entrypoint.

mod cli;
mod config;
mod dry_run;
mod new;
mod progress;
mod scenario_loader;
mod sink_format;
mod status;
mod test_cmd;

use std::process;
use std::sync::Arc;
use std::time::Instant;

use clap::Parser;
use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;
use sonda_core::CancellationToken;

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

    let cancel = CancellationToken::new();
    {
        let c = cancel.clone();
        ctrlc::set_handler(move || {
            c.cancel();
        })
        .expect("failed to register Ctrl+C handler");
    }

    let cli = Cli::parse();
    let verbosity = Verbosity::from_flags(cli.quiet, cli.verbose);
    let catalog = cli.catalog.as_deref();

    // Handled before the runtime is built: writing a completion script to
    // stdout needs no scheduler, and spinning up a multi-thread tokio
    // runtime to print text would make shell startup pay for it.
    if let Commands::Completions(ref args) = cli.command {
        print_completions(args.shell);
        return Ok(());
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match cli.command {
        Commands::Run(ref args) => run_scenario(&rt, args, &cli, catalog, verbosity, &cancel)?,
        Commands::List(ref args) => list_catalog(args, catalog)?,
        Commands::Show(ref args) => show_entry(args, catalog)?,
        Commands::New(ref args) => new::run(args)?,
        Commands::Test(ref args) => test_cmd::run(&rt, args, &cli, catalog, verbosity, &cancel)?,
        // Returned above, before the runtime exists.
        Commands::Completions(_) => unreachable!("completions is handled before the runtime"),
    }

    Ok(())
}

/// Write a completion script for `shell` to stdout.
///
/// Generated from the same [`Cli`] derive the binary parses with, so a new
/// subcommand or flag is completable the moment it exists — there is no
/// second list of commands here to fall behind the first.
fn print_completions(shell: clap_complete::Shell) {
    let mut command = <Cli as clap::CommandFactory>::command();
    clap_complete::generate(shell, &mut command, "sonda", &mut std::io::stdout());
}

fn run_scenario(
    rt: &tokio::runtime::Runtime,
    args: &cli::RunArgs,
    cli_opts: &Cli,
    catalog: Option<&std::path::Path>,
    verbosity: Verbosity,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let mut compiled = scenario_loader::load_scenario_compiled(&args.scenario, catalog)?;
    config::apply_run_overrides_compiled(&mut compiled, args)?;
    let has_gates = scenario_loader::has_while_clause(&compiled);

    if cli_opts.dry_run {
        let format = dry_run::parse_format(cli_opts.format.as_deref())?;
        // Validate through the pipeline the runtime uses, not a second list of
        // its rules.
        //
        // `--dry-run` returned here before doing this, which meant it never ran
        // `prepare_entries` — and therefore never ran `expand_scenario`, where
        // csv_replay derives its rate from the file's timestamps, fans a
        // multi-column capture out into one entry per series, and rejects a
        // file it cannot replay. So a scenario `sonda run` refuses printed
        // "Validation: OK", which is worse than saying nothing: the whole point
        // of the flag is to answer "would this run?" without running it, and CI
        // users reach for it precisely to catch this class before a deploy.
        //
        // The fix is to call the real thing rather than teach this path to
        // recognise the same failures — a transcription would diverge on
        // exactly the rule that was added last.
        //
        // AND IT HAS TO BE THE RIGHT REAL THING. `run` has two launch paths and
        // they do not share a rulebook: a file with a `while:` clause goes to
        // `launch_multi_compiled`, whose per-entry preparation refuses things
        // the ungated `prepare_entries` accepts — multi-column `csv_replay`
        // fan-out most of all, because a gate needs one entry per gated signal.
        // Calling only the ungated pipeline restored most of parity and left
        // the gated branch blessing files `run` refuses, which is worse than
        // the original bug: the flag now looks trustworthy. So dry-run
        // dispatches on `has_gates` exactly as the launch below does, and the
        // two branches call the two pipelines.
        //
        // The clone is because both pipelines consume the file and the printer
        // still needs it. It happens once, on a path that is about to exit.
        if has_gates {
            sonda_core::schedule::multi_runner::validate_compiled_gated(compiled.clone())
                .map_err(|e| anyhow::anyhow!("{}", e))?;
        } else {
            let entries = sonda_core::compiler::prepare::prepare(compiled.clone())
                .map_err(|e| anyhow::anyhow!("{}", e))?;
            sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;
        }
        dry_run::print_dry_run_compiled(&args.scenario, &compiled, format)?;
        return Ok(());
    }

    if has_gates {
        if verbosity == Verbosity::Verbose {
            status::print_version();
        }
        run_compiled_with_progress(rt, compiled, cancel, verbosity)?;
        return Ok(());
    }

    let entries =
        sonda_core::compiler::prepare::prepare(compiled).map_err(|e| anyhow::anyhow!("{}", e))?;
    let prepared = sonda_core::prepare_entries(entries).map_err(|e| anyhow::anyhow!("{}", e))?;

    if handle_pre_launch(&prepared, verbosity) {
        return Ok(());
    }

    if prepared.len() == 1 {
        let p = prepared.into_iter().next().expect("len checked above");
        run_single_scenario(rt, "cli-run".to_string(), p, cancel, verbosity)?;
    } else {
        launch_and_join_prepared(rt, "cli-run", prepared, cancel, verbosity)?;
    }
    Ok(())
}

fn list_catalog(args: &cli::ListArgs, catalog: Option<&std::path::Path>) -> anyhow::Result<()> {
    let dir =
        catalog.ok_or_else(|| anyhow::anyhow!("--catalog <dir> is required for `sonda list`"))?;
    let kind_filter = match args.kind.as_deref() {
        None => None,
        Some("runnable") => Some(sonda_core::catalog::EntryKind::Runnable),
        Some("composable") => Some(sonda_core::catalog::EntryKind::Composable),
        Some(other) => {
            anyhow::bail!("unknown --kind {other:?}: expected 'runnable' or 'composable'")
        }
    };

    let mut entries = sonda_core::catalog::enumerate(dir)?;
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
    let entries = sonda_core::catalog::enumerate(dir)?;
    let entry = entries
        .iter()
        .find(|e| e.name == name)
        .ok_or_else(|| anyhow::anyhow!("unknown catalog entry {:?}", name))?;
    let raw = std::fs::read_to_string(&entry.source_path)
        .map_err(|e| anyhow::anyhow!("failed to read {}: {e}", entry.source_path.display()))?;
    print!("{raw}");
    Ok(())
}

/// Print the pre-launch banner, and report whether the caller should stop.
///
/// It always returns `false` now. It used to carry a second `--dry-run`
/// implementation — printing the entries and a second "Validation: OK" — which
/// became unreachable when the dry-run branch in `run_scenario` started
/// returning before this call. `dry_run.rs` owns that output and is the only
/// thing that prints it. The `bool` stays because the shape is the useful one:
/// a future pre-launch check that should abort has somewhere to say so.
fn handle_pre_launch(prepared: &[PreparedEntry], verbosity: Verbosity) -> bool {
    if verbosity == Verbosity::Verbose {
        status::print_version();
        let total = prepared.len();
        for (i, p) in prepared.iter().enumerate() {
            status::print_config(&p.entry, i + 1, total);
        }
    }
    false
}

fn run_single_scenario(
    rt: &tokio::runtime::Runtime,
    name: String,
    prepared: PreparedEntry,
    cancel: &CancellationToken,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    status::print_start(&prepared.entry, verbosity, None);
    let mut handle = rt
        .block_on(sonda_core::launch_scenario(
            name,
            prepared.entry,
            cancel.child_token(),
            prepared.start_delay,
        ))
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
    rt: &tokio::runtime::Runtime,
    id_prefix: &str,
    prepared: Vec<PreparedEntry>,
    cancel: &CancellationToken,
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
        let handle = rt
            .block_on(sonda_core::launch_scenario(
                id,
                p.entry,
                cancel.child_token(),
                p.start_delay,
            ))
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
    rt: &tokio::runtime::Runtime,
    compiled: sonda_core::compiler::compile_after::CompiledFile,
    cancel: &CancellationToken,
    verbosity: Verbosity,
) -> anyhow::Result<()> {
    let handles = rt
        .block_on(sonda_core::schedule::multi_runner::launch_multi_compiled(
            compiled, None,
        ))
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let handle_cancels: Vec<CancellationToken> = handles.iter().map(|h| h.cancel.clone()).collect();
    let cancel_watcher = cancel.clone();
    let watcher = rt.spawn(async move {
        cancel_watcher.cancelled().await;
        for c in &handle_cancels {
            c.cancel();
        }
    });

    let progress = maybe_start_progress_multi(&handles, verbosity);

    let mut errors: Vec<String> = Vec::new();
    for mut handle in handles {
        if let Err(e) = handle.join(None) {
            errors.push(e.to_string());
        }
    }

    watcher.abort();
    rt.block_on(async {
        let _ = watcher.await;
    });

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
