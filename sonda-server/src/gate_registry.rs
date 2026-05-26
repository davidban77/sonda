//! Production [`GateBusResolver`] backing the HTTP server's cross-POST `while:` refs.

use std::collections::HashMap;
use std::sync::{Arc, RwLock, Weak};
use std::time::Instant;

use sonda_core::schedule::gate_bus::{
    GateBus, GateBusResolver, GateEdge, GateEdgeSender, PendingRef, PendingResolution,
    RegistryError, WhileSpec,
};
use sonda_core::schedule::stats::ScenarioStats;
use sonda_core::UnresolvedBehavior;

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

#[derive(Default)]
pub struct GateBusRegistry {
    buses: RwLock<HashMap<BusKey, Arc<GateBus>>>,
    subscribers: RwLock<HashMap<BusKey, Vec<SubscriberRef>>>,
    pending: RwLock<HashMap<String, PendingResolution>>,
}

impl GateBusRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Rewrite tracking keys when the caller assigns a new external handle id
    /// after a scenario has already been launched and recorded.
    pub fn rename_handle(&self, old_id: &str, new_id: &str) {
        if old_id == new_id {
            return;
        }
        let mut pending = self.pending.write().unwrap_or_else(|p| p.into_inner());
        if let Some(mut entry) = pending.remove(old_id) {
            entry.handle_id = new_id.to_string();
            pending.insert(new_id.to_string(), entry);
        }
        drop(pending);
        let mut subs = self.subscribers.write().unwrap_or_else(|p| p.into_inner());
        for refs in subs.values_mut() {
            for sub in refs.iter_mut() {
                if sub.handle_id == old_id {
                    sub.handle_id = new_id.to_string();
                }
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
        let mut buses = self.buses.write().unwrap_or_else(|p| p.into_inner());
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
            .unwrap_or_else(|p| p.into_inner())
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

    fn unregister(&self, scenario_name: &str) {
        let mut buses = self.buses.write().unwrap_or_else(|p| p.into_inner());
        let removed_keys: Vec<BusKey> = buses
            .keys()
            .filter(|(name, _)| name == scenario_name)
            .cloned()
            .collect();
        let mut removed_buses: Vec<(BusKey, Arc<GateBus>)> = Vec::with_capacity(removed_keys.len());
        for key in &removed_keys {
            if let Some(bus) = buses.remove(key) {
                removed_buses.push((key.clone(), bus));
            }
        }
        drop(buses);

        // Broadcast on the bus first so in-flight subscribers (those that called
        // subscribe_with_while_sender but whose track_subscriber has not yet landed
        // in the registry) still receive UpstreamGone.
        for (_, bus) in &removed_buses {
            bus.broadcast_upstream_gone();
        }

        let mut subs = self.subscribers.write().unwrap_or_else(|p| p.into_inner());
        let mut pending = self.pending.write().unwrap_or_else(|p| p.into_inner());

        for key in removed_keys {
            let Some(refs) = subs.remove(&key) else {
                continue;
            };
            for sub in refs {
                if sub.stats.strong_count() == 0 {
                    continue;
                }
                let _ = sub.sender.try_send(GateEdge::UpstreamGone);
                let handle_id = sub.handle_id.clone();
                let pending_entry = sub.into_pending(key.0.clone(), key.1.clone());
                pending.insert(handle_id, pending_entry);
            }
        }
    }

    fn sweep_pending(&self) -> usize {
        let buses = self.buses.read().unwrap_or_else(|p| p.into_inner());
        let mut pending = self.pending.write().unwrap_or_else(|p| p.into_inner());
        let mut subs = self.subscribers.write().unwrap_or_else(|p| p.into_inner());

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
                if let Ok(mut s) = stats_arc.write() {
                    s.cumulative_resolution_attempts =
                        s.cumulative_resolution_attempts.saturating_add(1);
                }
            }
            bus.subscribe_with_while_sender(entry.spec, entry.edge_sender.clone());
            let (key, sub) = SubscriberRef::from_pending(entry);
            subs.entry(key).or_default().push(sub);
            promoted += 1;
        }
        promoted
    }

    fn insert_pending(&self, pending: PendingResolution) {
        let mut map = self.pending.write().unwrap_or_else(|p| p.into_inner());
        map.insert(pending.handle_id.clone(), pending);
    }

    fn pending_for_handle(&self, handle_id: &str) -> Option<PendingRef> {
        let map = self.pending.read().unwrap_or_else(|p| p.into_inner());
        let entry = map.get(handle_id)?;
        Some(PendingRef::from_pending(
            entry,
            std::time::SystemTime::now(),
        ))
    }

    fn scenario_name_in_use(&self, scenario_name: &str) -> bool {
        let buses = self.buses.read().unwrap_or_else(|p| p.into_inner());
        buses.keys().any(|(name, _)| name == scenario_name)
    }

    fn track_subscriber(&self, pending: PendingResolution) {
        let mut subs = self.subscribers.write().unwrap_or_else(|p| p.into_inner());
        let (key, sub) = SubscriberRef::from_pending(pending);
        subs.entry(key).or_default().push(sub);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sonda_core::compiler::WhileOp;
    use std::sync::mpsc;
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
        let (tx, _rx) = mpsc::sync_channel::<GateEdge>(1);
        let (_alive, weak) = live_stats();
        let got = reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone());
        assert!(got.is_some());
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));
        let subs = reg.subscribers.read().unwrap();
        let row = subs
            .get(&("post-a".to_string(), "m".to_string()))
            .expect("subscribers row exists");
        assert_eq!(row.len(), 1);
        assert_eq!(row[0].handle_id, "h1");
    }

    #[test]
    fn t_reg_3_subscribe_with_no_upstream_returns_none_then_insert_pending() {
        let reg = GateBusRegistry::new();
        let (tx, _rx) = mpsc::sync_channel::<GateEdge>(1);
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
        let (tx, _rx) = mpsc::sync_channel::<GateEdge>(1);
        reg.insert_pending(make_pending("h1", "upstream", "m", weak, tx));

        let bus = Arc::new(GateBus::new());
        bus.tick(1.0);
        reg.register("upstream", "m", bus).expect("register");
        let promoted = reg.sweep_pending();
        assert_eq!(promoted, 1);
        assert!(reg.pending_for_handle("h1").is_none());
        let subs = reg.subscribers.read().unwrap();
        assert!(subs
            .get(&("upstream".to_string(), "m".to_string()))
            .is_some());
        drop(alive);
    }

    #[test]
    fn t_reg_5_unregister_moves_subscribers_to_pending_and_signals_gone() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");
        let (tx, rx) = mpsc::sync_channel::<GateEdge>(1);
        let (alive, weak) = live_stats();
        reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone())
            .expect("sub");
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));

        reg.unregister("post-a");
        let edge = rx
            .recv_timeout(Duration::from_millis(200))
            .expect("UpstreamGone within 200ms");
        assert_eq!(edge, GateEdge::UpstreamGone);
        assert!(reg.lookup("post-a", "m").is_none());
        assert!(
            reg.pending_for_handle("h1").is_some(),
            "subscriber must move to pending for re-resolution",
        );
        drop(alive);
    }

    #[test]
    fn t_reg_6_re_register_after_unregister_re_wires_existing_sender() {
        let reg = GateBusRegistry::new();
        let bus_a = Arc::new(GateBus::new());
        bus_a.tick(1.0);
        reg.register("post-a", "m", Arc::clone(&bus_a))
            .expect("reg-a");
        let (tx, rx) = mpsc::sync_channel::<GateEdge>(1);
        let (alive, weak) = live_stats();
        reg.subscribe(("post-a", "m"), "h1", weak.clone(), tx.clone())
            .expect("sub");
        reg.track_subscriber(make_pending("h1", "post-a", "m", weak, tx));

        reg.unregister("post-a");
        let _ = rx.recv_timeout(Duration::from_millis(200));

        let bus_b = Arc::new(GateBus::new());
        bus_b.tick(1.0);
        reg.register("post-a", "m", Arc::clone(&bus_b))
            .expect("re-reg");
        let promoted = reg.sweep_pending();
        assert_eq!(promoted, 1, "sweep must re-resolve the pending subscriber");

        let edge = rx
            .recv_timeout(Duration::from_millis(200))
            .expect("WhileOpen within 200ms");
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
    fn t_reg_8_dead_weak_is_silently_skipped_on_unregister_and_sweep() {
        let reg = GateBusRegistry::new();
        let bus = Arc::new(GateBus::new());
        reg.register("post-a", "m", Arc::clone(&bus)).expect("reg");
        let (tx, rx) = mpsc::sync_channel::<GateEdge>(1);
        {
            let (alive_local, weak) = live_stats();
            reg.subscribe(("post-a", "m"), "h-dead", weak.clone(), tx.clone())
                .expect("sub");
            reg.track_subscriber(make_pending("h-dead", "post-a", "m", weak, tx));
            drop(alive_local);
        }
        reg.unregister("post-a");
        let edge = rx.recv_timeout(Duration::from_millis(100));
        assert!(
            edge.is_err(),
            "no edge should be delivered for a dead-weak subscriber"
        );
        assert!(reg.pending_for_handle("h-dead").is_none());
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
                let (tx, _rx) = mpsc::sync_channel::<GateEdge>(1);
                let _ = r.subscribe(("post-a", "m"), &handle_id, weak.clone(), tx.clone());
                r.track_subscriber(make_pending(&handle_id, "post-a", "m", weak, tx));
            });
            threads.push(h);
        }
        for t in threads {
            t.join().expect("join");
        }
        let subs = reg.subscribers.read().unwrap();
        assert_eq!(subs.values().map(|v| v.len()).sum::<usize>(), 8);
        drop(kept);
    }
}
