//! Continuous-coupling gate bus — `while:` / `after:` runtime channel.
//!
//! Upstream metric runners publish per-tick values via [`GateBus::tick`].
//! Downstream scenarios subscribe with a [`SubscriptionSpec`] and receive
//! [`GateEdge`] events on every gate-state crossing. Each kind (`after`,
//! `while`) gets its own single-slot edge channel — the latest edge
//! always wins, keeping memory flat regardless of upstream chatter.

use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex, RwLock, Weak};
use std::time::{Duration, Instant};

use tokio::sync::Notify;

use crate::compiler::{UnresolvedBehavior, WhileOp};
use crate::schedule::stats::ScenarioStats;

const EDGE_EMPTY: u8 = 0;
const EDGE_AFTER_FIRED: u8 = 1;
const EDGE_WHILE_OPEN: u8 = 2;
const EDGE_WHILE_CLOSE: u8 = 3;
const EDGE_UPSTREAM_GONE: u8 = 4;

fn encode_edge(edge: GateEdge) -> u8 {
    match edge {
        GateEdge::AfterFired => EDGE_AFTER_FIRED,
        GateEdge::WhileOpen => EDGE_WHILE_OPEN,
        GateEdge::WhileClose => EDGE_WHILE_CLOSE,
        GateEdge::UpstreamGone => EDGE_UPSTREAM_GONE,
    }
}

fn decode_edge(raw: u8) -> Option<GateEdge> {
    match raw {
        EDGE_AFTER_FIRED => Some(GateEdge::AfterFired),
        EDGE_WHILE_OPEN => Some(GateEdge::WhileOpen),
        EDGE_WHILE_CLOSE => Some(GateEdge::WhileClose),
        EDGE_UPSTREAM_GONE => Some(GateEdge::UpstreamGone),
        _ => None,
    }
}

pub(crate) struct EdgeSlot {
    edge: AtomicU8,
    sender_dropped: AtomicBool,
    notify: Notify,
}

impl EdgeSlot {
    fn new() -> Self {
        Self {
            edge: AtomicU8::new(EDGE_EMPTY),
            sender_dropped: AtomicBool::new(false),
            notify: Notify::new(),
        }
    }
}

struct SenderShared {
    slot: Arc<EdgeSlot>,
}

impl Drop for SenderShared {
    fn drop(&mut self) {
        self.slot.sender_dropped.store(true, Ordering::Release);
        self.slot.notify.notify_waiters();
    }
}

/// Sender end of a per-subscriber gate-edge channel.
#[derive(Clone)]
pub struct GateEdgeSender(Arc<SenderShared>);

impl GateEdgeSender {
    /// Publish a new edge, overwriting any unobserved pending edge.
    pub fn send(&self, edge: GateEdge) {
        self.0.slot.edge.store(encode_edge(edge), Ordering::Release);
        self.0.slot.notify.notify_waiters();
    }
}

impl std::fmt::Debug for GateEdgeSender {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GateEdgeSender").finish_non_exhaustive()
    }
}

/// Receiver end of a per-subscriber gate-edge channel.
pub struct EdgeReceiver {
    slot: Arc<EdgeSlot>,
    drained_after_close: bool,
}

impl EdgeReceiver {
    /// Take the pending edge if any; synthesises `UpstreamGone` once when the sender has dropped silently.
    pub fn try_recv(&mut self) -> Option<GateEdge> {
        let raw = self.slot.edge.swap(EDGE_EMPTY, Ordering::AcqRel);
        let edge = decode_edge(raw);
        let sender_gone = self.slot.sender_dropped.load(Ordering::Acquire);
        if let Some(edge) = edge {
            if sender_gone {
                self.drained_after_close = true;
            }
            return Some(edge);
        }
        if self.drained_after_close {
            return None;
        }
        if sender_gone {
            self.drained_after_close = true;
            return Some(GateEdge::UpstreamGone);
        }
        None
    }

    /// Park until a fresh edge arrives or the sender drops.
    pub async fn wait_for_change(&mut self) {
        let notified = self.slot.notify.notified(); // create future BEFORE atomic check — captures Notify's call-counter so a subsequent notify_waiters() resolves us even if we never poll it before the check below
        tokio::pin!(notified);
        if self.slot.edge.load(Ordering::Acquire) != EDGE_EMPTY
            || self.slot.sender_dropped.load(Ordering::Acquire)
        {
            return;
        }
        notified.as_mut().await;
    }
}

/// Construct an unattached `(GateEdgeSender, EdgeReceiver)` pair.
pub fn gate_edge_channel() -> (GateEdgeSender, EdgeReceiver) {
    let slot = Arc::new(EdgeSlot::new());
    let shared = Arc::new(SenderShared {
        slot: Arc::clone(&slot),
    });
    (
        GateEdgeSender(shared),
        EdgeReceiver {
            slot,
            drained_after_close: false,
        },
    )
}

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
/// Carries up to two per-edge-kind receivers (one for `after`, one for `while`).
/// Both are polled by `try_recv` / `recv_edge_timeout`; per kind, only the
/// latest edge is buffered.
pub struct GateReceiver {
    after_rx: Option<EdgeReceiver>,
    while_rx: Option<EdgeReceiver>,
}

