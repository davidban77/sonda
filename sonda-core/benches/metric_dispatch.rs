//! Per-event cost of dispatching a metric write through a `dyn Sink`.
//!
//! Measures `Sink::write` on `MemorySink`, `Sink::write_metric_event` through
//! its default body (which forwards to `write`), and a sink that opts in by
//! overriding `write_metric_event`, with and without cloning the event on
//! every call. The event and the 64 encoded bytes are built once; nothing is
//! encoded inside the measured loop. The CI allocation gate lives separately
//! as `#[test] fn`s in `tests/metric_path_allocations.rs`; this bench is the
//! developer-facing profiler and asserts no timings.
//!
//! Run with `cargo bench -p sonda-core --bench metric_dispatch`.

use std::hint::black_box;
use std::sync::Arc;
use std::time::SystemTime;

use async_trait::async_trait;
use criterion::{criterion_group, criterion_main, BenchmarkId, Criterion, Throughput};
use sonda_core::model::metric::{Labels, MetricEvent, ValidatedMetricName};
use sonda_core::sink::memory::MemorySink;
use sonda_core::sink::Sink;
use sonda_core::SondaError;

/// Events dispatched per measured iteration, so `block_on` is amortised.
const EVENTS_PER_ITER: u64 = 10_000;

/// Stand-in for one encoded metric line.
const ENCODED: [u8; 64] = [b'x'; 64];

/// A sink that opts in to metric events and stores nothing.
struct OptInSink;

#[async_trait]
impl Sink for OptInSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        black_box(data);
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
        encoded: &[u8],
    ) -> Result<(), SondaError> {
        black_box(event);
        black_box(encoded);
        Ok(())
    }
}

fn metric_event() -> MetricEvent {
    let name = ValidatedMetricName::new("interface_in_octets").expect("valid metric name");
    let labels = Labels::from_pairs(&[
        ("device", "leaf-01"),
        ("interface", "Ethernet1"),
        ("region", "eu-west"),
        ("role", "leaf"),
    ])
    .expect("valid labels");
    MetricEvent::from_parts(name, 1.0, Arc::new(labels), SystemTime::now())
}

fn bench_metric_dispatch(c: &mut Criterion) {
    let rt = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("tokio runtime must build");
    let event = metric_event();
    let bytes: &[u8] = &ENCODED;

    let mut memory = Box::new(MemorySink::new());
    let mut opt_in: Box<dyn Sink> = Box::new(OptInSink);
    assert!(!memory.wants_metric_events());
    assert!(opt_in.wants_metric_events());

    // One untimed pass proves the memory rows deliver every byte.
    rt.block_on(async {
        let sink: &mut dyn Sink = black_box(memory.as_mut());
        for _ in 0..EVENTS_PER_ITER {
            sink.write(bytes).await.expect("memory write");
        }
    });
    assert_eq!(memory.buffer.len() as u64, EVENTS_PER_ITER * 64);
    memory.buffer.clear();

    let mut group = c.benchmark_group("metric_dispatch");
    group.throughput(Throughput::Elements(EVENTS_PER_ITER));

    group.bench_function(BenchmarkId::new("sink_write", "memory"), |b| {
        b.iter(|| {
            rt.block_on(async {
                let sink: &mut dyn Sink = black_box(memory.as_mut());
                for _ in 0..EVENTS_PER_ITER {
                    black_box(sink.write(bytes).await).expect("memory write");
                }
            });
            memory.buffer.clear();
        });
    });

    group.bench_function(
        BenchmarkId::new("sink_write_metric_event", "default"),
        |b| {
            b.iter(|| {
                rt.block_on(async {
                    let sink: &mut dyn Sink = black_box(memory.as_mut());
                    for _ in 0..EVENTS_PER_ITER {
                        black_box(sink.write_metric_event(&event, bytes).await)
                            .expect("memory write_metric_event");
                    }
                });
                memory.buffer.clear();
            });
        },
    );

    group.bench_function(BenchmarkId::new("sink_write_metric_event", "opt_in"), |b| {
        b.iter(|| {
            rt.block_on(async {
                let sink: &mut dyn Sink = black_box(opt_in.as_mut());
                for _ in 0..EVENTS_PER_ITER {
                    black_box(sink.write_metric_event(&event, bytes).await)
                        .expect("opt-in write_metric_event");
                }
            });
        });
    });

    group.bench_function(
        BenchmarkId::new("sink_write_metric_event", "opt_in_clone"),
        |b| {
            b.iter(|| {
                rt.block_on(async {
                    let sink: &mut dyn Sink = black_box(opt_in.as_mut());
                    for _ in 0..EVENTS_PER_ITER {
                        let owned = event.clone();
                        black_box(sink.write_metric_event(&owned, bytes).await)
                            .expect("opt-in write_metric_event");
                    }
                });
            });
        },
    );

    group.finish();
}

criterion_group!(benches, bench_metric_dispatch);
criterion_main!(benches);
