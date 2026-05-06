//! End-to-end coverage for `delay.close` close-emit behavior on the
//! `running → paused` debounce-commit edge.
//!
//! The runtime emits a Prometheus stale-NaN sample for every recently-active
//! `(metric_name, label_set)` tuple by default when the sink is `RemoteWrite`.
//! `delay.close.snap_to: <v>` replaces the stale marker with a literal
//! sample. `delay.close.stale_marker: false` disables the marker entirely.
//! Non-`remote_write` sinks default to no close-emit; `snap_to` opts in.
//!
//! The recent-metrics buffer is capped at `MAX_RECENT_METRICS = 100`, so
//! high-cardinality scenarios under-emit on close. v1.6 ships this as a
//! known limitation; v1.7 follow-up extends the buffer.

#![cfg(feature = "config")]
#![cfg(feature = "remote-write")]

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sonda_core::compiler::{DelayClause, WhileOp};
use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::remote_write::{parse_length_prefixed_timeseries, PROMETHEUS_STALE_NAN};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::gate_bus::{GateBus, SubscriptionSpec, WhileSpec};
use sonda_core::schedule::GateContext;
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::SondaError;

/// In-memory sink that captures every byte buffer the runner writes.
#[derive(Clone, Default)]
struct CaptureSink {
    buf: Arc<Mutex<Vec<u8>>>,
}

impl Sink for CaptureSink {
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.buf.lock().unwrap().extend_from_slice(data);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn while_gt_zero() -> SubscriptionSpec {
    SubscriptionSpec {
        after: None,
        while_: Some(WhileSpec {
            op: WhileOp::GreaterThan,
            threshold: 0.0,
        }),
    }
}

/// Run a metric scenario directly against the capture sink, bypassing
/// `launch_scenario_with_gates` (which constructs its own sink from
/// `SinkConfig`). Returns the captured buffer and the joined thread result.
fn run_with_capture(
    name: &str,
    rate: f64,
    duration_ms: u64,
    delay: Option<DelayClause>,
    sink_kind: SinkConfig,
    encoder: EncoderConfig,
    bus: Arc<GateBus>,
    open_at_start: bool,
) -> (Vec<u8>, Vec<sonda_core::schedule::stats::ScenarioState>) {
    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: name.to_string(),
            rate,
            duration: Some(format!("{duration_ms}ms")),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: sink_kind,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder,
    };
    let _ = config.base.name.clone();

    if open_at_start {
        bus.tick(1.0);
    } else {
        bus.tick(0.0);
    }
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();

    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));
    let stats_for_runner = Arc::clone(&stats);
    let states = Arc::new(Mutex::new(Vec::new()));
    let states_for_poll = Arc::clone(&states);
    let stats_for_poll = Arc::clone(&stats);

    // Drive the scenario on a background thread so the test thread can
    // close the gate at the right moment.
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let shutdown = Arc::new(AtomicBool::new(true));
        let gate_ctx = GateContext {
            gate_rx: rx,
            initial: init,
            delay,
            has_after: false,
            has_while: true,
            close_emit: None,
        };
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_runner),
            None,
            Some(gate_ctx),
        )
        .expect("runner must succeed");
        let _ = bus_for_thread;
    });

    // Open then close after a short delay so the runner accumulates labels
    // in stats.recent_metrics before the close commits.
    if !open_at_start {
        thread::sleep(Duration::from_millis(50));
        bus.tick(1.0);
    }
    let poll_deadline = Instant::now() + Duration::from_millis(150);
    while Instant::now() < poll_deadline {
        if let Ok(st) = stats_for_poll.read() {
            states_for_poll.lock().unwrap().push(st.state);
        }
        thread::sleep(Duration::from_millis(20));
    }

    bus.tick(0.0);
    runner.join().expect("runner thread joined");
    let buf = capture.buf.lock().unwrap().clone();
    let collected = states.lock().unwrap().clone();
    (buf, collected)
}

#[test]
fn remote_write_emits_stale_marker_on_running_to_paused() {
    let bus = Arc::new(GateBus::new());
    let (buf, _states) = run_with_capture(
        "downstream",
        100.0,
        2000,
        None,
        SinkConfig::RemoteWrite {
            url: "http://example.invalid/api/v1/write".to_string(),
            batch_size: None,
            retry: None,
        },
        EncoderConfig::RemoteWrite,
        Arc::clone(&bus),
        false,
    );

    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    // Single-label-set scenario emits exactly one stale marker on the
    // running → paused commit. A regression that emits N markers per
    // tuple (e.g. broken dedup) would silently slip through `>= 1`.
    assert_eq!(
        stale_count,
        1,
        "expected exactly one TimeSeries carrying the stale-NaN sample, got {stale_count} \
         (total series: {})",
        series.len()
    );
}

