//! Shared schedule loop for metrics, logs, histograms, and summaries.

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::compiler::{DelayClause, UnresolvedBehavior};
use crate::config::validate::StartTime;
use crate::config::OnSinkError;
use crate::model::log::LogEvent;
use crate::model::metric::MetricEvent;
use crate::schedule::gate_bus::{GateEdge, GateReceiver, InitialState};
use crate::schedule::stats::{ScenarioState, ScenarioStats};
use crate::schedule::{is_in_burst, is_in_gap, is_in_spike, time_until_gap_end};
use crate::sink::Sink;
use crate::SondaError;

use super::ParsedSchedule;

/// Minimum interval between rate-limited sink-error stderr emissions.
const SINK_WARN_INTERVAL: Duration = Duration::from_secs(60);

/// Per-scenario rate limiter for sink-error stderr warnings.
///
/// Stack-local in [`run_schedule_loop`]; not shared, not telemetry. Counts
/// suppressed errors and emits a single line at most once per
/// [`SINK_WARN_INTERVAL`].
struct SinkErrorRateLimiter {
    last_emit: Option<Instant>,
    suppressed_count: u64,
}

impl SinkErrorRateLimiter {
    fn new() -> Self {
        Self {
            last_emit: None,
            suppressed_count: 0,
        }
    }

    /// Record a sink error and emit a warning if the cooldown has elapsed.
    ///
    /// Always emits on the first call (so users see at least one line) and
    /// then at most once per [`SINK_WARN_INTERVAL`].
    fn observe(&mut self, scenario_name: &str, err: &std::io::Error) {
        self.suppressed_count += 1;
        let should_emit = self
            .last_emit
            .map(|t| t.elapsed() >= SINK_WARN_INTERVAL)
            .unwrap_or(true);
        if should_emit {
            eprintln!(
                "sonda: scenario '{}': {} sink errors in last {}s (last: {})",
                scenario_name,
                self.suppressed_count,
                SINK_WARN_INTERVAL.as_secs(),
                err
            );
            self.last_emit = Some(Instant::now());
            self.suppressed_count = 0;
        }
    }
}

/// The result returned by a per-tick callback.
///
/// Carries the information the shared loop needs to update stats after
/// the signal-specific work is done. Metric events for the tick are pushed
/// into the `events_buf` parameter passed to the callback, not returned
/// here — that buffer is reused across ticks to keep the hot path
/// allocation-free.
pub(crate) struct TickResult {
    /// Number of bytes written to the sink on this tick.
    pub bytes_written: u64,
    /// Whether this tick's write() delivered to the destination, or only
    /// buffered it (batching sinks). Drives delivery-health stat updates.
    pub delivered: bool,
}

/// Context passed to the per-tick callback.
///
/// Provides the tick index, spike window state, and dynamic labels so the
/// callback can build the correct labels for this tick.
pub(crate) struct TickContext<'a> {
    /// The monotonically increasing tick counter (0-based).
    pub tick: u64,
    /// The resolved cardinality spike windows from the schedule config.
    ///
    /// The callback uses these along with `elapsed` to determine which spike
    /// labels to inject.
    pub spike_windows: &'a [super::CardinalitySpikeWindow],
    /// The resolved dynamic labels from the schedule config.
    ///
    /// Dynamic labels are always-on: the callback injects their per-tick value
    /// into every event regardless of elapsed time.
    pub dynamic_labels: &'a [super::DynamicLabel],
    /// Elapsed time since the scenario started.
    ///
    /// Used by the callback to evaluate spike window state via [`is_in_spike`].
    pub elapsed: Duration,
    /// Wall-clock timestamp to stamp on this tick's events.
    ///
    /// Derived from the schedule's resolved `start_time` anchor plus `elapsed`,
    /// so a `start_time`-shifted scenario emits into a past or future window.
    pub wall_clock: SystemTime,
}

/// A pending sink write produced by a tick callback.
pub enum WriteCommand {
    Bytes(Vec<u8>),
    LogEvent { event: LogEvent, bytes: Vec<u8> },
}

/// Buffer of pending sink writes emitted by a tick callback.
#[derive(Default)]
pub struct TickOutput {
    pub writes: Vec<WriteCommand>,
}

impl TickOutput {
    pub fn clear(&mut self) {
        self.writes.clear();
    }
}

/// A per-tick callback that encodes events into `output`; the loop drains the
/// queue to the sink. The `events_buf` is pre-cleared before each call.
pub(crate) type TickFn<'a> = dyn FnMut(
        &TickContext<'_>,
        &mut TickOutput,
        &mut Vec<MetricEvent>,
    ) -> Result<TickResult, SondaError>
    + 'a;

/// Scenario-level wall-clock anchor: the resolved emission-time base plus the
/// monotonic instant the scenario began.
///
/// `wall_at(elapsed) = base + elapsed`. Captured once per scenario so a gated
/// scenario's clock advances continuously across pause/resume segments rather
/// than re-anchoring on each `Running` segment.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WallClock {
    base: SystemTime,
    scenario_start: Instant,
}

impl WallClock {
    /// Resolve the anchor from a schedule's `start_time` and a scenario-start instant.
    pub(crate) fn resolve(schedule: &ParsedSchedule, scenario_start: Instant) -> Self {
        let start_wall = SystemTime::now();
        let base = match schedule.start_time {
            StartTime::Now => start_wall,
            StartTime::Absolute(t) => t,
            StartTime::Offset { forward: true, by } => start_wall + by,
            StartTime::Offset { forward: false, by } => start_wall - by,
        };
        Self {
            base,
            scenario_start,
        }
    }

    /// The wall-clock timestamp for the tick happening `tick_elapsed` after the
    /// loop segment began. `tick_elapsed` is added to the scenario-level
    /// elapsed so the clock stays continuous across segments.
    fn wall_at(&self, segment_start: Instant, tick_elapsed: Duration) -> SystemTime {
        let scenario_elapsed = segment_start.duration_since(self.scenario_start) + tick_elapsed;
        self.base + scenario_elapsed
    }
}

/// Reason a [`CloseEmitFn`] is being invoked at gate-close commit time.
///
/// Resolved once at runner-build time and captured into the closure; the
/// gated loop no longer re-derives this on each commit.
#[derive(Debug, Clone, Copy, PartialEq)]
#[non_exhaustive]
pub enum CloseSignal {
    /// Emit a Prometheus stale-NaN sample for every recently-active series.
    StaleMarker,
    /// Emit one literal sample with this value for every recently-active series.
    SnapTo(f64),
}

/// Per-scenario callback invoked on every committed `running → paused`
/// transition. Closures push markers into the supplied [`TickOutput`].
pub type CloseEmitFn = Box<dyn FnMut(&mut TickOutput) -> Result<(), SondaError> + Send>;

/// Run the shared schedule loop until duration expires or shutdown is
/// signalled, then flush the sink and apply the sink-error policy.
pub(crate) async fn run_schedule_loop(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    sink: &mut Box<dyn Sink>,
    tick_fn: &mut TickFn<'_>,
) -> Result<(), SondaError> {
    let stats_for_flush = stats.clone();
    let loop_result = run_schedule_loop_with_initial_tick(
        schedule, rate, shutdown, stats, 0, None, None, sink, tick_fn,
    )
    .await;
    finalize_sink(schedule, stats_for_flush.as_ref(), sink, loop_result).await
}

async fn finalize_sink(
    schedule: &ParsedSchedule,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    sink: &mut Box<dyn Sink>,
    loop_result: Result<(), SondaError>,
) -> Result<(), SondaError> {
    match loop_result {
        Ok(()) => {
            let flush_result = sink.flush().await;
            apply_flush_policy(schedule, stats, flush_result)
        }
        Err(e) => Err(e),
    }
}

