//! `sonda test` — run a scenario and verify its `expect:` alert expectations.
//!
//! Orchestration and rendering only: every deadline decision is made by
//! `sonda_core::verify::evaluator` over [`Observation`] timelines.
//!
//! Exactly one **acquisition source** supplies those timelines, chosen by
//! the caller with `--prometheus-url` or `--alertmanager-url`. They answer
//! different questions about the same `expect:` block — *did the rule
//! fire?* versus *did the notification arrive?* — so the same expectations
//! verify two different hops of the alerting path.
//!
//! **Prometheus** (`--prometheus-url`) acquires in two stages:
//!
//! - **Live polling** (instant queries) runs while the scenario does and
//!   only decides *when each check has settled* — firing observed, resolved,
//!   or deadline covered.
//! - **Range queries** then reconstruct the verdict timeline from the
//!   samples the rule evaluator actually stored, at rule-evaluation
//!   resolution — so verdicts are not quantized by the poll interval or
//!   skewed by time spent polling other expectations. When range
//!   acquisition fails or disagrees with what live polling saw (remote-
//!   write lag), the live timeline is the warned-about fallback.
//!
//! **Alertmanager** (`--alertmanager-url`) has no range API and no stored
//! history, so it is live polling only. Its verdict timeline is the poll
//! timeline, with the first firing observation sharpened by the alert's own
//! `startsAt` stamp — bounded by our own observations, never beyond them
//! (`verify::alertmanager::refine_firing_timeline`). Verdict times on this
//! path therefore carry `--interval` precision, not sample precision, and
//! the report says so rather than borrowing the Prometheus path's
//! guarantees.
//!
//! Firing deadlines are measured from scenario start, resolution deadlines
//! from scenario end. The process exits non-zero when any expectation
//! fails — `sonda test` in CI turns alert rules into a test suite.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{bail, Context};
use owo_colors::{OwoColorize, Stream::Stderr};
use sonda_core::verify::alertmanager::{refine_firing_timeline, AlertmanagerClient};
use sonda_core::verify::evaluator::{evaluate_firing, evaluate_resolution, Observation, Outcome};
use sonda_core::verify::prometheus::{PrometheusClient, ServerClock};
use sonda_core::verify::{
    foreign_label_values, parse_expectations, AlertExpectation, AlertState, ExpectConfig,
};
use sonda_core::CancellationToken;

use crate::cli::{self, Cli, Verbosity};

/// Which check a verdict timeline is being acquired for — determines what
/// "the range data is missing something live polling saw" means.
#[derive(Clone, Copy)]
enum Check {
    Firing,
    Resolution,
}

/// The acquisition source the caller chose, before any client is built —
/// so the same choice can be reconnected for the poller thread.
#[derive(Clone, Copy)]
enum Target<'a> {
    Prometheus(&'a str),
    Alertmanager(&'a str),
}

impl Target<'_> {
    fn url(&self) -> &str {
        match self {
            Target::Prometheus(url) | Target::Alertmanager(url) => url,
        }
    }

    fn label(&self) -> &'static str {
        match self {
            Target::Prometheus(_) => "Prometheus",
            Target::Alertmanager(_) => "Alertmanager",
        }
    }

    /// Build a client. `am_offset` is ignored by the Prometheus variant.
    fn connect(&self, timeout: Duration, am_offset: f64) -> Source {
        match self {
            Target::Prometheus(url) => Source::Prometheus(PrometheusClient::new(url, timeout)),
            Target::Alertmanager(url) => Source::Alertmanager(
                AlertmanagerClient::new(url, timeout).with_clock_offset(am_offset),
            ),
        }
    }
}

/// The one acquisition source this run verifies against.
///
/// Both variants answer "is this expectation firing right now?"; only the
/// Prometheus one can also reconstruct history after the fact.
enum Source {
    Prometheus(PrometheusClient),
    Alertmanager(AlertmanagerClient),
}

/// What one poll of one expectation yielded, in the shape both acquisitions
/// can produce. The Prometheus path leaves `series` and `started_at` empty
/// — it recovers matched series from its range query instead.
#[derive(Default)]
struct Poll {
    series: Vec<BTreeMap<String, String>>,
    started_at: Option<f64>,
    suppressed: bool,
}

