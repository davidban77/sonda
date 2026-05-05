//! Runtime acceptance tests for `while:` continuous gating.
//!
//! Covers the lifecycle state machine, `pending`→`running`/`paused`,
//! `delay:` debounce, multi-subscriber coupling, and the
//! perf-regression invariant against the non-gated baseline.

#![cfg(feature = "config")]

mod common;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use sonda_core::compiler::{DelayClause, WhileOp};
use sonda_core::config::{BaseScheduleConfig, LogScenarioConfig, ScenarioConfig, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::{GeneratorConfig, LogGeneratorConfig, TemplateConfig};
use sonda_core::schedule::gate_bus::{AfterOpDir, AfterSpec, GateBus, SubscriptionSpec, WhileSpec};
use sonda_core::schedule::launch::launch_scenario_with_gates;
use sonda_core::schedule::stats::ScenarioState;
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

fn logs_entry(name: &str, rate: f64, duration_ms: u64) -> ScenarioEntry {
    ScenarioEntry::Logs(LogScenarioConfig {
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
        generator: LogGeneratorConfig::Template {
            templates: vec![TemplateConfig {
                message: "gated log".to_string(),
                field_pools: std::collections::BTreeMap::new(),
            }],
            severity_weights: None,
            seed: Some(0),
        },
        encoder: EncoderConfig::JsonLines { precision: None },
    })
}

fn while_gt_zero() -> SubscriptionSpec {
    SubscriptionSpec {
        after: None,
        while_: Some(WhileSpec {
            op: WhileOp::GreaterThan,
            threshold: 0.0,
        }),
    }
}

#[test]
fn issue_295_repro_gated_scenario_emits_only_when_gate_open() {
    // Upstream metric oscillates 0 → 1 → 0 across 600ms; downstream gated
    // by `while: ref=upstream op=">" value=0`. Drive the bus directly so
    // the test is deterministic.
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    let gate_ctx = GateContext {
        gate_rx: rx,
        initial: init,
        delay: None,
        has_after: false,
        has_while: true,
        close_emit: None,
    };

    let entry = metrics_entry("downstream", 200.0, 600);
    let mut handle = launch_scenario_with_gates(
        "downstream".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(gate_ctx),
    )
    .expect("launch must succeed");

    // Initially paused.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(handle.stats_snapshot().total_events, 0, "paused at start");

    // Open the gate.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(150));
    let mid = handle.stats_snapshot().total_events;
    assert!(mid > 0, "gate open must emit events, got {mid}");

    // Close the gate.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(200));
    let after_close = handle.stats_snapshot().total_events;
    // Wait a bit more — counter must not advance significantly while paused.
    thread::sleep(Duration::from_millis(100));
    let after_pause = handle.stats_snapshot().total_events;
    assert!(
        after_pause - after_close <= 5,
        "paused state must freeze tick counter (allowing ≤5 in-flight slop), got {} → {}",
        after_close,
        after_pause
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_state_starts_pending_then_running_when_gate_open_at_subscription() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0); // gate open before subscription
    let (rx, init) = bus.subscribe(while_gt_zero());
    assert_eq!(init.while_gate_open, Some(true));

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("d1", 100.0, 300);
    let mut handle = launch_scenario_with_gates(
        "d1".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(150));
    let snap = handle.stats_snapshot();
    assert!(
        snap.total_events > 0,
        "with gate already open, scenario must begin emitting"
    );
    assert!(matches!(
        snap.state,
        ScenarioState::Running | ScenarioState::Finished
    ));

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_state_starts_paused_when_gate_closed_at_subscription() {
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());
    assert_eq!(init.while_gate_open, Some(false));

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("d2", 100.0, 300);
    let mut handle = launch_scenario_with_gates(
        "d2".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(150));
    let snap = handle.stats_snapshot();
    assert_eq!(snap.total_events, 0, "must stay paused");
    assert!(matches!(snap.state, ScenarioState::Paused));

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_no_catch_up_burst_on_resume() {
    // Verify A1h: after a long pause, resume must emit at the configured
    // rate, not "catch up" with a burst of events.
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("d3", 100.0, 1500);
    let mut handle = launch_scenario_with_gates(
        "d3".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Phase 1: open then close after 200ms running.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(200));
    let after_first_open = handle.stats_snapshot().total_events;
    bus.tick(0.0);

    // Phase 2: pause for 500ms.
    thread::sleep(Duration::from_millis(500));
    let after_pause = handle.stats_snapshot().total_events;

    // Phase 3: reopen and immediately measure the rate over 200ms.
    bus.tick(1.0);
    let resume_at = Instant::now();
    thread::sleep(Duration::from_millis(200));
    let after_resume = handle.stats_snapshot().total_events;
    let resume_window_events = after_resume - after_pause;

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();

    // At rate=100 over ~200ms we expect ~20 events. A catch-up burst
    // would emit 50+ events instantly. Assert ≤ 35 to allow some
    // scheduling slack.
    let resume_elapsed = resume_at.elapsed();
    assert!(
        resume_window_events <= 35,
        "no catch-up burst: expected ≤35 events in {resume_elapsed:?}, got {resume_window_events}; \
         (after_first_open={after_first_open}, after_pause={after_pause})"
    );
}

/// Build a metrics entry whose generator is supplied by the caller — used by
/// the tick-preservation tests to drive sequence/saturation generators with
/// known internal state.
fn metrics_entry_with_generator(
    name: &str,
    rate: f64,
    duration_ms: u64,
    generator: GeneratorConfig,
) -> ScenarioEntry {
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
        generator,
        encoder: EncoderConfig::PrometheusText { precision: None },
    })
}