/// Run the schedule loop starting from `initial_tick`, optionally reporting the
/// last tick reached on exit through `last_tick_out`. Used by `gated_loop` to
/// continue the tick counter across pause/resume instead of restarting at 0.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_schedule_loop_with_initial_tick(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    initial_tick: u64,
    last_tick_out: Option<&AtomicU64>,
    wall: Option<WallClock>,
    sink: &mut Box<dyn Sink>,
    tick_fn: &mut TickFn<'_>,
) -> Result<(), SondaError> {
    let base_interval = Duration::from_secs_f64(1.0 / rate);

    let start = Instant::now();
    // A non-gated run has no scenario-level anchor: resolve one from this
    // loop's own start. A gated run threads its scenario-level anchor so the
    // emission clock stays continuous across pause/resume segments.
    let wall = wall.unwrap_or_else(|| WallClock::resolve(schedule, start));
    let mut next_deadline = start;
    let mut tick: u64 = initial_tick;

    // Stats tracking: snapshot of tick count and wall clock taken once per
    // second to compute current_rate.
    let mut rate_window_tick: u64 = 0;
    let mut rate_window_start = start;

    let mut sink_warn_limiter = SinkErrorRateLimiter::new();

    // Sized to cover typical histogram (10 buckets + 3) and summary
    // (~6 quantiles + 2) without reallocating.
    let mut events_buf: Vec<MetricEvent> = Vec::with_capacity(16);
    let mut tick_output = TickOutput {
        writes: Vec::with_capacity(16),
    };

    loop {
        // Check shutdown flag first — highest priority exit path.
        if let Some(flag) = shutdown {
            if !flag.load(Ordering::SeqCst) {
                break;
            }
        }

        let elapsed = start.elapsed();

        // Check duration limit.
        if let Some(total) = schedule.total_duration {
            if elapsed >= total {
                break;
            }
        }

        // Check gap window — sleep through it rather than busy-wait.
        // Gap always takes priority over burst: no events during a gap.
        if let Some(ref gap) = schedule.gap_window {
            if is_in_gap(elapsed, gap) {
                // Update stats to reflect gap state before sleeping.
                if let Some(ref s) = stats {
                    if let Ok(mut st) = s.write() {
                        st.in_gap = true;
                        st.in_burst = false;
                    }
                }
                let sleep_for = time_until_gap_end(elapsed, gap);
                if sleep_for > Duration::ZERO {
                    thread::sleep(sleep_for);
                }
                // After sleeping through the gap, reset the deadline so we
                // don't try to catch up for suppressed events. Re-derive
                // tick from elapsed time at base rate.
                let now = Instant::now();
                next_deadline = now;
                tick = initial_tick
                    + (start.elapsed().as_secs_f64() / base_interval.as_secs_f64()) as u64;
                continue;
            }
        }

        // We are not in a gap — `currently_in_gap` is always false here because
        // the gap branch above continues the loop instead of falling through.
        let currently_in_gap = false;

        // Determine the effective inter-event interval for this tick.
        let currently_in_burst;
        let effective_interval = if let Some(ref burst) = schedule.burst_window {
            if let Some(multiplier) = is_in_burst(elapsed, burst) {
                currently_in_burst = true;
                Duration::from_secs_f64(base_interval.as_secs_f64() / multiplier)
            } else {
                currently_in_burst = false;
                base_interval
            }
        } else {
            currently_in_burst = false;
            base_interval
        };

        // Deadline-based rate control.
        let now = Instant::now();
        if now < next_deadline {
            thread::sleep(next_deadline - now);
        }

        // Invoke the signal-specific tick callback.
        let ctx = TickContext {
            tick,
            spike_windows: &schedule.spike_windows,
            dynamic_labels: &schedule.dynamic_labels,
            elapsed,
            wall_clock: wall.wall_at(start, elapsed),
        };
        events_buf.clear();
        tick_output.clear();
        let tick_outcome = match tick_fn(&ctx, &mut tick_output, &mut events_buf) {
            Ok(mut result) => match drain_writes(&mut tick_output, sink).await {
                Ok(Some(delivered)) => {
                    result.delivered = delivered;
                    Ok(result)
                }
                Ok(None) => Ok(result),
                Err(e) => Err(e),
            },
            Err(e) => {
                tick_output.clear();
                Err(e)
            }
        };

        // Determine spike state for stats (check all spike windows).
        let currently_in_spike = schedule
            .spike_windows
            .iter()
            .any(|sw| is_in_spike(elapsed, sw));

        match tick_outcome {
            Ok(result) => {
                if let Some(ref s) = stats {
                    let window_elapsed = rate_window_start.elapsed();
                    let current_rate = if window_elapsed >= Duration::from_secs(1) {
                        let events_in_window = tick - rate_window_tick;
                        let r = events_in_window as f64 / window_elapsed.as_secs_f64();
                        rate_window_tick = tick;
                        rate_window_start = Instant::now();
                        r
                    } else {
                        s.read().map(|st| st.current_rate).unwrap_or(0.0)
                    };

                    if let Ok(mut st) = s.write() {
                        st.total_events += 1;
                        st.bytes_emitted += result.bytes_written;
                        st.current_rate = current_rate;
                        st.in_gap = currently_in_gap;
                        st.in_burst = currently_in_burst;
                        st.in_cardinality_spike = currently_in_spike;
                        if result.delivered {
                            st.consecutive_failures = 0;
                            st.last_successful_write_at = SystemTime::now()
                                .duration_since(UNIX_EPOCH)
                                .map(|d| d.as_nanos() as u64)
                                .ok();
                        }
                        for event in events_buf.drain(..) {
                            st.push_metric(event);
                        }
                    }
                }
            }
            Err(SondaError::Sink(io_err)) => match schedule.on_sink_error {
                OnSinkError::Warn => {
                    sink_warn_limiter.observe(&schedule.name, &io_err);
                    if let Some(ref s) = stats {
                        if let Ok(mut st) = s.write() {
                            st.errors = st.errors.saturating_add(1);
                            st.total_sink_failures = st.total_sink_failures.saturating_add(1);
                            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
                            st.last_sink_error = Some(io_err.to_string());
                            st.in_gap = currently_in_gap;
                            st.in_burst = currently_in_burst;
                            st.in_cardinality_spike = currently_in_spike;
                        }
                    }
                }
                OnSinkError::Fail => {
                    if let Some(ref s) = stats {
                        if let Ok(mut st) = s.write() {
                            st.errors = st.errors.saturating_add(1);
                            st.total_sink_failures = st.total_sink_failures.saturating_add(1);
                            st.consecutive_failures = st.consecutive_failures.saturating_add(1);
                            st.last_sink_error = Some(io_err.to_string());
                        }
                    }
                    return Err(SondaError::Sink(io_err));
                }
            },
            Err(other) => return Err(other),
        }

        next_deadline += effective_interval;
        tick += 1;
    }

    if let Some(out) = last_tick_out {
        out.store(tick, Ordering::SeqCst);
    }
    Ok(())
}

async fn drain_writes(
    output: &mut TickOutput,
    sink: &mut Box<dyn Sink>,
) -> Result<Option<bool>, SondaError> {
    let mut delivered: Option<bool> = None;
    for cmd in output.writes.drain(..) {
        match cmd {
            WriteCommand::Bytes(buf) => sink.write(&buf).await?,
            WriteCommand::LogEvent { event, bytes } => sink.write_log_event(&event, &bytes).await?,
        }
        delivered = Some(sink.last_write_delivered());
    }
    Ok(delivered)
}

/// Apply the scenario's sink-error policy to a flush call made at scenario
/// shutdown.
///
/// On `Warn`, emits one rate-limited stderr warning (sharing the same
/// SCENARIO_NAME format used during the loop) and returns `Ok(())`. On
/// `Fail`, propagates the error to the caller as before.
pub(crate) fn apply_flush_policy(
    schedule: &ParsedSchedule,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    flush_result: Result<(), SondaError>,
) -> Result<(), SondaError> {
    match flush_result {
        Ok(()) => Ok(()),
        Err(SondaError::Sink(io_err)) => match schedule.on_sink_error {
            OnSinkError::Warn => {
                eprintln!(
                    "sonda: scenario '{}': flush failed at shutdown: {}",
                    schedule.name, io_err
                );
                if let Some(s) = stats {
                    if let Ok(mut st) = s.write() {
                        st.errors = st.errors.saturating_add(1);
                        st.total_sink_failures = st.total_sink_failures.saturating_add(1);
                        st.consecutive_failures = st.consecutive_failures.saturating_add(1);
                        st.last_sink_error = Some(io_err.to_string());
                    }
                }
                Ok(())
            }
            OnSinkError::Fail => Err(SondaError::Sink(io_err)),
        },
        Err(other) => Err(other),
    }
}

/// Apply the scenario's sink-error policy to a close-emit invocation error.
///
/// On `Warn`, logs via the dedicated rate-limiter and swallows the error so
/// the gate transition still commits. On `Fail`, propagates.
fn apply_close_emit_policy(
    schedule: &ParsedSchedule,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    limiter: &mut SinkErrorRateLimiter,
    err: SondaError,
) -> Result<(), SondaError> {
    match err {
        SondaError::Sink(io_err) => match schedule.on_sink_error {
            OnSinkError::Warn => {
                limiter.observe(&schedule.name, &io_err);
                if let Some(s) = stats {
                    if let Ok(mut st) = s.write() {
                        st.errors = st.errors.saturating_add(1);
                        st.total_sink_failures = st.total_sink_failures.saturating_add(1);
                        st.last_sink_error = Some(io_err.to_string());
                    }
                }
                Ok(())
            }
            OnSinkError::Fail => Err(SondaError::Sink(io_err)),
        },
        other => Err(other),
    }
}

