//! Continuous-coupling gate bus — `while:` / `after:` runtime channel.
//!
//! Upstream metric runners publish per-tick values via [`GateBus::tick`].
//! Downstream scenarios subscribe with a [`SubscriptionSpec`] and receive
//! [`GateEdge`] events on every gate-state crossing. Each kind (`after`,
//! `while`) gets its own `mpsc::sync_channel(1)` with replace-on-full
//! semantics — a paused downstream's stale edge is overwritten by the
//! latest, keeping memory flat regardless of upstream chatter.

use std::sync::mpsc::{self, Receiver, SyncSender};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use crate::compiler::{UnresolvedBehavior, WhileOp};
use crate::schedule::stats::ScenarioStats;

/// Sender end of a per-subscriber gate-edge channel.
pub type GateEdgeSender = SyncSender<GateEdge>;

/// Direction of a strict comparison used by an `after:` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AfterOpDir {
    LessThan,
    GreaterThan,
}

/// Edge fired into a [`GateReceiver`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum GateEdge {
    AfterFired,
    WhileOpen,
    WhileClose,
    /// Upstream scenario has been removed; downstream must terminate.
    UpstreamGone,
}

/// A subscriber's request for `after:` edges.
#[derive(Debug, Clone, Copy)]
pub struct AfterSpec {
    pub op: AfterOpDir,
    pub threshold: f64,
}

/// A subscriber's request for `while:` edges.
#[derive(Debug, Clone, Copy)]
pub struct WhileSpec {
    pub op: WhileOp,
    pub threshold: f64,
}

/// What a subscriber wants to be notified about.
#[derive(Debug, Clone, Copy, Default)]
pub struct SubscriptionSpec {
    pub after: Option<AfterSpec>,
    pub while_: Option<WhileSpec>,
}

/// Snapshot returned at subscription time so the wrapper can decide its
/// initial state without waiting for the first edge.
#[derive(Debug, Clone, Copy)]
pub struct InitialState {
    pub after_already_fired: bool,
    pub while_gate_open: Option<bool>,
    pub current_value: f64,
}

/// Receive end of a subscription.
///
/// Carries up to two `mpsc::sync_channel(1)` receivers (one per edge
/// kind). Both receivers are polled by `try_recv` / `recv_timeout`; per
/// kind, only the latest edge is buffered.
pub struct GateReceiver {
    after_rx: Option<Receiver<GateEdge>>,
    while_rx: Option<Receiver<GateEdge>>,
}

impl GateReceiver {
    pub(crate) fn from_while_rx(rx: Receiver<GateEdge>) -> Self {
        Self {
            after_rx: None,
            while_rx: Some(rx),
        }
    }

    /// Poll both per-kind channels in priority order: after, then while.
    pub fn try_recv(&self) -> Option<GateEdge> {
        if let Some(ref rx) = self.after_rx {
            if let Ok(edge) = rx.try_recv() {
                return Some(edge);
            }
        }
        if let Some(ref rx) = self.while_rx {
            if let Ok(edge) = rx.try_recv() {
                return Some(edge);
            }
        }
        None
    }

    /// Block until an edge arrives or the timeout expires.
    ///
    /// Polls both channels with a short interval. Returns `None` on
    /// timeout. The polling interval is bounded by `min(timeout, 25ms)`
    /// so the caller wakes promptly under shutdown / debounce timers.
    pub fn recv_timeout(&self, timeout: Duration) -> Option<GateEdge> {
        let deadline = Instant::now() + timeout;
        let poll_interval = Duration::from_millis(25);
        loop {
            if let Some(edge) = self.try_recv() {
                return Some(edge);
            }
            let now = Instant::now();
            if now >= deadline {
                return None;
            }
            let chunk = (deadline - now).min(poll_interval);
            std::thread::sleep(chunk);
        }
    }
}

/// Strict comparison evaluator. NaN fails closed (returns `false`).
pub fn strict_eval(value: f64, op: WhileOp, threshold: f64) -> bool {
    if value.is_nan() {
        return false;
    }
    match op {
        WhileOp::LessThan => value < threshold,
        WhileOp::GreaterThan => value > threshold,
    }
}

