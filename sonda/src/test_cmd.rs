//! `sonda test` — run a scenario and verify its `expect:` alert expectations.
//!
//! Orchestration and rendering only: a poller thread records timestamped
//! [`Observation`]s of the Prometheus `ALERTS` metric while the scenario
//! runs through the exact same machinery as `sonda run`; every deadline
//! decision is made by `sonda_core::verify::evaluator` over those
//! timelines. Firing deadlines are measured from scenario start, resolution
//! deadlines from scenario end. The process exits non-zero when any
//! expectation fails — `sonda test` in CI turns alert rules into a test
//! suite.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use owo_colors::{OwoColorize, Stream::Stderr};
use sonda_core::verify::evaluator::{evaluate_firing, evaluate_resolution, Observation, Outcome};
use sonda_core::verify::prometheus::PrometheusClient;
use sonda_core::verify::{parse_expectations, AlertState, ExpectConfig};
use sonda_core::CancellationToken;

use crate::cli::{self, Cli, Verbosity};

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
    let query_timeout = sonda_core::config::validate::parse_duration(&args.query_timeout)
        .map_err(|e| anyhow::anyhow!("invalid --query-timeout: {e}"))?;

    // --dry-run validates and prints the scenario without emitting or
    // polling — and therefore without needing an endpoint at all.
    if cli_opts.dry_run {
        let run_args = run_args_for(&args.scenario);
        crate::run_scenario(rt, &run_args, cli_opts, catalog, verbosity, cancel)?;
        eprintln!(
            "  expect: {} alert expectation(s) parsed OK",
            expect.alerts.len()
        );
        return Ok(());
    }

    let Some(prometheus_url) = args.prometheus_url.as_deref() else {
        bail!("--prometheus-url is required (or set SONDA_PROMETHEUS_URL); only --dry-run works without it");
    };

    let client = PrometheusClient::new(prometheus_url, query_timeout);
    preflight(&client, &expect, cancel)
        .with_context(|| format!("prometheus preflight against {prometheus_url} failed"))?;

    // The firing poller runs alongside the scenario. `stop` cuts it short
    // when the scenario itself fails — no point waiting out deadlines to
    // report a sink error (review W3).
    let started_at = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let (timeline_tx, timeline_rx) = mpsc::channel::<Vec<Vec<Observation>>>();
    let poller = spawn_firing_poller(
        PrometheusClient::new(prometheus_url, query_timeout),
        expect.clone(),
        started_at,
        interval,
        cancel.clone(),
        Arc::clone(&stop),
        timeline_tx,
    )?;

    let run_args = run_args_for(&args.scenario);
    let run_result = crate::run_scenario(rt, &run_args, cli_opts, catalog, verbosity, cancel);
    let ended_at = Instant::now();

    if run_result.is_err() {
        stop.store(true, Ordering::SeqCst);
        let _ = poller.join();
        return run_result;
    }

    let poller_died = poller.join().is_err();
    let firing_timelines = timeline_rx.try_recv().ok();
    if cancel.is_cancelled() {
        bail!("interrupted before alert expectations could be verified");
    }

    // A panicked or silent poller yields no timelines; the evaluator maps
    // empty timelines to Undecided, and the report says why (review W4).
    let firing_timelines =
        firing_timelines.unwrap_or_else(|| vec![Vec::new(); expect.alerts.len()]);

    let firing_outcomes: Vec<Outcome> = expect
        .alerts
        .iter()
        .zip(&firing_timelines)
        .map(|(expectation, timeline)| {
            let deadline = expectation
                .firing_within()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            Ok(evaluate_firing(timeline, deadline))
        })
        .collect::<anyhow::Result<_>>()?;

    let resolution_outcomes = check_resolutions(
        &client,
        &expect,
        &firing_outcomes,
        ended_at,
        interval,
        cancel,
    )?;

    report(
        &expect,
        &firing_outcomes,
        &resolution_outcomes,
        poller_died,
        started_at,
        ended_at,
    )
}