/// Everything the live poller learned about one expectation beyond its
/// timeline: matched label sets (provenance) and Alertmanager's own view of
/// when the alert started.
#[derive(Default, Clone)]
struct Evidence {
    series: Vec<BTreeMap<String, String>>,
    started_at: Option<f64>,
    suppressed: bool,
}

/// What the live firing poller hands back: one timeline and one evidence
/// bundle per expectation, in expectation order.
struct FiringPoll {
    timelines: Vec<Vec<Observation>>,
    evidence: Vec<Evidence>,
}

impl FiringPoll {
    fn empty(alerts: usize) -> Self {
        Self {
            timelines: vec![Vec::new(); alerts],
            evidence: vec![Evidence::default(); alerts],
        }
    }
}

impl Evidence {
    fn absorb(&mut self, poll: Poll) {
        for labels in poll.series {
            if !self.series.contains(&labels) {
                self.series.push(labels);
            }
        }
        if let Some(starts) = poll.started_at {
            self.started_at = Some(self.started_at.map_or(starts, |cur: f64| cur.min(starts)));
        }
        self.suppressed |= poll.suppressed;
    }
}

impl Source {
    /// Poll one expectation. The state is the poller's settling signal; the
    /// rest is evidence the caller accumulates.
    fn poll(&self, expectation: &AlertExpectation) -> (Result<AlertState, String>, Poll) {
        match self {
            Source::Prometheus(client) => (
                client.alert_state(expectation).map_err(|e| e.to_string()),
                Poll::default(),
            ),
            Source::Alertmanager(client) => match client.alerts(expectation) {
                Ok(snapshot) => (
                    Ok(snapshot.state),
                    Poll {
                        series: snapshot.series,
                        started_at: snapshot.started_at,
                        suppressed: snapshot.suppressed,
                    },
                ),
                Err(e) => (Err(e.to_string()), Poll::default()),
            },
        }
    }

    /// Reachability + selector probe, used by the preflight gate.
    fn probe(&self, expectation: &AlertExpectation) -> anyhow::Result<()> {
        match self {
            Source::Prometheus(client) => client.alert_state(expectation).map(|_| ()),
            Source::Alertmanager(client) => client.alert_state(expectation).map(|_| ()),
        }
        .map_err(|e| anyhow::anyhow!("{e}"))
    }

    /// How the report names this source.
    fn label(&self) -> &'static str {
        match self {
            Source::Prometheus(_) => "Prometheus",
            Source::Alertmanager(_) => "Alertmanager",
        }
    }

    /// Alertmanager stamps live on its own clock; this is the measured
    /// server-minus-local offset used to translate them back.
    fn clock_offset_secs(&self) -> f64 {
        match self {
            Source::Prometheus(_) => 0.0,
            Source::Alertmanager(client) => client.clock_offset_secs(),
        }
    }
}

