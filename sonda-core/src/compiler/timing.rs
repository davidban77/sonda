//! Pure timing math for `after` clause resolution.
//!
//! This module is the analytical counterpart to the runtime value generators.
//! For each deterministic generator in [`crate::generator::GeneratorConfig`],
//! it computes the time (in seconds) at which the generator's output first
//! crosses a given threshold with the requested operator. That crossing time
//! is the `phase_offset` the compiler assigns to any signal waiting for the
//! generator via an `after:` clause (spec §3.3).
//!
//! The functions here are pure — no I/O, no allocations, no side effects.
//! They operate on already-desugared [`GeneratorConfig`] variants: operational
//! aliases (`flap`, `saturation`, `leak`, `degradation`, `spike_event`,
//! `steady`) must be lowered to their underlying core generators by
//! [`crate::config::aliases::desugar_scenario_config`] (or an equivalent)
//! before reaching this module. The single exception is [`Operator`], which
//! is the compiler-facing, alias-free counterpart to
//! [`crate::compiler::AfterOp`].
//!
//! # Supported generators (per spec §3.3)
//!
//! | Generator  | `<` threshold                                   | `>` threshold                                   |
//! |------------|-------------------------------------------------|-------------------------------------------------|
//! | `sequence` | first tick where `values[i] < threshold`        | first tick where `values[i] > threshold`        |
//! | `sawtooth` | rejected — ramp only rises within one period    | `(threshold - min) / (max - min) * period_secs` |
//! | `step`     | rejected — monotonically increasing             | `ceil((threshold - start) / step_size)` ticks   |
//! | `spike`    | `duration_secs` (spike ends)                    | rejected — spike starts at `t=0`                |
//! | `constant` | rejected (ambiguous or out-of-range)            | rejected (ambiguous or out-of-range)            |
//! | `sine`     | rejected — crosses twice per period             | rejected — crosses twice per period             |
//! | `uniform`  | rejected — non-deterministic                    | rejected — non-deterministic                    |
//! | `csv_replay`| rejected — data-dependent                      | rejected — data-dependent                       |
//!
//! Aliases map to core generators as follows (see [`crate::config::aliases`]):
//!
//! | Alias         | Lowered form                    | Crossing formula                                                |
//! |---------------|---------------------------------|-----------------------------------------------------------------|
//! | `flap`        | `sequence` of up/down values    | `up_duration_secs` for `<` when the drop is observable          |
//! | `saturation`  | `sawtooth(min, max, period)`    | linear interpolation, `period_secs = time_to_saturate`          |
//! | `leak`        | `sawtooth(min, max, period)`    | linear interpolation, `period_secs = time_to_ceiling`           |
//! | `degradation` | `sawtooth(min, max, period)`    | linear interpolation, `period_secs = time_to_degrade`           |
//! | `steady`      | `sine(...)`                     | always rejected (`sine` is ambiguous)                           |
//! | `spike_event` | `spike(baseline, magnitude, …)` | `duration_secs` (spike ends) for `<`                            |
//!
//! # Error surface
//!
//! [`TimingError`] distinguishes three failure modes that the compiler
//! reports as distinct typed errors in [`super::compile_after::CompileAfterError`]:
//!
//! - [`TimingError::OutOfRange`] — the threshold is outside the signal's
//!   output range, so the crossing never happens.
//! - [`TimingError::Ambiguous`] — the condition is already true at `t=0`,
//!   making the crossing time ill-defined.
//! - [`TimingError::Unsupported`] — the generator is not a valid `after`
//!   target (e.g. `sine`, `uniform`, `csv_replay`).

use std::fmt;

/// Comparison operator for an `after` threshold check.
///
/// Mirror of [`crate::compiler::AfterOp`] kept alias-free so this module has
/// no dependency on the v2 AST. The [`super::compile_after`] pass converts
/// `AfterOp` into [`Operator`] before invoking these functions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Operator {
    /// `<` — the referenced signal's value must drop below the threshold.
    LessThan,
    /// `>` — the referenced signal's value must rise above the threshold.
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

