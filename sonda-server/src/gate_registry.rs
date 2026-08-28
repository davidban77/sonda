//! Production [`GateBusResolver`] backing the HTTP server's cross-POST `while:` refs.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::Instant;

use sonda_core::schedule::gate_bus::{
    GateBus, GateBusResolver, GateEdge, GateEdgeSender, PendingRef, PendingResolution,
    RegistryError, WhileSpec,
};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::ScenarioState;
use sonda_core::UnresolvedBehavior;

use crate::locks::RecoveringLock;

type BusKey = (String, String);

struct SubscriberRef {
    handle_id: String,
    stats: Weak<RwLock<ScenarioStats>>,
    sender: GateEdgeSender,
    spec: WhileSpec,
    if_unresolved: UnresolvedBehavior,
    registered_at: Instant,
    attempts: u64,
}

impl SubscriberRef {
    fn from_pending(p: PendingResolution) -> (BusKey, Self) {
        let key = (p.scenario_name, p.entry_id);
        let sub = SubscriberRef {
            handle_id: p.handle_id,
            stats: p.stats,
            sender: p.edge_sender,
            spec: p.spec,
            if_unresolved: p.if_unresolved,
            registered_at: p.registered_at,
            attempts: p.attempts,
        };
        (key, sub)
    }

    fn into_pending(self, scenario_name: String, entry_id: String) -> PendingResolution {
        PendingResolution::new(
            self.handle_id,
            self.stats,
            self.sender,
            scenario_name,
            entry_id,
            self.if_unresolved,
            self.registered_at,
            self.attempts,
            self.spec,
        )
    }
}

const BUSES_LOCK: &str = "gate_buses";
const SUBSCRIBERS_LOCK: &str = "gate_subscribers";
const PENDING_LOCK: &str = "gate_pending";

pub struct GateBusRegistry {
    buses: RecoveringLock<HashMap<BusKey, Arc<GateBus>>>,
    subscribers: RecoveringLock<HashMap<BusKey, Vec<SubscriberRef>>>,
    pending: RecoveringLock<HashMap<String, PendingResolution>>,
}

impl Default for GateBusRegistry {
    fn default() -> Self {
        Self {
            buses: RecoveringLock::new(BUSES_LOCK, HashMap::new()),
            subscribers: RecoveringLock::new(SUBSCRIBERS_LOCK, HashMap::new()),
            pending: RecoveringLock::new(PENDING_LOCK, HashMap::new()),
        }
    }
}

impl GateBusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn lock_recoveries(&self) -> [(&'static str, u64); 3] {
        [
            (self.buses.name(), self.buses.recoveries()),
            (self.subscribers.name(), self.subscribers.recoveries()),
            (self.pending.name(), self.pending.recoveries()),
        ]
    }

    /// Rewrite tracking keys when the caller assigns a new external handle id
    /// after a scenario has already been launched and recorded.
    pub fn rename_handle(&self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        let mut pending = self.pending.write();
        if let Some(mut entry) = pending.remove(old_id) {
            entry.handle_id = new_id.to_string();
            pending.insert(new_id.to_string(), entry);
        }
        drop(pending);
        let mut subs = self.subscribers.write();
        for refs in subs.values_mut() {
            for sub in refs.iter_mut() {
                if sub.handle_id == old_id {
                    sub.handle_id = new_id.to_string();
                }
            }
        }
    }

    fn retire_buses(&self, scenario_name: &str, entry_ids: &[&str]) -> Vec<BusKey> {
        let mut buses = self.buses.write();
        let keys: Vec<BusKey> = buses
            .keys()
            .filter(|(name, entry_id)| {
                name == scenario_name && entry_ids.contains(&entry_id.as_str())
            })
            .cloned()
            .collect();
        let mut removed: Vec<BusKey> = Vec::with_capacity(keys.len());
        let mut retired: Vec<Arc<GateBus>> = Vec::with_capacity(keys.len());
        for key in keys {
            if let Some(bus) = buses.remove(&key) {
                removed.push(key);
                retired.push(bus);
            }
        }
        drop(buses);

        // Broadcast on the bus first so in-flight subscribers (those that called
        // subscribe_with_while_sender but whose track_subscriber has not yet landed
        // in the registry) still receive UpstreamGone.
        for bus in retired {
            bus.broadcast_upstream_gone();
        }
        removed
    }

    fn requeue_subscribers(&self, keys: &[BusKey]) {
        let mut subs = self.subscribers.write();
        let mut pending = self.pending.write();
        for key in keys {
            let Some(refs) = subs.remove(key) else {
                continue;
            };
            for sub in refs {
                if sub.stats.strong_count() == 0 {
                    continue;
                }
                sub.sender.send(GateEdge::UpstreamGone);
                let handle_id = sub.handle_id.clone();
                let pending_entry = sub.into_pending(key.0.clone(), key.1.clone());
                pending.insert(handle_id, pending_entry);
            }
        }
    }
}

