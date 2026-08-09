//! Pure verdict evaluation for alert expectations.
//!
//! The evaluator owns the one comparison this feature exists to make:
//! *did the observed transition happen at or before its deadline?* It is
//! deliberately feature-free and I/O-free — acquisition (polling a
//! Prometheus API now, Alertmanager later) produces an [`Observation`]
//! timeline, and the evaluator turns that timeline plus a deadline into an
//! [`Outcome`]. Every deadline decision is made here and nowhere else.
//!
//! Time base: observation timestamps are durations since a caller-chosen
//! anchor — scenario start for firing checks, scenario end for resolution
//! checks. The evaluator never reads a clock.

use std::time::Duration;

use crate::verify::AlertState;

/// One timestamped poll of an alert's state.
#[derive(Debug, Clone)]
pub struct Observation {
    /// Time of the observation, measured **when the query returned**,
    /// relative to the caller's anchor (scenario start or end).
    pub at: Duration,
    /// The observed state, or the stringified query error.
    pub state: Result<AlertState, String>,
}

impl Observation {
    /// Convenience constructor.
    pub fn new(at: Duration, state: Result<AlertState, String>) -> Self {
        Self { at, state }
    }
}

/// The verdict for one check (firing or resolution) of one expectation.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum Outcome {
    /// The transition was observed at `at`, and `at <= deadline`.
    Pass {
        /// When the transition was first observed.
        at: Duration,
    },
    /// The transition was observed, but only after the deadline.
    Late {
        /// When the transition was first observed.
        at: Duration,
        /// The deadline it missed.
        deadline: Duration,
    },
    /// Polling covered the deadline and the transition never happened.
    Missed {
        /// The deadline that passed.
        deadline: Duration,
        /// The most recent query error before the deadline, if any —
        /// distinguishes "the alert never fired" from "we could not ask".
        last_error: Option<String>,
    },
    /// Polling stopped before the deadline without observing the
    /// transition — no verdict is provable from the data. Callers must
    /// treat this as a failure and say *why* it is not a [`Outcome::Missed`]
    /// (e.g. the poller died).
    Undecided,
}

impl Outcome {
    /// Whether this outcome counts as a pass.
    pub fn passed(&self) -> bool {
        matches!(self, Outcome::Pass { .. })
    }
}

/// Evaluate a firing check: the alert must reach `Firing` by `deadline`.
///
/// Observation timestamps are relative to scenario start.
pub fn evaluate_firing(observations: &[Observation], deadline: Duration) -> Outcome {
    evaluate(observations, deadline, |state| {
        matches!(state, AlertState::Firing)
    })
}

/// Evaluate a resolution check: the alert must stop firing by `deadline`.
///
/// Observation timestamps are relative to scenario end. Query errors never
/// count as resolution — only a successful query showing a non-firing state
/// does.
pub fn evaluate_resolution(observations: &[Observation], deadline: Duration) -> Outcome {
    evaluate(observations, deadline, |state| {
        !matches!(state, AlertState::Firing)
    })
}