fn apply_close_emit_policy_flush(
    schedule: &ParsedSchedule,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    limiter: &mut SinkErrorRateLimiter,
    flush_result: Result<(), SondaError>,
) -> Result<(), SondaError> {
    match flush_result {
        Ok(()) => Ok(()),
        Err(e) => apply_close_emit_policy(schedule, stats, limiter, e),
    }
}

/// Called on every committed `running → paused` transition AND on the
/// single tail exit of `gated_loop`.
async fn invoke_close_emit(
    schedule: &ParsedSchedule,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    limiter: &mut SinkErrorRateLimiter,
    close_emit: Option<&mut CloseEmitFn>,
    sink: &mut Box<dyn Sink>,
) -> Result<(), SondaError> {
    let Some(emit) = close_emit else {
        return Ok(());
    };
    let mut output = TickOutput::default();
    let outcome = match emit(&mut output) {
        Ok(()) => drain_writes(&mut output, sink).await.map(|_| ()),
        Err(e) => Err(e),
    };
    match outcome {
        Ok(()) => {
            let flush_result = sink.flush().await;
            apply_close_emit_policy_flush(schedule, stats, limiter, flush_result)?;
        }
        Err(e) => apply_close_emit_policy(schedule, stats, limiter, e)?,
    }
    Ok(())
}

/// Maximum time spent blocked on a gate edge before re-checking shutdown.
///
/// 100ms keeps shutdown responsive while paused without burning CPU on
/// shorter polls; debounce timers can shorten any individual wakeup.
const PAUSED_POLL_INTERVAL: Duration = Duration::from_millis(100);

/// Gate-side context attached to a `gated_loop` run.
#[non_exhaustive]
pub struct GateContext {
    /// Receiver for `after:` and/or `while:` edges from the upstream bus.
    pub gate_rx: GateReceiver,
    /// Snapshot of the upstream gate state at subscription time.
    pub initial: InitialState,
    /// Open / close debounce windows applied to `while:` transitions.
    pub delay: Option<DelayClause>,
    /// Whether this scenario carries an `after:` clause (drives Pending entry).
    pub has_after: bool,
    /// Whether this scenario carries a `while:` clause (drives Paused entries).
    pub has_while: bool,
    /// Optional close-emit hook invoked on every committed `running → paused`
    /// transition. Metric runners with a `RemoteWrite` sink set this; logs,
    /// histograms, and summaries leave it `None`.
    pub close_emit: Option<CloseEmitFn>,
    pub if_unresolved: Option<UnresolvedBehavior>,
    pub start_unresolved: bool,
    /// Whether a gate-close should land the scenario in `Held` (snap-to opt-in) instead of `Paused`.
    pub holds_on_close: bool,
}

impl GateContext {
    /// Construct a `GateContext` with required pieces; optional fields default to `None` / `false`.
    pub fn new(gate_rx: GateReceiver, initial: InitialState) -> Self {
        Self {
            gate_rx,
            initial,
            delay: None,
            has_after: false,
            has_while: false,
            close_emit: None,
            if_unresolved: None,
            start_unresolved: false,
            holds_on_close: false,
        }
    }

    pub fn with_delay(mut self, delay: Option<DelayClause>) -> Self {
        self.delay = delay;
        self
    }

    pub fn with_has_after(mut self, has_after: bool) -> Self {
        self.has_after = has_after;
        self
    }

    pub fn with_has_while(mut self, has_while: bool) -> Self {
        self.has_while = has_while;
        self
    }

    pub fn with_close_emit(mut self, close_emit: Option<CloseEmitFn>) -> Self {
        self.close_emit = close_emit;
        self
    }

    pub fn with_if_unresolved(mut self, mode: Option<UnresolvedBehavior>) -> Self {
        self.if_unresolved = mode;
        self
    }

    pub fn with_start_unresolved(mut self, start_unresolved: bool) -> Self {
        self.start_unresolved = start_unresolved;
        self
    }

    pub fn with_holds_on_close(mut self, holds_on_close: bool) -> Self {
        self.holds_on_close = holds_on_close;
        self
    }
}

/// Run a signal scenario through the four-state lifecycle gate.
///
/// The wrapper owns the `pending → running ↔ paused → finished` state
/// machine. Each `Running` segment delegates to a fresh
/// [`run_schedule_loop`] call. The deadline resets to `Instant::now()`
/// on resume so no catch-up burst fires, but the tick counter is
/// preserved across pauses: a generator that emitted N events before
/// pause continues from tick N on resume.
///
/// On `WhileClose` the wrapper breaks out of the inner loop via a
/// segment-scoped flag, transitions to `Paused`, and awaits the next
/// gate edge until either the gate reopens or shutdown arrives.
///
/// Stats updates: `state` is written on every transition. While paused,
/// `current_rate` is reset to 0.0 and `elapsed_secs` keeps wall-clocking
/// (the underlying `started_at` Instant inside `ScenarioHandle` runs
/// against wall time regardless of pause state).
pub(crate) async fn gated_loop(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    mut gate_ctx: GateContext,
    sink: &mut Box<dyn Sink>,
    tick_fn: &mut TickFn<'_>,
) -> Result<(), SondaError> {
    let mut close_warn_limiter = SinkErrorRateLimiter::new();

    let stats_for_flush = stats.clone();
    let body_result = gated_loop_body(
        schedule,
        rate,
        shutdown,
        stats.as_ref(),
        &mut gate_ctx,
        &mut close_warn_limiter,
        sink,
        tick_fn,
    )
    .await;
    let gated_result = match body_result {
        Ok(LoopExit::Shutdown | LoopExit::DurationExpired | LoopExit::UpstreamFinished) => {
            match invoke_close_emit(
                schedule,
                stats.as_ref(),
                &mut close_warn_limiter,
                gate_ctx.close_emit.as_mut(),
                sink,
            )
            .await
            {
                Ok(()) => finish(stats),
                Err(e) => Err(e),
            }
        }
        Err(e) => Err(e),
    };

    finalize_sink(schedule, stats_for_flush.as_ref(), sink, gated_result).await
}