impl GateReceiver {
    #[cfg(feature = "config")]
    pub(crate) fn from_while_rx(rx: EdgeReceiver) -> Self {
        Self {
            after_rx: None,
            while_rx: Some(rx),
        }
    }

    /// Poll both per-kind channels in priority order: after, then while.
    pub fn try_recv(&mut self) -> Option<GateEdge> {
        if let Some(slot) = self.after_rx.as_mut() {
            if let Some(edge) = slot.try_recv() {
                return Some(edge);
            }
        }
        if let Some(slot) = self.while_rx.as_mut() {
            if let Some(edge) = slot.try_recv() {
                return Some(edge);
            }
        }
        None
    }

    /// Await an edge from any subscribed channel, returning when one arrives.
    pub async fn recv_edge(&mut self) -> Option<GateEdge> {
        if let Some(edge) = self.try_recv() {
            return Some(edge);
        }
        self.wait_for_change().await;
        self.try_recv()
    }

    /// Await an edge until the timeout elapses; returns `None` on timeout.
    pub async fn recv_edge_timeout(&mut self, timeout: Duration) -> Option<GateEdge> {
        if let Some(edge) = self.try_recv() {
            return Some(edge);
        }
        tokio::select! {
            _ = tokio::time::sleep(timeout) => None,
            _ = self.wait_for_change() => self.try_recv(),
        }
    }

    async fn wait_for_change(&mut self) {
        match (self.after_rx.as_mut(), self.while_rx.as_mut()) {
            (None, None) => std::future::pending::<()>().await,
            (Some(a), None) => a.wait_for_change().await,
            (None, Some(w)) => w.wait_for_change().await,
            (Some(a), Some(w)) => {
                tokio::select! {
                    _ = a.wait_for_change() => {}
                    _ = w.wait_for_change() => {}
                }
            }
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
    after_tx: Option<GateEdgeSender>,
    while_tx: Option<GateEdgeSender>,
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
            let (tx, rx) = gate_edge_channel();
            (Some(tx), Some(rx))
        } else {
            (None, None)
        };
        let (while_tx, while_rx) = if spec.while_.is_some() {
            let (tx, rx) = gate_edge_channel();
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
                tx.send(GateEdge::AfterFired);
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

    /// Subscribe an existing [`GateEdgeSender`] to the bus's `while:` events.
    pub fn subscribe_with_while_sender(
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
            while_tx.send(edge);
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
                        tx.send(GateEdge::AfterFired);
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
                        tx.send(edge);
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

    pub fn broadcast_upstream_gone(&self) {
        let inner = self
            .inner
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for sub in inner.subs.iter() {
            if let Some(ref tx) = sub.while_tx {
                tx.send(GateEdge::UpstreamGone);
            }
            if let Some(ref tx) = sub.after_tx {
                tx.send(GateEdge::UpstreamGone);
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

impl PendingResolution {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        handle_id: String,
        stats: Weak<RwLock<ScenarioStats>>,
        edge_sender: GateEdgeSender,
        scenario_name: String,
        entry_id: String,
        if_unresolved: UnresolvedBehavior,
        registered_at: Instant,
        attempts: u64,
        spec: WhileSpec,
    ) -> Self {
        Self {
            handle_id,
            stats,
            edge_sender,
            scenario_name,
            entry_id,
            if_unresolved,
            registered_at,
            attempts,
            spec,
        }
    }
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

impl PendingRef {
    /// Snapshot a [`PendingRef`] from a registry's [`PendingResolution`].
    pub fn from_pending(pending: &PendingResolution, now: std::time::SystemTime) -> Self {
        #[cfg(not(feature = "config"))]
        let _ = now;
        Self {
            scenario_name: pending.scenario_name.clone(),
            entry_id: pending.entry_id.clone(),
            if_unresolved: pending.if_unresolved,
            #[cfg(feature = "config")]
            registered_at: {
                let elapsed = pending.registered_at.elapsed();
                let wall = now.checked_sub(elapsed).unwrap_or(std::time::UNIX_EPOCH);
                chrono::DateTime::<chrono::Utc>::from(wall)
            },
            attempts: pending.attempts,
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RegistryError {
    #[error("scenario_name '{name}' is already in use by a running scenario")]
    DuplicateScenarioName { name: String },
    /// The upstream scenario this resolution was waiting on was cancelled before it could register.
    #[error("upstream '{scenario_name}/{entry_id}' was cancelled before resolving")]
    UpstreamCancelled {
        scenario_name: String,
        entry_id: String,
    },
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

    /// Record an active subscriber so [`unregister`](Self::unregister) can
    /// re-pend it for cross-POST re-resolution. Default impl is a no-op for
    /// resolvers that do not implement re-resolution tracking.
    fn track_subscriber(&self, _pending: PendingResolution) {}

    /// Signal that an upstream scenario will not register; resolve any pending
    /// waiters with [`RegistryError::UpstreamCancelled`] and notify them via
    /// [`GateEdge::UpstreamGone`]. Default impl is a no-op.
    fn cancel_pending_for_upstream(
        &self,
        _scenario_name: &str,
        _entry_id: &str,
    ) -> Vec<RegistryError> {
        Vec::new()
    }
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
        let (mut rx, _init) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
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
        let (mut rx, _init) = bus.subscribe(after_spec(AfterOpDir::GreaterThan, 1.0));
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
        let (mut rx, init) = bus.subscribe(after_spec(AfterOpDir::GreaterThan, 1.0));
        assert!(init.after_already_fired);
        assert_eq!(rx.try_recv(), Some(GateEdge::AfterFired));
    }

    #[test]
    fn replace_on_full_keeps_only_latest_edge() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
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
        assert!(count <= 1, "while: queue depth must stay <= 1, got {count}");
    }

    #[test]
    fn unchanged_value_short_circuits() {
        let bus = GateBus::new();
        let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
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
        let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
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
        let (mut rx, _init) = bus.subscribe(spec);

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
    fn recv_edge_timeout_returns_none_when_no_edge_arrives() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = GateBus::new();
            let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
            let r = rx.recv_edge_timeout(Duration::from_millis(40)).await;
            assert!(r.is_none());
        });
    }

    #[test]
    fn recv_edge_timeout_returns_edge_when_published_before_deadline() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = Arc::new(GateBus::new());
            bus.tick(0.0);
            let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
            let bus_for_writer = Arc::clone(&bus);
            tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(10)).await;
                bus_for_writer.drive_value(1.0);
            });
            let edge = rx.recv_edge_timeout(Duration::from_millis(200)).await;
            assert_eq!(edge, Some(GateEdge::WhileOpen));
        });
    }