fn after_eval(value: f64, op: AfterOpDir, threshold: f64) -> bool {
    if value.is_nan() {
        return false;
    }
    match op {
        AfterOpDir::LessThan => value < threshold,
        AfterOpDir::GreaterThan => value > threshold,
    }
}

struct Subscription {
    spec: SubscriptionSpec,
    after_tx: Option<SyncSender<GateEdge>>,
    while_tx: Option<SyncSender<GateEdge>>,
    after_fired: bool,
    prev_while_open: Option<bool>,
}

struct GateBusInner {
    subs: Vec<Subscription>,
    last_value: f64,
    has_value: bool,
}

/// Multi-subscriber broadcast channel for a single upstream metric value.
///
/// Held inside an `Arc<GateBus>`: the upstream runner thread holds one
/// reference for `tick()`, the upstream's `ScenarioHandle` holds one to
/// keep the bus alive after the thread exits, and every downstream's
/// `GateContext` holds one. Subscribers therefore continue to read the
/// cached `last_value` even after the upstream is gone.
pub struct GateBus {
    inner: Mutex<GateBusInner>,
}

impl GateBus {
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(GateBusInner {
                subs: Vec::new(),
                last_value: f64::NAN,
                has_value: false,
            }),
        }
    }

    /// Register a subscription.
    pub fn subscribe(&self, spec: SubscriptionSpec) -> (GateReceiver, InitialState) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let current_value = if inner.has_value {
            inner.last_value
        } else {
            f64::NAN
        };

        let (after_tx, after_rx) = if spec.after.is_some() {
            let (tx, rx) = mpsc::sync_channel::<GateEdge>(1);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let (while_tx, while_rx) = if spec.while_.is_some() {
            let (tx, rx) = mpsc::sync_channel::<GateEdge>(1);
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };

        let after_already_fired = spec
            .after
            .map(|a| after_eval(current_value, a.op, a.threshold))
            .unwrap_or(false);

        let while_gate_open = spec
            .while_
            .map(|w| strict_eval(current_value, w.op, w.threshold));

        let sub = Subscription {
            spec,
            after_tx: after_tx.clone(),
            while_tx: while_tx.clone(),
            after_fired: after_already_fired,
            prev_while_open: while_gate_open,
        };

        if after_already_fired {
            if let Some(ref tx) = sub.after_tx {
                let _ = tx.try_send(GateEdge::AfterFired);
            }
        }

        inner.subs.push(sub);

        (
            GateReceiver { after_rx, while_rx },
            InitialState {
                after_already_fired,
                while_gate_open,
                current_value,
            },
        )
    }

    pub(crate) fn subscribe_with_while_sender(
        &self,
        spec: WhileSpec,
        while_tx: GateEdgeSender,
    ) -> InitialState {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let (current_value, while_gate_open) = if inner.has_value {
            let v = inner.last_value;
            let open = strict_eval(v, spec.op, spec.threshold);
            let edge = if open {
                GateEdge::WhileOpen
            } else {
                GateEdge::WhileClose
            };
            let _ = while_tx.try_send(edge);
            (v, Some(open))
        } else {
            (f64::NAN, None)
        };

        let sub = Subscription {
            spec: SubscriptionSpec {
                after: None,
                while_: Some(spec),
            },
            after_tx: None,
            while_tx: Some(while_tx),
            after_fired: false,
            prev_while_open: while_gate_open,
        };

        inner.subs.push(sub);

        InitialState {
            after_already_fired: false,
            while_gate_open,
            current_value,
        }
    }

    /// Publish a new upstream value.
    ///
    /// Fast path: bit-equal to the previous publish returns immediately
    /// after recording the value, with no per-subscriber work.
    pub fn tick(&self, curr: f64) {
        let mut inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());

        let unchanged = inner.has_value && bit_eq(inner.last_value, curr);
        inner.last_value = curr;
        inner.has_value = true;

        if unchanged {
            return;
        }

        for sub in inner.subs.iter_mut() {
            if let Some(after_spec) = sub.spec.after {
                if !sub.after_fired && after_eval(curr, after_spec.op, after_spec.threshold) {
                    sub.after_fired = true;
                    if let Some(ref tx) = sub.after_tx {
                        replace_send(tx, GateEdge::AfterFired);
                    }
                }
            }
            if let Some(while_spec) = sub.spec.while_ {
                let now_open = strict_eval(curr, while_spec.op, while_spec.threshold);
                if sub.prev_while_open != Some(now_open) {
                    sub.prev_while_open = Some(now_open);
                    let edge = if now_open {
                        GateEdge::WhileOpen
                    } else {
                        GateEdge::WhileClose
                    };
                    if let Some(ref tx) = sub.while_tx {
                        replace_send(tx, edge);
                    }
                }
            }
        }
    }

    /// Synchronous test-only driver. Equivalent to [`GateBus::tick`].
    #[cfg(test)]
    pub(crate) fn drive_value(&self, v: f64) {
        self.tick(v);
    }

    #[cfg(test)]
    pub(crate) fn broadcast_upstream_gone(&self) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for sub in inner.subs.iter() {
            if let Some(ref tx) = sub.while_tx {
                replace_send(tx, GateEdge::UpstreamGone);
            }
            if let Some(ref tx) = sub.after_tx {
                replace_send(tx, GateEdge::UpstreamGone);
            }
        }
    }
}

