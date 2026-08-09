//! `sonda test` — run a scenario and verify its `expect:` alert expectations.
//!
//! Orchestration: a poller thread watches the Prometheus `ALERTS` metric
//! while the scenario runs through the exact same machinery as `sonda run`.
//! Firing deadlines are measured from scenario start; resolution deadlines
//! from scenario end. The process exits non-zero when any expectation fails,
//! which is the whole point — `sonda test` in CI turns alert rules into a
//! test suite.

use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use owo_colors::{OwoColorize, Stream::Stderr};
use sonda_core::verify::prometheus::{AlertState, PrometheusClient};
use sonda_core::verify::{parse_expectations, AlertExpectation, ExpectConfig};
use sonda_core::CancellationToken;

use crate::cli::{self, Cli, Verbosity};

/// Outcome of one expectation's firing check.
struct FiringOutcome {
    /// Index into `ExpectConfig::alerts`.
    index: usize,
    /// Seconds after scenario start when `firing` was first observed.
    fired_after: Option<Duration>,
    /// Last poll error, surfaced when the deadline passes without an answer.
    last_error: Option<String>,
}

/// Outcome of one expectation's resolution check.
struct ResolutionOutcome {
    index: usize,
    resolved_after: Option<Duration>,
}

pub fn run(
    rt: &tokio::runtime::Runtime,
    args: &cli::TestArgs,
    cli_opts: &Cli,
    catalog: Option<&std::path::Path>,
    verbosity: Verbosity,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    let yaml = crate::config::resolve_scenario_source(&args.scenario, catalog)?;
    let Some(expect) = parse_expectations(&yaml).map_err(|e| anyhow::anyhow!("{e}"))? else {
        bail!(
            "scenario {} has no `expect:` block — nothing to verify. \
             Add one (see the alert-testing guide) or use `sonda run`.",
            args.scenario
        );
    };
    let interval = sonda_core::config::validate::parse_duration(&args.interval)
        .map_err(|e| anyhow::anyhow!("invalid --interval: {e}"))?;

    // --dry-run validates and prints the scenario without emitting or
    // polling; the expectations were already validated above.
    if cli_opts.dry_run {
        let run_args = run_args_for(&args.scenario);
        crate::run_scenario(rt, &run_args, cli_opts, catalog, verbosity, cancel)?;
        eprintln!(
            "  expect: {} alert expectation(s) parsed OK",
            expect.alerts.len()
        );
        return Ok(());
    }

    let client = PrometheusClient::new(&args.prometheus_url);
    preflight(&client, &expect.alerts[0], cancel).with_context(|| {
        format!(
            "prometheus preflight against {} failed",
            args.prometheus_url
        )
    })?;

    // Firing poller runs alongside the scenario: deadlines are measured from
    // scenario start, and an alert may legitimately keep an expectation
    // waiting past scenario end (evaluation intervals lag emission).
    let started_at = Instant::now();
    let (outcome_tx, outcome_rx) = mpsc::channel::<FiringOutcome>();
    let poller = spawn_firing_poller(
        PrometheusClient::new(&args.prometheus_url),
        expect.clone(),
        started_at,
        interval,
        cancel.clone(),
        outcome_tx,
    )?;

    let run_args = run_args_for(&args.scenario);
    let run_result = crate::run_scenario(rt, &run_args, cli_opts, catalog, verbosity, cancel);
    let ended_at = Instant::now();

    let firing: Vec<FiringOutcome> = outcome_rx.iter().take(expect.alerts.len()).collect();
    let _ = poller.join();
    run_result?;

    if cancel.is_cancelled() {
        bail!("interrupted before alert expectations could be verified");
    }

    let resolutions = check_resolutions(&client, &expect, &firing, ended_at, interval, cancel)?;
    report(&expect, &firing, &resolutions, started_at, ended_at)
}

