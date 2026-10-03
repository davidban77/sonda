//! Exact allocation budget for the single-event metric write path.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` is process-wide: in the lib test binary the
//! counter would be shared with every other test running in parallel.
//!
//! The budget is a number, not "no panic", so a later change that boxes an
//! extra future, deep-clones a label map or formats a string on this path fails
//! here. `scheduler_baseline` cannot see a cost this small — it measures drift,
//! dropped ticks and process-wide RSS.

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

/// Sink that opts in, recording the identity it was handed.
struct EventSink {
    delivered: Arc<AtomicUsize>,
    names: Arc<Mutex<Vec<String>>>,
}

#[async_trait]
impl Sink for EventSink {
    async fn write(&mut self, _data: &[u8]) -> Result<(), SondaError> {
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
        self.names
            .lock()
            .expect("name log mutex poisoned")
            .push(event.name.arc().to_string());
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
            labels: None,
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

/// A sink that does not opt in must not pay for the event-carrying path.
///
/// `write_metric_event` is `#[async_trait]`, so reaching it costs a second
/// boxed future on top of `write`'s. 4 allocations per event is the cost before
/// the hook existed; the budget leaves one slot for per-run fixed cost
/// amortised across the ticks and still fails the 5-per-event double-box.
#[test]
fn plain_sink_pays_no_extra_allocation_per_event() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let mut sink: Box<dyn Sink> = Box::new(PlainSink {
        delivered: Arc::clone(&delivered),
    });
    let per_event = allocations_per_event(&mut sink, &delivered);

    assert!(
        per_event < 5.0,
        "a sink that does not want metric events must keep its original \
         per-event cost: {per_event:.3} allocations per event, budget < 5.0"
    );
}

/// The opt-in sink is actually handed the event, and its cost stays bounded.
#[test]
fn opt_in_sink_receives_events_within_a_bounded_budget() {
    let delivered = Arc::new(AtomicUsize::new(0));
    let names = Arc::new(Mutex::new(Vec::new()));
    let mut sink: Box<dyn Sink> = Box::new(EventSink {
        delivered: Arc::clone(&delivered),
        names: Arc::clone(&names),
    });
    let per_event = allocations_per_event(&mut sink, &delivered);

    let captured = names.lock().expect("name log mutex poisoned");
    assert!(
        captured.len() >= 500,
        "the opt-in sink must have been handed the events: got {}",
        captured.len()
    );
    assert!(
        captured.iter().all(|n| n == "alloc_probe"),
        "every delivered event must carry the scenario's metric name"
    );
    drop(captured);

    assert!(
        per_event < 7.0,
        "the opt-in path must stay bounded: {per_event:.3} allocations per \
         event, budget < 7.0"
    );
}