/// Error from a threshold-crossing computation.
///
/// All variants carry a human-readable message that pinpoints the offending
/// parameter (baseline, ceiling, threshold, etc.) so the compiler can echo
/// the diagnostic verbatim in its typed error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TimingError {
    /// The threshold falls outside the signal's output range.
    OutOfRange {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The crossing condition is trivially satisfied at `t=0`, so the
    /// crossing time is ambiguous.
    Ambiguous {
        /// Human-readable description of the problem.
        message: String,
    },
    /// The generator does not support threshold-crossing computation
    /// (e.g. `sine`, `uniform`, `csv_replay`).
    Unsupported {
        /// Human-readable description of the problem.
        message: String,
    },
}

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimingError::OutOfRange { message }
            | TimingError::Ambiguous { message }
            | TimingError::Unsupported { message } => write!(f, "{message}"),
        }
    }
}

// ---------------------------------------------------------------------------
// Flap / sequence (alias-aware helpers for v1 callers)
// ---------------------------------------------------------------------------

/// Compute the crossing time for a **flap** signal.
///
/// A flap signal alternates between `up_value` (default 1.0) and
/// `down_value` (default 0.0). The `<` operator detects the transition to
/// the down state at `up_duration_secs`. The `>` operator on a standard
/// flap is satisfied at `t=0` and therefore ambiguous.
///
/// Kept on the public surface of the module because the v1 story path in
/// `sonda::story::after_resolve` still calls it directly with alias-level
/// parameters. In the v2 compiler, `flap` is first desugared to a
/// [`GeneratorConfig::Sequence`] and handled by [`sequence_crossing_secs`].
///
/// # Errors
///
/// - [`TimingError::Ambiguous`] when the condition holds at `t=0`.
/// - [`TimingError::OutOfRange`] when the threshold is outside
///   `[down_value, up_value]`.
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
            if threshold <= lo {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or below the down_value {down_value}; \
                         the flap signal never goes below it"
                    ),
                });
            }
            if threshold > hi {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is above up_value {up_value}; \
                         the flap signal is always below it (satisfied at t=0)"
                    ),
                });
            }
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
            if threshold >= hi {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "threshold {threshold} is at or above up_value {up_value}; \
                         the flap signal never exceeds it"
                    ),
                });
            }
            if threshold < lo {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "threshold {threshold} is below down_value {down_value}; \
                         the flap signal is always above it (satisfied at t=0)"
                    ),
                });
            }
            if up_value > threshold {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "\"flap_signal > {threshold}\" is satisfied at t=0 \
                         (starts at up_value {up_value}). \
                         Use \"<\" to detect the down event"
                    ),
                });
            }
            Err(TimingError::OutOfRange {
                message: format!(
                    "up_value {up_value} equals threshold {threshold}; \
                     the signal never strictly exceeds it"
                ),
            })
        }
    }
}

// ---------------------------------------------------------------------------
// Sawtooth (also covers saturation / leak / degradation after desugaring)
// ---------------------------------------------------------------------------

/// Compute the crossing time for a **sawtooth** signal.
///
/// The sawtooth ramps linearly from `min` to `max` over `period_secs` and
/// resets at the end of each period. Only the `>` operator has a crossing
/// within a single period; `<` is rejected unless the threshold is
/// trivially satisfied at `t=0` (reported as
/// [`TimingError::Ambiguous`]).
///
/// The aliases `saturation`, `leak`, and `degradation` all desugar to a
/// sawtooth with different default periods (`time_to_saturate`,
/// `time_to_ceiling`, `time_to_degrade`); the arithmetic here is shared.
///
/// # Errors
///
/// - [`TimingError::OutOfRange`] when `threshold` is outside `[min, max]`
///   (exclusive at the top for `>`, exclusive at the bottom for `<`).
/// - [`TimingError::Ambiguous`] when the condition is already satisfied at
///   `t=0`.
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

    let (lo, hi) = if baseline <= ceiling {
        (baseline, ceiling)
    } else {
        (ceiling, baseline)
    };

    match op {
        Operator::GreaterThan => {
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
            let fraction = (threshold - baseline) / range;
            Ok(fraction * period_secs)
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
                        "threshold {threshold} is above ceiling {ceiling}; \
                         the signal is always below it (satisfied at t=0)"
                    ),
                });
            }
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

