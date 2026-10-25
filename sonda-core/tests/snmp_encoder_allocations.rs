//! Allocation budget for the `snmp` encoder: through the real runner as a
//! surcharge over the plain encode path, and `SnmpEncoder::encode_metric`
//! called directly in steady state and on a rejected series.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter is thread-local and each runner run uses a
//! current-thread runtime, so only allocations made on the measuring thread
//! inside a counted window are counted.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(feature = "snmp")]

// `common` is compiled only with `config`; the counter is needed without it.
#[path = "common/alloc.rs"]
mod counting;

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::snmp::{SnmpEncoder, SnmpEncoderConfig, SnmpValueType};
use sonda_core::encoder::{Encoder, EncoderConfig};
use sonda_core::generator::GeneratorConfig;
use sonda_core::model::metric::{Labels, MetricEvent};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{EncoderError, OnSinkError, SondaError};

#[global_allocator]
static ALLOCATOR: counting::CountingAlloc = counting::CountingAlloc;

const CALLS: usize = 1_000;

/// The allocations `body` made on this thread.
fn count_allocations(body: impl FnOnce()) -> usize {
    counting::count_allocations(body).1
}

/// One known allocation on this thread reads as one: this binary's counting
/// allocator is installed.
#[test]
fn a_known_allocation_counts_as_one() {
    counting::assert_count_allocations_counts();
}

/// Another thread allocating throughout a counted window adds nothing to it,
/// so this binary's cases can run in parallel.
#[test]
fn allocations_on_other_threads_are_not_counted() {
    const OTHER_ALLOCS: usize = 10_000;
    let go = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));
    let other = {
        let (go, done) = (Arc::clone(&go), Arc::clone(&done));
        std::thread::spawn(move || {
            while !go.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            for i in 0..OTHER_ALLOCS {
                std::hint::black_box(vec![i as u8; 16]);
                done.fetch_add(1, Ordering::SeqCst);
            }
        })
    };
    counting::assert_count_allocations_counts();
    let allocs = count_allocations(|| {
        go.store(true, Ordering::SeqCst);
        while done.load(Ordering::SeqCst) < OTHER_ALLOCS {
            std::hint::spin_loop();
        }
        std::hint::black_box(Box::new(1u8));
    });
    other.join().expect("allocating thread panicked");
    // Vacuity guard: the other thread did allocate, all of it inside the window.
    assert_eq!(done.load(Ordering::SeqCst), OTHER_ALLOCS);
    assert_eq!(
        allocs, 1,
        "only this thread's own allocation is counted, none of the other thread's"
    );
}

/// Sink that does not opt in, so it keeps the plain byte path.
struct PlainSink {
    delivered: Arc<AtomicUsize>,
    bytes: Arc<AtomicUsize>,
}

#[async_trait]
impl Sink for PlainSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        self.bytes.fetch_add(data.len(), Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn snmp_encoder_config() -> SnmpEncoderConfig {
    SnmpEncoderConfig {
        oids: HashMap::from([(
            "ifHCInOctets".to_string(),
            "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
        )]),
        types: HashMap::from([("ifHCInOctets".to_string(), SnmpValueType::Counter64)]),
        ..SnmpEncoderConfig::default()
    }
}

/// The two-label scenario with `encoder`: `device` is the agent label and
/// `ifIndex` the template's placeholder.
fn scenario(encoder: EncoderConfig) -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name: "ifHCInOctets".to_string(),
            rate: 1000.0,
            duration: Some("1s".to_string()),
            gaps: None,
            gap_windows: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            // Two labels, so a deep clone of the label set would cost
            // allocations and show up in the surcharge.
            labels: Some(HashMap::from([
                ("device".to_string(), "rtr-1".to_string()),
                ("ifIndex".to_string(), "3".to_string()),
            ])),
            sink: SinkConfig::Stdout,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        },
        generator: GeneratorConfig::Step {
            start: Some(0.0),
            step_size: 125_000.0,
            max: None,
        },
        encoder,
        metric_type: None,
        help: None,
    }
}

/// What one counted runner run with an encoder cost.
struct RunCost {
    /// Allocations in the counted run.
    allocs: usize,
    /// Events delivered in it.
    events: usize,
    /// Bytes per delivered event.
    bytes_per_event: f64,
    /// Allocations of building the encoder and encoding the scenario's
    /// first event: what each run pays once, since the runner builds its
    /// own encoder.
    once: usize,
}

