//! Steady-state benchmark for the `while:` runtime hot path.
//!
//! Measures the per-tick cost of the `gate_bus.tick(value)` call and the
//! gated_loop wrapper relative to an ungated baseline. The CI perf-gate
//! lives separately as a `#[test] fn` in `tests/while_runtime.rs`; this
//! bench is the developer-facing profiler.

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use criterion::{criterion_group, criterion_main, Criterion};
use sonda_core::compiler::WhileOp;
use sonda_core::config::{BaseScheduleConfig, ScenarioConfig, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::gate_bus::{GateBus, SubscriptionSpec, WhileSpec};
use sonda_core::schedule::launch::launch_scenario_with_gates;
use sonda_core::schedule::GateContext;
use sonda_core::sink::SinkConfig;

fn metrics_entry(name: &str, rate: f64, duration_ms: u64) -> ScenarioEntry {
    ScenarioEntry::Metrics(ScenarioConfig {
        base: BaseScheduleConfig {
            name: name.to_string(),
            rate,
            duration: Some(format!("{duration_ms}ms")),
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
    })
}

fn bench_baseline_ungated(c: &mut Criterion) {
    c.bench_function("baseline_ungated_300ms_at_1khz", |b| {
        b.iter(|| {
            let entry = metrics_entry("bench", 1000.0, 300);
            let shutdown = Arc::new(AtomicBool::new(true));
            let mut handle =
                launch_scenario_with_gates("bench".to_string(), entry, shutdown, None, None, None)
                    .unwrap();
            handle.join(Some(Duration::from_secs(2))).unwrap();
        });
    });
}

fn bench_gated_open(c: &mut Criterion) {
    c.bench_function("gated_open_300ms_at_1khz", |b| {
        b.iter(|| {
            let bus = Arc::new(GateBus::new());
            bus.tick(1.0);
            let (rx, init) = bus.subscribe(SubscriptionSpec {
                after: None,
                while_: Some(WhileSpec {
                    op: WhileOp::GreaterThan,
                    threshold: 0.0,
                }),
            });
            let entry = metrics_entry("gated", 1000.0, 300);
            let shutdown = Arc::new(AtomicBool::new(true));
            let mut handle = launch_scenario_with_gates(
                "gated".to_string(),
                entry,
                shutdown,
                None,
                Some(Arc::clone(&bus)),
                Some(GateContext {
                    gate_rx: rx,
                    initial: init,
                    delay: None,
                    has_after: false,
                    has_while: true,
                    close_emit: None,
                }),
            )
            .unwrap();
            handle.join(Some(Duration::from_secs(2))).unwrap();
        });
    });
}

fn bench_publishing_only(c: &mut Criterion) {
    // Upstream publishing tick() with no subscribers — measures the
    // fast-path early-out (lock + bit-equal compare).
    c.bench_function("bus_tick_no_subscribers", |b| {
        let bus = Arc::new(GateBus::new());
        b.iter(|| {
            for i in 0..1000u64 {
                bus.tick(i as f64);
            }
        });
    });
}

fn bench_subscribe_eval(c: &mut Criterion) {
    // Bus tick with one while-subscriber, alternating value to force edge.
    c.bench_function("bus_tick_with_one_while_subscriber", |b| {
        let bus = Arc::new(GateBus::new());
        let (_rx, _init) = bus.subscribe(SubscriptionSpec {
            after: None,
            while_: Some(WhileSpec {
                op: WhileOp::GreaterThan,
                threshold: 0.5,
            }),
        });
        b.iter(|| {
            for i in 0..1000u64 {
                bus.tick(if i % 2 == 0 { 1.0 } else { 0.0 });
            }
        });
    });
}

criterion_group!(
    benches,
    bench_baseline_ungated,
    bench_gated_open,
    bench_publishing_only,
    bench_subscribe_eval,
);
criterion_main!(benches);