/// Per-expectation live resolution polling result: `None` when the check
/// doesn't apply, otherwise the deadline and the recorded timeline.
type ResolutionPoll = Option<(Duration, Vec<Observation>)>;

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
    let query_step = sonda_core::config::validate::parse_duration(&args.query_step)
        .map_err(|e| anyhow::anyhow!("invalid --query-step: {e}"))?;
    if query_step.is_zero() {
        bail!("--query-step must be positive");
    }

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

    // Exactly one acquisition source. Both is a contradiction (which
    // verdict would win?), neither leaves nothing to ask.
    let target = match (
        args.prometheus_url.as_deref(),
        args.alertmanager_url.as_deref(),
    ) {
        (Some(_), Some(_)) => bail!(
            "--prometheus-url and --alertmanager-url are mutually exclusive — \
             `sonda test` verifies against exactly one acquisition source \
             (the rule evaluator or Alertmanager), so pick the hop you mean to test"
        ),
        (Some(url), None) => Target::Prometheus(url),
        (None, Some(url)) => Target::Alertmanager(url),
        (None, None) => bail!(
            "one of --prometheus-url (env SONDA_PROMETHEUS_URL) or --alertmanager-url \
             (env SONDA_ALERTMANAGER_URL) is required; only --dry-run works without one"
        ),
    };
    let url = target.url().to_string();

    preflight(&target.connect(query_timeout, 0.0), &expect, cancel).with_context(|| {
        format!(
            "{} preflight against {url} failed",
            target.label().to_lowercase()
        )
    })?;

    // Alertmanager's alert stamps come from *its* clock, measured from the
    // Date header — one-second granular, and only ever used to sharpen a
    // firing time within bounds we observed ourselves.
    let am_offset = match target {
        Target::Prometheus(_) => 0.0,
        Target::Alertmanager(_) => AlertmanagerClient::new(&url, query_timeout)
            .measure_clock_offset()
            .unwrap_or_else(|e| {
                eprintln!(
                    "  {} could not measure the Alertmanager clock ({e}); assuming zero offset \
                     — firing times refined from `startsAt` may be skewed by any clock difference",
                    warn_marker()
                );
                0.0
            }),
    };
    let client = target.connect(query_timeout, am_offset);

    // Verdict anchors must live on the *server's* clock — range samples
    // carry server timestamps, and subtracting a local anchor from them
    // bakes any clock skew straight into the verdicts. Alertmanager has no
    // `time()` endpoint and no range API, so this stays the identity there;
    // its own offset is applied to `startsAt` stamps instead.
    let clock = match &client {
        Source::Prometheus(prometheus) => prometheus.server_clock().unwrap_or_else(|e| {
            eprintln!(
                "  {} could not measure the server clock ({e}); assuming zero offset — \
                 verdict times may be skewed by any clock difference",
                warn_marker()
            );
            ServerClock::identity()
        }),
        Source::Alertmanager(_) => ServerClock::identity(),
    };

    // The firing poller runs alongside the scenario. `stop` cuts it short
    // when the scenario itself fails — no point waiting out deadlines to
    // report a sink error (review W3). The wall clock is captured at the
    // same moment as the monotonic anchor: range queries speak unix time,
    // observation timestamps speak durations-since-anchor.
    let started_wall = SystemTime::now();
    let started_at = Instant::now();
    let stop = Arc::new(AtomicBool::new(false));
    let (timeline_tx, timeline_rx) = mpsc::channel::<FiringPoll>();
    let poller = spawn_firing_poller(
        target.connect(query_timeout, am_offset),
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

    let ended_wall = started_wall + ended_at.duration_since(started_at);
    let poller_died = poller.join().is_err();
    let firing_poll = timeline_rx.try_recv().ok();
    if cancel.is_cancelled() {
        bail!("interrupted before alert expectations could be verified");
    }

    // A panicked or silent poller yields no live timelines; range
    // acquisition below can still recover a verdict from the datastore,
    // and when it can't, the evaluator maps the empty timeline to
    // Undecided and the report says why (review W4).
    let FiringPoll {
        timelines: live_firing,
        evidence,
    } = firing_poll.unwrap_or_else(|| FiringPoll::empty(expect.alerts.len()));

    // One settling pause before the verdict queries: the rule evaluator
    // remote-writes ALERTS on its own cadence, and the freshest samples
    // need a beat to land. Alertmanager holds no history for a later query
    // to find, so there is nothing to wait for on that path.
    if matches!(client, Source::Prometheus(_)) {
        std::thread::sleep(query_step.min(Duration::from_secs(5)));
    }
    if cancel.is_cancelled() {
        bail!("interrupted before alert expectations could be verified");
    }

    let mut warnings: Vec<String> = Vec::new();
    let mut matched_firing_series: Vec<BTreeMap<String, String>> = Vec::new();

    let anchor_start = clock.at(started_wall);
    // Alertmanager stamps are translated back to the local clock the
    // scenario anchors live on; `local_unix(started_wall)` is that anchor.
    let local_start = unix_secs(started_wall);
    let mut firing_outcomes: Vec<Outcome> = Vec::with_capacity(expect.alerts.len());
    for (index, expectation) in expect.alerts.iter().enumerate() {
        if cancel.is_cancelled() {
            bail!("interrupted before alert expectations could be verified");
        }
        let deadline = expectation
            .firing_within()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let (timeline, series) = match &client {
            Source::Prometheus(prometheus) => verdict_timeline(
                prometheus,
                &clock,
                expectation,
                &live_firing[index],
                deadline,
                anchor_start,
                query_step,
                Check::Firing,
                &mut warnings,
            ),
            // No stored history to query: the poll timeline *is* the
            // verdict timeline, sharpened by the alert's own startsAt.
            Source::Alertmanager(_) => (
                refine_firing_timeline(
                    &live_firing[index],
                    evidence[index]
                        .started_at
                        .map(|stamp| stamp - client.clock_offset_secs()),
                    local_start,
                ),
                evidence[index].series.clone(),
            ),
        };
        matched_firing_series.extend(series);
        firing_outcomes.push(evaluate_firing(&timeline, deadline));
    }

    let resolution_live = poll_resolutions(
        &client,
        &expect,
        &firing_outcomes,
        ended_at,
        interval,
        cancel,
    )?;

    let anchor_end = clock.at(ended_wall);
    let mut resolution_outcomes: Vec<Option<Outcome>> = Vec::with_capacity(expect.alerts.len());
    for (expectation, live) in expect.alerts.iter().zip(&resolution_live) {
        if cancel.is_cancelled() {
            bail!("interrupted during resolution checks");
        }
        let Some((deadline, live)) = live else {
            resolution_outcomes.push(None);
            continue;
        };
        let outcome = match &client {
            Source::Prometheus(prometheus) => {
                let (timeline, series) = verdict_timeline(
                    prometheus,
                    &clock,
                    expectation,
                    live,
                    *deadline,
                    anchor_end,
                    query_step,
                    Check::Resolution,
                    &mut warnings,
                );
                matched_firing_series.extend(series);
                evaluate_resolution(&timeline, *deadline)
            }
            // `startsAt` says nothing about when an alert ended, and the
            // stamp an expiring alert carries (`endsAt`) is a *deadline*
            // Alertmanager set in advance, not an observation — so the
            // resolution verdict stays exactly what polling saw.
            Source::Alertmanager(_) => evaluate_resolution(live, *deadline),
        };
        resolution_outcomes.push(Some(outcome));
    }

    // A silence stops the notification, not the alert: the expectation is
    // satisfied, but a test author asserting on a silenced alert is very
    // likely asserting on the wrong thing.
    for (expectation, found) in expect.alerts.iter().zip(&evidence) {
        if found.suppressed {
            warnings.push(format!(
                "{} matched an alert Alertmanager has suppressed (silenced or inhibited) — \
                 it still counts as firing, but no notification was delivered for it",
                expectation.alert
            ));
        }
    }

    // Provenance: an expectation that matched an alert carrying label
    // values this scenario never emitted is probably under-scoped —
    // ALERTS is global (#527 review).
    let emitted = emitted_label_values(&args.scenario, catalog);
    let mut foreign_seen: BTreeSet<(String, String)> = BTreeSet::new();
    for series in &matched_firing_series {
        for (key, value) in foreign_label_values(series, &emitted) {
            if foreign_seen.insert((key.clone(), value.clone())) {
                warnings.push(format!(
                    "matched an ALERTS series with {key}=\"{value}\", which this scenario \
                     never emits — another series may have triggered the alert; scope \
                     `expect.labels` to something unique to this scenario"
                ));
            }
        }
    }

    for warning in &warnings {
        eprintln!("  {} {warning}", warn_marker());
    }

    report(
        &expect,
        &firing_outcomes,
        &resolution_outcomes,
        poller_died,
        started_at,
        ended_at,
        client.label(),
        matches!(client, Source::Alertmanager(_)).then_some(interval),
    )
}