impl GateBusResolver for GateBusRegistry {
    fn register(
        &self,
        scenario_name: &str,
        entry_id: &str,
        bus: Arc<GateBus>,
    ) -> Result<(), RegistryError> {
        let mut buses = self.buses.write();
        let key = (scenario_name.to_string(), entry_id.to_string());
        if buses.contains_key(&key) {
            return Err(RegistryError::DuplicateScenarioName {
                name: scenario_name.to_string(),
            });
        }
        buses.insert(key, bus);
        Ok(())
    }

    fn lookup(&self, scenario_name: &str, entry_id: &str) -> Option<Arc<GateBus>> {
        self.buses
            .read()
            .get(&(scenario_name.to_string(), entry_id.to_string()))
            .cloned()
    }

    fn subscribe(
        &self,
        upstream: (&str, &str),
        _downstream_handle_id: &str,
        _downstream_stats: Weak<RwLock<ScenarioStats>>,
        _edge_sender: GateEdgeSender,
    ) -> Option<Arc<GateBus>> {
        // Tracking happens via `track_subscriber`, which carries the spec.
        self.lookup(upstream.0, upstream.1)
    }

    fn unregister_entries(&self, scenario_name: &str, entry_ids: &[&str]) {
        let removed = self.retire_buses(scenario_name, entry_ids);
        self.requeue_subscribers(&removed);
    }

    fn sweep_pending(&self) -> usize {
        let buses = self.buses.read();
        let mut pending = self.pending.write();
        let mut subs = self.subscribers.write();

        let mut promoted = 0usize;
        let keys: Vec<String> = pending.keys().cloned().collect();
        for handle_id in keys {
            let entry = pending
                .get(&handle_id)
                .expect("key from snapshot must exist");
            if entry.stats.strong_count() == 0 {
                pending.remove(&handle_id);
                continue;
            }
            let bus_key = (entry.scenario_name.clone(), entry.entry_id.clone());
            let Some(bus) = buses.get(&bus_key) else {
                continue;
            };
            let mut entry = pending.remove(&handle_id).expect("just looked up");
            entry.attempts = entry.attempts.saturating_add(1);
            if let Some(stats_arc) = entry.stats.upgrade() {
                let mut s = stats_arc.write().unwrap_or_else(|p| p.into_inner());
                s.cumulative_resolution_attempts =
                    s.cumulative_resolution_attempts.saturating_add(1);
            }
            bus.subscribe_with_while_sender(entry.spec, entry.edge_sender.clone());
            let (key, sub) = SubscriberRef::from_pending(entry);
            subs.entry(key).or_default().push(sub);
            promoted += 1;
        }
        promoted
    }

    fn insert_pending(&self, pending: PendingResolution) {
        let mut map = self.pending.write();
        map.insert(pending.handle_id.clone(), pending);
    }

    fn pending_for_handle(&self, handle_id: &str) -> Option<PendingRef> {
        let map = self.pending.read();
        let entry = map.get(handle_id)?;
        Some(PendingRef::from_pending(
            entry,
            std::time::SystemTime::now(),
        ))
    }