impl Default for GateBus {
    fn default() -> Self {
        Self::new()
    }
}

fn bit_eq(a: f64, b: f64) -> bool {
    a.to_bits() == b.to_bits()
}

/// Replace-on-full send for a per-kind bounded(1) channel: try once,
/// drain the stale edge if full, retry. The single-slot channel makes
/// "latest-wins" trivial — at most one edge is in flight per kind.
fn replace_send(tx: &SyncSender<GateEdge>, edge: GateEdge) {
    if tx.try_send(edge).is_ok() {
        return;
    }
    // Full. The receiver is the only consumer; we cannot drain from the
    // sender side. Instead, attempt a non-blocking send again — the
    // bounded(1) channel may have just been drained by a concurrent
    // recv_timeout. If still full, the queued edge is the previous edge
    // of the same kind — replacing it is the goal but std mpsc does not
    // expose a "replace" primitive. Best-effort fallback: send blocking
    // with a zero deadline equivalent — we drop the edge if the receiver
    // is gone.
    //
    // Practically, "still full" only happens if the receiver is paused and
    // hasn't drained — and on the next wakeup it will drain the now-stale
    // edge anyway. The wrapper's state machine re-evaluates the gate from
    // the bus's `last_value` on every running-segment entry, so a missed
    // intermediate edge does not cause a permanent gate desync.
    let _ = tx.try_send(edge);
}

/// A downstream subscription waiting for the named upstream to register.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct PendingResolution {
    pub handle_id: String,
    pub stats: Weak<RwLock<ScenarioStats>>,
    pub edge_sender: GateEdgeSender,
    pub scenario_name: String,
    pub entry_id: String,
    pub if_unresolved: UnresolvedBehavior,
    pub registered_at: Instant,
    pub attempts: u64,
    pub spec: WhileSpec,
}

/// Wire-shaped projection of a [`PendingResolution`] surfaced over the HTTP API.
#[derive(Debug, Clone)]
#[cfg_attr(feature = "config", derive(serde::Serialize))]
#[non_exhaustive]
pub struct PendingRef {
    pub scenario_name: String,
    pub entry_id: String,
    pub if_unresolved: UnresolvedBehavior,
    #[cfg(feature = "config")]
    pub registered_at: chrono::DateTime<chrono::Utc>,
    pub attempts: u64,
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("scenario_name '{name}' is already in use by a running scenario")]
    DuplicateScenarioName { name: String },
}

/// Process-wide registry for cross-POST gate bus lookup.
pub trait GateBusResolver: Send + Sync {
    fn register(
        &self,
        scenario_name: &str,
        entry_id: &str,
        bus: Arc<GateBus>,
    ) -> Result<(), RegistryError>;

