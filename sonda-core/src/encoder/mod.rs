//! Encoders serialize telemetry events into wire format bytes.
//!
//! All encoders implement the `Encoder` trait. They write into a caller-provided
//! `Vec<u8>` to avoid per-event allocations.

// pub mod prometheus;  // TODO: Phase 0 MVP

use crate::model::metric::MetricEvent;

/// Encodes telemetry events into a specific wire format.
///
/// Implementations should pre-build any invariant content (label prefixes,
/// metric name validation) at construction time.
pub trait Encoder: Send + Sync {
    /// Encode a metric event into the provided buffer.
    fn encode_metric(
        &self,
        event: &MetricEvent,
        buf: &mut Vec<u8>,
    ) -> Result<(), crate::SondaError>;
}