impl RunCost {
    /// Allocations per event, with the once-per-run cost taken out.
    fn per_event(&self) -> f64 {
        (self.allocs - self.once) as f64 / self.events as f64
    }
}

/// Drive the real runner with `encoder` and a [`PlainSink`] for one second at
/// 1 kHz, after a warm-up run on the same sink, and measure what building the
/// encoder and its first encode cost on their own.
fn run_cost(encoder: EncoderConfig) -> RunCost {
    let once = {
        let event = event_with(&[("device", "rtr-1"), ("ifIndex", "3")], 0.0);
        let mut buf = Vec::with_capacity(256);
        counting::assert_count_allocations_counts();
        count_allocations(|| {
            let built = sonda_core::encoder::create_encoder(&encoder).expect("the encoder builds");
            built
                .encode_metric(&event, &mut buf)
                .expect("the first event encodes");
        })
    };
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime must build");
    let delivered = Arc::new(AtomicUsize::new(0));
    let bytes = Arc::new(AtomicUsize::new(0));
    let mut sink: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
        bytes: Arc::clone(&bytes),
    });
    let config = scenario(encoder);
    let cancel = CancellationToken::new();
    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, &mut sink, &cancel, None)
            .await
            .expect("warm-up run must succeed");
    });

    delivered.store(0, Ordering::Relaxed);
    bytes.store(0, Ordering::Relaxed);
    let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
    counting::assert_count_allocations_counts();
    let allocs = count_allocations(|| {
        rt.block_on(async {
            sonda_core::schedule::runner::run_with_sink(
                &config,
                &mut sink,
                &cancel,
                Some(Arc::clone(&stats)),
            )
            .await
            .expect("counted run must succeed");
        })
    });
    let delivered = delivered.load(Ordering::Relaxed);
    let stats = stats.read().expect("stats lock");
    // Vacuity guards: the run emitted ~1000 events, none rejected, and every
    // one reached the sink with bytes in it.
    assert!(
        delivered >= 500,
        "a 1 s run at 1 kHz must deliver ~1000 events before the ratio means \
         anything; got {delivered}"
    );
    assert_eq!(stats.rejected_events, 0, "no event was rejected");
    assert_eq!(stats.total_events, delivered as u64);
    assert!(allocs >= once, "the counted run built its own encoder");
    RunCost {
        allocs,
        events: delivered,
        bytes_per_event: bytes.load(Ordering::Relaxed) as f64 / delivered as f64,
        once,
    }
}

/// Through the runner, the snmp encoder costs no allocation per event more
/// than `prometheus_text` on the same scenario and sink: each series' OID is
/// rendered once and copied after that, and the record is written into the
/// runner's reused buffer.
///
/// The runner builds its encoder per run, so each run also pays the
/// encoder's construction and its first-sight render once. That is measured
/// on its own for each encoder and taken out before the per-event figures
/// are compared; it is printed as a measurement.
#[test]
fn snmp_encoder_surcharge_over_the_plain_path_is_zero() {
    let plain = run_cost(EncoderConfig::PrometheusText { precision: None });
    let snmp = run_cost(EncoderConfig::Snmp(snmp_encoder_config()));
    // Vacuity guard: the snmp run wrote publish records, not empty writes:
    // the header byte, a VarBind of the 12-arc OID and a Counter64.
    assert!(plain.bytes_per_event > 0.0);
    assert!(
        snmp.bytes_per_event > 20.0,
        "{} bytes per record",
        snmp.bytes_per_event
    );
    let (plain_cost, snmp_cost) = (plain.per_event(), snmp.per_event());
    assert!(plain_cost > 0.0, "the plain path's figure was counted");
    let surcharge = snmp_cost - plain_cost;
    println!(
        "once per run: prometheus_text {} allocations, snmp {}; per event: \
         prometheus_text {plain_cost:.4}, snmp {snmp_cost:.4}, surcharge {surcharge:.4}",
        plain.once, snmp.once
    );
    let (plain, snmp) = (plain_cost, snmp_cost);
    assert!(
        surcharge < 0.01,
        "the snmp encoder is over budget: {snmp:.3} allocations per event against \
         {plain:.3} for prometheus_text, a surcharge of {surcharge:.3} (allowed < 0.01)"
    );
}

