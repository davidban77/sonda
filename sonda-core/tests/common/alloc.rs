//! A per-thread allocation counter for the allocation-budget tests.
//!
//! A test binary includes this file with `#[path = "common/alloc.rs"] mod
//! counting;` (the `common` module is compiled only with `config`) and installs
//! `#[global_allocator] static ALLOCATOR: counting::CountingAlloc = counting::CountingAlloc;`.
//! The counter is thread-local, so only allocations made on the measuring
//! thread inside a counted window are counted; other threads cannot add to it.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

thread_local! {
    // `const` initialisers: reading these never allocates, so the allocator
    // below can touch them.
    static COUNTING: Cell<bool> = const { Cell::new(false) };
    static ALLOCS: Cell<usize> = const { Cell::new(0) };
}

/// The system allocator, counting this thread's allocations while a
/// [`count_allocations`] window is open.
pub struct CountingAlloc;

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

/// Run `body` with allocation counting on for this thread and return its
/// result with the allocations it made on this thread.
pub fn count_allocations<T>(body: impl FnOnce() -> T) -> (T, usize) {
    ALLOCS.with(|n| n.set(0));
    COUNTING.with(|c| c.set(true));
    let out = body();
    COUNTING.with(|c| c.set(false));
    (out, ALLOCS.with(Cell::get))
}
