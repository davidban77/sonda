//! Exact allocation budget for the single-event metric write path.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` is process-wide: in the lib test binary the
//! counter would be shared with every other test running in parallel.
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

use std::alloc::{GlobalAlloc, Layout, System};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use tokio_util::sync::CancellationToken;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::model::metric::MetricEvent;
use sonda_core::sink::{Sink, SinkConfig};
use sonda_core::{OnSinkError, SondaError};

static ALLOCS: AtomicUsize = AtomicUsize::new(0);
static COUNTING: AtomicBool = AtomicBool::new(false);

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        if COUNTING.load(Ordering::Relaxed) {
            ALLOCS.fetch_add(1, Ordering::Relaxed);
        }
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

/// Serialises the counted windows. `ALLOCS`/`COUNTING` are process-wide, so two
/// tests measuring at once sum into one counter — which read as ~9 allocations
/// per event (4 + 5) instead of each test's own figure.
static MEASURING: Mutex<()> = Mutex::new(());

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
    let _guard = MEASURING.lock().expect("measuring mutex poisoned");

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
    ALLOCS.store(0, Ordering::Relaxed);
    COUNTING.store(true, Ordering::Relaxed);
    rt.block_on(async {
        sonda_core::schedule::runner::run_with_sink(&config, sink, &cancel, None)
            .await
            .expect("counted run must succeed");
    });
    COUNTING.store(false, Ordering::Relaxed);

    let allocs = ALLOCS.load(Ordering::Relaxed);
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