/// Confirm the endpoint answers the query API before starting the scenario.
/// A few retries tolerate a Prometheus that is still starting up in CI.
fn preflight(
    client: &PrometheusClient,
    probe: &AlertExpectation,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    const ATTEMPTS: u32 = 5;
    let mut last_err = None;
    for attempt in 0..ATTEMPTS {
        if cancel.is_cancelled() {
            bail!("interrupted");
        }
        match client.alert_state(probe) {
            Ok(_) => return Ok(()),
            Err(e) => last_err = Some(e),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    bail!(
        "{}",
        last_err.map_or_else(|| "unreachable".to_string(), |e| e.to_string())
    )
}

fn spawn_firing_poller(
    client: PrometheusClient,
    expect: ExpectConfig,
    started_at: Instant,
    interval: Duration,
    cancel: CancellationToken,
    outcomes: mpsc::Sender<FiringOutcome>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    // Deadlines validated at parse time; recompute here for the thread.
    let deadlines: Vec<Duration> = expect
        .alerts
        .iter()
        .map(|e| e.firing_within())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let handle = std::thread::Builder::new()
        .name("sonda-test-poller".into())
        .spawn(move || {
            let mut pending: Vec<usize> = (0..expect.alerts.len()).collect();
            let mut last_errors: Vec<Option<String>> = vec![None; expect.alerts.len()];
            while !pending.is_empty() && !cancel.is_cancelled() {
                let elapsed = started_at.elapsed();
                pending.retain(|&index| {
                    match client.alert_state(&expect.alerts[index]) {
                        Ok(AlertState::Firing) => {
                            let _ = outcomes.send(FiringOutcome {
                                index,
                                fired_after: Some(started_at.elapsed()),
                                last_error: None,
                            });
                            return false;
                        }
                        Ok(_) => last_errors[index] = None,
                        // Transient query errors don't fail the expectation;
                        // they surface only if the deadline passes.
                        Err(e) => last_errors[index] = Some(e.to_string()),
                    }
                    if elapsed >= deadlines[index] {
                        let _ = outcomes.send(FiringOutcome {
                            index,
                            fired_after: None,
                            last_error: last_errors[index].clone(),
                        });
                        return false;
                    }
                    true
                });
                if !pending.is_empty() {
                    std::thread::sleep(interval);
                }
            }
            // Cancellation: flush unfinished expectations as undecided so the
            // main thread's collect never blocks forever.
            for index in pending {
                let _ = outcomes.send(FiringOutcome {
                    index,
                    fired_after: None,
                    last_error: Some("interrupted".into()),
                });
            }
        })?;
    Ok(handle)
}

fn check_resolutions(
    client: &PrometheusClient,
    expect: &ExpectConfig,
    firing: &[FiringOutcome],
    ended_at: Instant,
    interval: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<ResolutionOutcome>> {
    let mut outcomes = Vec::new();
    for (index, expectation) in expect.alerts.iter().enumerate() {
        let Some(deadline) = expectation
            .resolves_within()
            .map_err(|e| anyhow::anyhow!("{e}"))?
        else {
            continue;
        };
        // An alert that never fired has nothing to resolve; the firing
        // failure already tells the story.
        let fired = firing
            .iter()
            .find(|o| o.index == index)
            .is_some_and(|o| o.fired_after.is_some());
        if !fired {
            continue;
        }
        let mut resolved_after = None;
        loop {
            if cancel.is_cancelled() {
                bail!("interrupted during resolution checks");
            }
            let elapsed = ended_at.elapsed();
            if let Ok(state) = client.alert_state(expectation) {
                if state != AlertState::Firing {
                    resolved_after = Some(elapsed);
                    break;
                }
            }
            if elapsed >= deadline {
                break;
            }
            std::thread::sleep(interval);
        }
        outcomes.push(ResolutionOutcome {
            index,
            resolved_after,
        });
    }
    Ok(outcomes)
}

fn report(
    expect: &ExpectConfig,
    firing: &[FiringOutcome],
    resolutions: &[ResolutionOutcome],
    started_at: Instant,
    ended_at: Instant,
) -> anyhow::Result<()> {
    let mut failures = 0usize;
    eprintln!();
    for (index, expectation) in expect.alerts.iter().enumerate() {
        let outcome = firing.iter().find(|o| o.index == index);
        match outcome.and_then(|o| o.fired_after) {
            Some(after) => {
                eprintln!(
                    "  {} {} firing after {:.0?} (within {})",
                    pass_marker(),
                    expectation.alert,
                    after,
                    expectation.firing_within
                );
            }
            None => {
                failures += 1;
                let detail = outcome
                    .and_then(|o| o.last_error.as_deref())
                    .map(|e| format!(" (last query error: {e})"))
                    .unwrap_or_default();
                eprintln!(
                    "  {} {} did not fire within {}{detail}",
                    fail_marker(),
                    expectation.alert,
                    expectation.firing_within
                );
            }
        }
        if let Some(resolution) = resolutions.iter().find(|r| r.index == index) {
            match resolution.resolved_after {
                Some(after) => eprintln!(
                    "  {} {} resolved after {:.0?} (within {} of scenario end)",
                    pass_marker(),
                    expectation.alert,
                    after,
                    expectation.resolves_within.as_deref().unwrap_or("-")
                ),
                None => {
                    failures += 1;
                    eprintln!(
                        "  {} {} still firing {} after scenario end",
                        fail_marker(),
                        expectation.alert,
                        expectation.resolves_within.as_deref().unwrap_or("-")
                    );
                }
            }
        }
    }
    let total = expect.alerts.len();
    let elapsed = ended_at.duration_since(started_at);
    eprintln!();
    if failures == 0 {
        eprintln!(
            "  {} {total} alert expectation(s) verified (scenario ran {:.0?})",
            pass_marker(),
            elapsed
        );
        Ok(())
    } else {
        bail!("{failures} alert expectation(s) failed");
    }
}

fn pass_marker() -> String {
    format!("{}", "PASS".if_supports_color(Stderr, |t| t.green()))
}

fn fail_marker() -> String {
    format!("{}", "FAIL".if_supports_color(Stderr, |t| t.red()))
}

/// `sonda test` runs the scenario exactly as written — no overrides.
fn run_args_for(scenario: &str) -> cli::RunArgs {
    cli::RunArgs {
        scenario: scenario.to_string(),
        duration: None,
        rate: None,
        sink: None,
        endpoint: None,
        encoder: None,
        output: None,
        labels: Vec::new(),
        on_sink_error: None,
    }
}