fn encoder() -> SnmpEncoder {
    SnmpEncoder::new(&SnmpEncoderConfig {
        drop_labels: vec!["spike".to_string()],
        ..snmp_encoder_config()
    })
    .expect("encoder config is valid")
}

fn event_with(labels: &[(&str, &str)], value: f64) -> MetricEvent {
    MetricEvent::with_timestamp(
        "ifHCInOctets".to_string(),
        value,
        Labels::from_pairs(labels).expect("labels are valid"),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .expect("metric name is valid")
}

/// After the first encode of a series, encoding it again allocates nothing,
/// and neither does each event of a 1 000-value spike on the dropped `spike`
/// label: every value is the one series.
#[test]
fn steady_state_encode_allocates_nothing() {
    let encoder = encoder();
    let event = event_with(&[("device", "rtr-1"), ("ifIndex", "3")], 1_250_000.0);
    let mut buf = Vec::with_capacity(4096);
    encoder
        .encode_metric(&event, &mut buf)
        .expect("warm-up encode succeeds");
    let one_record = buf.len();

    let mut calls = 0usize;
    counting::assert_count_allocations_counts();
    let allocs = count_allocations(|| {
        for _ in 0..CALLS {
            buf.clear();
            encoder
                .encode_metric(&event, &mut buf)
                .expect("steady-state encode succeeds");
            calls += 1;
        }
    });
    // Vacuity guard: the loop ran and each call wrote a whole record.
    assert_eq!(calls, CALLS);
    assert_eq!(buf.len(), one_record);
    assert!(one_record > 20);
    assert_eq!(
        allocs, 0,
        "steady-state encode must not allocate: {allocs} allocations over {calls} calls"
    );

    // Built before the window, so only the encodes are counted.
    let spiked: Vec<MetricEvent> = (0..CALLS)
        .map(|i| {
            let spike = format!("spike-{i}");
            event_with(
                &[("device", "rtr-1"), ("ifIndex", "3"), ("spike", &spike)],
                1_250_000.0,
            )
        })
        .collect();
    let mut spike_calls = 0usize;
    counting::assert_count_allocations_counts();
    let spike_allocs = count_allocations(|| {
        for event in &spiked {
            buf.clear();
            encoder
                .encode_metric(event, &mut buf)
                .expect("an event with a dropped label encodes");
            spike_calls += 1;
        }
    });
    assert_eq!(spike_calls, CALLS);
    assert_eq!(buf.len(), one_record);
    assert_eq!(
        spike_allocs, 0,
        "a spike on a dropped label must not allocate: {spike_allocs} allocations over \
         {spike_calls} distinct values"
    );
}

/// A series rejected on first sight, because its `ifIndex` is not an arc,
/// costs each later event exactly one allocation: the rejection's text,
/// copied into the error. Its OID is not rendered again.
#[test]
fn a_rejected_series_allocates_only_its_rejection_text() {
    let encoder = encoder();
    let event = event_with(&[("device", "rtr-1"), ("ifIndex", "eth0")], 1.0);
    let mut buf = Vec::with_capacity(4096);
    let rejection =
        |encoder: &SnmpEncoder, buf: &mut Vec<u8>| match encoder.encode_metric(&event, buf) {
            Err(SondaError::Encoder(EncoderError::EventRejected(reason))) => reason,
            other => panic!("expected a rejection, got {other:?}"),
        };
    let first = rejection(&encoder, &mut buf);

    let mut reasons = Vec::with_capacity(CALLS);
    let allocs = count_allocations(|| {
        for _ in 0..CALLS {
            reasons.push(rejection(&encoder, &mut buf));
        }
    });
    // Vacuity guard: every call was rejected, with the first call's text.
    assert_eq!(reasons.len(), CALLS);
    assert!(first.contains("eth0"), "{first}");
    assert!(reasons.iter().all(|reason| *reason == first));
    assert!(buf.is_empty(), "a rejected event writes nothing");
    let per_call = allocs as f64 / CALLS as f64;
    println!("allocations per rejected event: {per_call:.4}");
    assert_eq!(
        allocs, CALLS,
        "a rejected series costs one allocation per event (its cached text); measured \
         {per_call:.4}"
    );
}
