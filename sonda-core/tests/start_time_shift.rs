//! Behavioural coverage for the `start_time:` emission-time shift across all
//! four signal types and the gated close-emit recovery marker.

#![cfg(feature = "config")]

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, RwLock};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use sonda_core::compiler::WhileOp;
use sonda_core::config::{
    BaseScheduleConfig, DistributionConfig, HistogramScenarioConfig, LogScenarioConfig,
    ScenarioConfig, SummaryScenarioConfig,
};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
use sonda_core::schedule::gate_bus::{GateBus, SubscriptionSpec, WhileSpec};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::schedule::{histogram_runner, log_runner, runner, summary_runner, GateContext};
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{OnSinkError, SondaError};

type SharedBuf = Arc<Mutex<Vec<u8>>>;

struct ProbeSink(SharedBuf);

impl Sink for ProbeSink {
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.0
            .lock()
            .expect("probe sink mutex poisoned")
            .extend_from_slice(data);
        Ok(())
    }
    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn probe_sink() -> (Box<dyn Sink>, SharedBuf) {
    let buf: SharedBuf = Arc::new(Mutex::new(Vec::new()));
    let sink: Box<dyn Sink> = Box::new(ProbeSink(SharedBuf::clone(&buf)));
    (sink, buf)
}

fn base(name: &str, rate: f64, duration: &str, start_time: Option<&str>) -> BaseScheduleConfig {
    BaseScheduleConfig {
        name: name.to_string(),
        rate,
        duration: Some(duration.to_string()),
        gaps: None,
        bursts: None,
        cardinality_spikes: None,
        dynamic_labels: None,
        labels: None,
        sink: SinkConfig::Stdout,
        phase_offset: None,
        clock_group: None,
        clock_group_is_auto: None,
        start_time: start_time.map(str::to_string),
        jitter: None,
        jitter_seed: None,
        on_sink_error: OnSinkError::Warn,
    }
}

fn metric_config(
    name: &str,
    rate: f64,
    duration: &str,
    start_time: Option<&str>,
) -> ScenarioConfig {
    ScenarioConfig {
        base: base(name, rate, duration, start_time),
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    }
}

/// Parse the trailing integer-millisecond timestamp token from each
/// Prometheus text line. Returns timestamps in emission order.
fn prometheus_timestamps_ms(buf: &[u8]) -> Vec<i128> {
    std::str::from_utf8(buf)
        .expect("prometheus output is valid UTF-8")
        .lines()
        .filter(|l| !l.is_empty())
        .map(|l| {
            l.rsplit(' ')
                .next()
                .and_then(|t| t.parse::<i128>().ok())
                .unwrap_or_else(|| panic!("line has no integer ms timestamp: {l}"))
        })
        .collect()
}

fn now_ms() -> i128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i128
}

#[tokio::test]
async fn metric_absolute_past_start_time_anchors_first_event_at_exact_instant() {
    // 2026-05-08T14:00:00Z = 1_778_248_800 s since epoch.
    let anchor_ms: i128 = 1_778_248_800_000;
    let config = metric_config("up", 5.0, "1s", Some("2026-05-08T14:00:00Z"));

    let (mut sink, buf) = probe_sink();
    runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");

    let captured = buf.lock().unwrap();
    let timestamps = prometheus_timestamps_ms(&captured);
    assert!(
        timestamps.len() >= 2,
        "need at least 2 events to check advancement, got {}",
        timestamps.len()
    );

    // First tick is emitted at base + elapsed≈0; the loop's first deadline is
    // `start`, so elapsed is sub-millisecond. Assert it lands within 50ms of
    // the configured anchor — never at real wall-clock now.
    assert!(
        (timestamps[0] - anchor_ms).abs() < 50,
        "first event must anchor at the configured past instant {anchor_ms}, got {}",
        timestamps[0]
    );
    assert!(
        timestamps[0] < now_ms() - 86_400_000,
        "first event timestamp must be far in the past, got {}",
        timestamps[0]
    );

    // Subsequent ticks advance forward from the anchor, never re-anchoring to
    // real now. The loop snapshots `elapsed` at the top of each iteration
    // before the rate-limiting sleep, so the first ticks cluster near 0ms;
    // the span across the whole run reflects the elapsed scenario time.
    for w in timestamps.windows(2) {
        assert!(
            w[1] >= w[0],
            "timestamps must be monotonically non-decreasing, got {} then {}",
            w[0],
            w[1]
        );
    }
    let span = timestamps[timestamps.len() - 1] - timestamps[0];
    assert!(
        span > 300,
        "events must advance forward across the ~1s run from the past anchor, span={span}ms"
    );
    assert!(
        timestamps[timestamps.len() - 1] < now_ms() - 86_400_000,
        "even the last event must stay far in the past, got {}",
        timestamps[timestamps.len() - 1]
    );
}

