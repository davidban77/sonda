//! Per-event cost of dispatching a metric write through a `dyn Sink`.
//!
//! Measures `Sink::write` on `MemorySink` and on a sink that opts in by
//! overriding `write_metric_event`, then `write_metric_event` on each: through
//! the default body (which forwards to `write`) on `MemorySink`, and through
//! the override, with and without cloning the event on every call. Compare
//! rows on the same sink; the two sinks' `write` bodies differ. The event and
//! the 64 encoded bytes are built once; nothing is encoded inside the measured
//! loop.
//!
//! No production caller reaches `sink_write_metric_event/default`: the runners
//! and `emit_metric` call `write_metric_event` only when `wants_metric_events`
//! is true, and `MemorySink` returns false. The row is a reference for the
//! second boxed future that check avoids, not a cost a plain sink pays. The CI allocation gate lives separately
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
///
/// `writes` counts calls to `write`. The default `write_metric_event` forwards
/// to `write`, so a zero count after a `write_metric_event` pass proves the
/// override is the body being measured.
#[derive(Default)]
struct OptInSink {
    writes: u64,
}

#[async_trait]
impl Sink for OptInSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        black_box(data);
        self.writes += 1;
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
    let mut opt_in = Box::new(OptInSink::default());
    assert!(!memory.wants_metric_events());
    assert!(opt_in.wants_metric_events());

    // One untimed pass per method on each sink, so no row can time a path that
    // does not run. `MemorySink` must receive every byte through both `write`
    // and the default `write_metric_event`.
    rt.block_on(async {
        let sink: &mut dyn Sink = black_box(memory.as_mut());
        for _ in 0..EVENTS_PER_ITER {
            sink.write(bytes).await.expect("memory write");
            sink.write_metric_event(&event, bytes)
                .await
                .expect("memory write_metric_event");
        }
    });
    assert_eq!(
        memory.buffer.len() as u64,
        2 * EVENTS_PER_ITER * ENCODED.len() as u64
    );
    memory.buffer.clear();

    // `OptInSink` must reach its override without falling through to `write`,
    // and its `write` must run on the `sink_write/opt_in` path.
    rt.block_on(async {
        let sink: &mut dyn Sink = black_box(opt_in.as_mut());
        for _ in 0..EVENTS_PER_ITER {
            sink.write_metric_event(&event, bytes)
                .await
                .expect("opt-in write_metric_event");
        }
    });
    assert_eq!(opt_in.writes, 0, "write_metric_event fell through to write");
    rt.block_on(async {
        let sink: &mut dyn Sink = black_box(opt_in.as_mut());
        for _ in 0..EVENTS_PER_ITER {
            sink.write(bytes).await.expect("opt-in write");
        }
    });
    assert_eq!(opt_in.writes, EVENTS_PER_ITER);

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

    group.bench_function(BenchmarkId::new("sink_write", "opt_in"), |b| {
        b.iter(|| {
            rt.block_on(async {
                let sink: &mut dyn Sink = black_box(opt_in.as_mut());
                for _ in 0..EVENTS_PER_ITER {
                    black_box(sink.write(bytes).await).expect("opt-in write");
                }
            });
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