    fn scenario_name_in_use(&self, scenario_name: &str) -> bool {
        let buses = self.buses.read();
        buses.keys().any(|(name, _)| name == scenario_name)
    }

    fn track_subscriber(&self, pending: PendingResolution) {
        let mut subs = self.subscribers.write();
        let (key, sub) = SubscriberRef::from_pending(pending);
        subs.entry(key).or_default().push(sub);
    }

    fn cancel_pending_for_upstream(
        &self,
        scenario_name: &str,
        entry_id: &str,
    ) -> Vec<RegistryError> {
        let mut pending = self.pending.write();
        let matching: Vec<String> = pending
            .iter()
            .filter(|(_, p)| p.scenario_name == scenario_name && p.entry_id == entry_id)
            .map(|(k, _)| k.clone())
            .collect();
        let mut errors = Vec::with_capacity(matching.len());
        for handle_id in matching {
            let entry = pending.remove(&handle_id).expect("just snapshotted");
            entry.edge_sender.send(GateEdge::UpstreamGone);
            if let Some(stats_arc) = entry.stats.upgrade() {
                let mut s = stats_arc.write().unwrap_or_else(|p| p.into_inner());
                if s.state != ScenarioState::Finished {
                    s.transition_state(ScenarioState::Unresolved);
                }
            }
            errors.push(RegistryError::UpstreamCancelled {
                scenario_name: entry.scenario_name,
                entry_id: entry.entry_id,
            });
        }
        errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compiler::WhileOp;
    use sonda_core::schedule::gate_bus::{gate_edge_channel, EdgeReceiver};
    use std::thread;
    use std::time::Duration;

    fn while_spec() -> WhileSpec {
        WhileSpec {
            op: WhileOp::GreaterThan,
            threshold: 0.0,
        }
    }

    fn live_stats() -> (Arc<RwLock<ScenarioStats>>, Weak<RwLock<ScenarioStats>>) {
        let arc = Arc::new(RwLock::new(ScenarioStats::default()));
        let weak = Arc::downgrade(&arc);
        (arc, weak)
    }

    fn make_pending(
        handle_id: &str,
        scenario_name: &str,
        entry_id: &str,
        stats: Weak<RwLock<ScenarioStats>>,
        sender: GateEdgeSender,
    ) -> PendingResolution {
        PendingResolution::new(
            handle_id.to_string(),
            stats,
            sender,
            scenario_name.to_string(),
            entry_id.to_string(),
            UnresolvedBehavior::default(),
            Instant::now(),
            0,
            while_spec(),
        )
    }

    fn recv_timeout(rx: &mut EdgeReceiver, timeout: Duration) -> Result<GateEdge, ()> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("rt");
        rt.block_on(async {
            if let Some(edge) = rx.try_recv() {
                return Ok(edge);
            }
            if tokio::time::timeout(timeout, rx.wait_for_change())
                .await
                .is_err()
            {
                return Err(());
            }
            rx.try_recv().ok_or(())
        })
    }

