//! Allocation surcharge of the `snmp_agent` sink over a plain sink, through
//! the real runner, and over 10 000 writes made directly.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter is thread-local and each run uses a current-thread
//! runtime, so the runner, the sink and the agent's responder task all
//! allocate on the measuring thread, and nothing on any other thread is
//! counted.
//!
//! Both runs of a pair use the same two-label scenario and the same `snmp`
//! encoder, so the difference between them is what the sink adds per event.

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
use sonda_core::sink::snmp::{SnmpAgent, SnmpAgentConfig, SnmpAgentSink};
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{OnSinkError, SondaError};

#[global_allocator]
static ALLOCATOR: counting::CountingAlloc = counting::CountingAlloc;

use counting::count_allocations;

/// Sink that does not opt in, so it keeps the plain byte path.
struct PlainSink {
    delivered: Arc<AtomicUsize>,
}

#[async_trait]
impl Sink for PlainSink {
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

fn encoder_config() -> SnmpEncoderConfig {
    SnmpEncoderConfig {
        oids: HashMap::from([(
            "ifHCInOctets".to_string(),
            "1.3.6.1.2.1.31.1.1.1.6.{ifIndex}".to_string(),
        )]),
        types: HashMap::from([("ifHCInOctets".to_string(), SnmpValueType::Counter64)]),
        ..SnmpEncoderConfig::default()
    }
}

/// The two-label scenario: `device` is the agent label and `ifIndex` the
/// template's placeholder. The value steps, so every publish stores a new
/// `VarBind`.
fn scenario() -> ScenarioConfig {
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
            sink: SinkConfig::SnmpAgent(SnmpAgentConfig::new("127.0.0.1:0")),
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
        encoder: EncoderConfig::Snmp(encoder_config()),
        metric_type: None,
        help: None,
    }
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime must build")
}