/// Acquire the verdict timeline for one check: a range query over the
/// window live polling settled, falling back to the live timeline (with a
/// warning) when the range API fails or its data lags what polling saw.
/// Verdicts are still made only by the evaluator — this chooses *which
/// observations* it gets, never what they mean. All window arithmetic is
/// in **server** coordinates (`window_anchor` comes from [`ServerClock`]).
#[allow(clippy::too_many_arguments)]
fn verdict_timeline(
    client: &PrometheusClient,
    clock: &ServerClock,
    expectation: &sonda_core::verify::AlertExpectation,
    live: &[Observation],
    deadline: Duration,
    window_anchor: f64,
    query_step: Duration,
    check: Check,
    warnings: &mut Vec<String>,
) -> (Vec<Observation>, Vec<BTreeMap<String, String>>) {
    const ATTEMPTS: u32 = 3;
    let step_secs = query_step.as_secs_f64();
    // The window ends one step past where live polling settled; a dead
    // poller leaves no timeline, so aim one step past the deadline and let
    // the now-clamp below decide how much of that is real.
    let settled = live
        .last()
        .map(|o| o.at)
        .unwrap_or(deadline + query_step)
        .as_secs_f64();
    let desired_end = window_anchor + settled + step_secs;

    let mut last_error = None;
    for attempt in 0..ATTEMPTS {
        // Never query past the server's now: future grid points would read
        // as Inactive and could fabricate coverage the datastore doesn't
        // have. Local now would defeat the clamp under skew.
        let end = desired_end.min(clock.now());
        if end <= window_anchor {
            break;
        }
        match client.range_timeline(expectation, window_anchor, end, query_step, window_anchor) {
            Ok(timeline) => {
                if range_covers_live(live, &timeline.observations, check) {
                    return (timeline.observations, timeline.firing_series);
                }
                last_error = None;
            }
            Err(e) => last_error = Some(e.to_string()),
        }
        if attempt + 1 < ATTEMPTS {
            std::thread::sleep(Duration::from_secs(1));
        }
    }
    let reason = last_error.map_or_else(
        || "range data lags what live polling observed".to_string(),
        |e| format!("range query failed: {e}"),
    );
    warnings.push(format!(
        "{} verdict for {} uses the live poll timeline ({reason})",
        match check {
            Check::Firing => "firing",
            Check::Resolution => "resolution",
        },
        expectation.alert
    ));
    (live.to_vec(), Vec::new())
}

