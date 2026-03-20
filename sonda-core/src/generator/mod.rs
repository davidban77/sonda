//! Value generators produce f64 values for each tick.
//!
//! All generators implement the `ValueGenerator` trait and are constructed
//! via `create_generator()` which returns `Box<dyn ValueGenerator>`.

pub mod constant;
// pub mod uniform;    // TODO: Phase 0 MVP
// pub mod sine;       // TODO: Phase 0 MVP
// pub mod sawtooth;   // TODO: Phase 0 MVP
// pub mod counter;    // TODO: Phase 0 MVP

/// A generator produces a single f64 value for a given tick index.
///
/// Implementations must be deterministic for a given configuration and tick.
/// Side effects are not allowed in `value()`.
pub trait ValueGenerator: Send + Sync {
    /// Produce a value for the given tick index (0-based, monotonically increasing).
    fn value(&self, tick: u64) -> f64;
}