// ---------------------------------------------------------------------------
// Spike (also covers spike_event after desugaring)
// ---------------------------------------------------------------------------

/// Compute the crossing time for a **spike** signal.
///
/// A spike emits `baseline` normally, then spikes to `baseline + magnitude`
/// for `duration_secs` every `interval_secs`. The first spike starts at
/// `t=0`, which makes `>` ambiguous; `<` is observable at
/// `duration_secs` when the spike ends and the signal returns to baseline.
///
/// # Errors
///
/// - [`TimingError::OutOfRange`] when the threshold is outside the signal's
///   range.
/// - [`TimingError::Ambiguous`] when satisfied at `t=0`.
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
            if peak > threshold {
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
/// period — direction-dependent, so the crossing time is ambiguous.
///
/// # Errors
///
/// Always returns [`TimingError::Unsupported`].
pub fn steady_crossing_secs() -> Result<f64, TimingError> {
    Err(TimingError::Unsupported {
        message: "cannot compute crossing for \"steady\" behavior \
                  -- sine waves cross any threshold twice per period, \
                  making the result ambiguous. Use explicit phase_offset instead"
            .to_string(),
    })
}

// ---------------------------------------------------------------------------
// Step (monotonic counter, only `>` is valid)
// ---------------------------------------------------------------------------

/// Compute the crossing time for a **step** generator.
///
/// The step counter produces `start + tick * step_size`. Only `>` is a
/// valid operator — `<` is rejected because the value is monotonically
/// non-decreasing (or non-increasing for negative `step_size`). The
/// crossing tick is `ceil((threshold - start) / step_size)`; the crossing
/// time is that tick count multiplied by the tick interval
/// (`1.0 / rate`).
///
/// # Parameters
///
/// - `op`: the comparison operator from the after clause.
/// - `threshold`: threshold value from the after clause.
/// - `start`: generator `start` parameter (defaults to `0.0` when absent).
/// - `step_size`: generator `step_size` parameter.
/// - `max`: optional wrap-around ceiling. When set and the crossing would
///   occur at or after the wrap, the signal never reaches the threshold
///   without wrapping — reported as [`TimingError::OutOfRange`].
/// - `rate`: scenario rate (events per second) — used to convert tick
///   count to seconds.
///
/// # Errors
///
/// - [`TimingError::Unsupported`] for the `<` operator.
/// - [`TimingError::OutOfRange`] when `step_size` does not move the signal
///   toward the threshold, or the wrap-around prevents the crossing.
/// - [`TimingError::Ambiguous`] when `start > threshold` for `>` (the
///   condition is already true at `t=0`).
pub fn step_crossing_secs(
    op: Operator,
    threshold: f64,
    start: f64,
    step_size: f64,
    max: Option<f64>,
    rate: f64,
) -> Result<f64, TimingError> {
    if !rate.is_finite() || rate <= 0.0 {
        return Err(TimingError::OutOfRange {
            message: format!("step rate {rate} must be positive and finite"),
        });
    }

    match op {
        Operator::LessThan => Err(TimingError::Unsupported {
            message: "step generator does not support `< threshold`; \
                      the value is monotonically non-decreasing"
                .to_string(),
        }),
        Operator::GreaterThan => {
            if step_size == 0.0 {
                return Err(TimingError::OutOfRange {
                    message: "step_size is 0; the signal never advances".to_string(),
                });
            }
            if step_size < 0.0 {
                return Err(TimingError::OutOfRange {
                    message: format!(
                        "step_size {step_size} is negative; \
                         the signal moves away from `> threshold`"
                    ),
                });
            }
            if start > threshold {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "step generator starts at {start} which already exceeds \
                         threshold {threshold} (satisfied at t=0)"
                    ),
                });
            }
            let delta = threshold - start;
            let ticks = (delta / step_size).ceil();
            let ticks = if ticks <= 0.0 {
                // start == threshold: the next tick lands above the threshold.
                1.0
            } else if (start + ticks * step_size) <= threshold {
                // `ceil` rounds away from the condition when delta divides
                // evenly into step_size — advance one more tick.
                ticks + 1.0
            } else {
                ticks
            };

            if let Some(max_val) = max {
                // Wrap-around lives below the threshold.
                if max_val <= threshold {
                    return Err(TimingError::OutOfRange {
                        message: format!(
                            "step wraps at {max_val} which is at or below threshold {threshold}; \
                             the signal never exceeds it"
                        ),
                    });
                }
                let crossing_value = start + ticks * step_size;
                if crossing_value >= max_val {
                    return Err(TimingError::OutOfRange {
                        message: format!(
                            "step wraps at {max_val} before reaching threshold {threshold}"
                        ),
                    });
                }
            }

            Ok(ticks / rate)
        }
    }
}