/// Drive the real runner for one second at 1 kHz on `rt` and return
/// allocations per event and the events counted, having first warmed the
/// same sink outside the counted window so first-sight state (the sink's
/// decoded OID, the agent's instance) is not charged to the loop.
fn allocations_per_event(rt: &tokio::runtime::Runtime, sink: &mut Box<dyn Sink>) -> (f64, u64) {
    let config = scenario();
    let cancel = CancellationToken::new();
    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
            .await
            .expect("warm-up run must succeed");
    });

    let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        rt.block_on(async {
            sonda_core::schedule::runner::run_with_sink(
                &config,
                sink,
                &cancel,
                Some(Arc::clone(&stats)),
            )
            .await
            .expect("counted run must succeed");
        })
    });
    let stats = stats.read().expect("stats lock");
    let events = stats.total_events;
    // Vacuity guards: a run that emitted nothing would divide into a
    // flattering zero, and a rejected event would skip the sink.
    assert!(
        events >= 500,
        "a 1 s run at 1 kHz must emit ~1000 events before the ratio means \
         anything; got {events}"
    );
    assert_eq!(stats.rejected_events, 0, "no event was rejected");
    assert_eq!(stats.errors, 0, "no write failed");
    (allocs as f64 / events as f64, events)
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
    let ((), allocs) = count_allocations(|| {
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

/// The agent behind a new `snmp_agent` sink on an ephemeral port, built on
/// `rt`, with its responder polled once so its first run is not counted.
fn agent_sink(rt: &tokio::runtime::Runtime) -> (Box<dyn Sink>, Arc<SnmpAgent>) {
    let sink = rt
        .block_on(SnmpAgentSink::new(&SnmpAgentConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    rt.block_on(tokio::task::yield_now());
    let agent = Arc::clone(sink.agent());
    (Box::new(sink), agent)
}

/// Through the runner, the `snmp_agent` sink costs no allocation per event
/// more than a plain sink on the same scenario and encoder: each record's
/// `VarBind` is copied into one of two buffers the sink keeps for the
/// instance, whose last `Bytes` the agent has dropped.
#[test]
fn snmp_agent_surcharge_over_a_plain_sink_is_zero() {
    let rt = current_thread_runtime();
    let delivered = Arc::new(AtomicUsize::new(0));
    let mut plain: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let (plain_cost, plain_events) = allocations_per_event(&rt, &mut plain);
    assert!(
        delivered.load(Ordering::Relaxed) as u64 >= plain_events,
        "the plain sink received every event through write"
    );
    assert!(plain_cost > 0.0, "the plain sink's figure was counted");

    let (mut sink, agent) = agent_sink(&rt);
    let (agent_cost, _) = allocations_per_event(&rt, &mut sink);
    // Vacuity guard: the events reached the agent as one instance.
    assert_eq!(agent.instance_count(), 1, "the agent holds the instance");

    let surcharge = agent_cost - plain_cost;
    println!(
        "allocations per event: plain {plain_cost:.4}, snmp_agent {agent_cost:.4}, \
         surcharge {surcharge:.4}"
    );
    assert!(
        surcharge < 0.01,
        "the snmp_agent sink is over budget: {agent_cost:.3} allocations per event \
         against {plain_cost:.3} for a plain sink, a surcharge of {surcharge:.3} \
         (allowed < 0.01)"
    );
    drop(sink);
}

/// What a series' first record costs through the `snmp_agent` sink when it
/// carries an `ifName` column, encode and write together, printed as a
/// figure and not bounded: the sink decodes and stores the instance and the
/// column's instance once each.
#[test]
fn a_first_record_with_a_label_column_is_measured() {
    let rt = current_thread_runtime();
    let (mut sink, agent) = agent_sink(&rt);
    let encoder = SnmpEncoder::new(&column_encoder_config()).expect("a valid encoder");
    let events = interface_events(1, true);
    let mut buf = Vec::with_capacity(4096);
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        encoder
            .encode_metric(&events[0], &mut buf)
            .expect("encodes");
        rt.block_on(sink.write_metric_event(&events[0], &buf))
            .expect("the write succeeds");
    });
    // Vacuity guard: the record carried the column and the agent took both.
    assert_eq!(buf[1], 1, "one companion");
    assert_eq!(agent.instance_count(), 2, "the instance and its ifName");
    println!("first record with one label column, encode and write: {allocs} allocations");
    drop(sink);
}

/// Publishes [`publish_cost`] counts, after its warm-up.
const PUBLISHES: usize = 10_000;

/// [`encoder_config`] with `ifName` served as a column.
fn column_encoder_config() -> SnmpEncoderConfig {
    SnmpEncoderConfig {
        label_columns: HashMap::from([(
            "ifName".to_string(),
            "1.3.6.1.2.1.31.1.1.1.1.{ifIndex}".into(),
        )]),
        ..encoder_config()
    }
}

/// `series` events on distinct interfaces, with values that differ per
/// event, each named with `ifName` when `named`, built before any counted
/// window.
fn interface_events(series: usize, named: bool) -> Vec<MetricEvent> {
    (0..series)
        .map(|i| {
            let if_index = (i + 1).to_string();
            let if_name = format!("GigabitEthernet0/0/{i}");
            let mut labels = vec![("device", "rtr-1"), ("ifIndex", if_index.as_str())];
            if named {
                labels.push(("ifName", if_name.as_str()));
            }
            MetricEvent::with_timestamp(
                "ifHCInOctets".to_string(),
                (i * 1_000) as f64,
                Labels::from_pairs(&labels).expect("valid labels"),
                UNIX_EPOCH + Duration::from_secs(1_700_000_000),
            )
            .expect("a valid event")
        })
        .collect()
}

/// The allocations per event of writing `events` in turn, `PUBLISHES` times
/// in all, after a warm-up of two writes per event: through
/// [`Sink::write_metric_event`] when the sink takes metric events, else
/// through [`Sink::write`], as the runner calls them. Each event is encoded
/// with `config` into one reused buffer before its write.
fn publish_cost(
    rt: &tokio::runtime::Runtime,
    sink: &mut Box<dyn Sink>,
    events: &[MetricEvent],
    config: &SnmpEncoderConfig,
) -> f64 {
    let encoder = SnmpEncoder::new(config).expect("a valid encoder");
    let mut buf = Vec::with_capacity(4096);
    let mut write = |sink: &mut Box<dyn Sink>, event: &MetricEvent| {
        buf.clear();
        encoder.encode_metric(event, &mut buf).expect("encodes");
        rt.block_on(async {
            if sink.wants_metric_events() {
                sink.write_metric_event(event, &buf).await
            } else {
                sink.write(&buf).await
            }
        })
        .expect("the write succeeds");
    };
    for event in events.iter().chain(events) {
        write(sink, event);
    }
    let mut written = 0usize;
    counting::assert_count_allocations_counts();
    let ((), allocs) = count_allocations(|| {
        for event in events.iter().cycle().take(PUBLISHES) {
            write(sink, event);
            written += 1;
        }
    });
    // Vacuity guard: every counted write ran.
    assert_eq!(written, PUBLISHES);
    allocs as f64 / written as f64
}

/// Over 10 000 publishes after a warm-up of two per instance, the
/// `snmp_agent` sink allocates per event what a plain sink does, for a sink
/// publishing one instance and for one publishing two in turn, with and
/// without an `ifName` column: the column's instance arrives with the
/// series' first record and costs nothing after it.
#[rstest::rstest]
#[case::one_instance(1, false)]
#[case::two_instances(2, false)]
#[case::one_instance_with_a_label_column(1, true)]
#[case::two_instances_with_a_label_column(2, true)]
fn direct_publishes_cost_nothing_over_a_plain_sink(#[case] series: usize, #[case] named: bool) {
    let rt = current_thread_runtime();
    let events = interface_events(series, named);
    let config = if named {
        column_encoder_config()
    } else {
        encoder_config()
    };

    let delivered = Arc::new(AtomicUsize::new(0));
    let mut plain: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let plain_cost = publish_cost(&rt, &mut plain, &events, &config);
    assert!(delivered.load(Ordering::Relaxed) >= PUBLISHES);
    assert!(plain_cost > 0.0, "the plain sink's figure was counted");

    let (mut sink, agent) = agent_sink(&rt);
    let agent_cost = publish_cost(&rt, &mut sink, &events, &config);
    // Vacuity guard: every event reached the agent as its own instance, with
    // its ifName column's instance when named.
    let per_series = if named { 2 } else { 1 };
    assert_eq!(agent.instance_count(), series * per_series);

    let surcharge = agent_cost - plain_cost;
    println!(
        "allocations per publish, {series} instance(s), label column {named}: plain \
         {plain_cost:.4}, snmp_agent {agent_cost:.4}, surcharge {surcharge:.4}"
    );
    assert!(
        surcharge < 0.01,
        "the snmp_agent sink allocates {surcharge:.3} per publish more than a plain sink \
         (allowed < 0.01)"
    );
    drop(sink);
}
