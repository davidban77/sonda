//! Scheduling: rate control, duration, gap windows, burst windows.
//!
//! The scheduler controls *when* events are emitted. It does not know
//! *what* is being emitted — that is the generator and encoder's job.

// pub mod runner;  // TODO: Phase 0 MVP

use std::time::Duration;

/// Configuration for a gap window (intentional silent period).
#[derive(Debug, Clone)]
pub struct GapWindow {
    /// How often a gap occurs (e.g., every 2 minutes).
    pub every: Duration,
    /// How long the gap lasts (e.g., 20 seconds).
    pub duration: Duration,
}

/// Configuration for a burst window (high-rate period).
#[derive(Debug, Clone)]
pub struct BurstWindow {
    /// How often a burst occurs.
    pub every: Duration,
    /// How long the burst lasts.
    pub duration: Duration,
    /// Rate multiplier during the burst.
    pub multiplier: f64,
}

/// Schedule configuration for a scenario.
#[derive(Debug, Clone)]
pub struct Schedule {
    /// Target events per second.
    pub rate: f64,
    /// Total run duration. None means run indefinitely.
    pub duration: Option<Duration>,
    /// Optional recurring gap window.
    pub gap: Option<GapWindow>,
    /// Optional recurring burst window (post-MVP).
    pub burst: Option<BurstWindow>,
}