#[allow(clippy::too_many_arguments)]
async fn gated_loop_body(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    gate_ctx: &mut GateContext,
    close_warn_limiter: &mut SinkErrorRateLimiter,
    sink: &mut Box<dyn Sink>,
    tick_fn: &mut TickFn<'_>,
) -> Result<LoopExit, SondaError> {
    let started_at = Instant::now();
    let wall = WallClock::resolve(schedule, started_at);

    let mut state = if gate_ctx.start_unresolved {
        ScenarioState::Unresolved
    } else {
        ScenarioState::Pending
    };
    let mut after_satisfied = if gate_ctx.has_after {
        gate_ctx.initial.after_already_fired
    } else {
        true
    };
    let mut while_open = if gate_ctx.has_while {
        gate_ctx.initial.while_gate_open.unwrap_or(false)
    } else {
        true
    };

    let mut debounce = DebounceState::from_clause(gate_ctx.delay.as_ref());

    let mut next_tick: u64 = 0;

    let paused_zero_rate = matches!(
        state,
        ScenarioState::Unresolved | ScenarioState::Paused | ScenarioState::Held
    );
    write_state(stats, state, paused_zero_rate);

    loop {
        if shutdown_requested(shutdown) {
            return Ok(LoopExit::Shutdown);
        }
        if duration_expired(schedule, started_at) {
            return Ok(LoopExit::DurationExpired);
        }

        match state {
            ScenarioState::Pending => {
                if !after_satisfied {
                    let wait = remaining_until(schedule, started_at, PAUSED_POLL_INTERVAL);
                    match gate_ctx.gate_rx.recv_edge_timeout(wait).await {
                        Some(GateEdge::AfterFired) => {
                            after_satisfied = true;
                        }
                        Some(GateEdge::WhileOpen) => {
                            while_open = true;
                        }
                        Some(GateEdge::WhileClose) => {
                            while_open = false;
                        }
                        Some(GateEdge::UpstreamGone) => {
                            if gate_ctx.if_unresolved.is_none() {
                                return Ok(LoopExit::UpstreamFinished);
                            }
                            state = ScenarioState::Unresolved;
                            while_open = false;
                            write_state(stats, ScenarioState::Unresolved, true);
                            debounce.reset();
                            continue;
                        }
                        None => {
                            continue;
                        }
                    }
                    continue;
                }
                if !gate_ctx.has_while {
                    state = ScenarioState::Running;
                    write_state(stats, ScenarioState::Running, false);
                    continue;
                }
                if while_open {
                    state = ScenarioState::Running;
                    write_state(stats, ScenarioState::Running, false);
                } else {
                    let close_state = close_target_state(gate_ctx.holds_on_close, stats);
                    state = close_state;
                    write_state(stats, close_state, true);
                }
            }
            ScenarioState::Running => {
                let segment_running = Arc::new(AtomicBool::new(true));
                let last_tick = Arc::new(AtomicU64::new(next_tick));
                let exit = run_running_segment(
                    schedule,
                    rate,
                    shutdown,
                    stats.cloned(),
                    gate_ctx,
                    &segment_running,
                    next_tick,
                    Arc::clone(&last_tick),
                    wall,
                    sink,
                    tick_fn,
                    false,
                )
                .await?;
                next_tick = last_tick.load(Ordering::SeqCst);

                if shutdown_requested(shutdown) {
                    return Ok(LoopExit::Shutdown);
                }
                if duration_expired(schedule, started_at) {
                    return Ok(LoopExit::DurationExpired);
                }
                if exit == SegmentExit::UpstreamGone {
                    if gate_ctx.if_unresolved.is_none() {
                        return Ok(LoopExit::UpstreamFinished);
                    }
                    invoke_close_emit(
                        schedule,
                        stats,
                        close_warn_limiter,
                        gate_ctx.close_emit.as_mut(),
                        sink,
                    )
                    .await?;
                    state = ScenarioState::Unresolved;
                    while_open = false;
                    write_state(stats, ScenarioState::Unresolved, true);
                    debounce.reset();
                    continue;
                }
                if exit == SegmentExit::WhileClose
                    && !debounce_close_to_paused(
                        schedule, started_at, shutdown, gate_ctx, &debounce,
                    )
                    .await
                {
                    while_open = true;
                    continue;
                }
                if exit == SegmentExit::WhileClose {
                    invoke_close_emit(
                        schedule,
                        stats,
                        close_warn_limiter,
                        gate_ctx.close_emit.as_mut(),
                        sink,
                    )
                    .await?;
                }
                let close_state = close_target_state(gate_ctx.holds_on_close, stats);
                state = close_state;
                while_open = false;
                write_state(stats, close_state, true);
                debounce.reset();
            }
            ScenarioState::Paused | ScenarioState::Held => {
                let now = Instant::now();
                let mut wakeup = PAUSED_POLL_INTERVAL;
                if let Some(d) = debounce.next_wakeup(now) {
                    wakeup = wakeup.min(d);
                }
                if let Some(remaining) = remaining_duration(schedule, started_at) {
                    wakeup = wakeup.min(remaining);
                }

                let recv = gate_ctx.gate_rx.recv_edge_timeout(wakeup).await;
                let now = Instant::now();
                match recv {
                    Some(GateEdge::WhileOpen) => {
                        while_open = true;
                        debounce.observe(GateEdge::WhileOpen, now);
                    }
                    Some(GateEdge::WhileClose) => {
                        while_open = false;
                        debounce.observe(GateEdge::WhileClose, now);
                    }
                    Some(GateEdge::AfterFired) => {
                        after_satisfied = true;
                    }
                    Some(GateEdge::UpstreamGone) => {
                        if gate_ctx.if_unresolved.is_none() {
                            return Ok(LoopExit::UpstreamFinished);
                        }
                        state = ScenarioState::Unresolved;
                        while_open = false;
                        write_state(stats, ScenarioState::Unresolved, true);
                        debounce.reset();
                        continue;
                    }
                    None => {}
                }

                if let Some(due) = debounce.fire_if_due(now) {
                    match due {
                        GateEdge::WhileOpen => {
                            if while_open {
                                state = ScenarioState::Running;
                                write_state(stats, ScenarioState::Running, false);
                            }
                        }
                        GateEdge::WhileClose => {}
                        GateEdge::AfterFired => {}
                        GateEdge::UpstreamGone => {}
                    }
                }
            }
            ScenarioState::Unresolved => {
                let mode = gate_ctx.if_unresolved.unwrap_or_default();
                match mode {
                    UnresolvedBehavior::Open => {
                        let segment_running = Arc::new(AtomicBool::new(true));
                        let last_tick = Arc::new(AtomicU64::new(next_tick));
                        let exit = run_running_segment(
                            schedule,
                            rate,
                            shutdown,
                            stats.cloned(),
                            gate_ctx,
                            &segment_running,
                            next_tick,
                            Arc::clone(&last_tick),
                            wall,
                            sink,
                            tick_fn,
                            true,
                        )
                        .await?;
                        next_tick = last_tick.load(Ordering::SeqCst);

                        if shutdown_requested(shutdown) {
                            return Ok(LoopExit::Shutdown);
                        }
                        if duration_expired(schedule, started_at) {
                            return Ok(LoopExit::DurationExpired);
                        }
                        match exit {
                            SegmentExit::WhileClose => {
                                invoke_close_emit(
                                    schedule,
                                    stats,
                                    close_warn_limiter,
                                    gate_ctx.close_emit.as_mut(),
                                    sink,
                                )
                                .await?;
                                let close_state =
                                    close_target_state(gate_ctx.holds_on_close, stats);
                                state = close_state;
                                while_open = false;
                                write_state(stats, close_state, true);
                                debounce.reset();
                            }
                            SegmentExit::UpstreamGone => {
                                // Already Unresolved; stay.
                            }
                            SegmentExit::WhileOpen => {
                                state = ScenarioState::Running;
                                while_open = true;
                                write_state(stats, ScenarioState::Running, false);
                                debounce.reset();
                            }
                            SegmentExit::ShutdownOrDuration => {}
                        }
                    }
                    UnresolvedBehavior::Closed | UnresolvedBehavior::Pending => {
                        let wakeup = remaining_until(schedule, started_at, PAUSED_POLL_INTERVAL);
                        match gate_ctx.gate_rx.recv_edge_timeout(wakeup).await {
                            Some(GateEdge::WhileOpen) => {
                                while_open = true;
                                state = ScenarioState::Pending;
                                write_state(stats, ScenarioState::Pending, false);
                                debounce.reset();
                            }
                            Some(GateEdge::WhileClose) => {
                                while_open = false;
                                let close_state =
                                    close_target_state(gate_ctx.holds_on_close, stats);
                                state = close_state;
                                write_state(stats, close_state, true);
                                debounce.reset();
                            }
                            Some(GateEdge::AfterFired) => {
                                after_satisfied = true;
                            }
                            Some(GateEdge::UpstreamGone) => {}
                            None => {}
                        }
                    }
                }
            }
            ScenarioState::Finished => {
                // Structurally dead; kept so `match state` stays exhaustive.
                return Ok(LoopExit::Shutdown);
            }
        }
    }
}

fn close_target_state(
    holds_on_close: bool,
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
) -> ScenarioState {
    if !holds_on_close {
        return ScenarioState::Paused;
    }
    let has_emitted = stats
        .and_then(|s| s.read().ok().map(|st| !st.current_values.is_empty()))
        .unwrap_or(false);
    if has_emitted {
        ScenarioState::Held
    } else {
        ScenarioState::Paused
    }
}

fn shutdown_requested(shutdown: Option<&AtomicBool>) -> bool {
    shutdown.map(|f| !f.load(Ordering::SeqCst)).unwrap_or(false)
}

fn duration_expired(schedule: &ParsedSchedule, started_at: Instant) -> bool {
    schedule
        .total_duration
        .map(|total| started_at.elapsed() >= total)
        .unwrap_or(false)
}

fn remaining_duration(schedule: &ParsedSchedule, started_at: Instant) -> Option<Duration> {
    schedule.total_duration.map(|total| {
        let elapsed = started_at.elapsed();
        if elapsed >= total {
            Duration::ZERO
        } else {
            total - elapsed
        }
    })
}

fn remaining_until(schedule: &ParsedSchedule, started_at: Instant, default: Duration) -> Duration {
    match remaining_duration(schedule, started_at) {
        Some(r) => r.min(default),
        None => default,
    }
}

fn write_state(
    stats: Option<&Arc<RwLock<ScenarioStats>>>,
    state: ScenarioState,
    paused_zero_rate: bool,
) {
    if let Some(s) = stats {
        if let Ok(mut st) = s.write() {
            if st.state != state {
                st.transition_state(state);
            }
            if paused_zero_rate {
                st.current_rate = 0.0;
            }
        }
    }
}

// INVARIANT: only callable from the tail of `gated_loop`. The body cannot
// reach it — `gated_loop_body` returns `Result<LoopExit, _>` by construction
// and has no way to produce `Result<(), _>` directly. Adding a caller
// elsewhere bypasses the close-emit flush.
fn finish(stats: Option<Arc<RwLock<ScenarioStats>>>) -> Result<(), SondaError> {
    if let Some(s) = stats {
        if let Ok(mut st) = s.write() {
            if st.state != ScenarioState::Finished {
                st.transition_state(ScenarioState::Finished);
            }
        }
    }
    Ok(())
}