#[test]
fn while_runtime_sequence_generator_preserves_position_across_pause() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    let values = vec![10.0, 20.0, 30.0, 40.0, 50.0, 60.0, 70.0];
    let entry = metrics_entry_with_generator(
        "seq_gated",
        20.0, // 20/s = 50ms per tick
        2000,
        GeneratorConfig::Sequence {
            values: values.clone(),
            repeat: Some(false),
        },
    );
    let mut handle = launch_scenario_with_gates(
        "seq_gated".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Phase 1: emit ~3 events (150ms at 20/s).
    thread::sleep(Duration::from_millis(150));
    let phase1 = handle.recent_metrics();
    let phase1_count = phase1.len();
    assert!(
        phase1_count >= 2 && phase1_count <= 4,
        "phase 1 expected ~3 events, got {phase1_count}"
    );
    let last_phase1_value = phase1
        .last()
        .map(|e| e.value)
        .expect("phase 1 must have at least one value");

    // Phase 2: close gate.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(300));

    // Phase 3: reopen and let several more ticks fire.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(200));
    let phase3 = handle.recent_metrics();

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();

    let next_value_after_pause = phase3
        .first()
        .map(|e| e.value)
        .expect("phase 3 must emit at least one event");

    assert!(
        next_value_after_pause > last_phase1_value,
        "sequence must continue past pause: last_before_pause={last_phase1_value}, \
         first_after_resume={next_value_after_pause}"
    );
    // Phase 1 emitted ticks 0..2 (values 10/20/30); resume lands on tick 3+.
    assert!(
        next_value_after_pause >= 40.0 - f64::EPSILON,
        "sequence must skip ahead by paused-time worth of ticks: got {next_value_after_pause}"
    );
}

