//! Allocation surcharge of the `gnmi_target` sink over a plain sink, through
//! the real runner.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter is thread-local and each run uses a current-thread
//! runtime, so the runner, the sink and the target's listener task all
//! allocate on the measuring thread, and nothing on any other thread is
//! counted.
//!
//! Both runs use the same two-label scenario and the same `gnmi` encoder, so
//! the difference between them is what the sink adds per event.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(all(feature = "gnmi", feature = "config"))]

mod common;
// `common` is compiled only with `config`; the counter is needed without it.
#[path = "common/alloc.rs"]
mod counting;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig};
use sonda_core::encoder::{Encoder, EncoderConfig};
use sonda_core::generator::GeneratorConfig;
use sonda_core::model::metric::{Labels, MetricEvent};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::sink::gnmi::{GnmiTarget, GnmiTargetConfig, GnmiTargetSink};
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

fn scenario() -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name: "alloc_probe".to_string(),
            rate: 1000.0,
            duration: Some("1s".to_string()),
            gaps: None,
            gap_windows: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            // Two labels, so a deep clone of the label set would cost
            // allocations and show up in the surcharge.
            labels: Some(std::collections::HashMap::from([
                ("device".to_string(), "leaf-1".to_string()),
                ("site".to_string(), "ams".to_string()),
            ])),
            sink: SinkConfig::GnmiTarget(GnmiTargetConfig::new("127.0.0.1:0")),
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        // `device` is the default target label; `site` is in the path.
        encoder: EncoderConfig::Gnmi(GnmiEncoderConfig {
            path: Some("/sites/site[name={site}]/{name}".to_string()),
            ..GnmiEncoderConfig::default()
        }),
        metric_type: None,
        help: None,
    }
}

/// Drive the real runner for one second at 1 kHz on `rt` and return
/// allocations per event, having first warmed the same sink outside the
/// counted window so first-sight state (the encoder's path cache, the
/// target's series slot) is not charged to the loop.
fn allocations_per_event(rt: &tokio::runtime::Runtime, sink: &mut Box<dyn Sink>) -> (f64, u64) {
    let config = scenario();
    let cancel = CancellationToken::new();

    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
            .await
            .expect("warm-up run must succeed");
    });

    let stats = Arc::new(std::sync::RwLock::new(ScenarioStats::default()));
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
    let events = stats.read().expect("stats lock").total_events;
    // Vacuity guard: a run that emitted nothing would divide into a flattering
    // zero, so assert the events exist before asserting the ratio.
    assert!(
        events >= 500,
        "a 1 s run at 1 kHz must emit ~1000 events before the ratio means \
         anything; got {events}"
    );
    (allocs as f64 / events as f64, events)
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime must build")
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
    let ((), allocs) = count_allocations(|| {
        go.store(true, Ordering::SeqCst);
        while done.load(Ordering::SeqCst) < OTHER_ALLOCS {
            std::hint::spin_loop();
        }
    });
    other.join().expect("allocating thread panicked");
    // Vacuity guard: the other thread did allocate, all of it inside the window.
    assert_eq!(done.load(Ordering::SeqCst), OTHER_ALLOCS);
    assert_eq!(allocs, 0, "another thread's allocations were counted");
}

/// The `gnmi_target` sink costs less than 1.5 allocations per event more
/// than a plain sink, with no subscriber connected. Publishing copies the
/// encoded notification once.
#[test]
fn gnmi_target_surcharge_over_a_plain_sink_is_under_one_and_a_half() {
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

    let target_sink = rt
        .block_on(GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    let target: Arc<GnmiTarget> = Arc::clone(target_sink.target());
    let mut sink: Box<dyn Sink> = Box::new(target_sink);
    let (target_cost, _) = allocations_per_event(&rt, &mut sink);

    // Vacuity guards: the events reached the target as one series, and
    // nobody was subscribed to cost a broadcast clone.
    assert_eq!(target.series_count(), 1, "the target holds the series");
    assert_eq!(target.subscriber_count(), 0);

    let surcharge = target_cost - plain_cost;
    println!(
        "allocations per event: plain {plain_cost:.3}, gnmi_target {target_cost:.3}, \
         surcharge {surcharge:.3}"
    );
    assert!(
        surcharge < 1.5,
        "the gnmi_target sink is over budget: {target_cost:.3} allocations per \
         event against {plain_cost:.3} for a plain sink, a surcharge of \
         {surcharge:.3} (allowed < 1.5)"
    );
    drop(sink);
    drop(target);
}

/// Dropping a sink releases its series. With nobody subscribed, the release
/// allocates nothing: no delete is encoded. The target is held, so dropping
/// the sink does not stop it.
#[test]
fn a_release_with_no_receiver_allocates_nothing() {
    let rt = current_thread_runtime();
    let mut sink = rt
        .block_on(GnmiTargetSink::new(&GnmiTargetConfig::new("127.0.0.1:0")))
        .expect("bind an ephemeral port");
    let target: Arc<GnmiTarget> = Arc::clone(sink.target());
    let encoder = GnmiEncoder::new(&GnmiEncoderConfig {
        path: Some("/sites/site[name={site}]/{name}".to_string()),
        ..GnmiEncoderConfig::default()
    })
    .expect("valid encoder");
    for site in ["ams", "fra", "lon"] {
        let event = MetricEvent::new(
            "up".to_string(),
            1.0,
            Labels::from_pairs(&[("site", site)]).expect("valid labels"),
        )
        .expect("valid event");
        let mut buf = Vec::new();
        encoder.encode_metric(&event, &mut buf).expect("encodes");
        rt.block_on(sink.write_metric_event(&event, &buf))
            .expect("published");
    }
    // Vacuity guards: the series are in the table and nobody is subscribed.
    assert_eq!(target.series_count(), 3);
    assert_eq!(target.subscriber_count(), 0);

    let ((), allocs) = count_allocations(|| drop(sink));

    assert_eq!(target.series_count(), 0, "the drop released every series");
    assert_eq!(
        allocs, 0,
        "releasing with no receiver allocated {allocs} times"
    );
    drop(target);
}