#[test]
fn snap_to_replaces_stale_marker_with_literal_value() {
    let delay = DelayClause {
        open: None,
        close: Some(Duration::from_millis(0)),
        close_stale_marker: None,
        close_snap_to: Some(0.0),
    };

    let bus = Arc::new(GateBus::new());
    let (buf, _states) = run_with_capture(
        "snap_metric",
        100.0,
        2000,
        Some(delay),
        SinkConfig::RemoteWrite {
            url: "http://example.invalid/api/v1/write".to_string(),
            batch_size: None,
            retry: None,
        },
        EncoderConfig::RemoteWrite,
        Arc::clone(&bus),
        false,
    );

    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count, 0,
        "snap_to must replace the stale marker — no NaN samples expected"
    );

    // The post-close snap sample carries value 0.0; the running samples are
    // 1.0. So at least one sample with bit-equal 0.0 is present.
    let zero_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == 0.0_f64.to_bits())
        })
        .count();
    assert!(zero_count >= 1, "expected at least one snap_to=0 sample");
}

#[test]
fn stale_marker_disabled_emits_no_close_sample() {
    let delay = DelayClause {
        open: None,
        close: Some(Duration::from_millis(0)),
        close_stale_marker: Some(false),
        close_snap_to: None,
    };

    let bus = Arc::new(GateBus::new());
    let (buf, _states) = run_with_capture(
        "no_close",
        100.0,
        2000,
        Some(delay),
        SinkConfig::RemoteWrite {
            url: "http://example.invalid/api/v1/write".to_string(),
            batch_size: None,
            retry: None,
        },
        EncoderConfig::RemoteWrite,
        Arc::clone(&bus),
        false,
    );

    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count, 0,
        "stale_marker:false must suppress every NaN close sample"
    );
}

#[test]
fn non_remote_write_sink_no_close_marker_by_default() {
    use sonda_core::sink::memory::MemorySink;

    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "stdout_metric".to_string(),
            rate: 50.0,
            duration: Some("1500ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
    };

    let mut sink = MemorySink::new();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let bus_for_thread = Arc::clone(&bus);
    let stats_for_thread = Arc::clone(&stats);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: None,
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
        sink
    });

    thread::sleep(Duration::from_millis(50));
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(150));
    bus.tick(0.0);
    let sink_after = runner.join().expect("runner joined");

    // Every emitted line must be a running-state sample with value 1.0
    // (the constant generator). A close-emit closure on a non-remote-write
    // sink with no `snap_to` should never be installed, so no additional
    // `NaN` (StaleMarker) or `0` (SnapTo(0)) sample line can appear.
    let text = std::str::from_utf8(&sink_after.buffer).expect("utf-8");
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    for line in &lines {
        let value_token = line
            .split_whitespace()
            .nth(1)
            .unwrap_or_else(|| panic!("malformed prometheus line: {line}"));
        let value: f64 = value_token
            .parse()
            .unwrap_or_else(|_| panic!("non-f64 value '{value_token}' in line: {line}"));
        assert_eq!(
            value, 1.0,
            "non-remote-write default must only emit running-state samples (value 1.0), \
             got {value} in line: {line}"
        );
    }
}

#[test]
fn non_remote_write_sink_with_snap_to_emits_one_sample() {
    use sonda_core::sink::memory::MemorySink;

    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let delay = DelayClause {
        open: None,
        close: Some(Duration::from_millis(0)),
        close_stale_marker: None,
        close_snap_to: Some(99.0),
    };

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "snap_text".to_string(),
            rate: 100.0,
            duration: Some("2000ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
    };

    let mut sink = MemorySink::new();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let bus_for_thread = Arc::clone(&bus);
    let stats_for_thread = Arc::clone(&stats);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: Some(delay),
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
        sink
    });

    thread::sleep(Duration::from_millis(50));
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(150));
    bus.tick(0.0);
    let sink_after = runner.join().expect("runner joined");

    // Tokenize each line as `<name> <value> <timestamp>` rather than substring
    // match — the assertion stays robust if PrometheusText ever renders integer
    // values as `99.0` or trailing whitespace differently.
    let text = std::str::from_utf8(&sink_after.buffer).expect("utf-8");
    let snap_count = text
        .lines()
        .filter(|l| !l.is_empty())
        .filter(|l| l.split_whitespace().next() == Some("snap_text"))
        .filter_map(|l| {
            l.split_whitespace()
                .nth(1)
                .and_then(|t| t.parse::<f64>().ok())
        })
        .filter(|v| (*v - 99.0).abs() < f64::EPSILON)
        .count();
    assert_eq!(
        snap_count, 1,
        "expected exactly one snap_text sample with value 99.0, got {snap_count} in:\n{text}"
    );
}

#[test]
fn debounce_cancelled_close_emits_no_stale_marker() {
    let delay = DelayClause {
        open: None,
        close: Some(Duration::from_millis(300)),
        close_stale_marker: None,
        close_snap_to: None,
    };

    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "dbnc".to_string(),
            rate: 100.0,
            duration: Some("2000ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: Some(delay),
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    // Let the scenario run for ~150ms so recent_metrics fills.
    thread::sleep(Duration::from_millis(150));
    let buf_before = capture.buf.lock().unwrap().clone();

    // Brief close-then-reopen well within the 300ms debounce window.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(50));
    bus.tick(1.0);

    // Give any in-flight close-emit ~100ms to flush before we snapshot.
    // The brief close-then-reopen window ended ~50ms ago; if a stale
    // marker was erroneously emitted, it lands here.
    thread::sleep(Duration::from_millis(100));
    let buf_after_window = capture.buf.lock().unwrap().clone();
    let appended = &buf_after_window[buf_before.len()..];

    // Inspect ONLY the bytes appended during the cancelled-close window.
    // Anything emitted later (e.g. on duration-expiry close) is out of
    // scope for this test and would erode the assertion if included.
    let series = parse_length_prefixed_timeseries(appended).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count, 0,
        "debounce-cancelled close must not emit a stale marker; got {stale_count}"
    );

    // Let the runner exit cleanly.
    runner.join().expect("runner joined");
}

