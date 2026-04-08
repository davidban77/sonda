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
                status::print_start(&p.entry, verbosity, None);
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
                    None,
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
            status::print_start(&p.entry, verbosity, None);
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
                None,
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
            status::print_start(&p.entry, verbosity, None);
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
                None,
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
            status::print_start(&p.entry, verbosity, None);
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
                None,
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
            let scenario = scenarios::get(&show_args.name).ok_or_else(|| {
                let names = scenarios::available_names();
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
        status::print_start(&p.entry, verbosity, None);
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
            None,
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