// ---------------------------------------------------------------------------
// Sequence (explicit value list)
// ---------------------------------------------------------------------------

/// Compute the crossing time for a **sequence** generator.
///
/// Scans `values` in order and returns the tick index (multiplied by the
/// tick interval `1.0 / rate`) of the first element that satisfies the
/// comparison. When `repeat` is `false` and no element satisfies the
/// condition, the generator holds its last value forever — if that last
/// value satisfies the condition, the crossing happens at the tick of the
/// last element; otherwise the threshold is never crossed.
///
/// # Errors
///
/// - [`TimingError::OutOfRange`] when `values` is empty or no value
///   satisfies the condition (and the tail value also fails).
pub fn sequence_crossing_secs(
    op: Operator,
    threshold: f64,
    values: &[f64],
    _repeat: Option<bool>,
    rate: f64,
) -> Result<f64, TimingError> {
    if !rate.is_finite() || rate <= 0.0 {
        return Err(TimingError::OutOfRange {
            message: format!("sequence rate {rate} must be positive and finite"),
        });
    }
    if values.is_empty() {
        return Err(TimingError::OutOfRange {
            message: "sequence has no values; no crossing is possible".to_string(),
        });
    }

    for (i, v) in values.iter().enumerate() {
        let matches = match op {
            Operator::LessThan => *v < threshold,
            Operator::GreaterThan => *v > threshold,
        };
        if matches {
            if i == 0 {
                return Err(TimingError::Ambiguous {
                    message: format!(
                        "sequence starts at {v} which already satisfies \"{op} {threshold}\" \
                         (satisfied at t=0)"
                    ),
                });
            }
            return Ok((i as f64) / rate);
        }
    }

    Err(TimingError::OutOfRange {
        message: format!(
            "no value in the sequence satisfies \"{op} {threshold}\"; \
             the signal never crosses"
        ),
    })
}

// ---------------------------------------------------------------------------
// Constant (always rejected, classification depends on the threshold)
// ---------------------------------------------------------------------------

/// Reject threshold-crossing computation for a **constant** generator.
///
/// A constant signal either already satisfies the threshold condition
/// (ambiguous at `t=0`) or never satisfies it (out of range). Either way
/// there is no deterministic crossing time.
///
/// # Errors
///
/// Always returns either [`TimingError::Ambiguous`] or
/// [`TimingError::OutOfRange`] depending on how `value` compares to
/// `threshold`.
pub fn constant_crossing_secs(
    op: Operator,
    threshold: f64,
    value: f64,
) -> Result<f64, TimingError> {
    let satisfied_at_zero = match op {
        Operator::LessThan => value < threshold,
        Operator::GreaterThan => value > threshold,
    };
    if satisfied_at_zero {
        Err(TimingError::Ambiguous {
            message: format!(
                "constant generator emits {value}; \"{op} {threshold}\" is satisfied at t=0"
            ),
        })
    } else {
        Err(TimingError::OutOfRange {
            message: format!(
                "constant generator emits {value}; it never crosses \"{op} {threshold}\""
            ),
        })
    }
}