#[test]
fn duration_expiry_while_gate_open_emits_stale_marker() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "duration_expiry".to_string(),
            rate: 100.0,
            duration: Some("200ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: None,
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    runner.join().expect("runner joined");

    let buf = capture.buf.lock().unwrap().clone();
    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count,
        1,
        "duration expiry while gate open must emit exactly one stale marker, got {stale_count} \
         (total series: {})",
        series.len()
    );
}

#[test]
fn paused_to_finished_via_duration_after_running_emits_stale_marker() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "paused_finish".to_string(),
            rate: 100.0,
            duration: Some("400ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: Some(DelayClause {
                    open: Some(Duration::from_millis(0)),
                    close: Some(Duration::from_millis(0)),
                    close_stale_marker: None,
                    close_snap_to: None,
                }),
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    // Let the scenario run for ~150ms, then close the gate so the loop
    // commits running → paused well before duration expires (400ms total).
    thread::sleep(Duration::from_millis(150));
    bus.tick(0.0);
    runner.join().expect("runner joined");

    let buf = capture.buf.lock().unwrap().clone();
    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count,
        1,
        "expected exactly one stale marker (no duplicate on paused→finished); got {stale_count} \
         (total series: {})",
        series.len()
    );
}

#[test]
fn pending_to_finished_via_duration_emits_no_stale_marker() {
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "never_ran".to_string(),
            rate: 100.0,
            duration: Some("200ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: None,
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    runner.join().expect("runner joined");

    let buf = capture.buf.lock().unwrap().clone();
    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count, 0,
        "no recent tuples were ever recorded — close-emit must produce zero markers; got {stale_count}"
    );
    assert_eq!(
        series.len(),
        0,
        "scenario never reached Running — no series should reach the wire; got {}",
        series.len()
    );
}

#[test]
fn multi_cycle_running_paused_to_finished_emits_one_stale_per_running_to_paused() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "multi_cycle".to_string(),
            rate: 100.0,
            duration: Some("700ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: Some(DelayClause {
                    open: Some(Duration::from_millis(0)),
                    close: Some(Duration::from_millis(0)),
                    close_stale_marker: None,
                    close_snap_to: None,
                }),
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    // Cycle 1: run ~100ms, pause ~50ms.
    thread::sleep(Duration::from_millis(100));
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(50));
    // Cycle 2: resume, run ~100ms, pause and let duration expire.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(100));
    bus.tick(0.0);
    runner.join().expect("runner joined");

    let buf = capture.buf.lock().unwrap().clone();
    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count,
        2,
        "expected one stale marker per running→paused transition (2 cycles); got {stale_count} \
         (total series: {})",
        series.len()
    );
}

#[test]
fn shutdown_while_gate_open_emits_stale_marker() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let capture = CaptureSink::default();
    let mut sink_handle = capture.clone();
    let stats = Arc::new(std::sync::RwLock::new(
        sonda_core::schedule::stats::ScenarioStats::default(),
    ));
    let shutdown = Arc::new(AtomicBool::new(true));

    let config = ScenarioConfig {
        base: BaseScheduleConfig {
            name: "shutdown_during_run".to_string(),
            rate: 100.0,
            duration: Some("5000ms".to_string()),
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::RemoteWrite {
                url: "http://example.invalid/api/v1/write".to_string(),
                batch_size: None,
                retry: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: sonda_core::OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::RemoteWrite,
    };

    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let shutdown_for_thread = Arc::clone(&shutdown);
    let runner = thread::spawn(move || {
        let _ = bus_for_thread;
        sonda_core::schedule::runner::run_with_sink_gated(
            &config,
            &mut sink_handle,
            Some(shutdown_for_thread.as_ref()),
            Some(stats_for_thread),
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: None,
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("runner must succeed");
    });

    thread::sleep(Duration::from_millis(150));
    shutdown.store(false, std::sync::atomic::Ordering::SeqCst);
    runner.join().expect("runner joined");

    let buf = capture.buf.lock().unwrap().clone();
    let series = parse_length_prefixed_timeseries(&buf).expect("parse ok");
    let stale_count = series
        .iter()
        .filter(|ts| {
            ts.samples
                .iter()
                .any(|s| s.value.to_bits() == PROMETHEUS_STALE_NAN.to_bits())
        })
        .count();
    assert_eq!(
        stale_count,
        1,
        "shutdown while gate open must emit exactly one stale marker, got {stale_count} \
         (total series: {})",
        series.len()
    );
}
