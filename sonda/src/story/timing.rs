//! Pure timing functions for computing when a signal crosses a threshold.
//!
//! Each supported behavior alias has a deterministic formula that computes
//! the time (in seconds) at which the signal first crosses the given
//! threshold. These functions are pure — no I/O, no side effects, no
//! allocations.
//!
//! The formulas mirror the generator math from `sonda-core`:
//!
//! | Alias | Generator | Formula |
//! |-------|-----------|---------|
//! | **flap** | Sequence (up/down) | `<` threshold: `up_duration_secs` |
//! | **saturation** | Sawtooth (baseline→ceiling, repeating) | linear interpolation |
//! | **leak** | Sawtooth (baseline→ceiling, one-shot) | same as saturation |
//! | **degradation** | Sawtooth + jitter | same as saturation |
//! | **spike_event** | Spike (baseline + magnitude pulses) | spike start or end |
//! | **steady** | Sine + jitter | **not supported** (ambiguous) |

use std::fmt;

/// Error from threshold-crossing computation.
#[derive(Debug, Clone, PartialEq)]
pub enum TimingError {
    /// The threshold falls outside the signal's output range.
    OutOfRange {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The behavior alias does not support threshold-crossing computation.
    Unsupported {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The crossing condition is ambiguous or trivially satisfied at t=0.
    Ambiguous {
        /// Human-readable description of the problem.
        message: String,
    },
}

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimingError::OutOfRange { message } => write!(f, "{message}"),
            TimingError::Unsupported { message } => write!(f, "{message}"),
            TimingError::Ambiguous { message } => write!(f, "{message}"),
        }
    }
}

/// Comparison operator parsed from an `after` clause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `<` — signal drops below threshold.
    LessThan,
    /// `>` — signal rises above threshold.
    GreaterThan,
}

impl fmt::Display for Operator {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Operator::LessThan => write!(f, "<"),
            Operator::GreaterThan => write!(f, ">"),
        }
    }
}

/// Compute the time offset (in seconds) at which a **flap** signal first
/// crosses the given threshold.
///
/// A flap signal alternates between `up_value` (default 1.0) and `down_value`
/// (default 0.0). The `<` operator detects the transition to the down state
/// (at `up_duration_secs`). The `>` operator on a standard flap (up_value=1,
/// down_value=0) is satisfied at t=0 which is ambiguous.
///
/// # Errors
///
/// - `Ambiguous` when the condition is satisfied at t=0.
/// - `OutOfRange` when the threshold is outside `[down_value, up_value]`.
pub fn flap_crossing_secs(
    op: Operator,
    threshold: f64,
    up_duration_secs: f64,
    _down_duration_secs: f64,
    up_value: f64,
    down_value: f64,
) -> Result<f64, TimingError> {
    let (lo, hi) = if up_value >= down_value {
        (down_value, up_value)
    } else {
        (up_value, down_value)
    };

    match op {
        Operator::LessThan => {
            // "< threshold" — we want the signal to drop below threshold.
            // The flap signal starts at up_value for up_duration, then drops to down_value.
            if threshold <= lo {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or below the down_value {down_value}; \
                         the flap signal never goes below it"
                    ),
                });
            }
            if threshold > hi {
                // Signal starts below threshold — satisfied at t=0.
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is above up_value {up_value}; \
                         the flap signal is always below it (satisfied at t=0)"
                    ),
                });
            }
            // The drop happens at the transition from up to down phase.
            // If down_value < threshold, that's at up_duration_secs.
            if down_value < threshold {
                Ok(up_duration_secs)
            } else {
                Err(TimingError::OutOfRange {
                    message: format!(
                        "down_value {down_value} is not less than threshold {threshold}; \
                         the flap signal never drops below {threshold}"
                    ),
                })
            }
        }
        Operator::GreaterThan => {
            // "> threshold" — we want the signal to rise above threshold.
            if threshold >= hi {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or above up_value {up_value}; \
                         the flap signal never exceeds it"
                    ),
                });
            }
            if threshold < lo {
                // Signal starts above threshold at t=0.
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is below down_value {down_value}; \
                         the flap signal is always above it (satisfied at t=0)"
                    ),
                });
            }
            // If up_value > threshold, the signal starts in the up state which
            // already satisfies the condition at t=0.
            if up_value > threshold {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "\"{} > {threshold}\" is satisfied at t=0 \
                         (starts at up_value {up_value}). \
                         Use \"<\" to detect the down event",
                        "flap_signal"
                    ),
                });
            }
            // up_value == threshold but down_value < threshold:
            // the signal returns to up_value after down phase, but up_value
            // is not strictly greater than threshold. This is not satisfiable.
            Err(TimingError::OutOfRange {
                message: format!(
                    "up_value {up_value} equals threshold {threshold}; \
                     the signal never strictly exceeds it"
                ),
            })
        }
    }
}