/// Reason a `Running` segment exited.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SegmentExit {
    /// Upstream gate transitioned to closed.
    WhileClose,
    /// Upstream scenario was removed mid-segment.
    UpstreamGone,
    /// Upstream gate transitioned to open while running under `if_unresolved: open`.
    WhileOpen,
    /// User-shutdown flag cleared, or scenario duration expired.
    ShutdownOrDuration,
}

/// Reason the `gated_loop` body terminated cleanly.
///
/// The body cannot call `finish(stats)` directly — it can only return a
/// `LoopExit`, and the outer `gated_loop` is the sole site that pairs
/// `invoke_close_emit` with `finish(stats)`. Adding a new clean-exit
/// reason means adding a variant here; the compiler will then point at
/// the tail's match and force the contributor to think about close-emit.
///
/// Errors are NOT modeled as a variant: they propagate through the body's
/// `Result<LoopExit, SondaError>` return type so the type system carries
/// the "did this exit cleanly?" signal end-to-end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LoopExit {
    Shutdown,
    DurationExpired,
    UpstreamFinished,
}

/// Wait `delay.close` for either a fresh `WhileOpen` (cancel) or the
/// debounce timer to fire (commit). Returns `true` when the transition
/// to `Paused` should commit, `false` when the close was cancelled by a
/// reopen within the debounce window.
async fn debounce_close_to_paused(
    schedule: &ParsedSchedule,
    started_at: Instant,
    shutdown: Option<&AtomicBool>,
    gate_ctx: &mut GateContext,
    debounce: &DebounceState,
) -> bool {
    if debounce.delay_close.is_zero() {
        return true;
    }

    let deadline = Instant::now() + debounce.delay_close;
    loop {
        if shutdown_requested(shutdown) || duration_expired(schedule, started_at) {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return true;
        }
        let mut wait = (deadline - now).min(PAUSED_POLL_INTERVAL);
        if let Some(remaining) = remaining_duration(schedule, started_at) {
            wait = wait.min(remaining);
        }
        match gate_ctx.gate_rx.recv_edge_timeout(wait).await {
            Some(GateEdge::WhileOpen) => return false,
            Some(GateEdge::WhileClose) => {}
            Some(GateEdge::AfterFired) => {}
            Some(GateEdge::UpstreamGone) => return true,
            None => {}
        }
    }
}

/// Run one `Running` segment: a fresh `run_schedule_loop` with a wrapped
/// `tick_fn` that polls the gate channel after every successful tick.
/// On `WhileClose` the segment_running flag is cleared so the inner loop
/// exits at its top-of-loop shutdown check.
///
/// `initial_tick` seeds the inner loop's tick counter on resume; `last_tick`
/// captures the next tick the inner loop would have fired so the next segment
/// continues from there.
#[allow(clippy::too_many_arguments)]
async fn run_running_segment(
    schedule: &ParsedSchedule,
    rate: f64,
    shutdown: Option<&AtomicBool>,
    stats: Option<Arc<RwLock<ScenarioStats>>>,
    gate_ctx: &mut GateContext,
    segment_running: &Arc<AtomicBool>,
    initial_tick: u64,
    last_tick: Arc<AtomicU64>,
    wall: WallClock,
    sink: &mut Box<dyn Sink>,
    tick_fn: &mut TickFn<'_>,
    exit_on_while_open: bool,
) -> Result<SegmentExit, SondaError> {
    let saw_close = Arc::new(AtomicBool::new(false));
    let saw_gone = Arc::new(AtomicBool::new(false));
    let saw_open = Arc::new(AtomicBool::new(false));

    // The inner loop's `shutdown` parameter wants "true = keep running."
    // We pass our segment flag, and we additionally drain the user
    // shutdown into the segment flag inside the wrapped tick.
    let user_shutdown_for_wrapper = shutdown;
    let segment_for_wrapper = Arc::clone(segment_running);
    let saw_close_for_wrapper = Arc::clone(&saw_close);
    let saw_gone_for_wrapper = Arc::clone(&saw_gone);
    let saw_open_for_wrapper = Arc::clone(&saw_open);
    let gate_rx = &mut gate_ctx.gate_rx;

    type WrappedTick<'a> = Box<
        dyn FnMut(
                &TickContext<'_>,
                &mut TickOutput,
                &mut Vec<MetricEvent>,
            ) -> Result<TickResult, SondaError>
            + 'a,
    >;
    let mut wrapped: WrappedTick<'_> = Box::new(
        move |ctx: &TickContext<'_>,
              output: &mut TickOutput,
              events_buf: &mut Vec<MetricEvent>|
              -> Result<TickResult, SondaError> {
            let outcome = tick_fn(ctx, output, events_buf);

            // Poll for gate edges after the tick. On WhileClose, break out.
            while let Some(edge) = gate_rx.try_recv() {
                match edge {
                    GateEdge::WhileClose => {
                        saw_close_for_wrapper.store(true, Ordering::SeqCst);
                        segment_for_wrapper.store(false, Ordering::SeqCst);
                    }
                    GateEdge::WhileOpen => {
                        if exit_on_while_open {
                            saw_open_for_wrapper.store(true, Ordering::SeqCst);
                            segment_for_wrapper.store(false, Ordering::SeqCst);
                        }
                    }
                    GateEdge::AfterFired => {
                        // Already past the after gate.
                    }
                    GateEdge::UpstreamGone => {
                        saw_gone_for_wrapper.store(true, Ordering::SeqCst);
                        segment_for_wrapper.store(false, Ordering::SeqCst);
                    }
                }
            }

            // Honor user shutdown immediately (don't wait for next loop iter).
            if let Some(user) = user_shutdown_for_wrapper {
                if !user.load(Ordering::SeqCst) {
                    segment_for_wrapper.store(false, Ordering::SeqCst);
                }
            }

            outcome
        },
    );

    run_schedule_loop_with_initial_tick(
        schedule,
        rate,
        Some(segment_running.as_ref()),
        stats,
        initial_tick,
        Some(last_tick.as_ref()),
        Some(wall),
        sink,
        wrapped.as_mut(),
    )
    .await?;

    Ok(if saw_gone.load(Ordering::SeqCst) {
        SegmentExit::UpstreamGone
    } else if saw_close.load(Ordering::SeqCst) {
        SegmentExit::WhileClose
    } else if saw_open.load(Ordering::SeqCst) {
        SegmentExit::WhileOpen
    } else {
        SegmentExit::ShutdownOrDuration
    })
}

/// Open / close debounce timers for `while:` transitions.
struct DebounceState {
    delay_open: Duration,
    delay_close: Duration,
    pending_open_at: Option<Instant>,
    pending_close_at: Option<Instant>,
}

impl DebounceState {
    fn from_clause(clause: Option<&DelayClause>) -> Self {
        let (delay_open, delay_close) = match clause {
            Some(c) => (
                c.open.unwrap_or(Duration::ZERO),
                c.close.unwrap_or(Duration::ZERO),
            ),
            None => (Duration::ZERO, Duration::ZERO),
        };
        Self {
            delay_open,
            delay_close,
            pending_open_at: None,
            pending_close_at: None,
        }
    }

    fn observe(&mut self, edge: GateEdge, now: Instant) {
        match edge {
            GateEdge::WhileOpen => {
                self.pending_close_at = None;
                if self.delay_open.is_zero() {
                    self.pending_open_at = Some(now);
                } else {
                    self.pending_open_at = Some(now + self.delay_open);
                }
            }
            GateEdge::WhileClose => {
                self.pending_open_at = None;
                if self.delay_close.is_zero() {
                    self.pending_close_at = Some(now);
                } else {
                    self.pending_close_at = Some(now + self.delay_close);
                }
            }
            GateEdge::AfterFired => {}
            GateEdge::UpstreamGone => {}
        }
    }

    fn next_wakeup(&self, now: Instant) -> Option<Duration> {
        let mut soonest: Option<Duration> = None;
        for t in [self.pending_open_at, self.pending_close_at]
            .into_iter()
            .flatten()
        {
            let d = t.saturating_duration_since(now);
            soonest = Some(match soonest {
                Some(s) => s.min(d),
                None => d,
            });
        }
        soonest
    }

    fn fire_if_due(&mut self, now: Instant) -> Option<GateEdge> {
        if let Some(t) = self.pending_open_at {
            if now >= t {
                self.pending_open_at = None;
                return Some(GateEdge::WhileOpen);
            }
        }
        if let Some(t) = self.pending_close_at {
            if now >= t {
                self.pending_close_at = None;
                return Some(GateEdge::WhileClose);
            }
        }
        None
    }