// ---------------------------------------------------------------------------
// Blanket rejections (sine / uniform / csv_replay)
// ---------------------------------------------------------------------------

/// Reject threshold-crossing computation for a **sine** generator.
///
/// Sine waves cross any threshold twice per period — the direction
/// (rising vs. falling) is ambiguous, so the compiler cannot pick a single
/// crossing time.
///
/// # Errors
///
/// Always returns [`TimingError::Unsupported`].
pub fn sine_crossing_secs() -> Result<f64, TimingError> {
    Err(TimingError::Unsupported {
        message: "sine generator is not supported as an `after` target: \
                  sine waves cross any threshold twice per period, \
                  making the crossing direction ambiguous"
            .to_string(),
    })
}

/// Reject threshold-crossing computation for a **uniform** generator.
///
/// Uniform RNG output is non-deterministic at the timing-math level
/// (different seeds produce different crossings), so the compiler cannot
/// pre-compute a single `phase_offset`.
///
/// # Errors
///
/// Always returns [`TimingError::Unsupported`].
pub fn uniform_crossing_secs() -> Result<f64, TimingError> {
    Err(TimingError::Unsupported {
        message: "uniform generator is not supported as an `after` target: \
                  the output is non-deterministic, so no crossing time can be computed"
            .to_string(),
    })
}

