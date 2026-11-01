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
//! | `flap`        | `sequence` of up/down values    | first change into the state the predicate holds in, after phase |
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
/// A flap signal alternates between `up_value` (default 1.0) for
/// `up_duration_secs` and `down_value` (default 0.0) for
/// `down_duration_secs`, starting `phase_secs` into that cycle (taken modulo
/// the cycle; a negative or non-finite phase reads as 0). "Up" and "down"
/// are states, not directions: `down_value` may be above `up_value`, as it
/// is for the `oper_state` and `admin_state` enums (up 1, down 2).
///
/// The crossing is the first instant `signal op threshold` becomes true,
/// for either orientation. The predicate is evaluated on each of the two
/// values:
///
/// - It holds in exactly one state, and the phase starts the signal in the
///   other: the crossing is the first change into the state it holds in,
///   `up_duration_secs - phase` from the up state, `up_duration_secs +
///   down_duration_secs - phase` from the down state.
/// - It holds in exactly one state, and the signal starts in that state: it
///   is satisfied at `t=0`.
/// - It holds in both states: it is satisfied at `t=0`, always.
/// - It holds in neither: it never becomes true.
///
/// The `after:` resolution in [`super::compile_after`] calls this with the
/// alias parameters of an undesugared `flap`.
///
/// # Errors
///
/// - [`TimingError::Ambiguous`] when the condition holds at `t=0`; the
///   message names the state the signal starts in and its value.
/// - [`TimingError::OutOfRange`] when the condition holds for neither value;
///   the message names both values and the threshold.
pub fn flap_crossing_secs(
    op: Operator,
    threshold: f64,
    up_duration_secs: f64,
    down_duration_secs: f64,
    up_value: f64,
    down_value: f64,
    phase_secs: f64,
) -> Result<f64, TimingError> {
    let cycle_secs = up_duration_secs + down_duration_secs;
    let into_cycle = if cycle_secs > 0.0 && phase_secs.is_finite() && phase_secs > 0.0 {
        phase_secs.rem_euclid(cycle_secs)
    } else {
        0.0
    };
    let starts_up = into_cycle == 0.0 || into_cycle < up_duration_secs;

    let holds = |value: f64| match op {
        Operator::LessThan => value < threshold,
        Operator::GreaterThan => value > threshold,
    };
    let (side, other_side) = match op {
        Operator::LessThan => ("below", "at or above"),
        Operator::GreaterThan => ("above", "at or below"),
    };

    let (up_holds, down_holds) = (holds(up_value), holds(down_value));
    let starts_where_it_holds = if starts_up { up_holds } else { down_holds };

    match (up_holds, down_holds) {
        (false, false) => Err(TimingError::OutOfRange {
            message: format!(
                "up_value {up_value} and down_value {down_value} are both {other_side} \
                 threshold {threshold}; the flap signal is never {side} it"
            ),
        }),
        (true, true) => Err(TimingError::Ambiguous {
            message: format!(
                "up_value {up_value} and down_value {down_value} are both {side} \
                 threshold {threshold}; the flap signal is always {side} it \
                 (satisfied at t=0)"
            ),
        }),
        _ if starts_where_it_holds => {
            let (state, field, value, other_state, other_field, other_value) = if starts_up {
                ("up", "up_value", up_value, "down", "down_value", down_value)
            } else {
                ("down", "down_value", down_value, "up", "up_value", up_value)
            };
            let start = if into_cycle > 0.0 {
                format!("the phase starts it {into_cycle}s into the cycle, in the {state} state")
            } else {
                format!("it starts in the {state} state")
            };
            let opposite = match op {
                Operator::LessThan => Operator::GreaterThan,
                Operator::GreaterThan => Operator::LessThan,
            };
            // The predicate fails on the other value, so the opposite operator
            // holds there unless that value equals the threshold.
            let hint = if other_value != threshold {
                format!(
                    ". Use \"{opposite}\" to detect the change to the {other_state} state \
                     ({other_field} {other_value})"
                )
            } else {
                String::new()
            };
            Err(TimingError::Ambiguous {
                message: format!(
                    "\"flap_signal {op} {threshold}\" is satisfied at t=0 \
                     ({start} at {field} {value}){hint}"
                ),
            })
        }
        _ if starts_up => Ok(up_duration_secs - into_cycle),
        _ => Ok(cycle_secs - into_cycle),
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
    #[case::less_than_one_returns_up_duration(                Operator::LessThan,    1.0,  60.0, 30.0, (1.0, 0.0),   0.0,   Expect::Ok(60.0))]
    #[case::less_than_half_returns_up_duration(               Operator::LessThan,    0.5,  10.0,  5.0, (1.0, 0.0),   0.0,   Expect::Ok(10.0))]
    #[case::greater_than_zero_is_ambiguous(                   Operator::GreaterThan, 0.0,  10.0,  5.0, (1.0, 0.0),   0.0,   Expect::Ambiguous)]
    #[case::less_than_zero_is_out_of_range(                   Operator::LessThan,    0.0,  10.0,  5.0, (1.0, 0.0),   0.0,   Expect::OutOfRange)]
    #[case::threshold_above_up_value_less_than_is_ambiguous(  Operator::LessThan,    2.0,  10.0,  5.0, (1.0, 0.0),   0.0,   Expect::Ambiguous)]
    #[case::threshold_above_up_value_greater_than_out_of_range(Operator::GreaterThan, 2.0,  10.0,  5.0, (1.0, 0.0),   0.0,   Expect::OutOfRange)]
    #[case::custom_values(                                    Operator::LessThan,    75.0, 20.0, 10.0, (100.0, 50.0), 0.0,  Expect::Ok(20.0))]
    // Phased: the 60s up / 5s down flap of port 15 of examples/if-mib-switch.yaml.
    #[case::phase_moves_the_first_drop_earlier(               Operator::LessThan,    0.5,  60.0,  5.0, (1.0, 0.0),   30.0,  Expect::Ok(30.0))]
    #[case::phase_is_taken_modulo_the_cycle(                  Operator::LessThan,    0.5,  60.0,  5.0, (1.0, 0.0),   95.0,  Expect::Ok(30.0))]
    #[case::phase_of_whole_cycles_is_no_phase(                Operator::LessThan,    0.5,  60.0,  5.0, (1.0, 0.0),   130.0, Expect::Ok(60.0))]
    #[case::phase_in_the_up_state_greater_than_is_ambiguous(  Operator::GreaterThan, 0.5,  60.0,  5.0, (1.0, 0.0),   30.0,  Expect::Ambiguous)]
    #[case::phase_at_the_drop_less_than_is_ambiguous(         Operator::LessThan,    0.5,  60.0,  5.0, (1.0, 0.0),   60.0,  Expect::Ambiguous)]
    #[case::phase_in_the_down_state_less_than_is_ambiguous(   Operator::LessThan,    0.5,  60.0,  5.0, (1.0, 0.0),   62.0,  Expect::Ambiguous)]
    #[case::phase_at_the_drop_greater_than_returns_the_rise(  Operator::GreaterThan, 0.5,  60.0,  5.0, (1.0, 0.0),   60.0,  Expect::Ok(5.0))]
    #[case::phase_in_the_down_state_greater_than_the_rise(    Operator::GreaterThan, 0.5,  60.0,  5.0, (1.0, 0.0),   62.0,  Expect::Ok(3.0))]
    #[case::phase_does_not_move_an_out_of_range_threshold(    Operator::LessThan,    0.0,  60.0,  5.0, (1.0, 0.0),   30.0,  Expect::OutOfRange)]
    fn flap_crossing(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] up_duration: f64,
        #[case] down_duration: f64,
        #[case] values: (f64, f64),
        #[case] phase: f64,
        #[case] expect: Expect,
    ) {
        let (up_value, down_value) = values;
        let result = flap_crossing_secs(
            op,
            threshold,
            up_duration,
            down_duration,
            up_value,
            down_value,
            phase,
        );
        assert_outcome(result, expect);
    }

    /// `enum: oper_state`'s values: down (2) above up (1).
    const INVERTED: (f64, f64) = (1.0, 2.0);
    /// The same two values the other way round: up (2) above down (1).
    const UPRIGHT: (f64, f64) = (2.0, 1.0);

    // Both orientations of a 60s up / 5s down flap (port 15's durations),
    // each op, thresholds below, at, between and above the two values, from
    // the start of the cycle (phase 0), 30s into the up state, and 2s into
    // the down state (phase 62). The crossing is the first instant the
    // predicate becomes true, whichever value is the higher one.
    #[rustfmt::skip]
    #[rstest]
    #[case::inverted_less_than_below_both_never_holds(              Operator::LessThan,    0.5, INVERTED, 0.0,  Expect::OutOfRange)]
    #[case::inverted_less_than_below_both_phased_never_holds(       Operator::LessThan,    0.5, INVERTED, 30.0, Expect::OutOfRange)]
    #[case::inverted_less_than_at_the_up_value_never_holds(         Operator::LessThan,    1.0, INVERTED, 30.0, Expect::OutOfRange)]
    #[case::inverted_less_than_between_holds_in_the_up_start(       Operator::LessThan,    1.5, INVERTED, 0.0,  Expect::Ambiguous)]
    #[case::inverted_less_than_between_phased_up_holds_at_t0(       Operator::LessThan,    1.5, INVERTED, 30.0, Expect::Ambiguous)]
    #[case::inverted_less_than_between_from_down_is_the_rise(       Operator::LessThan,    1.5, INVERTED, 62.0, Expect::Ok(3.0))]
    #[case::inverted_less_than_at_the_down_value_holds_up(          Operator::LessThan,    2.0, INVERTED, 0.0,  Expect::Ambiguous)]
    #[case::inverted_less_than_at_the_down_value_from_down(         Operator::LessThan,    2.0, INVERTED, 62.0, Expect::Ok(3.0))]
    #[case::inverted_less_than_above_both_always_holds(             Operator::LessThan,    2.5, INVERTED, 0.0,  Expect::Ambiguous)]
    #[case::inverted_less_than_above_both_from_down_always_holds(   Operator::LessThan,    2.5, INVERTED, 62.0, Expect::Ambiguous)]
    #[case::inverted_greater_than_below_both_always_holds(          Operator::GreaterThan, 0.5, INVERTED, 0.0,  Expect::Ambiguous)]
    #[case::inverted_greater_than_below_both_phased_always_holds(   Operator::GreaterThan, 0.5, INVERTED, 30.0, Expect::Ambiguous)]
    #[case::inverted_greater_than_at_the_up_value_is_the_drop(      Operator::GreaterThan, 1.0, INVERTED, 0.0,  Expect::Ok(60.0))]
    #[case::inverted_greater_than_between_is_the_drop(              Operator::GreaterThan, 1.5, INVERTED, 0.0,  Expect::Ok(60.0))]
    #[case::inverted_greater_than_between_phased_drop_comes_earlier(Operator::GreaterThan, 1.5, INVERTED, 30.0, Expect::Ok(30.0))]
    #[case::inverted_greater_than_between_from_down_holds_at_t0(    Operator::GreaterThan, 1.5, INVERTED, 62.0, Expect::Ambiguous)]
    #[case::inverted_greater_than_at_the_down_value_never_holds(    Operator::GreaterThan, 2.0, INVERTED, 0.0,  Expect::OutOfRange)]
    #[case::inverted_greater_than_above_both_never_holds(           Operator::GreaterThan, 2.5, INVERTED, 0.0,  Expect::OutOfRange)]
    #[case::inverted_greater_than_above_both_phased_never_holds(    Operator::GreaterThan, 2.5, INVERTED, 30.0, Expect::OutOfRange)]
    #[case::upright_less_than_below_both_never_holds(               Operator::LessThan,    0.5, UPRIGHT,  0.0,  Expect::OutOfRange)]
    #[case::upright_less_than_between_is_the_drop(                  Operator::LessThan,    1.5, UPRIGHT,  0.0,  Expect::Ok(60.0))]
    #[case::upright_less_than_between_phased_drop_comes_earlier(    Operator::LessThan,    1.5, UPRIGHT,  30.0, Expect::Ok(30.0))]
    #[case::upright_less_than_between_from_down_holds_at_t0(        Operator::LessThan,    1.5, UPRIGHT,  62.0, Expect::Ambiguous)]
    #[case::upright_less_than_above_both_always_holds(              Operator::LessThan,    2.5, UPRIGHT,  0.0,  Expect::Ambiguous)]
    #[case::upright_greater_than_below_both_always_holds(           Operator::GreaterThan, 0.5, UPRIGHT,  0.0,  Expect::Ambiguous)]
    #[case::upright_greater_than_between_holds_in_the_up_start(     Operator::GreaterThan, 1.5, UPRIGHT,  0.0,  Expect::Ambiguous)]
    #[case::upright_greater_than_between_phased_up_holds_at_t0(     Operator::GreaterThan, 1.5, UPRIGHT,  30.0, Expect::Ambiguous)]
    #[case::upright_greater_than_between_from_down_is_the_rise(     Operator::GreaterThan, 1.5, UPRIGHT,  62.0, Expect::Ok(3.0))]
    #[case::upright_greater_than_above_both_never_holds(            Operator::GreaterThan, 2.5, UPRIGHT,  0.0,  Expect::OutOfRange)]
    fn flap_crossing_either_orientation(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] values: (f64, f64),
        #[case] phase: f64,
        #[case] expect: Expect,
    ) {
        let (up_value, down_value) = values;
        let result = flap_crossing_secs(op, threshold, 60.0, 5.0, up_value, down_value, phase);
        assert_outcome(result, expect);
    }

    // A refusal names the two values and the threshold as they are, whichever
    // value is the higher one. Port 15's flap: oper_state, 60s up, 5s down.
    #[rustfmt::skip]
    #[rstest]
    #[case::never_below_names_both_values(Operator::LessThan, 1.0, 30.0,
        "up_value 1 and down_value 2 are both at or above threshold 1; \
         the flap signal is never below it")]
    #[case::never_above_names_both_values(Operator::GreaterThan, 2.5, 30.0,
        "up_value 1 and down_value 2 are both at or below threshold 2.5; \
         the flap signal is never above it")]
    #[case::always_below_names_both_values(Operator::LessThan, 2.5, 0.0,
        "up_value 1 and down_value 2 are both below threshold 2.5; \
         the flap signal is always below it (satisfied at t=0)")]
    #[case::up_start_names_the_up_value_and_the_way_to_the_drop(Operator::LessThan, 1.5, 30.0,
        "\"flap_signal < 1.5\" is satisfied at t=0 (the phase starts it 30s into the cycle, \
         in the up state at up_value 1). Use \">\" to detect the change to the down state \
         (down_value 2)")]
    #[case::down_start_names_the_down_value_and_the_way_to_the_rise(Operator::GreaterThan, 1.5, 62.0,
        "\"flap_signal > 1.5\" is satisfied at t=0 (the phase starts it 62s into the cycle, \
         in the down state at down_value 2). Use \"<\" to detect the change to the up state \
         (up_value 1)")]
    #[case::no_way_out_when_the_other_value_is_the_threshold(Operator::LessThan, 2.0, 0.0,
        "\"flap_signal < 2\" is satisfied at t=0 (it starts in the up state at up_value 1)")]
    fn flap_refusal_names_the_values_and_the_threshold(
        #[case] op: Operator,
        #[case] threshold: f64,
        #[case] phase: f64,
        #[case] want: &str,
    ) {
        let (up_value, down_value) = INVERTED;
        let err = flap_crossing_secs(op, threshold, 60.0, 5.0, up_value, down_value, phase)
            .expect_err("the flap does not resolve");
        assert_eq!(err.to_string(), want);
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
