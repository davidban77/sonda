//! Exact allocation budget for the single-event metric write path.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter itself is thread-local, so only allocations made on the
//! measuring thread inside a counted window are counted; the runner is driven
//! on that thread by a current-thread runtime, and tests running in parallel
//! on other threads cannot add to it. The same holds for the write path itself:
//! an allocation it makes on another thread (a `spawn_blocking` write, a
//! background task) is not counted and cannot fail these budgets.
//!
//! Two checks, both numbers rather than "no panic":
//!
//! - the plain path has an absolute budget at its measured baseline, so a
//!   regression every sink pays fails;
//! - the opt-in path is measured against the plain path in the same test, so
//!   anything only an opt-in sink pays — an extra boxed future, a deep-cloned
//!   label set, a formatted string — fails however long the encoded line is.
//!
//! The scenario carries two labels on purpose. With none, cloning the empty
//! label map allocates nothing and a label deep-clone would pass unnoticed.
//!
//! `scheduler_baseline` cannot see a cost this small: it measures drift,
//! dropped ticks and process-wide RSS.

// The whole file drives `schedule::runner`, which the `runtime` feature owns.
#![cfg(all(feature = "runtime", feature = "config"))]

#[path = "common/alloc.rs"]
mod counting;

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::model::metric::MetricEvent;
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{OnSinkError, SondaError};

#[global_allocator]
static ALLOCATOR: counting::CountingAlloc = counting::CountingAlloc;

/// The allocations `body` made on this thread.
fn count_allocations(body: impl FnOnce()) -> usize {
    counting::count_allocations(body).1
}

/// Another thread allocating throughout a counted window adds nothing to it.
/// This is what lets the cases here run in parallel with each other.
#[test]
fn allocations_on_other_threads_are_not_counted() {
    const OTHER_ALLOCS: usize = 10_000;
    let go = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));

    // Spawned before the window opens; inside it, this thread only flips a
    // flag and spins, neither of which allocates.
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

    let allocs = count_allocations(|| {
        go.store(true, Ordering::SeqCst);
        while done.load(Ordering::SeqCst) < OTHER_ALLOCS {
            std::hint::spin_loop();
        }
    });
    other.join().expect("allocating thread panicked");

    // Vacuity guard: the other thread did allocate, all of it inside the window.
    assert_eq!(done.load(Ordering::SeqCst), OTHER_ALLOCS);
    assert_eq!(
        allocs, 0,
        "allocations on another thread leaked into this thread's count"
    );
}

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

/// Sink that opts in. It checks the identity it was handed without allocating
/// — a `String` or a growing `Vec` here would be charged to the path under
/// measurement and hide a regression of the same size.
struct EventSink {
    delivered: Arc<AtomicUsize>,
    wrong_name: Arc<AtomicUsize>,
    plain_writes: Arc<AtomicUsize>,
}

#[async_trait]
impl Sink for EventSink {
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
        self.plain_writes.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
    async fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
    fn wants_metric_events(&self) -> bool {
        true
    }
    async fn write_metric_event(
        &mut self,
        event: &MetricEvent,
        _encoded: &[u8],
    ) -> Result<(), SondaError> {
        self.delivered.fetch_add(1, Ordering::Relaxed);
        if event.name.arc().as_ref() != "alloc_probe" {
            self.wrong_name.fetch_add(1, Ordering::Relaxed);
        }
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
            // Non-empty so a deep clone of the label set costs allocations;
            // see the module doc.
            labels: Some(std::collections::HashMap::from([
                ("device".to_string(), "leaf-1".to_string()),
                ("site".to_string(), "ams".to_string()),
            ])),
            sink: SinkConfig::Memory {
                capture: false,
                max_events: None,
                capture_handle: None,
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Fail,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    }
}

/// Drive the real runner for one second at 1 kHz and return allocations per
/// delivered event, having first warmed the same sink outside the counted
/// window so lazily-built state is not charged to the loop.
fn allocations_per_event(sink: &mut Box<dyn Sink>, delivered: &AtomicUsize) -> f64 {
    let rt = tokio::runtime::Builder::new_current_thread()
        .enable_time()
        .build()
        .expect("runtime must build");
    let config = scenario();
    let cancel = CancellationToken::new();

    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
            .await
            .expect("warm-up run must succeed");
    });

    delivered.store(0, Ordering::Relaxed);
    let allocs = count_allocations(|| {
        rt.block_on(async {
            sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
                .await
                .expect("counted run must succeed");
        });
    });
    let n = delivered.load(Ordering::Relaxed);
    // Vacuity guard: a run that emitted nothing would divide into a flattering
    // zero, so assert the events exist before asserting the ratio.
    assert!(
        n >= 500,
        "a 1 s run at 1 kHz must deliver ~1000 events before the ratio means \
         anything; got {n}"
    );
    allocs as f64 / n as f64
}

/// Allocations per event for a sink that does not opt in.
fn plain_cost() -> f64 {
    let delivered = Arc::new(AtomicUsize::new(0));
    let mut sink: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    allocations_per_event(&mut sink, &delivered)
}

/// A sink that does not opt in must not pay for the event-carrying path.
///
/// `Sink` is `#[async_trait]`, so reaching the *default* `write_metric_event`
/// costs a second boxed future on top of the `write` it forwards to. The
/// labelled scenario measures 1.02 allocations per event: the one boxed
/// future of `write`, the encode buffer being recycled through the write
/// queue. The budget is that plus 0.5, so the 2.02-per-event double-box
/// fails, as does an encode buffer that regrows every tick (5.01).
#[test]
fn plain_sink_pays_no_extra_allocation_per_event() {
    let per_event = plain_cost();
    assert!(
        per_event < 1.6,
        "the plain metric write path is over budget: {per_event:.3} allocations \
         per event, budget < 1.6. About 2.0 means a sink that does not opt in \
         is reaching the default write_metric_event; about 5.0 means \
         push_metric no longer hands back a recycled encode buffer"
    );
}

/// The opt-in path costs the same as the plain one.
///
/// A sink that overrides `write_metric_event` never calls `write`, so it boxes
/// exactly one future per event, as a plain sink's `write` does. Checked
/// relative to the plain path measured in this same test, so the bound stays
/// tight whatever the encoded line costs, including after the encode buffer
/// stops regrowing.
#[test]
fn opt_in_sink_receives_events_at_the_plain_cost() {
    let plain = plain_cost();

    let delivered = Arc::new(AtomicUsize::new(0));
    let wrong_name = Arc::new(AtomicUsize::new(0));
    let plain_writes = Arc::new(AtomicUsize::new(0));
    let mut sink: Box<dyn Sink> = Box::new(EventSink {
        delivered: Arc::clone(&delivered),
        wrong_name: Arc::clone(&wrong_name),
        plain_writes: Arc::clone(&plain_writes),
    });
    // `allocations_per_event` asserts >= 500 deliveries before dividing.
    let opt_in = allocations_per_event(&mut sink, &delivered);

    assert_eq!(
        wrong_name.load(Ordering::Relaxed),
        0,
        "every delivered event must carry the scenario's metric name"
    );
    assert_eq!(
        plain_writes.load(Ordering::Relaxed),
        0,
        "an opt-in sink must receive no metric through the plain write path"
    );
    assert!(
        opt_in - plain < 0.5,
        "the opt-in path must cost what the plain path costs: opt-in \
         {opt_in:.3} vs plain {plain:.3} allocations per event, a surcharge of \
         {:.3} (allowed < 0.5)",
        opt_in - plain
    );
}