/// Confirm the endpoint answers a query for **every** expectation before
/// starting the scenario — a malformed selector on any of them should fail
/// here, not mid-run (review M3). Retries apply only to the first
/// expectation, as a connectivity gate tolerating a Prometheus still
/// starting up in CI; once it answers, the remaining selectors are probed
/// exactly once each. Against a dead endpoint the total cost is therefore
/// bounded by 3 query timeouts, independent of expectation count (review M4).
fn preflight(
    client: &PrometheusClient,
    expect: &ExpectConfig,
    cancel: &CancellationToken,
) -> anyhow::Result<()> {
    const ATTEMPTS: u32 = 3;
    let Some(first) = expect.alerts.first() else {
        return Ok(());
    };
    let mut gate = None;
    for attempt in 0..ATTEMPTS {
        if cancel.is_cancelled() {
            bail!("interrupted");
        }
        match client.alert_state(first) {
            Ok(_) => {
                gate = None;
                break;
            }
            Err(e) => gate = Some(e),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    if let Some(e) = gate {
        bail!("{e}");
    }
    for expectation in &expect.alerts[1..] {
        if cancel.is_cancelled() {
            bail!("interrupted");
        }
        client
            .alert_state(expectation)
            .map_err(|e| anyhow::anyhow!("{e}"))?;
    }
    Ok(())
}

/// Poll every expectation until it is settled — firing observed, or a fresh
/// post-query elapsed time at/past its deadline — then send the complete
/// per-expectation timelines. Timestamps are captured when each query
/// returns, never reused across queries (review blocker 1).
#[allow(clippy::too_many_arguments)]
fn spawn_firing_poller(
    client: PrometheusClient,
    expect: ExpectConfig,
    started_at: Instant,
    interval: Duration,
    cancel: CancellationToken,
    stop: Arc<AtomicBool>,
    timelines_out: mpsc::Sender<Vec<Vec<Observation>>>,
) -> anyhow::Result<std::thread::JoinHandle<()>> {
    let deadlines: Vec<Duration> = expect
        .alerts
        .iter()
        .map(|e| e.firing_within())
        .collect::<Result<_, _>>()
        .map_err(|e| anyhow::anyhow!("{e}"))?;

    let handle = std::thread::Builder::new()
        .name("sonda-test-poller".into())
        .spawn(move || {
            let mut timelines: Vec<Vec<Observation>> = vec![Vec::new(); expect.alerts.len()];
            let mut pending: Vec<usize> = (0..expect.alerts.len()).collect();
            while !pending.is_empty() && !cancel.is_cancelled() && !stop.load(Ordering::SeqCst) {
                pending.retain(|&index| {
                    let state = client
                        .alert_state(&expect.alerts[index])
                        .map_err(|e| e.to_string());
                    let at = started_at.elapsed();
                    let fired = matches!(state, Ok(AlertState::Firing));
                    timelines[index].push(Observation::new(at, state));
                    // Settled once firing is seen or coverage of the
                    // deadline is recorded; the evaluator makes the verdict.
                    !(fired || at >= deadlines[index])
                });
                if !pending.is_empty() {
                    std::thread::sleep(interval);
                }
            }
            let _ = timelines_out.send(timelines);
        })?;
    Ok(handle)
}

/// Poll resolution for every expectation that fired, recording query errors
/// as observations (review W1) with timestamps captured after each query.
/// Timestamps are relative to scenario end.
///
/// All pending expectations are polled in **one** loop, each settling
/// independently against the shared `ended_at` anchor — exactly like the
/// firing poller. Serializing them would let one expectation's wait push
/// another's first query past its own deadline, leaving that window
/// unobserved (round-2 review blocker).
fn check_resolutions(
    client: &PrometheusClient,
    expect: &ExpectConfig,
    firing_outcomes: &[Outcome],
    ended_at: Instant,
    interval: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<Option<Outcome>>> {
    let mut timelines: Vec<Option<(Duration, Vec<Observation>)>> = expect
        .alerts
        .iter()
        .enumerate()
        .map(|(index, expectation)| {
            let deadline = expectation
                .resolves_within()
                .map_err(|e| anyhow::anyhow!("{e}"))?;
            // Resolution applies only when a deadline is declared and the
            // alert actually fired (on time or late) — a never-fired alert's
            // story is already told by its firing failure.
            let fired = matches!(
                firing_outcomes[index],
                Outcome::Pass { .. } | Outcome::Late { .. }
            );
            Ok(deadline.filter(|_| fired).map(|d| (d, Vec::new())))
        })
        .collect::<anyhow::Result<_>>()?;

    let mut pending: Vec<usize> = timelines
        .iter()
        .enumerate()
        .filter_map(|(index, entry)| entry.as_ref().map(|_| index))
        .collect();
    while !pending.is_empty() {
        if cancel.is_cancelled() {
            bail!("interrupted during resolution checks");
        }
        pending.retain(|&index| {
            let Some((deadline, timeline)) = &mut timelines[index] else {
                return false;
            };
            let state = client
                .alert_state(&expect.alerts[index])
                .map_err(|e| e.to_string());
            let at = ended_at.elapsed();
            let resolved = matches!(state, Ok(ref s) if !matches!(s, AlertState::Firing));
            timeline.push(Observation::new(at, state));
            !(resolved || at >= *deadline)
        });
        if !pending.is_empty() {
            std::thread::sleep(interval);
        }
    }

    Ok(timelines
        .into_iter()
        .map(|entry| entry.map(|(deadline, timeline)| evaluate_resolution(&timeline, deadline)))
        .collect())
}

fn report(
    expect: &ExpectConfig,
    firing: &[Outcome],
    resolutions: &[Option<Outcome>],
    poller_died: bool,
    started_at: Instant,
    ended_at: Instant,
) -> anyhow::Result<()> {
    let mut failures = 0usize;
    eprintln!();
    for (index, expectation) in expect.alerts.iter().enumerate() {
        let alert = &expectation.alert;
        match &firing[index] {
            Outcome::Pass { at } => eprintln!(
                "  {} {alert} firing after {:.0?} (within {})",
                pass_marker(),
                at,
                expectation.firing_within
            ),
            Outcome::Late { at, .. } => {
                failures += 1;
                eprintln!(
                    "  {} {alert} fired after {:.0?} — later than firing_within {}",
                    fail_marker(),
                    at,
                    expectation.firing_within
                );
            }
            Outcome::Missed { last_error, .. } => {
                failures += 1;
                let detail = last_error
                    .as_deref()
                    .map(|e| format!(" (last query error: {e})"))
                    .unwrap_or_default();
                eprintln!(
                    "  {} {alert} did not fire within {}{detail}",
                    fail_marker(),
                    expectation.firing_within
                );
            }
            Outcome::Undecided {
                observed_at,
                last_error,
            } => {
                failures += 1;
                let why = if poller_died {
                    "the poller thread stopped unexpectedly".to_string()
                } else {
                    undecided_reason(observed_at, "firing")
                };
                eprintln!(
                    "  {} {alert}: no verdict — {why}, so firing within {} is unproven{}",
                    fail_marker(),
                    expectation.firing_within,
                    error_detail(last_error)
                );
            }
            // Outcome is #[non_exhaustive]; treat future variants as failures.
            other => {
                failures += 1;
                eprintln!(
                    "  {} {alert}: unrecognized firing outcome {other:?}",
                    fail_marker()
                );
            }
        }
        let resolves_within = expectation.resolves_within.as_deref().unwrap_or("-");
        match &resolutions[index] {
            None => {}
            Some(Outcome::Pass { at }) => eprintln!(
                "  {} {alert} resolved after {:.0?} (within {resolves_within} of scenario end)",
                pass_marker(),
                at
            ),
            Some(Outcome::Late { at, .. }) => {
                failures += 1;
                eprintln!(
                    "  {} {alert} resolved after {:.0?} — later than resolves_within {resolves_within}",
                    fail_marker(),
                    at
                );
            }
            Some(Outcome::Missed { last_error, .. }) => {
                failures += 1;
                let detail = last_error
                    .as_deref()
                    .map(|e| format!(" (last query error: {e})"))
                    .unwrap_or_default();
                eprintln!(
                    "  {} {alert} still firing {resolves_within} after scenario end{detail}",
                    fail_marker()
                );
            }
            Some(Outcome::Undecided {
                observed_at,
                last_error,
            }) => {
                failures += 1;
                eprintln!(
                    "  {} {alert}: no resolution verdict — {}, so resolving within {resolves_within} is unproven{}",
                    fail_marker(),
                    undecided_reason(observed_at, "resolution"),
                    error_detail(last_error)
                );
            }
            Some(other) => {
                failures += 1;
                eprintln!(
                    "  {} {alert}: unrecognized resolution outcome {other:?}",
                    fail_marker()
                );
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

/// Explain an [`Outcome::Undecided`]: either the transition was eventually
/// seen but nothing successful covered the deadline window, or polling
/// stopped before the deadline.
fn undecided_reason(observed_at: &Option<Duration>, transition: &str) -> String {
    match observed_at {
        Some(at) => format!(
            "{transition} was first observed at {at:.0?} with no successful query inside the window"
        ),
        None => "polling ended before the deadline".to_string(),
    }
}

fn error_detail(last_error: &Option<String>) -> String {
    last_error
        .as_deref()
        .map(|e| format!(" (last query error: {e})"))
        .unwrap_or_default()
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