    #[test]
    fn drained_after_close_latch_drains_pending_then_returns_none() {
        let (tx, rx) = gate_edge_channel();
        tx.send(GateEdge::WhileOpen);
        tx.send(GateEdge::WhileClose);
        drop(tx);
        let mut rx = GateReceiver {
            after_rx: None,
            while_rx: Some(rx),
        };
        assert_eq!(rx.try_recv(), Some(GateEdge::WhileClose));
        assert!(rx.try_recv().is_none());
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn drained_after_close_latch_synthesises_upstream_gone_when_sender_drops_silent() {
        let (tx, rx) = gate_edge_channel();
        drop(tx);
        let mut rx = GateReceiver {
            after_rx: None,
            while_rx: Some(rx),
        };
        assert_eq!(rx.try_recv(), Some(GateEdge::UpstreamGone));
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn drained_after_close_latch_returns_close_then_none_when_sender_drops_after_close() {
        let (tx, rx) = gate_edge_channel();
        tx.send(GateEdge::WhileClose);
        drop(tx);
        let mut rx = GateReceiver {
            after_rx: None,
            while_rx: Some(rx),
        };
        assert_eq!(rx.try_recv(), Some(GateEdge::WhileClose));
        assert!(rx.try_recv().is_none());
        assert!(rx.try_recv().is_none());
    }

    #[test]
    fn many_subscribers_receive_independently() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (mut rx_a, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        let (mut rx_b, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 5.0));

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

    #[test]
    fn gate_receiver_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<GateReceiver>();
    }

    #[test]
    fn gate_edge_sender_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<GateEdgeSender>();
    }

    #[test]
    fn edge_receiver_is_send_and_sync() {
        fn check<T: Send + Sync>() {}
        check::<EdgeReceiver>();
    }

    #[test]
    fn broadcast_upstream_gone_delivers_to_all_subscribers() {
        let bus = GateBus::new();
        bus.tick(0.0);
        let (mut rx_a, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        let (mut rx_b, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
        bus.broadcast_upstream_gone();
        assert_eq!(rx_a.try_recv(), Some(GateEdge::UpstreamGone));
        assert_eq!(rx_b.try_recv(), Some(GateEdge::UpstreamGone));
    }

    #[test]
    fn broadcast_then_consume_then_poll_returns_none_indefinitely() {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .unwrap();
        rt.block_on(async {
            let bus = GateBus::new();
            bus.tick(0.0);
            let (mut rx, _) = bus.subscribe(while_spec(WhileOp::GreaterThan, 0.0));
            bus.broadcast_upstream_gone();
            assert_eq!(rx.try_recv(), Some(GateEdge::UpstreamGone));
            assert!(rx.try_recv().is_none());
            assert!(rx.try_recv().is_none());
            let r = rx.recv_edge_timeout(Duration::from_millis(20)).await;
            assert!(
                r.is_none(),
                "wait_for_change must park while sender clones are still alive in the bus"
            );
        });
    }

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