    fn reset(&mut self) {
        self.pending_open_at = None;
        self.pending_close_at = None;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use async_trait::async_trait;

    use super::*;
    use crate::schedule::{BurstWindow, GapWindow};

    type WriteLog = Arc<Mutex<Vec<Vec<u8>>>>;

    /// Shared-buffer test sink that lets tests inspect captured writes.
    struct SharedSink {
        writes: WriteLog,
        fail_at: Option<usize>,
    }

    fn shared_sink() -> (Box<dyn Sink>, WriteLog) {
        let writes: WriteLog = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink {
            writes: Arc::clone(&writes),
            fail_at: None,
        };
        (Box::new(sink) as Box<dyn Sink>, writes)
    }

    fn shared_sink_failing(fail_at: usize) -> (Box<dyn Sink>, WriteLog) {
        let writes: WriteLog = Arc::new(Mutex::new(Vec::new()));
        let sink = SharedSink {
            writes: Arc::clone(&writes),
            fail_at: Some(fail_at),
        };
        (Box::new(sink) as Box<dyn Sink>, writes)
    }

    #[async_trait]
    impl Sink for SharedSink {
        async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
            let mut writes = self.writes.lock().expect("shared sink mutex poisoned");
            if let Some(n) = self.fail_at {
                if writes.len() == n {
                    return Err(SondaError::Sink(std::io::Error::other(
                        "capturing sink test failure",
                    )));
                }
            }
            writes.push(data.to_vec());
            Ok(())
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            Ok(())
        }
    }

    struct NullSink;