/// Compute the time offset (in seconds) at which a **sawtooth-based** signal
/// (saturation, leak, degradation) first crosses the given threshold.
///
/// These aliases all desugar to a sawtooth that ramps linearly from `baseline`
/// to `ceiling` over `period_secs`. The crossing time is computed via linear
/// interpolation.
///
/// # Errors
///
/// - `OutOfRange` when the threshold is outside `[baseline, ceiling]`.
/// - `Ambiguous` when the condition is trivially satisfied at t=0.
pub fn sawtooth_crossing_secs(
    op: Operator,
    threshold: f64,
    baseline: f64,
    ceiling: f64,
    period_secs: f64,
) -> Result<f64, TimingError> {
    let range = ceiling - baseline;
    if range.abs() < f64::EPSILON {
        return Err(TimingError::OutOfRange {
            message: format!(
                "baseline ({baseline}) equals ceiling ({ceiling}); \
                 the signal is constant and cannot cross any threshold"
            ),
        });
    }

    // Normalize so we always think of baseline < ceiling.
    let (lo, hi) = if baseline <= ceiling {
        (baseline, ceiling)
    } else {
        (ceiling, baseline)
    };

    match op {
        Operator::GreaterThan => {
            // "> threshold" — signal ramps from baseline toward ceiling.
            if threshold >= hi {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or above ceiling {ceiling}; \
                         the signal never exceeds it"
                    ),
                });
            }
            if threshold < lo {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is below baseline {baseline}; \
                         the signal starts above it (satisfied at t=0)"
                    ),
                });
            }
            // Linear interpolation: t = (threshold - baseline) / (ceiling - baseline) * period
            let fraction = (threshold - baseline) / range;
            Ok(fraction * period_secs)
        }
        Operator::LessThan => {
            // "< threshold" — the sawtooth starts at baseline and ramps up.
            // It only goes below baseline after a reset (which is a saturation
            // behavior). For a single ramp, the signal never goes below baseline.
            if threshold <= lo {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or below baseline {baseline}; \
                         the signal never drops below it"
                    ),
                });
            }
            if threshold > hi {
                // Signal is always below threshold (starts at baseline < threshold).
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is above ceiling {ceiling}; \
                         the signal is always below it (satisfied at t=0)"
                    ),
                });
            }
            // For a ramp from baseline to ceiling, the signal starts below
            // threshold if baseline < threshold. That means it's satisfied at t=0.
            if baseline < threshold {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "signal starts at baseline {baseline} which is already \
                         below threshold {threshold} (satisfied at t=0)"
                    ),
                });
            }
            Err(TimingError::OutOfRange {
                message: format!(
                    "the sawtooth ramps from {baseline} toward {ceiling}; \
                     it does not cross below {threshold} during the ramp"
                ),
            })
        }
    }
}