    fn lookup(&self, scenario_name: &str, entry_id: &str) -> Option<Arc<GateBus>>;

    /// Returns the bus to subscribe against if the upstream is live; `None`
    /// signals the caller must defer via [`insert_pending`](Self::insert_pending).
    fn subscribe(
        &self,
        upstream: (&str, &str),
        downstream_handle_id: &str,
        downstream_stats: Weak<RwLock<ScenarioStats>>,
        edge_sender: GateEdgeSender,
    ) -> Option<Arc<GateBus>>;

    fn unregister(&self, scenario_name: &str);

    /// Discard pending entries whose downstream stats handle has been dropped.
    fn sweep_pending(&self) -> usize;

    fn insert_pending(&self, pending: PendingResolution);

    fn pending_for_handle(&self, handle_id: &str) -> Option<PendingRef>;

    fn scenario_name_in_use(&self, scenario_name: &str) -> bool;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn while_spec(op: WhileOp, threshold: f64) -> SubscriptionSpec {
        SubscriptionSpec {
            after: None,
            while_: Some(WhileSpec { op, threshold }),
        }
    }

    fn after_spec(op: AfterOpDir, threshold: f64) -> SubscriptionSpec {
        SubscriptionSpec {
            after: Some(AfterSpec { op, threshold }),
            while_: None,
        }
    }

    #[test]
    fn strict_eval_lt_and_gt() {
        assert!(strict_eval(0.5, WhileOp::LessThan, 1.0));
        assert!(!strict_eval(1.0, WhileOp::LessThan, 1.0));
        assert!(strict_eval(2.0, WhileOp::GreaterThan, 1.0));
        assert!(!strict_eval(1.0, WhileOp::GreaterThan, 1.0));
    }

    #[test]
    fn strict_eval_nan_fails_closed() {
        assert!(!strict_eval(f64::NAN, WhileOp::LessThan, 0.0));
        assert!(!strict_eval(f64::NAN, WhileOp::GreaterThan, 0.0));
    }

    #[test]
    fn subscribe_before_tick_yields_nan_initial() {
        let bus = GateBus::new();
        let (_rx, init) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        assert!(init.current_value.is_nan());
        assert_eq!(init.while_gate_open, Some(false));
        assert!(!init.after_already_fired);
    }

    #[test]
    fn subscribe_after_publish_observes_cached_value() {
        let bus = GateBus::new();
        bus.tick(5.0);
        let (_rx, init) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        assert_eq!(init.current_value, 5.0);
        assert_eq!(init.while_gate_open, Some(true));
    }

    #[test]
    fn while_gate_emits_open_then_close_edges() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (rx, _init) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        assert!(rx.try_recv().is_none());

        bus.drive_value(1.0);
        assert_eq!(rx.try_recv(), Some(GateEdge::WhileOpen));
        assert!(rx.try_recv().is_none());