    #[async_trait]
    impl Sink for NullSink {
        async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
            Ok(())
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            Ok(())
        }
    }

    fn null_sink() -> Box<dyn Sink> {
        Box::new(NullSink) as Box<dyn Sink>
    }

    #[tokio::test]
    async fn drain_writes_preserves_command_order() {
        let (mut sink, writes) = shared_sink();
        let mut output = TickOutput::default();
        output.writes.push(WriteCommand::Bytes(b"first".to_vec()));
        output.writes.push(WriteCommand::Bytes(b"second".to_vec()));
        output.writes.push(WriteCommand::Bytes(b"third".to_vec()));
        let delivered = drain_writes(&mut output, &mut sink)
            .await
            .expect("drain must succeed");
        let captured = writes.lock().unwrap().clone();
        assert_eq!(
            captured,
            vec![b"first".to_vec(), b"second".to_vec(), b"third".to_vec()]
        );
        assert!(output.writes.is_empty(), "queue must be empty after drain");
        assert_eq!(delivered, Some(true));
    }

    #[tokio::test]
    async fn drain_writes_short_circuits_on_sink_error() {
        let (mut sink, writes) = shared_sink_failing(1);
        let mut output = TickOutput::default();
        output.writes.push(WriteCommand::Bytes(b"first".to_vec()));
        output.writes.push(WriteCommand::Bytes(b"second".to_vec()));
        output.writes.push(WriteCommand::Bytes(b"third".to_vec()));
        let result = drain_writes(&mut output, &mut sink).await;
        assert!(
            result.is_err(),
            "sink error must propagate from drain_writes"
        );
        assert_eq!(
            *writes.lock().unwrap(),
            vec![b"first".to_vec()],
            "only writes before the error must reach the sink"
        );
    }

    struct PanicSink;

    #[async_trait]
    impl Sink for PanicSink {
        async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
            panic!("synthetic sink panic");
        }
        async fn flush(&mut self) -> Result<(), SondaError> {
            panic!("synthetic flush panic");
        }
    }

    #[tokio::test]
    #[should_panic(expected = "synthetic sink panic")]
    async fn drain_writes_panic_in_sink_propagates() {
        let mut sink: Box<dyn Sink> = Box::new(PanicSink);
        let mut output = TickOutput::default();
        output
            .writes
            .push(WriteCommand::Bytes(b"panic-me".to_vec()));
        let _ = drain_writes(&mut output, &mut sink).await;
    }

    #[tokio::test]
    #[should_panic(expected = "synthetic flush panic")]
    async fn finalize_sink_panic_in_flush_propagates() {
        let mut sink: Box<dyn Sink> = Box::new(PanicSink);
        let schedule = minimal_schedule(None);
        let _ = finalize_sink(&schedule, None, &mut sink, Ok(())).await;
    }

    #[test]
    fn loop_exit_variants_are_exhaustive_for_clean_exits() {
        fn assert_clean_exit_match(le: LoopExit) {
            match le {
                LoopExit::Shutdown => {}
                LoopExit::DurationExpired => {}
                LoopExit::UpstreamFinished => {}
            }
        }
        assert_clean_exit_match(LoopExit::Shutdown);
        assert_clean_exit_match(LoopExit::DurationExpired);
        assert_clean_exit_match(LoopExit::UpstreamFinished);
    }

    /// Build a minimal ParsedSchedule for testing.
    fn minimal_schedule(duration: Option<Duration>) -> ParsedSchedule {
        ParsedSchedule {
            total_duration: duration,
            gap_window: None,
            burst_window: None,
            spike_windows: Vec::new(),
            dynamic_labels: Vec::new(),
            on_sink_error: OnSinkError::Warn,
            name: "test".to_string(),
            start_time: crate::config::validate::StartTime::Now,
        }
    }

    // ---- Basic loop: runs for duration, emits events -------------------------

    /// The loop emits events at the configured rate for the configured duration.
    #[tokio::test]
    async fn loop_emits_events_for_duration() {
        let schedule = minimal_schedule(Some(Duration::from_millis(500)));

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 6,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            20.0, // 20 events/sec for 500ms = ~10 events
            None,
            None,
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        assert!(
            event_count > 5,
            "expected ~10 events at 20/s for 500ms, got {event_count}"
        );
        assert!(
            event_count < 20,
            "expected ~10 events, got {event_count} (too many)"
        );
    }

    // ---- Shutdown flag: stops the loop early --------------------------------

    /// Clearing the shutdown flag stops the loop before duration expires.
    #[tokio::test]
    async fn loop_stops_on_shutdown_flag() {
        use std::sync::atomic::AtomicBool;

        let schedule = minimal_schedule(None); // indefinite
        let mut event_count: u64 = 0;

        // Spawn a thread to clear the flag after 200ms.
        let shutdown_arc = Arc::new(AtomicBool::new(true));
        let flag_clone = Arc::clone(&shutdown_arc);
        let handle = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(200));
            flag_clone.store(false, Ordering::SeqCst);
        });

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            Some(shutdown_arc.as_ref()),
            None,
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        handle.join().expect("thread must complete");

        assert!(
            event_count > 0,
            "some events should have been emitted before shutdown"
        );
    }

    // ---- Gap window: suppresses events during gap ---------------------------

    /// Events are suppressed during a gap window.
    #[tokio::test]
    async fn loop_suppresses_events_during_gap() {
        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_secs(2)),
            gap_window: Some(GapWindow {
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9), // gap from 1s to 10s
            }),
            burst_window: None,
            spike_windows: Vec::new(),
            dynamic_labels: Vec::new(),
            on_sink_error: OnSinkError::Warn,
            name: "test".to_string(),
            start_time: crate::config::validate::StartTime::Now,
        };

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(&schedule, 100.0, None, None, &mut sink, &mut tick_fn)
            .await
            .expect("loop must succeed");

        // Only ~100 events from the first 1s before the gap kicks in.
        assert!(
            event_count < 150,
            "gap should suppress events: expected < 150, got {event_count}"
        );
    }

    // ---- Burst window: increases event rate ---------------------------------

    /// Burst window increases the effective rate.
    #[tokio::test]
    async fn loop_increases_rate_during_burst() {
        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_secs(1)),
            gap_window: None,
            burst_window: Some(BurstWindow {
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9), // burst covers full 1s run
                multiplier: 5.0,
            }),
            spike_windows: Vec::new(),
            dynamic_labels: Vec::new(),
            on_sink_error: OnSinkError::Warn,
            name: "test".to_string(),
            start_time: crate::config::validate::StartTime::Now,
        };

        let mut event_count: u64 = 0;
        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            event_count += 1;
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(&schedule, 10.0, None, None, &mut sink, &mut tick_fn)
            .await
            .expect("loop must succeed");

        // Without burst: ~10 events. With 5x burst: ~50 events.
        assert!(
            event_count > 15,
            "burst should increase event count: expected >15, got {event_count}"
        );
    }

    // ---- Stats tracking: updates stats arc ----------------------------------

    /// Stats are updated correctly when a stats arc is provided.
    #[tokio::test]
    async fn loop_updates_stats() {
        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Ok(TickResult {
                bytes_written: 42,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            st.total_events > 0,
            "stats must track total_events, got {}",
            st.total_events
        );
        assert!(
            st.bytes_emitted > 0,
            "stats must track bytes_emitted, got {}",
            st.bytes_emitted
        );
    }

    // ---- Stats tracking: metric events pushed to buffer ---------------------

    /// When the tick callback returns a MetricEvent, it is pushed to the stats buffer.
    #[tokio::test]
    async fn loop_pushes_metric_events_to_stats_buffer() {
        use crate::model::metric::{Labels, MetricEvent};

        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            let event = MetricEvent::new("test".to_string(), 1.0, Labels::default())
                .expect("valid metric name");
            events_buf.push(event);
            Ok(TickResult {
                bytes_written: 10,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let st = stats.read().expect("lock must not be poisoned");
        assert!(
            !st.current_values.is_empty(),
            "stats current_values must contain at least one series"
        );
    }

    // ---- Tick context: spike windows are passed to callback -----------------

    /// The tick callback receives spike windows in the context.
    #[tokio::test]
    async fn loop_passes_spike_windows_to_tick_fn() {
        use crate::config::SpikeStrategy;
        use crate::schedule::CardinalitySpikeWindow;

        let schedule = ParsedSchedule {
            total_duration: Some(Duration::from_millis(100)),
            gap_window: None,
            burst_window: None,
            spike_windows: vec![CardinalitySpikeWindow {
                label: "pod".to_string(),
                every: Duration::from_secs(10),
                duration: Duration::from_secs(9),
                cardinality: 5,
                strategy: SpikeStrategy::Counter,
                prefix: "pod-".to_string(),
                seed: 0,
            }],
            dynamic_labels: Vec::new(),
            on_sink_error: OnSinkError::Warn,
            name: "test".to_string(),
            start_time: crate::config::validate::StartTime::Now,
        };

        let mut saw_spike_windows = false;
        let mut tick_fn = |ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            if !ctx.spike_windows.is_empty() {
                saw_spike_windows = true;
            }
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(&schedule, 100.0, None, None, &mut sink, &mut tick_fn)
            .await
            .expect("loop must succeed");

        assert!(
            saw_spike_windows,
            "tick callback must receive spike windows"
        );
    }

    // ---- Error propagation: encoder errors propagate regardless of policy ----

    #[tokio::test]
    async fn loop_propagates_encoder_error_under_warn_policy() {
        let schedule = minimal_schedule(Some(Duration::from_secs(10)));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Err(SondaError::Encoder(crate::EncoderError::NotSupported(
                "synthetic encoder failure".to_string(),
            )))
        };

        let mut sink = null_sink();
        let result = run_schedule_loop(&schedule, 10.0, None, None, &mut sink, &mut tick_fn).await;

        assert!(
            matches!(result, Err(SondaError::Encoder(_))),
            "encoder errors must propagate regardless of sink-error policy"
        );
    }

    #[tokio::test]
    async fn loop_propagates_sink_error_under_fail_policy() {
        let mut schedule = minimal_schedule(Some(Duration::from_secs(10)));
        schedule.on_sink_error = OnSinkError::Fail;

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Err(SondaError::Sink(std::io::Error::other("test error")))
        };

        let mut sink = null_sink();
        let result = run_schedule_loop(&schedule, 10.0, None, None, &mut sink, &mut tick_fn).await;

        assert!(
            matches!(result, Err(SondaError::Sink(_))),
            "Fail policy must propagate sink errors"
        );
    }

    #[tokio::test]
    async fn fail_policy_records_stats_before_propagating() {
        let mut schedule = minimal_schedule(Some(Duration::from_secs(10)));
        schedule.on_sink_error = OnSinkError::Fail;
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::ConnectionRefused,
                "fail-before-die",
            )))
        };

        let mut sink = null_sink();
        let result = run_schedule_loop(
            &schedule,
            10.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await;

        assert!(
            matches!(result, Err(SondaError::Sink(_))),
            "Fail policy must still propagate the sink error"
        );

        let st = stats.read().expect("stats lock");
        assert_eq!(st.errors, 1, "errors must be incremented before exit");
        assert_eq!(
            st.total_sink_failures, 1,
            "total_sink_failures must be incremented before exit"
        );
        assert_eq!(
            st.consecutive_failures, 1,
            "consecutive_failures must be incremented before exit"
        );
        assert!(
            st.last_sink_error.is_some(),
            "last_sink_error must be populated before exit"
        );
        assert!(
            st.last_sink_error
                .as_ref()
                .unwrap()
                .contains("fail-before-die"),
            "last_sink_error must carry the io error message"
        );
    }

    #[tokio::test]
    async fn loop_swallows_sink_error_under_warn_policy_and_continues() {
        // 200ms run with rate=50: ~10 ticks. All return sink errors. Loop
        // must complete without propagating.
        let schedule = minimal_schedule(Some(Duration::from_millis(200)));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Err(SondaError::Sink(std::io::Error::other("transient")))
        };

        let mut sink = null_sink();
        let result = run_schedule_loop(&schedule, 50.0, None, None, &mut sink, &mut tick_fn).await;

        assert!(
            result.is_ok(),
            "Warn policy must swallow sink errors and complete: {result:?}"
        );
    }

    #[tokio::test]
    async fn warn_policy_updates_sink_failure_stats() {
        let schedule = minimal_schedule(Some(Duration::from_millis(150)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Err(SondaError::Sink(std::io::Error::new(
                std::io::ErrorKind::ConnectionReset,
                "boom",
            )))
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("warn policy must complete");

        let st = stats.read().expect("stats lock");
        assert!(
            st.total_sink_failures > 0,
            "total_sink_failures must be > 0"
        );
        assert_eq!(
            st.consecutive_failures, st.total_sink_failures,
            "no successful writes — consecutive == total"
        );
        assert!(st.last_sink_error.is_some(), "last_sink_error must be Some");
        assert_eq!(
            st.last_successful_write_at, None,
            "no successful write happened, must remain None"
        );
        assert!(st.errors > 0, "errors counter must increment too");
    }

    #[tokio::test]
    async fn alternating_ok_err_resets_consecutive_failures() {
        let schedule = minimal_schedule(Some(Duration::from_millis(300)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let mut counter: u64 = 0;

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            counter += 1;
            if counter.is_multiple_of(2) {
                Ok(TickResult {
                    bytes_written: 8,
                    delivered: true,
                })
            } else {
                Err(SondaError::Sink(std::io::Error::other("alt")))
            }
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("warn must succeed");

        let st = stats.read().expect("stats lock");
        assert!(
            st.consecutive_failures <= 1,
            "consecutive_failures must reset on Ok, got {}",
            st.consecutive_failures
        );
        assert!(st.total_sink_failures > 0);
        assert!(st.total_events > 0);
        assert!(st.last_successful_write_at.is_some());
    }

    #[tokio::test]
    async fn buffered_write_does_not_update_delivery_health_stats() {
        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Ok(TickResult {
                bytes_written: 12,
                delivered: false,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let st = stats.read().expect("stats lock");
        assert!(
            st.total_events > 0,
            "buffered writes still count as generated events"
        );
        assert!(
            st.bytes_emitted > 0,
            "buffered writes still count toward bytes_emitted"
        );
        assert_eq!(
            st.last_successful_write_at, None,
            "a buffered write must not advance last_successful_write_at"
        );
        assert_eq!(
            st.consecutive_failures, 0,
            "a buffered write must not touch consecutive_failures"
        );
    }

    #[tokio::test]
    async fn delivered_write_updates_delivery_health_stats() {
        let schedule = minimal_schedule(Some(Duration::from_millis(200)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Ok(TickResult {
                bytes_written: 12,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let st = stats.read().expect("stats lock");
        assert!(st.total_events > 0, "delivered writes count as events");
        assert!(
            st.last_successful_write_at.is_some(),
            "a delivered write must advance last_successful_write_at"
        );
        assert_eq!(
            st.consecutive_failures, 0,
            "a delivered write keeps consecutive_failures at zero"
        );
    }

    // ---- Rate limiter unit tests --------------------------------------------

    #[test]
    fn rate_limiter_emits_first_error_immediately() {
        let mut limiter = SinkErrorRateLimiter::new();
        let err = std::io::Error::other("first");
        limiter.observe("scenario_a", &err);
        assert!(
            limiter.last_emit.is_some(),
            "first call must emit and record timestamp"
        );
    }

    #[test]
    fn rate_limiter_suppresses_subsequent_errors_within_interval() {
        let mut limiter = SinkErrorRateLimiter::new();
        let err = std::io::Error::other("boom");
        for _ in 0..1000 {
            limiter.observe("scenario_b", &err);
        }
        // The first call emits and resets count to 0; subsequent 999 calls
        // accumulate without emitting.
        assert_eq!(
            limiter.suppressed_count, 999,
            "999 errors must be suppressed after the first emission"
        );
    }

    // ---- rstest matrix: policy × error variant ------------------------------

    #[rustfmt::skip]
    #[rstest::rstest]
    #[case::warn_sink_continues(   OnSinkError::Warn, ErrKind::Sink,    PolicyOutcome::Ok)]
    #[case::fail_sink_propagates(  OnSinkError::Fail, ErrKind::Sink,    PolicyOutcome::SinkErr)]
    #[case::warn_encoder_propagates(OnSinkError::Warn, ErrKind::Encoder, PolicyOutcome::EncoderErr)]
    #[case::fail_encoder_propagates(OnSinkError::Fail, ErrKind::Encoder, PolicyOutcome::EncoderErr)]
    #[case::warn_config_propagates( OnSinkError::Warn, ErrKind::Config,  PolicyOutcome::ConfigErr)]
    #[case::fail_config_propagates( OnSinkError::Fail, ErrKind::Config,  PolicyOutcome::ConfigErr)]
    fn policy_matrix(
        #[case] policy: OnSinkError,
        #[case] err_kind: ErrKind,
        #[case] expected: PolicyOutcome,
    ) {
        let mut schedule = minimal_schedule(Some(Duration::from_millis(150)));
        schedule.on_sink_error = policy;

        let mut tick_fn = |_ctx: &TickContext<'_>, _output: &mut TickOutput, _events_buf: &mut Vec<MetricEvent>| -> Result<TickResult, SondaError> {
            match err_kind {
                ErrKind::Sink => Err(SondaError::Sink(std::io::Error::other(
                    "matrix",
                ))),
                ErrKind::Encoder => Err(SondaError::Encoder(crate::EncoderError::NotSupported(
                    "matrix".to_string(),
                ))),
                ErrKind::Config => Err(SondaError::Config(crate::ConfigError::invalid("matrix"))),
            }
        };

        let mut sink = null_sink();
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        let result = rt.block_on(run_schedule_loop(
            &schedule, 30.0, None, None, &mut sink, &mut tick_fn,
        ));

        match expected {
            PolicyOutcome::Ok => assert!(result.is_ok(), "must complete: {result:?}"),
            PolicyOutcome::SinkErr => {
                assert!(matches!(result, Err(SondaError::Sink(_))), "got {result:?}")
            }
            PolicyOutcome::EncoderErr => assert!(
                matches!(result, Err(SondaError::Encoder(_))),
                "got {result:?}"
            ),
            PolicyOutcome::ConfigErr => assert!(
                matches!(result, Err(SondaError::Config(_))),
                "got {result:?}"
            ),
        }
    }

    #[derive(Clone, Copy)]
    enum ErrKind {
        Sink,
        Encoder,
        Config,
    }

    #[derive(Clone, Copy)]
    enum PolicyOutcome {
        Ok,
        SinkErr,
        EncoderErr,
        ConfigErr,
    }

    // ---- apply_flush_policy --------------------------------------------------

    #[test]
    fn apply_flush_policy_warn_swallows_sink_error() {
        let mut schedule = minimal_schedule(None);
        schedule.on_sink_error = OnSinkError::Warn;
        let flush_err = Err(SondaError::Sink(std::io::Error::other("flush")));
        let out = apply_flush_policy(&schedule, None, flush_err);
        assert!(out.is_ok(), "warn policy must swallow flush sink errors");
    }

    #[test]
    fn apply_flush_policy_fail_propagates_sink_error() {
        let mut schedule = minimal_schedule(None);
        schedule.on_sink_error = OnSinkError::Fail;
        let flush_err = Err(SondaError::Sink(std::io::Error::other("flush")));
        let out = apply_flush_policy(&schedule, None, flush_err);
        assert!(matches!(out, Err(SondaError::Sink(_))));
    }

    #[test]
    fn apply_flush_policy_propagates_non_sink_errors() {
        let schedule = minimal_schedule(None);
        let flush_err = Err(SondaError::Encoder(crate::EncoderError::NotSupported(
            "non-sink".to_string(),
        )));
        let out = apply_flush_policy(&schedule, None, flush_err);
        assert!(matches!(out, Err(SondaError::Encoder(_))));
    }

    // ---- Contract: TickResult fields ----------------------------------------

    #[tokio::test]
    async fn run_schedule_loop_with_initial_tick_seeds_first_tick_value() {
        let schedule = minimal_schedule(Some(Duration::from_millis(150)));
        let observed_first = std::sync::Mutex::new(None::<u64>);

        let mut tick_fn = |ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            let mut g = observed_first.lock().unwrap();
            if g.is_none() {
                *g = Some(ctx.tick);
            }
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop_with_initial_tick(
            &schedule,
            50.0,
            None,
            None,
            30,
            None,
            None,
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        assert_eq!(
            *observed_first.lock().unwrap(),
            Some(30),
            "first tick must equal initial_tick when initial_tick > 0"
        );
    }

    #[tokio::test]
    async fn run_schedule_loop_with_initial_tick_reports_last_tick() {
        let schedule = minimal_schedule(Some(Duration::from_millis(150)));
        let last_tick = AtomicU64::new(0);

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           _events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            Ok(TickResult {
                bytes_written: 0,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop_with_initial_tick(
            &schedule,
            50.0,
            None,
            None,
            10,
            Some(&last_tick),
            None,
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let final_tick = last_tick.load(Ordering::SeqCst);
        assert!(
            final_tick > 10,
            "last_tick must advance past initial_tick, got {final_tick}"
        );
    }

    #[test]
    fn tick_result_carries_all_fields() {
        let result = TickResult {
            bytes_written: 100,
            delivered: true,
        };

        assert_eq!(result.bytes_written, 100);
        assert!(result.delivered);
    }

    #[tokio::test]
    async fn events_buf_capacity_does_not_grow_under_metrics_workload() {
        use crate::model::metric::{Labels, MetricEvent};
        use std::cell::Cell;

        let schedule = minimal_schedule(Some(Duration::from_millis(500)));
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let first_ptr: Cell<Option<usize>> = Cell::new(None);

        let mut tick_fn = |_ctx: &TickContext<'_>,
                           _output: &mut TickOutput,
                           events_buf: &mut Vec<MetricEvent>|
         -> Result<TickResult, SondaError> {
            let event =
                MetricEvent::new("test".to_string(), 1.0, Labels::default()).expect("valid name");
            assert_eq!(
                events_buf.len(),
                0,
                "events_buf must be cleared by the loop before each tick"
            );
            assert_eq!(
                events_buf.capacity(),
                16,
                "events_buf capacity must stay at the pre-allocated 16 for a 1-event-per-tick workload"
            );
            let ptr = events_buf.as_ptr() as usize;
            match first_ptr.get() {
                None => first_ptr.set(Some(ptr)),
                Some(p) => assert_eq!(
                    ptr, p,
                    "events_buf backing allocation must be reused across ticks"
                ),
            }
            events_buf.push(event);
            Ok(TickResult {
                bytes_written: 10,
                delivered: true,
            })
        };

        let mut sink = null_sink();
        run_schedule_loop(
            &schedule,
            50.0,
            None,
            Some(Arc::clone(&stats)),
            &mut sink,
            &mut tick_fn,
        )
        .await
        .expect("loop must succeed");

        let st = stats.read().expect("lock");
        assert!(
            st.total_events > 1,
            "loop must have executed multiple ticks to exercise the buffer reuse"
        );
        assert!(
            first_ptr.get().is_some(),
            "tick_fn must have observed at least one events_buf pointer"
        );
    }

    #[test]
    fn close_target_state_paused_when_holds_off() {
        assert_eq!(close_target_state(false, None), ScenarioState::Paused);
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        assert_eq!(
            close_target_state(false, Some(&stats)),
            ScenarioState::Paused
        );
    }

    #[test]
    fn close_target_state_paused_when_holds_on_but_no_samples() {
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        assert_eq!(
            close_target_state(true, Some(&stats)),
            ScenarioState::Paused,
            "snap_to with zero emissions must remain Paused"
        );
        assert_eq!(
            close_target_state(true, None),
            ScenarioState::Paused,
            "no stats means no emissions known — must remain Paused"
        );
    }

    #[test]
    fn close_target_state_held_when_holds_on_and_samples_present() {
        use crate::model::metric::{Labels, MetricEvent};

        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        {
            let mut st = stats.write().unwrap();
            let event = MetricEvent::new("up".to_string(), 1.0, Labels::default())
                .expect("valid metric name");
            st.push_metric(event);
        }
        assert_eq!(close_target_state(true, Some(&stats)), ScenarioState::Held);
    }

    #[test]
    fn gate_context_with_holds_on_close_setter_round_trip() {
        use crate::schedule::gate_bus::{GateBus, SubscriptionSpec};

        let bus = GateBus::new();
        bus.tick(0.0);
        let (rx, init) = bus.subscribe(SubscriptionSpec {
            after: None,
            while_: None,
        });
        let ctx = GateContext::new(rx, init).with_holds_on_close(true);
        assert!(ctx.holds_on_close);
    }
}