/// Shared walk: find the first observation whose state satisfies `is_success`
/// and compare its timestamp against the deadline. When none does, the
/// verdict depends on whether polling provably covered the deadline.
fn evaluate(
    observations: &[Observation],
    deadline: Duration,
    is_success: impl Fn(&AlertState) -> bool,
) -> Outcome {
    let mut last_error = None;
    let mut covered = false;
    for observation in observations {
        match &observation.state {
            Ok(state) if is_success(state) => {
                return if observation.at <= deadline {
                    Outcome::Pass { at: observation.at }
                } else {
                    Outcome::Late {
                        at: observation.at,
                        deadline,
                    }
                };
            }
            Ok(_) => {}
            Err(e) => {
                // Only errors before the deadline explain a miss; a late
                // error cannot be why the transition wasn't seen in time.
                if observation.at <= deadline {
                    last_error = Some(e.clone());
                }
            }
        }
        if observation.at >= deadline {
            covered = true;
        }
    }
    if covered {
        Outcome::Missed {
            deadline,
            last_error,
        }
    } else {
        Outcome::Undecided
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(v: u64) -> Duration {
        Duration::from_millis(v)
    }

    fn firing(at: u64) -> Observation {
        Observation::new(ms(at), Ok(AlertState::Firing))
    }

    fn inactive(at: u64) -> Observation {
        Observation::new(ms(at), Ok(AlertState::Inactive))
    }

    fn pending(at: u64) -> Observation {
        Observation::new(ms(at), Ok(AlertState::Pending))
    }

    fn error(at: u64, msg: &str) -> Observation {
        Observation::new(ms(at), Err(msg.to_string()))
    }

    #[test]
    fn firing_on_time_passes() {
        let outcome = evaluate_firing(&[inactive(100), pending(200), firing(300)], ms(500));
        assert_eq!(outcome, Outcome::Pass { at: ms(300) });
    }

    #[test]
    fn firing_exactly_at_deadline_passes() {
        let outcome = evaluate_firing(&[firing(500)], ms(500));
        assert_eq!(outcome, Outcome::Pass { at: ms(500) });
    }

    #[test]
    fn firing_after_deadline_is_late_not_pass() {
        // Blocker 1, reproduction B: observed 3s after a 200ms deadline.
        let outcome = evaluate_firing(&[firing(3000)], ms(200));
        assert_eq!(
            outcome,
            Outcome::Late {
                at: ms(3000),
                deadline: ms(200)
            }
        );
        assert!(!outcome.passed());
    }

    #[test]
    fn no_firing_with_coverage_is_missed() {
        let outcome = evaluate_firing(&[inactive(100), inactive(600)], ms(500));
        assert_eq!(
            outcome,
            Outcome::Missed {
                deadline: ms(500),
                last_error: None
            }
        );
    }

    #[test]
    fn missed_carries_last_error_before_deadline() {
        let outcome = evaluate_firing(
            &[
                error(100, "refused"),
                error(400, "timed out"),
                inactive(600),
            ],
            ms(500),
        );
        assert_eq!(
            outcome,
            Outcome::Missed {
                deadline: ms(500),
                last_error: Some("timed out".to_string())
            }
        );
    }

    #[test]
    fn errors_after_deadline_do_not_explain_a_miss() {
        let outcome = evaluate_firing(&[inactive(100), error(600, "late error")], ms(500));
        assert_eq!(
            outcome,
            Outcome::Missed {
                deadline: ms(500),
                last_error: None
            }
        );
    }

    #[test]
    fn polling_that_stops_early_is_undecided() {
        // Coverage not proven: last observation is before the deadline.
        let outcome = evaluate_firing(&[inactive(100), inactive(200)], ms(500));
        assert_eq!(outcome, Outcome::Undecided);
    }

    #[test]
    fn empty_timeline_is_undecided() {
        assert_eq!(evaluate_firing(&[], ms(500)), Outcome::Undecided);
    }

    #[test]
    fn resolution_on_time_passes() {
        let outcome = evaluate_resolution(&[firing(100), inactive(180)], ms(200));
        assert_eq!(outcome, Outcome::Pass { at: ms(180) });
    }

    #[test]
    fn resolution_after_deadline_is_late() {
        // Blocker 1, reproduction A: resolved 322ms after a 200ms deadline.
        let outcome = evaluate_resolution(&[inactive(322)], ms(200));
        assert_eq!(
            outcome,
            Outcome::Late {
                at: ms(322),
                deadline: ms(200)
            }
        );
    }

    #[test]
    fn query_errors_never_count_as_resolution() {
        // W1: a dead endpoint must not read as "resolved" — and with only
        // errors through the deadline, the miss carries the last error.
        let outcome = evaluate_resolution(
            &[firing(50), error(150, "connection refused"), firing(250)],
            ms(200),
        );
        assert_eq!(
            outcome,
            Outcome::Missed {
                deadline: ms(200),
                last_error: Some("connection refused".to_string())
            }
        );
    }

    #[test]
    fn pending_counts_as_resolved() {
        // Resolution means "not firing" — pending after recovery qualifies.
        let outcome = evaluate_resolution(&[firing(50), pending(120)], ms(200));
        assert_eq!(outcome, Outcome::Pass { at: ms(120) });
    }
}