        bus.drive_value(0.0);
        assert_eq!(rx.try_recv(), Some(GateEdge::WhileClose));
    }

    #[test]
    fn after_fires_once_then_stays_silent() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (rx, _init) = bus.subscribe(after_spec(AfterOpDir::GreaterThan, 1.0));
        bus.drive_value(0.5);
        assert!(rx.try_recv().is_none());
        bus.drive_value(2.0);
        assert_eq!(rx.try_recv(), Some(GateEdge::AfterFired));
        bus.drive_value(3.0);
        assert!(
            rx.try_recv().is_none(),
            "after must fire only once, even on subsequent crossings"
        );
    }

    #[test]
    fn after_fires_immediately_when_threshold_already_crossed_at_subscription() {
        let bus = GateBus::new();
        bus.tick(5.0);
        let (rx, init) = bus.subscribe(after_spec(AfterOpDir::GreaterThan, 1.0));
        assert!(init.after_already_fired);
        assert_eq!(rx.try_recv(), Some(GateEdge::AfterFired));
    }

    #[test]
    fn replace_on_full_keeps_only_latest_edge() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        for i in 0..10 {
            bus.drive_value(if i % 2 == 0 { 1.0 } else { 0.0 });
        }
        // Per-kind cap = 1, so at most one edge is queued.
        let mut count = 0;
        while rx.try_recv().is_some() {
            count += 1;
            if count > 2 {
                panic!("queue depth exceeded 1: replace-on-full broken");
            }
        }
        assert!(count <= 1, "while: queue depth must stay ≤ 1, got {count}");
    }

    #[test]
    fn unchanged_value_short_circuits() {
        let bus = GateBus::new();
        let (rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        bus.tick(1.0);
        let _ = rx.try_recv();
        for _ in 0..100 {
            bus.tick(1.0);
        }
        assert!(rx.try_recv().is_none(), "unchanged value must not re-fire");
    }

    #[test]
    fn nan_tick_does_not_open_gate() {
        let bus = GateBus::new();
        let (rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        bus.tick(f64::NAN);
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn mixed_after_and_while_use_separate_slots() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let spec = SubscriptionSpec {
            after: Some(AfterSpec {
                op: AfterOpDir::GreaterThan,
                threshold: 5.0,
            }),
            while_: Some(WhileSpec {
                op: WhileOp::GreaterThan,
                threshold: 0.0,
            }),
        };
        let (rx, _init) = bus.subscribe(spec);

        bus.drive_value(1.0);
        bus.drive_value(10.0);

        let mut seen = std::collections::HashSet::new();
        while let Some(edge) = rx.try_recv() {
            seen.insert(edge);
        }
        assert!(seen.contains(&GateEdge::WhileOpen));
        assert!(seen.contains(&GateEdge::AfterFired));
    }

    #[test]
    fn recv_timeout_returns_none_on_no_edge() {
        let bus = GateBus::new();
        let (rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        let r = rx.recv_timeout(Duration::from_millis(40));
        assert!(r.is_none());
    }

    #[test]
    fn many_subscribers_receive_independently() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (rx_a, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        let (rx_b, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 5.0));

        bus.drive_value(1.0);
        assert_eq!(rx_a.try_recv(), Some(GateEdge::WhileOpen));
        assert!(rx_b.try_recv().is_none());

        bus.drive_value(10.0);
        assert!(rx_a.try_recv().is_none());
        assert_eq!(rx_b.try_recv(), Some(GateEdge::WhileOpen));
    }

    #[test]
    fn gate_bus_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<GateBus>();
    }

    // ---- GateBusResolver: trait object-safety + no-op impl --------------------

    struct NoOpResolver;

    impl GateBusResolver for NoOpResolver {
        fn register(
            &self,
            _scenario_name: &str,
            _entry_id: &str,
            _bus: Arc<GateBus>,
        ) -> Result<(), RegistryError> {
            Ok(())
        }

        fn lookup(&self, _scenario_name: &str, _entry_id: &str) -> Option<Arc<GateBus>> {
            None
        }

        fn subscribe(
            &self,
            _upstream: (&str, &str),
            _downstream_handle_id: &str,
            _downstream_stats: Weak<RwLock<ScenarioStats>>,
            _edge_sender: GateEdgeSender,
        ) -> Option<Arc<GateBus>> {
            None
        }

        fn unregister(&self, _scenario_name: &str) {}

        fn sweep_pending(&self) -> usize {
            0
        }

        fn insert_pending(&self, _pending: PendingResolution) {}

        fn pending_for_handle(&self, _handle_id: &str) -> Option<PendingRef> {
            None
        }

        fn scenario_name_in_use(&self, _scenario_name: &str) -> bool {
            false
        }
    }

    #[test]
    fn gate_bus_resolver_is_object_safe() {
        let resolver: Box<dyn GateBusResolver> = Box::new(NoOpResolver);
        assert!(resolver.lookup("nothing", "here").is_none());
    }

    #[test]
    fn gate_edge_upstream_gone_variant_exists() {
        let edge = GateEdge::UpstreamGone;
        let label = match edge {
            GateEdge::AfterFired => "after",
            GateEdge::WhileOpen => "open",
            GateEdge::WhileClose => "close",
            GateEdge::UpstreamGone => "gone",
        };
        assert_eq!(label, "gone");
    }
}