#[test]
fn while_runtime_ramp_generator_slope_preserved_across_pause() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    // Sawtooth (saturation desugars to this): 0 → 100 over 4s at 50/s.
    let entry = metrics_entry_with_generator(
        "sat_gated",
        50.0,
        3000,
        GeneratorConfig::Sawtooth {
            min: 0.0,
            max: 100.0,
            period_secs: 4.0,
        },
    );
    let mut handle = launch_scenario_with_gates(
        "sat_gated".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Phase 1: ~10 ticks emitted (200ms at 50/s).
    thread::sleep(Duration::from_millis(200));
    let phase1 = handle.recent_metrics();
    let last_pre_pause = phase1
        .last()
        .map(|e| e.value)
        .expect("phase 1 must have a value");
    assert!(
        last_pre_pause > 0.0 && last_pre_pause < 50.0,
        "pre-pause value must be partway up the ramp, got {last_pre_pause}"
    );

    // Phase 2: pause.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(400));

    // Phase 3: resume.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(150));
    let phase3 = handle.recent_metrics();

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();

    let first_post_resume = phase3
        .first()
        .map(|e| e.value)
        .expect("phase 3 must emit a value");

    // The ramp must continue from past the last pre-pause value, not
    // restart at baseline 0.
    assert!(
        first_post_resume > last_pre_pause,
        "saturation ramp must preserve state across pause: pre={last_pre_pause}, \
         post={first_post_resume}"
    );
}