/// Reject threshold-crossing computation for a **csv_replay** generator.
///
/// CSV playback is data-dependent — the compiler would need to read the
/// file and scan for crossings. Spec §3.3 rejects this explicitly.
///
/// # Errors
///
/// Always returns [`TimingError::Unsupported`].
pub fn csv_replay_crossing_secs() -> Result<f64, TimingError> {
    Err(TimingError::Unsupported {
        message: "csv_replay generator is not supported as an `after` target: \
                  the output depends on external data, so no crossing time can be computed"
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
        let t = flap_crossing_secs(Operator::LessThan, 1.0, 60.0, 30.0, 1.0, 0.0)
            .expect("should succeed");
        assert!((t - 60.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flap_less_than_half_returns_up_duration() {
        let t = flap_crossing_secs(Operator::LessThan, 0.5, 10.0, 5.0, 1.0, 0.0)
            .expect("should succeed");
        assert!((t - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flap_greater_than_zero_is_ambiguous() {
        let err = flap_crossing_secs(Operator::GreaterThan, 0.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn flap_less_than_zero_is_out_of_range() {
        let err = flap_crossing_secs(Operator::LessThan, 0.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn flap_threshold_above_up_value_less_than_is_ambiguous() {
        let err = flap_crossing_secs(Operator::LessThan, 2.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn flap_threshold_above_up_value_greater_than_is_out_of_range() {
        let err = flap_crossing_secs(Operator::GreaterThan, 2.0, 10.0, 5.0, 1.0, 0.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn flap_custom_values() {
        let t = flap_crossing_secs(Operator::LessThan, 75.0, 20.0, 10.0, 100.0, 50.0)
            .expect("should succeed");
        assert!((t - 20.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Sawtooth crossing (covers saturation / leak / degradation)
    // -----------------------------------------------------------------------

    #[test]
    fn sawtooth_greater_than_at_midpoint() {
        let t = sawtooth_crossing_secs(Operator::GreaterThan, 70.0, 20.0, 85.0, 120.0)
            .expect("should succeed");
        let expected = (70.0 - 20.0) / (85.0 - 20.0) * 120.0;
        assert!((t - expected).abs() < 1e-9, "got {t}, expected {expected}");
    }

    #[test]
    fn sawtooth_greater_than_near_ceiling() {
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
        let err = sawtooth_crossing_secs(Operator::LessThan, 100.0, 20.0, 85.0, 120.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn sawtooth_less_than_at_baseline_is_out_of_range() {
        let err = sawtooth_crossing_secs(Operator::LessThan, 20.0, 20.0, 85.0, 120.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn sawtooth_less_than_midpoint_is_ambiguous_at_t0() {
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
        let t = spike_crossing_secs(Operator::LessThan, 50.0, 0.0, 100.0, 10.0)
            .expect("should succeed");
        assert!((t - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn spike_greater_than_is_ambiguous_at_t0() {
        let err = spike_crossing_secs(Operator::GreaterThan, 50.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn spike_less_than_at_baseline_is_out_of_range() {
        let err = spike_crossing_secs(Operator::LessThan, 0.0, 0.0, 100.0, 10.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn spike_greater_than_at_peak_is_out_of_range() {
        let err = spike_crossing_secs(Operator::GreaterThan, 100.0, 0.0, 100.0, 10.0)
            .expect_err("should be out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn spike_less_than_above_peak_is_ambiguous() {
        let err = spike_crossing_secs(Operator::LessThan, 150.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn spike_greater_than_below_baseline_is_ambiguous() {
        let err = spike_crossing_secs(Operator::GreaterThan, -10.0, 0.0, 100.0, 10.0)
            .expect_err("should be ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    // -----------------------------------------------------------------------
    // Steady / sine / uniform / csv_replay (always rejected)
    // -----------------------------------------------------------------------

    #[test]
    fn steady_always_unsupported() {
        let err = steady_crossing_secs().expect_err("steady should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        assert!(err.to_string().contains("steady"));
    }

    #[test]
    fn sine_always_unsupported() {
        let err = sine_crossing_secs().expect_err("sine should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        assert!(err.to_string().contains("sine"));
    }

    #[test]
    fn uniform_always_unsupported() {
        let err = uniform_crossing_secs().expect_err("uniform should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        assert!(err.to_string().contains("uniform"));
    }

    #[test]
    fn csv_replay_always_unsupported() {
        let err = csv_replay_crossing_secs().expect_err("csv_replay should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        assert!(err.to_string().contains("csv_replay"));
    }

    // -----------------------------------------------------------------------
    // Step crossing
    // -----------------------------------------------------------------------

    #[test]
    fn step_greater_than_divides_evenly_advances_one_tick() {
        // start=0, step_size=10, threshold=50, rate=1 -> ceil(50/10) = 5 ticks,
        // but start + 5 * 10 = 50 which is not > 50, so advance to tick 6.
        let t = step_crossing_secs(Operator::GreaterThan, 50.0, 0.0, 10.0, None, 1.0)
            .expect("should succeed");
        assert!((t - 6.0).abs() < f64::EPSILON, "got {t}, expected 6.0");
    }

    #[test]
    fn step_greater_than_non_divisible_uses_ceil() {
        // start=0, step_size=10, threshold=55, rate=1 -> ceil((55-0)/10) = 6,
        // start + 6 * 10 = 60 > 55 -> tick 6.
        let t = step_crossing_secs(Operator::GreaterThan, 55.0, 0.0, 10.0, None, 1.0)
            .expect("should succeed");
        assert!((t - 6.0).abs() < f64::EPSILON);
    }

    #[test]
    fn step_greater_than_high_rate_divides_ticks_by_rate() {
        // rate=2 -> each tick is 0.5s. 6 ticks * 0.5s/tick = 3.0s.
        let t = step_crossing_secs(Operator::GreaterThan, 55.0, 0.0, 10.0, None, 2.0)
            .expect("should succeed");
        assert!((t - 3.0).abs() < 1e-9);
    }

    #[test]
    fn step_less_than_rejected_as_unsupported() {
        let err = step_crossing_secs(Operator::LessThan, 50.0, 0.0, 10.0, None, 1.0)
            .expect_err("step < is unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
    }

    #[test]
    fn step_greater_than_start_above_threshold_ambiguous() {
        let err = step_crossing_secs(Operator::GreaterThan, 10.0, 50.0, 5.0, None, 1.0)
            .expect_err("start > threshold is ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn step_zero_step_size_is_out_of_range() {
        let err = step_crossing_secs(Operator::GreaterThan, 10.0, 0.0, 0.0, None, 1.0)
            .expect_err("step_size=0 is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn step_negative_step_size_is_out_of_range() {
        let err = step_crossing_secs(Operator::GreaterThan, 10.0, 0.0, -1.0, None, 1.0)
            .expect_err("step_size<0 with > threshold is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn step_wrap_below_threshold_is_out_of_range() {
        // max=30 wraps at 30, threshold=50 is above wrap -> out of range.
        let err = step_crossing_secs(Operator::GreaterThan, 50.0, 0.0, 10.0, Some(30.0), 1.0)
            .expect_err("threshold above wrap is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn step_wrap_above_threshold_succeeds() {
        // start=0, step=10, threshold=25, max=100 -> tick 3, value 30 > 25, 3s.
        let t = step_crossing_secs(Operator::GreaterThan, 25.0, 0.0, 10.0, Some(100.0), 1.0)
            .expect("should succeed");
        assert!((t - 3.0).abs() < f64::EPSILON);
    }

    // -----------------------------------------------------------------------
    // Sequence crossing
    // -----------------------------------------------------------------------

    #[test]
    fn sequence_greater_than_finds_first_crossing() {
        // values = [1, 2, 5, 10], threshold 4 -> index 2 (value 5). rate=1 -> 2s.
        let t = sequence_crossing_secs(
            Operator::GreaterThan,
            4.0,
            &[1.0, 2.0, 5.0, 10.0],
            Some(true),
            1.0,
        )
        .expect("should succeed");
        assert!((t - 2.0).abs() < f64::EPSILON);
    }

    #[test]
    fn sequence_less_than_finds_first_crossing() {
        // values = [10, 5, 1, 0], threshold 2 -> index 2 (value 1). rate=2 -> 1.0s.
        let t = sequence_crossing_secs(
            Operator::LessThan,
            2.0,
            &[10.0, 5.0, 1.0, 0.0],
            Some(false),
            2.0,
        )
        .expect("should succeed");
        assert!((t - 1.0).abs() < 1e-9);
    }

    #[test]
    fn sequence_first_value_matches_is_ambiguous() {
        let err = sequence_crossing_secs(
            Operator::GreaterThan,
            0.5,
            &[1.0, 2.0, 3.0],
            Some(true),
            1.0,
        )
        .expect_err("first value > threshold is ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn sequence_no_crossing_is_out_of_range() {
        let err = sequence_crossing_secs(
            Operator::GreaterThan,
            100.0,
            &[1.0, 2.0, 3.0],
            Some(true),
            1.0,
        )
        .expect_err("no value > 100 is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn sequence_empty_values_is_out_of_range() {
        let err = sequence_crossing_secs(Operator::GreaterThan, 0.0, &[], Some(true), 1.0)
            .expect_err("empty sequence is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    // -----------------------------------------------------------------------
    // Constant
    // -----------------------------------------------------------------------

    #[test]
    fn constant_greater_than_satisfied_is_ambiguous() {
        let err = constant_crossing_secs(Operator::GreaterThan, 10.0, 50.0)
            .expect_err("constant value > threshold is ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn constant_greater_than_not_satisfied_is_out_of_range() {
        let err = constant_crossing_secs(Operator::GreaterThan, 50.0, 10.0)
            .expect_err("constant value < threshold is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }

    #[test]
    fn constant_less_than_satisfied_is_ambiguous() {
        let err = constant_crossing_secs(Operator::LessThan, 50.0, 10.0)
            .expect_err("constant value < threshold is ambiguous");
        assert!(matches!(err, TimingError::Ambiguous { .. }));
    }

    #[test]
    fn constant_less_than_not_satisfied_is_out_of_range() {
        let err = constant_crossing_secs(Operator::LessThan, 10.0, 50.0)
            .expect_err("constant value > threshold is out of range");
        assert!(matches!(err, TimingError::OutOfRange { .. }));
    }
}