/// Whether the range grid reflects the decisive state live polling saw —
/// remote-write lag can leave the freshest samples unwritten at query time.
fn range_covers_live(live: &[Observation], range: &[Observation], check: Check) -> bool {
    match check {
        Check::Firing => {
            let live_fired = live
                .iter()
                .any(|o| matches!(o.state, Ok(AlertState::Firing)));
            let range_fired = range
                .iter()
                .any(|o| matches!(o.state, Ok(AlertState::Firing)));
            !live_fired || range_fired
        }
        Check::Resolution => {
            let live_resolved = live
                .iter()
                .any(|o| matches!(o.state, Ok(ref s) if !matches!(s, AlertState::Firing)));
            let range_still_firing_at_end =
                matches!(range.last().map(|o| &o.state), Some(Ok(AlertState::Firing)));
            !live_resolved || !range_still_firing_at_end
        }
    }
}

/// Every label value the compiled scenario emits, keyed by label name.
/// Best-effort: the scenario already compiled and ran, so a load failure
/// here only disables provenance warnings, never the verdict.
fn emitted_label_values(
    scenario: &str,
    catalog: Option<&std::path::Path>,
) -> BTreeMap<String, BTreeSet<String>> {
    let mut emitted: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    if let Ok(compiled) = crate::scenario_loader::load_scenario_compiled(scenario, catalog) {
        for entry in &compiled.entries {
            if let Some(labels) = &entry.labels {
                for (key, value) in labels {
                    emitted
                        .entry(key.clone())
                        .or_default()
                        .insert(value.clone());
                }
            }
        }
    }
    emitted
}

/// Confirm the endpoint answers a query for **every** expectation before
/// starting the scenario — a malformed selector on any of them should fail
/// here, not mid-run (review M3). Retries apply only to the first
/// expectation, as a connectivity gate tolerating a Prometheus still
/// starting up in CI; once it answers, the remaining selectors are probed
/// exactly once each. Against a dead endpoint the total cost is therefore
/// bounded by 3 query timeouts, independent of expectation count (review M4).
fn preflight(
    client: &Source,
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
        match client.probe(first) {
            Ok(()) => {
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
        client.probe(expectation)?;
    }
    Ok(())
}

/// Poll every expectation until it is settled — firing observed, or a fresh
/// post-query elapsed time at/past its deadline — then send the complete
/// per-expectation timelines. Timestamps are captured when each query
/// returns, never reused across queries (review blocker 1).
#[allow(clippy::too_many_arguments)]
fn spawn_firing_poller(
    client: Source,
    expect: ExpectConfig,
    started_at: Instant,
    interval: Duration,
    cancel: CancellationToken,
    stop: Arc<AtomicBool>,
    timelines_out: mpsc::Sender<FiringPoll>,
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
            let mut poll = FiringPoll::empty(expect.alerts.len());
            let mut pending: Vec<usize> = (0..expect.alerts.len()).collect();
            while !pending.is_empty() && !cancel.is_cancelled() && !stop.load(Ordering::SeqCst) {
                pending.retain(|&index| {
                    let (state, extra) = client.poll(&expect.alerts[index]);
                    let at = started_at.elapsed();
                    let fired = matches!(state, Ok(AlertState::Firing));
                    poll.evidence[index].absorb(extra);
                    poll.timelines[index].push(Observation::new(at, state));
                    // Settled once firing is seen or coverage of the
                    // deadline is recorded; the evaluator makes the verdict.
                    !(fired || at >= deadlines[index])
                });
                if !pending.is_empty() {
                    std::thread::sleep(interval);
                }
            }
            let _ = timelines_out.send(poll);
        })?;
    Ok(handle)
}