/// Compute the time offset (in seconds) at which a **spike_event** signal
/// first crosses the given threshold.
///
/// A spike_event produces `baseline` normally, then spikes to
/// `baseline + spike_height` for `spike_duration_secs` every
/// `spike_interval_secs`. The first spike starts at t=0.
///
/// - `> threshold`: satisfied at t=0 if baseline + spike_height > threshold
/// - `< threshold`: satisfied at `spike_duration_secs` when spike ends
///
/// # Errors
///
/// - `OutOfRange` when the threshold is outside the signal's range.
/// - `Ambiguous` when satisfied at t=0.
pub fn spike_crossing_secs(
    op: Operator,
    threshold: f64,
    baseline: f64,
    spike_height: f64,
    spike_duration_secs: f64,
) -> Result<f64, TimingError> {
    let peak = baseline + spike_height;
    let lo = baseline.min(peak);
    let hi = baseline.max(peak);

    match op {
        Operator::GreaterThan => {
            if threshold >= hi {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or above peak value {peak}; \
                         the signal never exceeds it"
                    ),
                });
            }
            if threshold < lo {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is below baseline {baseline}; \
                         the signal is always above it (satisfied at t=0)"
                    ),
                });
            }
            // The spike starts at t=0, so if baseline + spike_height > threshold,
            // it's satisfied immediately.
            if peak > threshold {
                // Signal starts at peak at t=0 — crossing happens at the spike start.
                // This is t=0 for the first spike.
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "\"> {threshold}\" is satisfied at t=0 (spike starts immediately \
                         at peak {peak}). Use \"<\" to detect when the spike ends"
                    ),
                });
            }
            Err(TimingError::OutOfRange {
                message: format!("peak {peak} does not exceed threshold {threshold}"),
            })
        }
        Operator::LessThan => {
            if threshold <= lo {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or below baseline {baseline}; \
                         the signal never drops below it"
                    ),
                });
            }
            if threshold > hi {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is above peak {peak}; \
                         the signal is always below it (satisfied at t=0)"
                    ),
                });
            }
            // The spike ends after spike_duration_secs, returning to baseline.
            // If baseline < threshold and peak >= threshold, the crossing
            // happens when the spike ends.
            if baseline < threshold {
                Ok(spike_duration_secs)
            } else {
                Err(TimingError::OutOfRange {
                    message: format!(
                        "baseline {baseline} is not below threshold {threshold}; \
                         the signal does not cross below it when the spike ends"
                    ),
                })
            }
        }
    }
}