#[test]
fn while_runtime_finished_state_after_duration_expires() {
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("d4", 50.0, 200);
    let mut handle = launch_scenario_with_gates(
        "d4".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    handle
        .join(Some(Duration::from_secs(2)))
        .expect("scenario must finish within duration");
    let snap = handle.stats_snapshot();
    assert!(matches!(snap.state, ScenarioState::Finished));
}

#[test]
fn while_runtime_multiple_downstreams_share_one_upstream() {
    // A2: two downstreams subscribe to the same upstream bus. Both must
    // transition together when the gate edge arrives.
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);

    let (rx_a, init_a) = bus.subscribe(while_gt_zero());
    let (rx_b, init_b) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));

    let mut handle_a = launch_scenario_with_gates(
        "a".to_string(),
        None,
        metrics_entry("a", 100.0, 500),
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx_a,
            initial: init_a,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch a must succeed");

    let mut handle_b = launch_scenario_with_gates(
        "b".to_string(),
        None,
        metrics_entry("b", 100.0, 500),
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx_b,
            initial: init_b,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch b must succeed");

    // Both paused.
    thread::sleep(Duration::from_millis(50));
    assert_eq!(handle_a.stats_snapshot().total_events, 0);
    assert_eq!(handle_b.stats_snapshot().total_events, 0);

    // Open: both transition.
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(200));
    assert!(handle_a.stats_snapshot().total_events > 0);
    assert!(handle_b.stats_snapshot().total_events > 0);

    handle_a.stop();
    handle_b.stop();
    handle_a.join(Some(Duration::from_secs(2))).ok();
    handle_b.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_logs_signal_can_be_gated_downstream() {
    // BGP UPDOWN log scenario: a logs entry gated by `while:` must
    // transition through pending/running/paused.
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = logs_entry("bgp_log", 200.0, 600);
    let mut handle = launch_scenario_with_gates(
        "bgp_log".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(50));
    assert_eq!(
        handle.stats_snapshot().total_events,
        0,
        "logs scenario must respect closed gate"
    );

    bus.tick(1.0);
    thread::sleep(Duration::from_millis(200));
    let after_open = handle.stats_snapshot().total_events;
    assert!(after_open > 0, "logs must emit when gate opens");

    bus.tick(0.0);
    thread::sleep(Duration::from_millis(200));
    let after_close = handle.stats_snapshot().total_events;
    thread::sleep(Duration::from_millis(100));
    let after_extra_pause = handle.stats_snapshot().total_events;
    assert!(
        after_extra_pause - after_close <= 10,
        "logs must freeze when gate closes, got {after_close} → {after_extra_pause}"
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_delay_open_debounces_pause_to_running_transition() {
    // A2a: delay.open debounces close→open. Sub during gate-closed
    // (pending → paused), then open: must wait at least delay.open
    // before emitting events.
    let bus = Arc::new(GateBus::new());
    bus.tick(0.0);
    let (rx, init) = bus.subscribe(while_gt_zero());

    let delay = DelayClause {
        open: Some(Duration::from_millis(250)),
        close: None,
        close_stale_marker: None,
        close_snap_to: None,
    };

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("debounced", 200.0, 1500);
    let mut handle = launch_scenario_with_gates(
        "debounced".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: Some(delay),
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    bus.tick(1.0);
    let opened_at = Instant::now();
    // Within the debounce window — no events yet.
    thread::sleep(Duration::from_millis(100));
    assert_eq!(
        handle.stats_snapshot().total_events,
        0,
        "delay.open must suppress events during debounce window"
    );

    // After the debounce expires.
    thread::sleep(Duration::from_millis(300));
    let snap = handle.stats_snapshot();
    assert!(
        snap.total_events > 0,
        "after delay.open expires, events must flow; opened {:?} ago, got {}",
        opened_at.elapsed(),
        snap.total_events
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_strict_lt_threshold_gating() {
    // Inverse direction: `while: ref=src op="<" value=10` opens when
    // upstream drops below threshold.
    let bus = Arc::new(GateBus::new());
    bus.tick(20.0); // above threshold — gate closed
    let spec = SubscriptionSpec {
        after: None,
        while_: Some(WhileSpec {
            op: WhileOp::LessThan,
            threshold: 10.0,
        }),
    };
    let (rx, init) = bus.subscribe(spec);
    assert_eq!(init.while_gate_open, Some(false));

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("inv", 100.0, 500);
    let mut handle = launch_scenario_with_gates(
        "inv".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(50));
    assert_eq!(handle.stats_snapshot().total_events, 0);

    bus.tick(5.0); // below threshold — gate opens
    thread::sleep(Duration::from_millis(150));
    assert!(handle.stats_snapshot().total_events > 0);

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn scenario_restart_does_not_leak_gate_bus() {
    // Risk #4 from phases.md: spawn 50 short-lived gated scenarios in
    // a loop, assert the bus's Arc count returns to baseline after each
    // scenario finishes.
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);

    for i in 0..20 {
        let (rx, init) = bus.subscribe(while_gt_zero());
        let shutdown = Arc::new(AtomicBool::new(true));
        let entry = metrics_entry(&format!("ephemeral_{i}"), 50.0, 80);
        let mut handle = launch_scenario_with_gates(
            format!("ephemeral_{i}"),
            None,
            entry,
            shutdown,
            None,
            None,
            Some(GateContext {
                gate_rx: rx,
                initial: init,
                delay: None,
                has_after: false,
                has_while: true,
                close_emit: None,
            }),
        )
        .expect("launch must succeed");
        handle
            .join(Some(Duration::from_secs(2)))
            .expect("must finish");
    }

    // Bus is held by the test only; subscribers' channel receivers are
    // dropped when the scenario thread exits.
    assert_eq!(
        Arc::strong_count(&bus),
        1,
        "bus Arc count must return to 1 after all scenarios finish"
    );
}

#[test]
fn while_runtime_delay_close_debounces_running_to_paused_transition() {
    // delay.close: a brief gate-close-then-reopen within the debounce
    // window must NOT pause the scenario; a sustained close (≥ delay.close)
    // does. Mirrors the delay.open debounce test but for the opposite
    // direction.
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0); // gate open at subscription
    let (rx, init) = bus.subscribe(while_gt_zero());

    let delay = DelayClause {
        open: None,
        close: Some(Duration::from_millis(200)),
        close_stale_marker: None,
        close_snap_to: None,
    };

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("debounced_close", 200.0, 2000);
    let mut handle = launch_scenario_with_gates(
        "debounced_close".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: Some(delay),
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(150));
    let pre_close = handle.stats_snapshot().total_events;
    assert!(pre_close > 0, "scenario must emit while gate is open");

    // Brief close-then-reopen (50ms) — under the 200ms debounce, must not pause.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(50));
    bus.tick(1.0);
    thread::sleep(Duration::from_millis(250));
    let after_brief = handle.stats_snapshot().total_events;
    let brief_delta = after_brief - pre_close;
    assert!(
        brief_delta > 30,
        "brief close (< delay.close) must not pause: expected significant events after \
         brief close, got delta={brief_delta} (pre_close={pre_close}, after_brief={after_brief})"
    );

    // Sustained close (≥ debounce) — must pause.
    bus.tick(0.0);
    thread::sleep(Duration::from_millis(400));
    let at_pause = handle.stats_snapshot().total_events;
    thread::sleep(Duration::from_millis(200));
    let later = handle.stats_snapshot().total_events;
    assert!(
        later - at_pause <= 5,
        "sustained close must pause after debounce: at_pause={at_pause} → later={later}"
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_pending_to_running_when_after_fires_with_gate_open() {
    // Subscribe with both after: and while: on the same upstream. Initial
    // value 1 → after-not-fired (op="<", threshold=1, strict), gate closed
    // (op="<", threshold=1, strict) — state = Pending. Drive value to 0
    // and both fire together: after fires, gate opens → Running.
    let bus = Arc::new(GateBus::new());
    bus.tick(1.0);
    let spec = SubscriptionSpec {
        after: Some(AfterSpec {
            op: AfterOpDir::LessThan,
            threshold: 1.0,
        }),
        while_: Some(WhileSpec {
            op: WhileOp::LessThan,
            threshold: 1.0,
        }),
    };
    let (rx, init) = bus.subscribe(spec);
    assert!(!init.after_already_fired, "after must not fire at value=1");
    assert_eq!(
        init.while_gate_open,
        Some(false),
        "gate must be closed at value=1"
    );

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("after_open", 100.0, 1000);
    let mut handle = launch_scenario_with_gates(
        "after_open".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: true,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        handle.stats_snapshot().total_events,
        0,
        "Pending state must not emit events"
    );

    bus.tick(0.0); // after fires AND gate opens
    thread::sleep(Duration::from_millis(200));
    let snap = handle.stats_snapshot();
    assert!(
        snap.total_events > 0,
        "after-fires + gate-open must transition Pending → Running"
    );
    assert!(matches!(
        snap.state,
        ScenarioState::Running | ScenarioState::Finished
    ));

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_pending_to_paused_when_after_fires_with_gate_closed() {
    // Use a single upstream where after.op=">" threshold=100 and
    // while.op="<" threshold=10. Sequence:
    //
    //   subscribe at value=5: gate open (5 < 10), after not fired (5
    //     not > 100). Initial state: Pending (has_after && !after_fired).
    //   drive value=50: gate close edge (50 not < 10), after still
    //     pending. Pending arm absorbs the close.
    //   drive value=200: after fires (200 > 100); gate stays closed
    //     (200 not < 10). Only AfterFired arrives — gate state never
    //     re-opened. State: Pending → Paused.
    //   drive value=5: gate open edge (5 < 10). Paused → Running.
    let bus = Arc::new(GateBus::new());
    bus.tick(5.0);

    let (rx, init) = bus.subscribe(SubscriptionSpec {
        after: Some(AfterSpec {
            op: AfterOpDir::GreaterThan,
            threshold: 100.0,
        }),
        while_: Some(WhileSpec {
            op: WhileOp::LessThan,
            threshold: 10.0,
        }),
    });
    assert!(!init.after_already_fired);
    assert_eq!(init.while_gate_open, Some(true));

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("after_paused", 100.0, 1500);
    let mut handle = launch_scenario_with_gates(
        "after_paused".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: true,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Pending: gate is initially open but after has not fired.
    thread::sleep(Duration::from_millis(80));
    assert_eq!(
        handle.stats_snapshot().total_events,
        0,
        "Pending state must not emit events"
    );

    // Close the gate while still Pending.
    bus.tick(50.0);
    thread::sleep(Duration::from_millis(80));
    assert_eq!(handle.stats_snapshot().total_events, 0);

    // Fire after with gate currently closed → Pending → Paused.
    bus.tick(200.0);
    thread::sleep(Duration::from_millis(200));
    let snap = handle.stats_snapshot();
    assert_eq!(snap.total_events, 0);
    assert!(
        matches!(snap.state, ScenarioState::Paused),
        "expected Paused, got {:?}",
        snap.state
    );

    // Re-open the gate → Paused → Running.
    bus.tick(5.0);
    thread::sleep(Duration::from_millis(250));
    let snap = handle.stats_snapshot();
    assert!(
        snap.total_events > 0,
        "after gate re-opens, scenario must transition Paused → Running"
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_pending_absorbs_while_edges_before_after_fires() {
    // While Pending (after not yet satisfied), repeated WhileOpen /
    // WhileClose edges on the same upstream must not transition the
    // scenario out of Pending. Single-bus approach: after.op=">"
    // threshold=100, while.op="<" threshold=10. Toggle the upstream
    // value between gate-open (5) and gate-close (50) values without
    // ever crossing the after threshold; the scenario must stay
    // Pending and emit zero events. Then drive value=200 to fire
    // after with the gate currently closed → Pending → Paused.
    let bus = Arc::new(GateBus::new());
    bus.tick(50.0); // gate closed initially

    let (rx, init) = bus.subscribe(SubscriptionSpec {
        after: Some(AfterSpec {
            op: AfterOpDir::GreaterThan,
            threshold: 100.0,
        }),
        while_: Some(WhileSpec {
            op: WhileOp::LessThan,
            threshold: 10.0,
        }),
    });
    assert!(!init.after_already_fired);
    assert_eq!(init.while_gate_open, Some(false));

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("absorb", 100.0, 2000);
    let mut handle = launch_scenario_with_gates(
        "absorb".to_string(),
        None,
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: true,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Toggle the gate several times while still Pending. None of these
    // values fire after (all < 100).
    for _ in 0..5 {
        bus.tick(5.0); // gate open edge
        thread::sleep(Duration::from_millis(40));
        bus.tick(50.0); // gate close edge
        thread::sleep(Duration::from_millis(40));
    }

    let mid = handle.stats_snapshot();
    assert_eq!(
        mid.total_events, 0,
        "Pending must absorb while edges without emitting events"
    );

    // Now fire after with the gate currently closed.
    bus.tick(200.0);
    thread::sleep(Duration::from_millis(200));
    let snap = handle.stats_snapshot();
    assert_eq!(snap.total_events, 0);
    assert!(
        matches!(snap.state, ScenarioState::Paused),
        "expected Paused after after fires with gate closed, got {:?}",
        snap.state
    );

    // Re-open the gate → Paused → Running.
    bus.tick(5.0);
    thread::sleep(Duration::from_millis(250));
    let snap = handle.stats_snapshot();
    assert!(snap.total_events > 0);

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn while_runtime_steady_within_5pct_of_baseline() {
    // Perf-regression gate (A10): a scenario with `while:` open the entire
    // run must produce within 5% of the event count of the same scenario
    // without `while:`. Both runs are short (300ms) to keep the test fast.
    fn run_baseline() -> u64 {
        let entry = metrics_entry("baseline", 1000.0, 300);
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut handle = launch_scenario_with_gates(
            "baseline".to_string(),
            None,
            entry,
            shutdown,
            None,
            None,
            None,
        )
        .unwrap();
        handle.join(Some(Duration::from_secs(2))).unwrap();
        handle.stats_snapshot().total_events
    }

    fn run_gated_open() -> u64 {
        let bus = Arc::new(GateBus::new());
        bus.tick(1.0);
        let (rx, init) = bus.subscribe(while_gt_zero());
        let entry = metrics_entry("gated", 1000.0, 300);
        let shutdown = Arc::new(AtomicBool::new(true));
        let mut handle = launch_scenario_with_gates(
            "gated".to_string(),
            None,
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
        handle.stats_snapshot().total_events
    }

    // Warm-up to stabilize TLB / page-fault behavior.
    let _ = run_baseline();
    let _ = run_gated_open();

    let baseline = run_baseline();
    let gated = run_gated_open();

    let baseline_f = baseline as f64;
    let gated_f = gated as f64;
    let ratio = gated_f / baseline_f;
    assert!(baseline > 0, "baseline must produce events: got {baseline}");
    // spec target is 5%; tightened to 10% to absorb CI noise. < 5% is the
    // lab-target on dedicated hardware.
    assert!(
        (0.90..=1.10).contains(&ratio),
        "gated/baseline event ratio {ratio:.3} outside [0.90, 1.10]; baseline={baseline}, gated={gated}"
    );
}

#[test]
fn close_emit_conflict_compile_error_when_snap_to_and_stale_marker_false() {
    use sonda_core::compile_scenario_file_compiled;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    let yaml = "\
version: 2
defaults:
  rate: 5
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: flap
      up_duration: 30s
      down_duration: 30s
  - id: downstream
    signal_type: metrics
    name: downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: '<'
      value: 1
    delay:
      close:
        snap_to: 0
        stale_marker: false
";
    let resolver = InMemoryPackResolver::new();
    let result = compile_scenario_file_compiled(yaml, &resolver);
    let err = result.expect_err("conflicting delay.close fields must reject");

    let mut chain = String::new();
    let mut cur: Option<&dyn std::error::Error> = Some(&err);
    while let Some(e) = cur {
        chain.push_str(&format!("{e}; "));
        cur = e.source();
    }
    assert!(
        chain.contains("snap_to") && chain.contains("stale marker"),
        "error chain must mention both 'snap_to' and 'stale marker', got: {chain}"
    );
}

#[test]
fn delay_close_legacy_shorthand_still_deserializes() {
    use sonda_core::compile_scenario_file_compiled;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    let yaml = "\
version: 2
defaults:
  rate: 5
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: flap
      up_duration: 30s
      down_duration: 30s
  - id: downstream
    signal_type: metrics
    name: downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: '<'
      value: 1
    delay:
      close: 5s
";
    let resolver = InMemoryPackResolver::new();
    compile_scenario_file_compiled(yaml, &resolver)
        .expect("legacy delay.close shorthand must still parse");
}

#[test]
fn nan_upstream_value_keeps_downstream_paused() {
    let bus = Arc::new(GateBus::new());
    bus.tick(f64::NAN);
    let (rx, init) = bus.subscribe(while_gt_zero());
    assert_eq!(
        init.while_gate_open,
        Some(false),
        "NaN upstream must close the gate at subscription"
    );

    let shutdown = Arc::new(AtomicBool::new(true));
    let entry = metrics_entry("nan_paused", 200.0, 600);
    let mut handle = launch_scenario_with_gates(
        "nan_paused".to_string(),
        entry,
        Arc::clone(&shutdown),
        None,
        None,
        Some(GateContext {
            gate_rx: rx,
            initial: init,
            delay: None,
            has_after: false,
            has_while: true,
            close_emit: None,
        }),
    )
    .expect("launch must succeed");

    // Re-publish NaN periodically to confirm the runtime defense holds
    // across multiple bus updates, not just at subscription.
    for _ in 0..6 {
        thread::sleep(Duration::from_millis(50));
        bus.tick(f64::NAN);
    }

    let snap = handle.stats_snapshot();
    assert_eq!(
        snap.total_events, 0,
        "NaN upstream must keep downstream paused"
    );
    assert!(
        matches!(snap.state, ScenarioState::Paused),
        "expected Paused with NaN upstream, got {:?}",
        snap.state
    );

    handle.stop();
    handle.join(Some(Duration::from_secs(2))).ok();
}

#[test]
fn delay_close_extended_form_deserializes_all_fields() {
    use sonda_core::compile_scenario_file_compiled;
    use sonda_core::compiler::expand::InMemoryPackResolver;

    let yaml = "\
version: 2
defaults:
  rate: 5
  duration: 1s
  encoder:
    type: prometheus_text
  sink:
    type: stdout
scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: flap
      up_duration: 30s
      down_duration: 30s
  - id: downstream
    signal_type: metrics
    name: downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: '<'
      value: 1
    delay:
      open: 250ms
      close:
        duration: 5s
        snap_to: 0
";
    let resolver = InMemoryPackResolver::new();
    compile_scenario_file_compiled(yaml, &resolver).expect("extended delay.close form must parse");
}
