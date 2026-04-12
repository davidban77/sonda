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
    /// A duration parameter on an operational alias (e.g. `flap.up_duration`
    /// or `saturation.time_to_saturate`) failed to parse.
    ///
    /// Surfaced as a distinct variant — rather than folded into
    /// [`TimingError::OutOfRange`] — so the compiler can map it to
    /// [`super::compile_after::CompileAfterError::InvalidDuration`] with the
    /// offending field name preserved, matching how top-level duration
    /// fields like `after.delay` and `phase_offset` are reported.
    InvalidDuration {
        /// Which alias parameter carried the bad value
        /// (e.g. `"flap.up_duration"`).
        field: &'static str,
        /// The offending string as written.
        input: String,
        /// The underlying parse error message.
        reason: String,
    },
}

impl fmt::Display for TimingError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TimingError::OutOfRange { message }
            | TimingError::Ambiguous { message }
            | TimingError::Unsupported { message } => write!(f, "{message}"),
            TimingError::InvalidDuration {
                field,
                input,
                reason,
            } => write!(f, "invalid duration '{input}' in {field}: {reason}"),
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
/// - `max`: optional wrap-around ceiling. The runtime only wraps when
///   `max > start`; a `max` value at or below `start` is inactive and
///   growth is unbounded, so this function mirrors that behaviour. When
///   `max > start` and the crossing would occur at or after the wrap, the
///   signal never reaches the threshold without wrapping — reported as
///   [`TimingError::OutOfRange`].
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
                // The runtime (see StepGenerator::value) only wraps when
                // `max_val > start`. If `max_val <= start` the configured
                // max is inactive — growth is unbounded and the normal
                // step math below applies unchanged.
                if max_val > start {
                    // Wrap-around lives below the threshold.
                    if max_val <= threshold {
                        return Err(TimingError::OutOfRange {
                            message: format!(
                                "step wraps at {max_val} which is at or below threshold \
                                 {threshold}; the signal never exceeds it"
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
/// Scans `values` in order once and returns the tick index (multiplied by
/// the tick interval `1.0 / rate`) of the first element that satisfies
/// the comparison. If no element matches, returns
/// [`TimingError::OutOfRange`].
///
/// # Why `repeat` is ignored
///
/// The `repeat` flag is part of the generator's runtime contract — it
/// controls what the generator emits *after* the sequence is exhausted —
/// but it does not change which values are scanned for a crossing:
///
/// - If `repeat == true`, the sequence cycles forever. The same set of
///   values is re-emitted on every cycle, so if none of them crosses the
///   threshold in one pass, none ever will.
/// - If `repeat == false`, the runtime holds the last value indefinitely
///   after the sequence ends. "Holding" the tail is not a *crossing* — it
///   is at most a steady-state satisfaction — so the crossing-time math
///   still considers only the explicit values listed in `values`.
///
/// Either way the compiler's answer is determined by the first matching
/// element inside `values`. A parameter name is retained on the signature
/// to keep the call sites in `compile_after::crossing_secs` uniform with
/// the [`GeneratorConfig::Sequence`] layout.
///
/// # Errors
///
/// - [`TimingError::OutOfRange`] when `values` is empty or no element
///   satisfies the comparison.
/// - [`TimingError::Ambiguous`] when `values[0]` already satisfies the
///   comparison (the crossing happens at `t=0`).
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

    use rstest::rstest;

    /// Expected outcome from a crossing computation.
    ///
    /// Collapses the three-way outcome (numeric result, typed error variant)
    /// into a single value so rstest tables can mix success and failure
    /// cases on equal footing. The float tolerance (`1e-9`) is the tightest
    /// that survives the existing regression fixtures and matches the
    /// `sawtooth` / `high-rate step` precision bound used by the v1 tests.
    #[derive(Debug, Clone, Copy)]
    enum Expect {
        /// Result must be `Ok(v)` with `|actual - v| < 1e-9`.
        Ok(f64),
        /// Result must be `Err(TimingError::Ambiguous { .. })`.
        Ambiguous,
        /// Result must be `Err(TimingError::OutOfRange { .. })`.
        OutOfRange,
        /// Result must be `Err(TimingError::Unsupported { .. })`.
        Unsupported,
    }

    /// Assert that `result` matches the [`Expect`] outcome.
    ///
    /// Centralizes the float-tolerance and `matches!` checks so each
    /// `#[rstest]` table stays compact.
    #[track_caller]
    fn assert_outcome(result: Result<f64, TimingError>, expect: Expect) {
        match (result, expect) {
            (Ok(actual), Expect::Ok(want)) => {
                assert!(
                    (actual - want).abs() < 1e-9,
                    "expected Ok({want}), got Ok({actual})"
                );
            }
            (Err(TimingError::Ambiguous { .. }), Expect::Ambiguous) => {}
            (Err(TimingError::OutOfRange { .. }), Expect::OutOfRange) => {}
            (Err(TimingError::Unsupported { .. }), Expect::Unsupported) => {}
            (actual, expect) => {
                panic!("expected {expect:?}, got {actual:?}");
            }
        }
    }

    // -----------------------------------------------------------------------
    // Flap crossing
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    #[case::less_than_one_returns_up_duration(                Operator::LessThan,    1.0,  60.0, 30.0, 1.0,   0.0,  Expect::Ok(60.0))]
    #[case::less_than_half_returns_up_duration(               Operator::LessThan,    0.5,  10.0,  5.0, 1.0,   0.0,  Expect::Ok(10.0))]
    #[case::greater_than_zero_is_ambiguous(                   Operator::GreaterThan, 0.0,  10.0,  5.0, 1.0,   0.0,  Expect::Ambiguous)]
    #[case::less_than_zero_is_out_of_range(                   Operator::LessThan,    0.0,  10.0,  5.0, 1.0,   0.0,  Expect::OutOfRange)]
    #[case::threshold_above_up_value_less_than_is_ambiguous(  Operator::LessThan,    2.0,  10.0,  5.0, 1.0,   0.0,  Expect::Ambiguous)]
    #[case::threshold_above_up_value_greater_than_out_of_range(Operator::GreaterThan, 2.0,  10.0,  5.0, 1.0,   0.0,  Expect::OutOfRange)]
    #[case::custom_values(                                    Operator::LessThan,    75.0, 20.0, 10.0, 100.0, 50.0, Expect::Ok(20.0))]
    fn flap_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] up_duration: f64,
        #[case] down_duration: f64,
        #[case] up_value: f64,
        #[case] down_value: f64,
        #[case] expect: Expect,
    ) {
        let result = flap_crossing_secs(
            op,
            threshold,
            up_duration,
            down_duration,
            up_value,
            down_value,
        );
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Sawtooth crossing (covers saturation / leak / degradation)
    //
    // Midpoint and near-ceiling cases use the analytical formula
    // `(threshold - baseline) / (ceiling - baseline) * period`.
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    #[case::greater_than_at_midpoint(             Operator::GreaterThan, 70.0,  20.0, 85.0, 120.0, Expect::Ok((70.0 - 20.0) / (85.0 - 20.0) * 120.0))]
    #[case::greater_than_near_ceiling(            Operator::GreaterThan, 84.0,  20.0, 85.0, 120.0, Expect::Ok((84.0 - 20.0) / (85.0 - 20.0) * 120.0))]
    #[case::greater_than_at_ceiling_out_of_range( Operator::GreaterThan, 85.0,  20.0, 85.0, 120.0, Expect::OutOfRange)]
    #[case::greater_than_below_baseline_ambiguous(Operator::GreaterThan, 10.0,  20.0, 85.0, 120.0, Expect::Ambiguous)]
    #[case::less_than_above_ceiling_ambiguous(    Operator::LessThan,    100.0, 20.0, 85.0, 120.0, Expect::Ambiguous)]
    #[case::less_than_at_baseline_out_of_range(   Operator::LessThan,    20.0,  20.0, 85.0, 120.0, Expect::OutOfRange)]
    #[case::less_than_midpoint_ambiguous_at_t0(   Operator::LessThan,    50.0,  20.0, 85.0, 120.0, Expect::Ambiguous)]
    #[case::equal_baseline_ceiling_out_of_range(  Operator::GreaterThan, 50.0,  50.0, 50.0, 120.0, Expect::OutOfRange)]
    fn sawtooth_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] baseline: f64,
        #[case] ceiling: f64,
        #[case] period: f64,
        #[case] expect: Expect,
    ) {
        let result = sawtooth_crossing_secs(op, threshold, baseline, ceiling, period);
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Spike crossing
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    #[case::less_than_returns_spike_duration(    Operator::LessThan,     50.0,   0.0, 100.0, 10.0, Expect::Ok(10.0))]
    #[case::greater_than_ambiguous_at_t0(        Operator::GreaterThan,  50.0,   0.0, 100.0, 10.0, Expect::Ambiguous)]
    #[case::less_than_at_baseline_out_of_range(  Operator::LessThan,      0.0,   0.0, 100.0, 10.0, Expect::OutOfRange)]
    #[case::greater_than_at_peak_out_of_range(   Operator::GreaterThan, 100.0,   0.0, 100.0, 10.0, Expect::OutOfRange)]
    #[case::less_than_above_peak_ambiguous(      Operator::LessThan,    150.0,   0.0, 100.0, 10.0, Expect::Ambiguous)]
    #[case::greater_than_below_baseline_ambiguous(Operator::GreaterThan, -10.0,  0.0, 100.0, 10.0, Expect::Ambiguous)]
    fn spike_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] baseline: f64,
        #[case] peak: f64,
        #[case] spike_duration: f64,
        #[case] expect: Expect,
    ) {
        let result = spike_crossing_secs(op, threshold, baseline, peak, spike_duration);
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Step crossing
    //
    // `max` is encoded as an `Option<f64>`; the table uses `None` to
    // represent the unbounded case and `Some(v)` for explicit wraps. One
    // case (`inactive_max`) exercises the StepGenerator-runtime quirk
    // where `max <= start` disables wrap entirely.
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    // start=0, step_size=10, threshold=50, rate=1 -> ceil(50/10) = 5 ticks,
    // but 5*10 = 50 is not > 50 so we bump to tick 6.
    #[case::divides_evenly_advances_one_tick(Operator::GreaterThan, 50.0,  0.0,  10.0, None,        1.0, Expect::Ok(6.0))]
    // ceil((55-0)/10) = 6 ticks, 6*10 = 60 > 55.
    #[case::non_divisible_uses_ceil(         Operator::GreaterThan, 55.0,  0.0,  10.0, None,        1.0, Expect::Ok(6.0))]
    // rate=2 -> each tick is 0.5s, so 6 ticks * 0.5s = 3.0s.
    #[case::high_rate_divides_ticks_by_rate( Operator::GreaterThan, 55.0,  0.0,  10.0, None,        2.0, Expect::Ok(3.0))]
    #[case::less_than_unsupported(           Operator::LessThan,    50.0,  0.0,  10.0, None,        1.0, Expect::Unsupported)]
    #[case::start_above_threshold_ambiguous( Operator::GreaterThan, 10.0, 50.0,   5.0, None,        1.0, Expect::Ambiguous)]
    #[case::zero_step_size_out_of_range(     Operator::GreaterThan, 10.0,  0.0,   0.0, None,        1.0, Expect::OutOfRange)]
    #[case::negative_step_size_out_of_range( Operator::GreaterThan, 10.0,  0.0,  -1.0, None,        1.0, Expect::OutOfRange)]
    // max=30 wraps at 30, threshold=50 is above the wrap.
    #[case::wrap_below_threshold_out_of_range(Operator::GreaterThan, 50.0, 0.0,  10.0, Some(30.0),  1.0, Expect::OutOfRange)]
    // tick 3, value 30 > 25.
    #[case::wrap_above_threshold_succeeds(    Operator::GreaterThan, 25.0, 0.0,  10.0, Some(100.0), 1.0, Expect::Ok(3.0))]
    // max <= start means wrap is inactive at runtime; threshold 50 must
    // still be reachable via unbounded step growth (same shape as the
    // divisible case above) -> 6 ticks at rate=1 -> 6.0s.
    #[case::inactive_max(                     Operator::GreaterThan, 50.0, 0.0,  10.0, Some(-5.0),  1.0, Expect::Ok(6.0))]
    fn step_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] start: f64,
        #[case] step_size: f64,
        #[case] max: Option<f64>,
        #[case] rate: f64,
        #[case] expect: Expect,
    ) {
        let result = step_crossing_secs(op, threshold, start, step_size, max, rate);
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Sequence crossing
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    // values = [1, 2, 5, 10], threshold 4 -> index 2 (value 5). rate=1 -> 2s.
    #[case::greater_than_finds_first_crossing(Operator::GreaterThan,   4.0, &[1.0, 2.0, 5.0, 10.0], Some(true),  1.0, Expect::Ok(2.0))]
    // values = [10, 5, 1, 0], threshold 2 -> index 2 (value 1). rate=2 -> 1.0s.
    #[case::less_than_finds_first_crossing(   Operator::LessThan,      2.0, &[10.0, 5.0, 1.0, 0.0], Some(false), 2.0, Expect::Ok(1.0))]
    #[case::first_value_matches_ambiguous(    Operator::GreaterThan,   0.5, &[1.0, 2.0, 3.0],       Some(true),  1.0, Expect::Ambiguous)]
    #[case::no_crossing_out_of_range(         Operator::GreaterThan, 100.0, &[1.0, 2.0, 3.0],       Some(true),  1.0, Expect::OutOfRange)]
    #[case::empty_values_out_of_range(        Operator::GreaterThan,   0.0, &[],                    Some(true),  1.0, Expect::OutOfRange)]
    fn sequence_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] values: &[f64],
        #[case] repeat: Option<bool>,
        #[case] rate: f64,
        #[case] expect: Expect,
    ) {
        let result = sequence_crossing_secs(op, threshold, values, repeat, rate);
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Constant
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    #[case::greater_than_satisfied_ambiguous(    Operator::GreaterThan, 10.0, 50.0, Expect::Ambiguous)]
    #[case::greater_than_not_satisfied_out_of_range(Operator::GreaterThan, 50.0, 10.0, Expect::OutOfRange)]
    #[case::less_than_satisfied_ambiguous(       Operator::LessThan,    50.0, 10.0, Expect::Ambiguous)]
    #[case::less_than_not_satisfied_out_of_range(Operator::LessThan,    10.0, 50.0, Expect::OutOfRange)]
    fn constant_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] value: f64,
        #[case] expect: Expect,
    ) {
        let result = constant_crossing_secs(op, threshold, value);
        assert_outcome(result, expect);
    }

    // -----------------------------------------------------------------------
    // Unsupported-by-nature generators (steady, sine, uniform, csv_replay).
    //
    // Each is rejected regardless of inputs — they take no parameters at
    // all at this layer — so the table keys on the generator's name and
    // asserts both the [`TimingError::Unsupported`] variant and that the
    // rendered message mentions the generator so compiler diagnostics
    // stay identifiable.
    // -----------------------------------------------------------------------

    #[rustfmt::skip]
    #[rstest]
    #[case::steady(    "steady",     steady_crossing_secs     as fn() -> Result<f64, TimingError>)]
    #[case::sine(      "sine",       sine_crossing_secs       as fn() -> Result<f64, TimingError>)]
    #[case::uniform(   "uniform",    uniform_crossing_secs    as fn() -> Result<f64, TimingError>)]
    #[case::csv_replay("csv_replay", csv_replay_crossing_secs as fn() -> Result<f64, TimingError>)]
    fn always_unsupported(#[case] name: &str, #[case] f: fn() -> Result<f64, TimingError>) {
        let err = f().expect_err("generator should be unsupported");
        assert!(matches!(err, TimingError::Unsupported { .. }));
        assert!(
            err.to_string().contains(name),
            "error message should mention '{name}', got: {err}"
        );
    }
}