    #[test]
    fn t_reg_1_register_then_lookup_roundtrip() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "metric1", Arc::clone(&bus))
            .expect("register");
        let looked = reg.lookup("post-a", "metric1").expect("lookup hit");
        assert!(Arc::ptr_eq(&bus, &looked));
        assert!(reg.lookup("post-a", "other").is_none());
        assert!(reg.lookup("post-b", "metric1").is_none());
    }

    #[test]
    fn t_reg_2_subscribe_with_live_upstream_returns_bus_and_track_records_subscriber() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");
        let (tx, _rx) = gate_edge_channel();
        let (_alive, weak) = live_stats();
        let got = reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone());
        assert!(got.is_some());
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));
        let subs = reg.subscribers.read();
        let row = subs
            .get(&("post-a".to_string(), "m".to_string()))
            .expect("subscribers row exists");
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].handle_id, "h1");
    }

    #[test]
    fn t_reg_3_subscribe_with_no_upstream_returns_none_then_insert_pending() {
        let reg = GateBusRegistry::new();
        let (tx, _rx) = gate_edge_channel();
        let (_alive, weak) = live_stats();
        let got = reg.subscribe(("missing", "m"), "h2", weak.clone(), tx.clone());
        assert!(got.is_none());
        reg.insert_pending(make_pending("h2", "missing", "m", weak, tx));
        let pending_ref = reg.pending_for_handle("h2").expect("pending present");
        assert_eq!(pending_ref.scenario_name, "missing");
        assert_eq!(pending_ref.entry_id, "m");
    }

    #[test]
    fn t_reg_4_sweep_pending_resolves_after_register() {
        let reg = GateBusRegistry::new();
        let (alive, weak) = live_stats();
        let (tx, _rx) = gate_edge_channel();
        reg.insert_pending(make_pending("h1", "upstream", "m", weak, tx));

        let bus = Arc::new(GateBus::new());
        bus.tick(1.0);
        reg.register("upstream", "m", bus).expect("register");
        let promoted = reg.sweep_pending();
        assert_eq!(promoted, 1);
        assert!(reg.pending_for_handle("h1").is_none());
        let subs = reg.subscribers.read();
        assert!(subs
            .get(&("upstream".to_string(), "m".to_string()))
            .is_some());
        drop(alive);
    }

    #[test]
    fn t_reg_5_unregister_entries_moves_subscribers_to_pending_and_signals_gone() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");
        let (tx, mut rx) = gate_edge_channel();
        let (alive, weak) = live_stats();
        reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone())
            .expect("sub");
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));

        reg.unregister_entries("post-a", &["m"]);
        let edge =
            recv_timeout(&mut rx, Duration::from_millis(200)).expect("UpstreamGone within 200ms");
        assert_eq!(edge, GateEdge::UpstreamGone);
        assert!(reg.lookup("post-a", "m").is_none());
        assert!(
            reg.pending_for_handle("h1").is_some(),
            "subscriber must move to pending for re-resolution",
        );
        drop(alive);
    }

    #[test]
    fn unregister_entries_removes_only_the_named_entries_buses() {
        let reg = GateBusRegistry::new();
        reg.register("post-a", "m1", Arc::new(GateBus::new()))
            .expect("reg a/m1");
        reg.register("post-a", "m2", Arc::new(GateBus::new()))
            .expect("reg a/m2");
        reg.register("post-b", "m1", Arc::new(GateBus::new()))
            .expect("reg b/m1");

        reg.unregister_entries("post-a", &["m1"]);

        assert!(reg.lookup("post-a", "m1").is_none());
        assert!(
            reg.lookup("post-a", "m2").is_some(),
            "an entry of the same scenario name that was not named must stay registered"
        );
        assert!(reg.lookup("post-b", "m1").is_some());
        assert!(reg.scenario_name_in_use("post-a"));
    }

    #[test]
    fn unregister_entries_requeues_only_the_removed_entrys_subscribers() {
        let reg = GateBusRegistry::new();
        let bus_1 = Arc::new(GateBus::new());
        let bus_2 = Arc::new(GateBus::new());
        reg.register("post-a", "m1", Arc::clone(&bus_1))
            .expect("reg m1");
        reg.register("post-a", "m2", Arc::clone(&bus_2))
            .expect("reg m2");
        let (tx_1, mut rx_1) = gate_edge_channel();
        let (tx_2, mut rx_2) = gate_edge_channel();
        let (alive_1, weak_1) = live_stats();
        let (alive_2, weak_2) = live_stats();
        reg.subscribe(("post-a", "m1"), "h1", weak_1.clone(), tx_1.clone())
            .expect("sub m1");
        reg.subscribe(("post-a", "m2"), "h2", weak_2.clone(), tx_2.clone())
            .expect("sub m2");
        reg.track_subscriber(make_pending("h1", "post-a", "m1", weak_1, tx_1));
        reg.track_subscriber(make_pending("h2", "post-a", "m2", weak_2, tx_2));

        reg.unregister_entries("post-a", &["m1"]);

        assert_eq!(
            recv_timeout(&mut rx_1, Duration::from_millis(200)),
            Ok(GateEdge::UpstreamGone)
        );
        assert!(reg.pending_for_handle("h1").is_some());
        assert!(
            recv_timeout(&mut rx_2, Duration::from_millis(50)).is_err(),
            "a subscriber of an entry that stays registered must not be signalled"
        );
        assert!(reg.pending_for_handle("h2").is_none());
        drop((alive_1, alive_2));
    }

    #[test]
    fn unregister_entries_leaves_pending_waiters_on_other_entries_of_the_same_name() {
        let reg = GateBusRegistry::new();
        reg.register("post-a", "m1", Arc::new(GateBus::new()))
            .expect("reg m1");
        let (tx, mut rx) = gate_edge_channel();
        let (alive, weak) = live_stats();
        reg.insert_pending(make_pending("h-waiting", "post-a", "m2", weak, tx));

        reg.unregister_entries("post-a", &["m1"]);

        assert!(
            reg.pending_for_handle("h-waiting").is_some(),
            "a waiter on an entry that was never removed must keep waiting"
        );
        assert!(
            recv_timeout(&mut rx, Duration::from_millis(50)).is_err(),
            "a waiter on an entry that was never removed must not be signalled"
        );
        drop(alive);
    }

    #[test]
    fn unregister_entries_with_no_entry_ids_removes_nothing() {
        let reg = GateBusRegistry::new();
        reg.register("post-a", "m1", Arc::new(GateBus::new()))
            .expect("reg m1");

        reg.unregister_entries("post-a", &[]);
        reg.unregister_entries("post-a", &["not-an-entry"]);

        assert!(reg.lookup("post-a", "m1").is_some());
    }

    #[test]
    fn t_reg_6_re_register_after_unregister_entries_re_wires_existing_sender() {
        let reg = GateBusRegistry::new();
        let bus_a = Arc::new(GateBus::new());
        bus_a.tick(1.0);
        reg.register("post-a", "m", Arc::clone(&bus_a))
            .expect("reg-a");
        let (tx, mut rx) = gate_edge_channel();
        let (alive, weak) = live_stats();
        reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone())
            .expect("sub");
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));

        reg.unregister_entries("post-a", &["m"]);
        let _ = recv_timeout(&mut rx, Duration::from_millis(200));

        let bus_b = Arc::new(GateBus::new());
        bus_b.tick(1.0);
        reg.register("post-a", "m", Arc::clone(&bus_b))
            .expect("re-reg");
        let promoted = reg.sweep_pending();
        assert_eq!(promoted, 1, "sweep must re-resolve the pending subscriber");

        let edge =
            recv_timeout(&mut rx, Duration::from_millis(200)).expect("WhileOpen within 200ms");
        assert_eq!(edge, GateEdge::WhileOpen);
        drop(alive);
    }

    #[test]
    fn t_reg_7_scenario_name_in_use_reports_correctly() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        assert!(!reg.scenario_name_in_use("post-a"));
        reg.register("post-a", "m", bus).expect("reg");
        assert!(reg.scenario_name_in_use("post-a"));
        assert!(!reg.scenario_name_in_use("post-b"));
    }

    #[test]
    fn t_reg_8_dead_weak_is_silently_skipped_on_unregister_entries_and_sweep() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");
        let (tx, mut rx) = gate_edge_channel();
        {
            let (alive_local, weak) = live_stats();
            reg.subscribe(("post-a", "m"), "h-dead", weak.clone(), tx.clone())
                .expect("sub");
            reg.track_subscriber(make_pending("h-dead", "post-a", "m", weak, tx));
            drop(alive_local);
        }
        reg.unregister_entries("post-a", &["m"]);
        let _ = recv_timeout(&mut rx, Duration::from_millis(100));
        assert!(reg.pending_for_handle("h-dead").is_none());
    }

    #[test]
    fn downstream_resolves_with_upstream_cancelled_error_when_upstream_cancels() {
        let reg = GateBusRegistry::new();
        let (tx, mut rx) = gate_edge_channel();
        let (alive, weak) = live_stats();
        reg.insert_pending(make_pending(
            "h-pending",
            "post-upstream",
            "metric_a",
            weak,
            tx,
        ));
        assert!(
            reg.pending_for_handle("h-pending").is_some(),
            "pending entry must be present before cancellation"
        );

        let errors = reg.cancel_pending_for_upstream("post-upstream", "metric_a");

        assert_eq!(
            errors.len(),
            1,
            "exactly one error must be returned for the cancelled pending entry"
        );
        assert!(
            matches!(
                &errors[0],
                RegistryError::UpstreamCancelled { scenario_name, entry_id }
                if scenario_name == "post-upstream" && entry_id == "metric_a"
            ),
            "error must be UpstreamCancelled for the matching upstream, got: {:?}",
            errors[0]
        );
        assert!(
            reg.pending_for_handle("h-pending").is_none(),
            "pending entry must be removed after cancellation"
        );
        let edge = recv_timeout(&mut rx, Duration::from_millis(200))
            .expect("waiter must receive UpstreamGone within 200ms");
        assert_eq!(edge, GateEdge::UpstreamGone);
        drop(alive);
    }

    #[test]
    fn cancel_pending_for_upstream_only_affects_matching_upstream() {
        let reg = GateBusRegistry::new();
        let (tx_a, mut rx_a) = gate_edge_channel();
        let (tx_b, mut rx_b) = gate_edge_channel();
        let (alive_a, weak_a) = live_stats();
        let (alive_b, weak_b) = live_stats();
        reg.insert_pending(make_pending(
            "h-a",
            "post-upstream",
            "metric_a",
            weak_a,
            tx_a,
        ));
        reg.insert_pending(make_pending("h-b", "post-other", "metric_b", weak_b, tx_b));

        let errors = reg.cancel_pending_for_upstream("post-upstream", "metric_a");
        assert_eq!(errors.len(), 1);
        assert!(
            reg.pending_for_handle("h-a").is_none(),
            "matching pending must be removed"
        );
        assert!(
            reg.pending_for_handle("h-b").is_some(),
            "non-matching pending must remain"
        );
        assert_eq!(
            recv_timeout(&mut rx_a, Duration::from_millis(200)),
            Ok(GateEdge::UpstreamGone),
            "matching waiter must receive UpstreamGone"
        );
        assert!(
            recv_timeout(&mut rx_b, Duration::from_millis(50)).is_err(),
            "non-matching waiter must not receive any edge"
        );
        drop(alive_a);
        drop(alive_b);
    }

    #[test]
    fn unregister_entries_leaves_an_unresolved_waiter_on_the_removed_entry_pending() {
        let reg = GateBusRegistry::new();
        reg.register("post-a", "m1", Arc::new(GateBus::new()))
            .expect("reg m1");
        let (tx, mut rx) = gate_edge_channel();
        let (alive, weak) = live_stats();
        reg.insert_pending(make_pending("h-pending", "post-a", "m1", weak, tx));

        reg.unregister_entries("post-a", &["m1"]);

        assert!(
            reg.pending_for_handle("h-pending").is_some(),
            "a waiter that never resolved must keep waiting for a re-POST of the same name"
        );
        assert!(
            recv_timeout(&mut rx, Duration::from_millis(50)).is_err(),
            "a waiter that never resolved must not be signalled"
        );
        drop(alive);
    }

    #[test]
    fn t_reg_duplicate_register_returns_err() {
        // Same (scenario_name, entry_id) pair → reject.
        // Same scenario_name with different entry_id → allowed (multi-entry POST).
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m1", Arc::clone(&bus)).expect("reg");
        reg.register("post-a", "m2", Arc::clone(&bus))
            .expect("multi-entry under same scenario_name must succeed");
        let err = reg
            .register("post-a", "m1", bus)
            .expect_err("duplicate (scenario_name, entry_id) pair must reject");
        assert!(matches!(err, RegistryError::DuplicateScenarioName { .. }));
    }

    fn poison_stats(stats: &Arc<RwLock<ScenarioStats>>) {
        let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = stats.write().expect("first write must succeed");
            panic!("intentional poison");
        }));
        assert!(panicked.is_err(), "the poisoning panic must have happened");
        assert!(stats.is_poisoned());
    }

    fn state_of(stats: &Arc<RwLock<ScenarioStats>>) -> ScenarioState {
        stats.read().unwrap_or_else(|p| p.into_inner()).state
    }

    #[test]
    fn cancelling_an_upstream_marks_a_downstream_unresolved_through_its_poisoned_stats_lock() {
        let reg = GateBusRegistry::new();
        let (alive, weak) = live_stats();
        let (tx, _rx) = gate_edge_channel();
        reg.insert_pending(make_pending("h1", "upstream", "m", weak, tx));
        poison_stats(&alive);

        reg.cancel_pending_for_upstream("upstream", "m");

        assert_eq!(state_of(&alive), ScenarioState::Unresolved);
    }

    #[test]
    fn sweep_counts_the_resolution_attempt_through_a_poisoned_stats_lock() {
        let reg = GateBusRegistry::new();
        let (alive, weak) = live_stats();
        let (tx, _rx) = gate_edge_channel();
        reg.insert_pending(make_pending("h1", "upstream", "m", weak, tx));
        poison_stats(&alive);
        let bus = Arc::new(GateBus::new());
        bus.tick(1.0);
        reg.register("upstream", "m", bus).expect("register");

        assert_eq!(reg.sweep_pending(), 1);

        let attempts = alive
            .read()
            .unwrap_or_else(|p| p.into_inner())
            .cumulative_resolution_attempts;
        assert_eq!(attempts, 1);
    }

    #[test]
    fn poisoned_registry_locks_still_resolve_a_pending_subscriber() {
        let reg = GateBusRegistry::new();
        crate::locks::poison(&reg.buses);
        crate::locks::poison(&reg.subscribers);
        crate::locks::poison(&reg.pending);

        let (alive, weak) = live_stats();
        let (tx, _rx) = gate_edge_channel();
        reg.insert_pending(make_pending("h1", "upstream", "m", weak, tx));
        let bus = Arc::new(GateBus::new());
        bus.tick(1.0);
        reg.register("upstream", "m", bus).expect("register");

        assert_eq!(reg.sweep_pending(), 1);
        assert!(reg.pending_for_handle("h1").is_none());
        assert!(reg
            .subscribers
            .read()
            .contains_key(&("upstream".to_string(), "m".to_string())));
        for (lock, recoveries) in reg.lock_recoveries() {
            assert!(recoveries > 0, "{lock} must have counted its recoveries");
        }
        drop(alive);
    }

    #[test]
    fn t22_concurrent_subscribe_does_not_deadlock() {
        let reg = Arc::new(GateBusRegistry::new());
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");

        let mut threads = Vec::new();
        let mut kept: Vec<Arc<RwLock<ScenarioStats>>> = Vec::new();
        for i in 0..8 {
            let r = Arc::clone(&reg);
            let stats = Arc::new(RwLock::new(ScenarioStats::default()));
            let weak = Arc::downgrade(&stats);
            kept.push(stats);
            let handle_id = format!("h{i}");
            let h = thread::spawn(move || {
                let (tx, _rx) = gate_edge_channel();
                let _ = r.subscribe(("post-a", "m"), &handle_id, weak.clone(), tx.clone());
                r.track_subscriber(make_pending(&handle_id, "post-a", "m", weak, tx));
            });
            threads.push(h);
        }
        for t in threads {
            t.join().expect("join");
        }
        let subs = reg.subscribers.read();
        assert_eq!(subs.values().map(|v| v.len()).sum::<usize>(), 8);
        drop(kept);
    }
}