#[tokio::test]
async fn metric_relative_future_offset_shifts_events_ahead() {
    let before = now_ms();
    let config = metric_config("up", 10.0, "400ms", Some("+24h"));

    let (mut sink, buf) = probe_sink();
    runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let after = now_ms();

    let captured = buf.lock().unwrap();
    let timestamps = prometheus_timestamps_ms(&captured);
    assert!(!timestamps.is_empty(), "expected emitted events");

    let shift = Duration::from_secs(24 * 3600).as_millis() as i128;
    // base = start_wall + 24h, and start_wall ∈ [before, after]. The first
    // event sits at base + ~0.
    assert!(
        timestamps[0] >= before + shift && timestamps[0] <= after + shift + 50,
        "first event must be ~24h ahead of scenario start: got {}, window [{}, {}]",
        timestamps[0],
        before + shift,
        after + shift + 50
    );
}

#[tokio::test]
async fn metric_relative_past_offset_shifts_events_back() {
    let before = now_ms();
    let config = metric_config("up", 10.0, "400ms", Some("-7d"));

    let (mut sink, buf) = probe_sink();
    runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let after = now_ms();

    let captured = buf.lock().unwrap();
    let timestamps = prometheus_timestamps_ms(&captured);
    assert!(!timestamps.is_empty(), "expected emitted events");

    let shift = Duration::from_secs(7 * 86_400).as_millis() as i128;
    assert!(
        timestamps[0] >= before - shift && timestamps[0] <= after - shift + 50,
        "first event must be ~7 days back: got {}, window [{}, {}]",
        timestamps[0],
        before - shift,
        after - shift + 50
    );
}

#[test]
fn omitted_start_time_yields_now_variant() {
    use sonda_core::config::validate::{parse_start_time, StartTime};
    // The resolved schedule treats an absent field as `now`.
    assert_eq!(parse_start_time("now").unwrap(), StartTime::Now);
}

#[tokio::test]
async fn metric_without_start_time_emits_at_real_now() {
    let before = now_ms();
    let config = metric_config("up", 10.0, "400ms", None);

    let (mut sink, buf) = probe_sink();
    runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let after = now_ms();

    let captured = buf.lock().unwrap();
    let timestamps = prometheus_timestamps_ms(&captured);
    assert!(!timestamps.is_empty(), "expected emitted events");
    for ts in &timestamps {
        assert!(
            *ts >= before - 50 && *ts <= after + 50,
            "default-path event must be stamped at real now: got {ts}, window [{before}, {after}]"
        );
    }
}

/// 2026-05-08T14:00:00Z anchor — used by every signal-type shift test.
const ANCHOR_RFC3339: &str = "2026-05-08T14:00:00Z";
const ANCHOR_MS: i128 = 1_778_248_800_000;

fn assert_all_in_past_window(timestamps: &[i128]) {
    assert!(!timestamps.is_empty(), "expected emitted events");
    for ts in timestamps {
        assert!(
            (*ts - ANCHOR_MS).abs() < 5_000,
            "event must land in the absolute-past window near {ANCHOR_MS}, got {ts}"
        );
        assert!(
            *ts < now_ms() - 86_400_000,
            "event must be far in the past, got {ts}"
        );
    }
}

#[tokio::test]
async fn metric_signal_honours_absolute_past_shift() {
    let config = metric_config("up", 5.0, "600ms", Some(ANCHOR_RFC3339));
    let (mut sink, buf) = probe_sink();
    runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let captured = buf.lock().unwrap();
    assert_all_in_past_window(&prometheus_timestamps_ms(&captured));
}