/// Live-poll resolution for every expectation that fired, recording query
/// errors as observations (review W1) with timestamps captured after each
/// query, relative to scenario end. This only decides *when each check has
/// settled* — resolved, or deadline covered; the verdict timeline comes
/// from a range query afterwards.
///
/// All pending expectations are polled in **one** loop, each settling
/// independently against the shared `ended_at` anchor — exactly like the
/// firing poller. Serializing them would let one expectation's wait push
/// another's first query past its own deadline, leaving that window
/// unobserved (round-2 review blocker).
fn poll_resolutions(
    client: &Source,
    expect: &ExpectConfig,
    firing_outcomes: &[Outcome],
    ended_at: Instant,
    interval: Duration,
    cancel: &CancellationToken,
) -> anyhow::Result<Vec<ResolutionPoll>> {
    let mut timelines: Vec<ResolutionPoll> = expect
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
            let (state, _) = client.poll(&expect.alerts[index]);
            let at = ended_at.elapsed();
            let resolved = matches!(state, Ok(ref s) if !matches!(s, AlertState::Firing));
            timeline.push(Observation::new(at, state));
            !(resolved || at >= *deadline)
        });
        if !pending.is_empty() {
            std::thread::sleep(interval);
        }
    }

    Ok(timelines)
}

/// Render the verdicts.
///
/// `poll_precision` is `Some` only for acquisitions with no stored history
/// to query — Alertmanager — and states the resolution their transition
/// times actually carry. The Prometheus path's times come from stored
/// samples on a measured server clock (#528); saying the same thing about
/// a live-polled path would be a claim the data does not support.
#[allow(clippy::too_many_arguments)]
fn report(
    expect: &ExpectConfig,
    firing: &[Outcome],
    resolutions: &[Option<Outcome>],
    poller_died: bool,
    started_at: Instant,
    ended_at: Instant,
    source: &str,
    poll_precision: Option<Duration>,
) -> anyhow::Result<()> {
    let mut failures = 0usize;
    eprintln!();
    for (index, expectation) in expect.alerts.iter().enumerate() {
        let alert = &expectation.alert;
        match &firing[index] {
            Outcome::Pass { at } => eprintln!(
                "  {} {alert} firing after {} (within {})",
                pass_marker(),
                fmt_after(*at),
                expectation.firing_within
            ),
            Outcome::Late { at, .. } => {
                failures += 1;
                eprintln!(
                    "  {} {alert} fired after {} — later than firing_within {}",
                    fail_marker(),
                    fmt_after(*at),
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
                "  {} {alert} resolved after {} (within {resolves_within} of scenario end)",
                pass_marker(),
                fmt_after(*at)
            ),
            Some(Outcome::Late { at, .. }) => {
                failures += 1;
                eprintln!(
                    "  {} {alert} resolved after {} — later than resolves_within {resolves_within}",
                    fail_marker(),
                    fmt_after(*at)
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
    if let Some(interval) = poll_precision {
        eprintln!(
            "  note: {source} keeps no queryable history, so these times come from live polling \
             every {interval:.0?}, sharpened by each alert's own startsAt stamp \
             (~1s clock resolution) — not from stored samples"
        );
    }
    if failures == 0 {
        eprintln!(
            "  {} {total} alert expectation(s) verified via {source} (scenario ran {:.0?})",
            pass_marker(),
            elapsed
        );
        Ok(())
    } else {
        bail!("{failures} alert expectation(s) failed (verified via {source})");
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

/// Render a transition offset; zero reads "0s", not "0ns".
fn fmt_after(at: Duration) -> String {
    if at.is_zero() {
        "0s".to_string()
    } else {
        format!("{at:.0?}")
    }
}

/// A wall-clock instant as unix seconds.
fn unix_secs(t: SystemTime) -> f64 {
    t.duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn pass_marker() -> String {
    format!("{}", "PASS".if_supports_color(Stderr, |t| t.green()))
}

fn fail_marker() -> String {
    format!("{}", "FAIL".if_supports_color(Stderr, |t| t.red()))
}

fn warn_marker() -> String {
    format!("{}", "WARN".if_supports_color(Stderr, |t| t.yellow()))
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
