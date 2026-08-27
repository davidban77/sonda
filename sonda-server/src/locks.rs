//! Shared-state lock policy: a poisoned lock is recovered, counted, and reported through
//! `sonda_server_lock_recoveries_total`, so one panicking handler costs the operation it
//! panicked in rather than every later acquisition for the life of the process.
//!
//! Recovery itself is not new for most of this state — the three gate-registry locks and the
//! two request-metrics locks already recovered in place, each at its own call site. What is new
//! is that one type states the policy, the `scenarios` map follows it too, and every recovery is
//! counted.
//!
//! Recovery is sound only where no critical section can leave a torn invariant behind. The
//! argument, per lock:
//!
//! - `scenarios` — rows are independent. A panic between two inserts of one batch leaves the
//!   earlier rows admitted and reachable and `AdoptionGuard` stops the tail it still holds. What
//!   the map itself holds reads exactly like a batch whose later entries were never admitted.
//!   Two things survive the guard: the one handle in flight at the panic escapes still running,
//!   and having dropped its permit it no longer counts against `--max-scenarios`; and the guard's
//!   `unregister` is keyed by scenario name alone, so the rows it already handed over keep
//!   running and stay listed but lose their gate buses, and their downstreams see `UpstreamGone`.
//! - `request_counters`, `request_histograms` — values are atomics and the update run under the
//!   guard cannot panic, so at worst one observation is lost.
//! - `gate_buses` — rows are independent and keyed by `(scenario_name, entry_id)`. `unregister`
//!   removes a pre-collected key set one key at a time, so a panic mid-loop leaves the keys it
//!   has not reached registered rather than any row half-removed.
//! - `gate_subscribers`, `gate_pending` — a subscriber moves between the two maps while both
//!   guards are held, so a panic mid-move can drop an in-flight reference; its edge sender drops
//!   with it, which the downstream reads as `UpstreamGone` and settles in `unresolved` rather
//!   than waiting forever. A panic part-way through `unregister` likewise leaves the subscriber
//!   rows it has not reached in place after their bus is gone: those downstreams have already had
//!   `UpstreamGone` broadcast to them, and since only `pending` is ever re-resolved they will not
//!   be re-resolved again while the process lives. Nothing reads a row left behind as something
//!   it is not.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard};

use tracing::warn;

pub struct RecoveringLock<T> {
    inner: RwLock<T>,
    name: &'static str,
    recoveries: AtomicU64,
}

impl<T> RecoveringLock<T> {
    pub fn new(name: &'static str, value: T) -> Self {
        Self {
            inner: RwLock::new(value),
            name,
            recoveries: AtomicU64::new(0),
        }
    }

    pub fn read(&self) -> RwLockReadGuard<'_, T> {
        match self.inner.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.record_recovery();
                poisoned.into_inner()
            }
        }
    }

    pub fn write(&self) -> RwLockWriteGuard<'_, T> {
        match self.inner.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                self.record_recovery();
                poisoned.into_inner()
            }
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    #[cfg(test)]
    pub fn is_poisoned(&self) -> bool {
        self.inner.is_poisoned()
    }

    pub fn recoveries(&self) -> u64 {
        self.recoveries.load(Ordering::Relaxed)
    }

    fn record_recovery(&self) {
        // Poisoning is sticky, so only the first recovery is worth a log line.
        if self.recoveries.fetch_add(1, Ordering::Relaxed) == 0 {
            warn!(
                lock = self.name,
                "recovered a poisoned lock — a handler panicked while holding it; state guarded by this lock may have lost an update"
            );
        }
    }
}

/// Leave `lock` poisoned, the way a handler that panics while holding it does.
#[cfg(test)]
pub fn poison<T>(lock: &RecoveringLock<T>) {
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = lock.write();
        panic!("intentional poison");
    }));
    assert!(panicked.is_err(), "the poisoning panic must have happened");
    assert!(lock.is_poisoned(), "the lock must be poisoned afterwards");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn read_returns_the_value_written_under_a_healthy_lock() {
        let lock = RecoveringLock::new("t", vec![1u8, 2, 3]);
        lock.write().push(4);
        assert_eq!(*lock.read(), vec![1, 2, 3, 4]);
    }

    #[test]
    fn a_healthy_lock_records_no_recoveries() {
        let lock = RecoveringLock::new("t", 0u8);
        drop(lock.read());
        drop(lock.write());
        assert_eq!(lock.recoveries(), 0);
    }

    #[test]
    fn read_after_poisoning_yields_the_state_the_panicking_writer_left() {
        let lock = RecoveringLock::new("t", vec![1u8]);
        poison(&lock);
        assert_eq!(*lock.read(), vec![1]);
    }

    #[test]
    fn write_after_poisoning_still_mutates() {
        let lock = RecoveringLock::new("t", vec![1u8]);
        poison(&lock);
        lock.write().push(2);
        assert_eq!(*lock.read(), vec![1, 2]);
    }

    #[test]
    fn every_recovered_acquisition_is_counted() {
        let lock = RecoveringLock::new("t", 0u8);
        poison(&lock);
        drop(lock.read());
        drop(lock.write());
        assert_eq!(lock.recoveries(), 2);
    }

    #[test]
    fn the_name_is_carried_for_reporting() {
        assert_eq!(RecoveringLock::new("scenarios", ()).name(), "scenarios");
    }

    #[test]
    fn recoveries_from_other_threads_are_visible() {
        let lock = Arc::new(RecoveringLock::new("t", 0u64));
        poison(&lock);
        let readers: Vec<_> = (0..4)
            .map(|_| {
                let lock = Arc::clone(&lock);
                std::thread::spawn(move || {
                    drop(lock.read());
                })
            })
            .collect();
        for reader in readers {
            reader.join().expect("reader must not panic");
        }
        assert_eq!(lock.recoveries(), 4);
    }

    #[test]
    fn recovering_lock_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<RecoveringLock<Vec<u8>>>();
    }
}