#[tokio::test]
async fn log_signal_honours_absolute_past_shift() {
    let config = LogScenarioConfig {
        base: base("logshift", 5.0, "600ms", Some(ANCHOR_RFC3339)),
        generator: LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "event".to_string(),
                field_pools: std::collections::BTreeMap::new(),
            }],
            severity_weights: None,
            seed: Some(0),
        },
        encoder: EncoderConfig::JsonLines { precision: None },
    };

    let (mut sink, buf) = probe_sink();
    log_runner::run_logs_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");

    // JSON Lines stamps an RFC 3339 millis timestamp; the year is sufficient
    // to confirm the log event carries the shifted wall_clock, not real now.
    let captured = buf.lock().unwrap();
    let text = std::str::from_utf8(&captured).expect("valid UTF-8");
    let lines: Vec<&str> = text.lines().filter(|l| !l.is_empty()).collect();
    assert!(!lines.is_empty(), "expected emitted log lines");
    for line in &lines {
        assert!(
            line.contains("\"timestamp\":\"2026-05-08T14:00:0"),
            "log event timestamp must carry the shifted wall_clock; line: {line}"
        );
    }
}

#[tokio::test]
async fn histogram_signal_honours_absolute_past_shift() {
    let config = HistogramScenarioConfig {
        base: base("latency", 5.0, "600ms", Some(ANCHOR_RFC3339)),
        buckets: Some(vec![0.1, 0.5, 1.0]),
        distribution: DistributionConfig::Normal {
            mean: 0.2,
            stddev: 0.05,
        },
        observations_per_tick: Some(20),
        mean_shift_per_sec: None,
        seed: Some(42),
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    };

    let (mut sink, buf) = probe_sink();
    histogram_runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let captured = buf.lock().unwrap();
    assert_all_in_past_window(&prometheus_timestamps_ms(&captured));
}

#[tokio::test]
async fn summary_signal_honours_absolute_past_shift() {
    let config = SummaryScenarioConfig {
        base: base("rpc_duration", 5.0, "600ms", Some(ANCHOR_RFC3339)),
        quantiles: Some(vec![0.5, 0.9]),
        distribution: DistributionConfig::Normal {
            mean: 0.1,
            stddev: 0.02,
        },
        observations_per_tick: Some(20),
        mean_shift_per_sec: None,
        seed: Some(42),
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    };

    let (mut sink, buf) = probe_sink();
    summary_runner::run_with_sink(&config, &mut sink, None, None)
        .await
        .expect("run must succeed");
    let captured = buf.lock().unwrap();
    assert_all_in_past_window(&prometheus_timestamps_ms(&captured));
}

#[test]
fn gated_close_emit_marker_lands_in_shifted_window() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(SubscriptionSpec {
        after: None,
        while_: Some(WhileSpec {
            op: WhileOp::GreaterThan,
            threshold: 0.0,
        }),
    });

    // snap_to opts a non-remote-write sink into close-emit; the recovery
    // marker derives its timestamp from the last shifted emission.
    let delay = sonda_core::compiler::DelayClause {
        open: None,
        close: Some(Duration::from_millis(0)),
        close_stale_marker: None,
        close_snap_to: Some(0.0),
    };

    let config = metric_config("gated", 50.0, "2000ms", Some(ANCHOR_RFC3339));
    let (sink_init, buf) = probe_sink();
    let stats = Arc::new(RwLock::new(ScenarioStats::default()));
    let stats_for_thread = Arc::clone(&stats);
    let bus_for_thread = Arc::clone(&bus);
    let buf_for_assert = SharedBuf::clone(&buf);

    let runner_handle = thread::spawn(move || {
        let _ = bus_for_thread;
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut sink = sink_init;
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime must build");
        rt.block_on(runner::run_with_sink_gated(
            &config,
            &mut sink,
            Some(shutdown.as_ref()),
            Some(stats_for_thread),
            None,
            Some(
                GateContext::new(rx, init)
                    .with_delay(Some(delay))
                    .with_has_while(true),
            ),
        ))
        .expect("gated runner must succeed");
    });

    // Let the scenario accumulate shifted emissions, then close the gate so
    // the running → paused commit fires the close-emit marker.
    thread::sleep(Duration::from_millis(150));
    bus.tick(0.0);
    runner_handle.join().expect("runner joined");

    let captured = buf_for_assert.lock().unwrap();
    let timestamps = prometheus_timestamps_ms(&captured);
    assert!(
        timestamps.len() >= 2,
        "expected running samples plus a close marker, got {}",
        timestamps.len()
    );
    // Every emitted sample — running samples AND the close-emit recovery
    // marker — must sit in the shifted window, never at real wall-clock now.
    for ts in &timestamps {
        assert!(
            (*ts - ANCHOR_MS).abs() < 5_000,
            "close-emit marker / running sample must land in the shifted window near \
             {ANCHOR_MS}, got {ts}"
        );
        assert!(
            *ts < now_ms() - 86_400_000,
            "no emitted sample may be stamped at real now, got {ts}"
        );
    }
}
