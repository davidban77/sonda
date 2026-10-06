//! Allocation budget for `GnmiEncoder::encode_metric` in steady state.
//!
//! Lives in its own integration test rather than a `#[cfg(test)] mod tests`
//! because `#[global_allocator]` replaces the allocator for the whole test
//! binary. The counter itself is thread-local, so only allocations made on the
//! measuring thread inside a counted window are counted; tests and helpers on
//! other threads cannot add to it.
//!
//! Both cases encode into the same pre-grown buffer, so the only difference
//! between a first-sight encode and a repeat is the encoder's per-series path
//! cache.

// The `--no-default-features` test job compiles this file to nothing.
#![cfg(feature = "gnmi")]

mod common;

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};

use sonda_core::encoder::gnmi::{GnmiEncoder, GnmiEncoderConfig, GnmiValueType};
use sonda_core::encoder::Encoder;
use sonda_core::model::metric::{Labels, MetricEvent};

thread_local! {
    // `const` initialisers: reading these never allocates, so the allocator
    // below can touch them.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

struct CountingAlloc;

unsafe impl GlobalAlloc for CountingAlloc {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        // `try_with` fails only while a thread's locals are being torn down,
        // which is never inside a counted window.
        let _ = COUNTING.try_with(|counting| {
            if counting.get() {
                let _ = ALLOCS.try_with(|n| n.set(n.get() + 1));
            }
        });
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAlloc = CountingAlloc;

const CALLS: usize = 1_000;

fn encoder() -> GnmiEncoder {
    GnmiEncoder::new(&GnmiEncoderConfig {
        path: Some("/interfaces/interface[name={ifName}]/state/counters/{name}".to_string()),
        values: HashMap::from([("in_octets".to_string(), GnmiValueType::Uint)]),
        ..GnmiEncoderConfig::default()
    })
    .expect("encoder config is valid")
}

fn event(if_name: &str, value: f64) -> MetricEvent {
    MetricEvent::with_timestamp(
        "in_octets".to_string(),
        value,
        Labels::from_pairs(&[("device", "rtr-1"), ("ifName", if_name)]).expect("labels are valid"),
        UNIX_EPOCH + Duration::from_secs(1_700_000_000),
    )
    .expect("metric name is valid")
}

/// Run `body` with allocation counting on for this thread and return the
/// allocations it made on this thread.
fn count_allocations(body: impl FnOnce()) -> usize {
    ALLOCS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    body();
    COUNTING.with(|c| c.set(false));
    ALLOCS.with(Cell::get)
}

/// Another thread allocating throughout a counted window adds nothing to it.
/// This is what lets the cases here run in parallel with each other and with
/// `common`'s tests.
#[test]
fn allocations_on_other_threads_are_not_counted() {
    const OTHER_ALLOCS: usize = 10_000;
    let go = Arc::new(AtomicBool::new(false));
    let done = Arc::new(AtomicUsize::new(0));

    // Spawned before the window opens; inside it, this thread only flips a
    // flag and spins, neither of which allocates.
    let other = {
        let (go, done) = (Arc::clone(&go), Arc::clone(&done));
        std::thread::spawn(move || {
            while !go.load(Ordering::SeqCst) {
                std::hint::spin_loop();
            }
            for i in 0..OTHER_ALLOCS {
                std::hint::black_box(vec![i as u8; 16]);
                done.fetch_add(1, Ordering::SeqCst);
            }
        })
    };

    let allocs = count_allocations(|| {
        go.store(true, Ordering::SeqCst);
        while done.load(Ordering::SeqCst) < OTHER_ALLOCS {
            std::hint::spin_loop();
        }
    });
    other.join().expect("allocating thread panicked");

    // Vacuity guard: the other thread did allocate, all of it inside the window.
    assert_eq!(done.load(Ordering::SeqCst), OTHER_ALLOCS);
    assert_eq!(
        allocs, 0,
        "allocations on another thread leaked into this thread's count"
    );
}

/// After the first encode of a series, encoding it again allocates nothing:
/// the path bytes come from the cache and `buf` is reused.
#[test]
fn steady_state_encode_allocates_nothing() {
    let encoder = encoder();
    let event = event("GigabitEthernet0/0/0", 125_000.0);
    let mut buf = Vec::with_capacity(4096);
    encoder
        .encode_metric(&event, &mut buf)
        .expect("warm-up encode succeeds");
    let one_notification = buf.len();

    let mut calls = 0usize;
    let allocs = count_allocations(|| {
        for _ in 0..CALLS {
            buf.clear();
            encoder
                .encode_metric(&event, &mut buf)
                .expect("steady-state encode succeeds");
            calls += 1;
        }
    });

    // Vacuity guard: the loop ran and each call wrote a whole notification.
    assert_eq!(calls, CALLS, "every counted call must have run");
    assert_eq!(
        buf.len(),
        one_notification,
        "each call writes one notification"
    );
    assert!(one_notification > 0);

    let per_call = allocs as f64 / calls as f64;
    assert!(
        per_call < 0.01,
        "steady-state encode must not allocate: {allocs} allocations over {calls} \
         calls = {per_call:.4}/call, budget < 0.01"
    );
}

/// Sixteen distinct series through one encoder and one pre-grown buffer:
/// the first encode of each series allocates (it renders and caches the
/// path), every later encode does not.
#[test]
fn sixteen_series_allocate_only_on_first_sight() {
    let encoder = encoder();
    let events: Vec<MetricEvent> = (0..16)
        .map(|i| event(&format!("GigabitEthernet0/0/{i}"), i as f64))
        .collect();
    let mut buf = Vec::with_capacity(4096);

    let mut first_sight_calls = 0usize;
    let first_sight = count_allocations(|| {
        for e in &events {
            buf.clear();
            encoder.encode_metric(e, &mut buf).expect("encode succeeds");
            first_sight_calls += 1;
        }
    });
    assert_eq!(first_sight_calls, 16);
    assert!(
        first_sight >= 16,
        "the first encode of each series renders and caches its path, so it \
         must allocate: {first_sight} allocations over 16 new series"
    );

    let mut calls = 0usize;
    let steady = count_allocations(|| {
        for i in 0..CALLS {
            buf.clear();
            encoder
                .encode_metric(&events[i % events.len()], &mut buf)
                .expect("encode succeeds");
            calls += 1;
        }
    });

    assert_eq!(calls, CALLS, "every counted call must have run");
    assert!(!buf.is_empty());
    let per_call = steady as f64 / calls as f64;
    assert!(
        per_call < 0.01,
        "cached series must not allocate: {steady} allocations over {calls} calls \
         across 16 series = {per_call:.4}/call, budget < 0.01"
    );
}