/// Reject threshold-crossing computation for the **steady** behavior.
///
/// Steady desugars to a sine wave, which crosses any threshold twice per
/// period — the crossing direction is ambiguous. Stories should use a
/// different behavior or explicit `phase_offset`.
///
/// # Errors
///
/// Always returns `TimingError::Unsupported`.
pub fn steady_crossing_secs() -> Result<f64, TimingError> {
    Err(TimingError::Unsupported {
        message: "cannot compute crossing for \"steady\" behavior \
                  -- sine waves cross any threshold twice per period, \
                  making the result ambiguous. Use explicit phase_offset instead"
            .to_string(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // Flap crossing
    // -----------------------------------------------------------------------

    #[test]
    fn flap_less_than_one_returns_up_duration() {
        // Standard flap: up_value=1.0, down_value=0.0, up_duration=60s, down_duration=30s
        // "interface_oper_state < 1" -> 60s (when it drops to 0)
        let t = flap_crossing_secs(Operator::LessThan, 1.0, 60.0, 30.0, 1.0, 0.0)
            .expect("should succeed");
        assert!((t - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flap_less_than_half_returns_up_duration() {
        // "oper_state < 0.5" with up=1.0, down=0.0 -> crossing at up_duration
        let t = flap_crossing_secs(Operator::LessThan, 0.5, 10.0, 5.0, 1.0, 0.0)
            .expect("should succeed");
        assert!((t - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flap_greater_than_zero_is_ambiguous() {
        // "oper_state > 0" starts at up_value=1.0 which is > 0 at t=0
        let err = flap_crossing_secs(Operator::GreaterThan, 0.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn flap_less_than_zero_is_out_of_range() {
        // threshold=0 with down_value=0 -> signal never goes below 0
        let err = flap_crossing_secs(Operator::LessThan, 0.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn flap_threshold_above_up_value_less_than_is_ambiguous() {
        // threshold=2.0 with up_value=1.0 -> signal is always below 2.0
        let err = flap_crossing_secs(Operator::LessThan, 2.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn flap_threshold_above_up_value_greater_than_is_out_of_range() {
        // threshold=2.0 with up_value=1.0 -> signal never exceeds 2.0
        let err = flap_crossing_secs(Operator::GreaterThan, 2.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn flap_custom_values() {
        // up_value=100, down_value=50, "< 75" -> triggers at up_duration when
        // signal drops to 50
        let t = flap_crossing_secs(Operator::LessThan, 75.0, 20.0, 10.0, 100.0, 50.0)
            .expect("should succeed");
        assert!((t - 20.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Sawtooth crossing (saturation / leak / degradation)
    // -----------------------------------------------------------------------

    #[test]
    fn sawtooth_greater_than_at_midpoint() {
        // baseline=20, ceiling=85, period=120s
        // "> 70" -> (70-20)/(85-20) * 120 = 50/65 * 120 = 92.307...s
        let t = sawtooth_crossing_secs(Operator::GreaterThan, 70.0, 20.0, 85.0, 120.0)
            .expect("should succeed");
        let expected = (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert!((t - expected).abs() < 1e-9, "got {t}, expected {expected}");
    }

    #[test]
    fn sawtooth_greater_than_near_ceiling() {
        // "> 84" with baseline=20, ceiling=85, period=120
        let t = sawtooth_crossing_secs(Operator::GreaterThan, 84.0, 20.0, 85.0, 120.0)
            .expect("should succeed");
        let expected = (84.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert!((t - expected).abs() < 1e-9);
    }

    #[test]
    fn sawtooth_greater_than_at_ceiling_is_out_of_range() {
        let err = sawtooth_crossing_secs(Operator::GreaterThan, 85.0, 20.0, 85.0, 120.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn sawtooth_greater_than_below_baseline_is_ambiguous() {
        let err = sawtooth_crossing_secs(Operator::GreaterThan, 10.0, 20.0, 85.0, 120.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn sawtooth_less_than_above_ceiling_is_ambiguous() {
        // threshold > ceiling -> signal is always below
        let err = sawtooth_crossing_secs(Operator::LessThan, 100.0, 20.0, 85.0, 120.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn sawtooth_less_than_at_baseline_is_out_of_range() {
        // threshold == baseline -> signal never drops below
        let err = sawtooth_crossing_secs(Operator::LessThan, 20.0, 20.0, 85.0, 120.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn sawtooth_less_than_midpoint_is_ambiguous_at_t0() {
        // Signal starts at baseline=20 which is < 50, so "< 50" is satisfied at t=0.
        let err = sawtooth_crossing_secs(Operator::LessThan, 50.0, 20.0, 85.0, 120.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn sawtooth_equal_baseline_ceiling_is_out_of_range() {
        let err = sawtooth_crossing_secs(Operator::GreaterThan, 50.0, 50.0, 50.0, 120.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    // -----------------------------------------------------------------------
    // Spike crossing
    // -----------------------------------------------------------------------

    #[test]
    fn spike_less_than_returns_spike_duration() {
        // baseline=0, spike_height=100, spike_duration=10s
        // "< 50" -> when spike ends at t=10s, returns to baseline=0 which is < 50
        let t = spike_crossing_secs(Operator::LessThan, 50.0, 0.0, 100.0, 10.0)
            .expect("should succeed");
        assert!((t - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spike_greater_than_is_ambiguous_at_t0() {
        // Spike starts immediately at peak=100 > 50 at t=0
        let err = spike_crossing_secs(Operator::GreaterThan, 50.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn spike_less_than_at_baseline_is_out_of_range() {
        // threshold=0 with baseline=0 -> signal never drops below 0
        let err = spike_crossing_secs(Operator::LessThan, 0.0, 0.0, 100.0, 10.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn spike_greater_than_at_peak_is_out_of_range() {
        // threshold=100 with peak=100 -> signal never exceeds 100
        let err = spike_crossing_secs(Operator::GreaterThan, 100.0, 0.0, 100.0, 10.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn spike_less_than_above_peak_is_ambiguous() {
        // threshold=150 with peak=100 -> always below
        let err = spike_crossing_secs(Operator::LessThan, 150.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn spike_greater_than_below_baseline_is_ambiguous() {
        // threshold=-10 with baseline=0 -> always above
        let err = spike_crossing_secs(Operator::GreaterThan, -10.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    // -----------------------------------------------------------------------
    // Steady (always rejected)
    // -----------------------------------------------------------------------

    #[test]
    fn steady_always_unsupported() {
        let err = steady_crossing_secs().expect_err("steady should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        let msg = err.to_string();
        assert!(
            msg.contains("steady"),
            "error message should mention steady, got: {msg}"
        );
    }
}
