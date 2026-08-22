//! Resample fetched series onto the requested step grid.
//!
//! Pure and feature-free. The whole module is one rule with one exception, and
//! both are load-bearing:
//!
//! **A grid point takes the value the TSDB reported at that instant, or `NaN`
//! if the TSDB reported nothing there.** Values are never interpolated,
//! averaged, carried forward, or otherwise invented — if the database had no
//! sample, neither does the replay. That is what makes this path exact rather
//! than a fit.
//!
//! # Why gaps become `NaN` and not absent rows
//!
//! The spec offered two representations for a staleness gap: absent CSV
//! samples, or the scenario's `gaps:` config. Both were measured against the
//! engine and both are wrong.
//!
//! *Absent samples are disqualifying.* `CsvReplayGenerator::load_column`
//! silently skips any cell that does not parse as `f64`, so a hole in the data
//! shortens the value vector rather than leaving a space in it. Measured: a
//! four-row CSV with one empty cell replays three values, and every sample
//! after the hole arrives one step early. A capture that shifts its own
//! incident is not a replay of anything.
//!
//! *`gaps:` cannot express the shape.* [`crate::schedule::GapWindow`] is
//! `every` + `duration` — strictly periodic. Real staleness is irregular: one
//! outage at t+120, another at t+400. A periodic window cannot represent that
//! without lying about when the silence happened.
//!
//! `NaN` parses through `f64::from_str`, so the generator pushes it like any
//! other value and the grid stays aligned. It is the only one of the three
//! that preserves *when* things happened, which is the property the whole
//! feature exists to deliver.
//!
//! # The limitation this leaves, stated plainly
//!
//! A `NaN` sample is still a *sample*. The replay emits a point at that
//! instant carrying `NaN`, where production emitted nothing at all. Value
//! fidelity and timing are exact; **absence is not reproduced**. An
//! `absent()`-style alert that fired against the original silence will not
//! fire against the replay. Reproducing true silence needs per-series gap
//! windows at arbitrary offsets, which the schedule layer does not have — a
//! core engine change, out of scope here and recorded rather than guessed at.

use super::FetchedSeries;
use std::collections::BTreeMap;

/// The sample grid a capture is resampled onto.
///
/// Grid point `n` is exactly `start + n * step`, which is also the instant
/// replay tick `n` stands for. Nothing else in this module gets to define
/// where a sample lands.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Grid {
    /// First grid point, unix seconds.
    pub start: f64,
    /// Spacing between grid points, seconds. Always > 0.
    pub step: f64,
    /// Number of grid points, counting `start`.
    pub len: usize,
}

impl Grid {
    /// Build the grid covering `start..=end` at `step`.
    ///
    /// The last point is the largest `start + n * step` that is `<= end`, so a
    /// range that is not a whole multiple of the step stops short rather than
    /// running past the requested window.
    ///
    /// Returns `None` when `step` is not positive or `end` precedes `start` —
    /// callers turn that into a configuration error with their own wording.
    pub fn new(start: f64, end: f64, step: f64) -> Option<Self> {
        if !step.is_finite() || step <= 0.0 || !start.is_finite() || !end.is_finite() || end < start
        {
            return None;
        }
        // +1 for the inclusive start point. The epsilon absorbs the float
        // error in cases like (end - start) / step == 5.999999999999999 for a
        // range that is exactly six steps wide.
        let spans = ((end - start) / step + 1e-9).floor();
        if !spans.is_finite() || spans < 0.0 {
            return None;
        }
        Some(Grid {
            start,
            step,
            len: spans as usize + 1,
        })
    }

    /// The instant grid point `n` stands for, unix seconds.
    pub fn point(&self, n: usize) -> f64 {
        self.start + n as f64 * self.step
    }
}

/// One series resampled onto the grid.
#[derive(Debug, Clone, PartialEq)]
pub struct NormalizedSeries {
    /// The series' label set, `__name__` included when the query kept it.
    pub labels: BTreeMap<String, String>,
    /// One value per grid point. `NaN` marks a grid point the TSDB had no
    /// sample for — see the module docs for why absence is spelled this way.
    pub values: Vec<f64>,
}

impl NormalizedSeries {
    /// How many grid points carry no data.
    ///
    /// Counted rather than inferred so the CLI can report it and a test can
    /// assert on it.
    pub fn gap_count(&self) -> usize {
        self.values.iter().filter(|v| v.is_nan()).count()
    }
}

/// Resample one fetched series onto `grid`.
///
/// A sample counts for grid point `n` when its timestamp is within half a
/// millisecond of `grid.point(n)`. Prometheus aligns range-query samples to
/// the requested grid, so in practice this is an exact match; the tolerance
/// exists because the timestamps arrive as JSON floats, not because the
/// values are being snapped to somewhere they did not come from.
///
/// Samples that match no grid point are dropped: they are outside the
/// requested window, and inventing a grid point for them would move data the
/// caller did not ask for.
pub fn normalize(series: &FetchedSeries, grid: Grid) -> NormalizedSeries {
    // Half a millisecond, the finest resolution the Prometheus API expresses.
    const TOLERANCE_SECS: f64 = 0.0005;

    let mut values = vec![f64::NAN; grid.len];
    for &(ts, value) in &series.samples {
        if !ts.is_finite() {
            continue;
        }
        // Which grid point is this closest to?
        let n = ((ts - grid.start) / grid.step).round();
        if n < 0.0 || n >= grid.len as f64 {
            continue;
        }
        let n = n as usize;
        if (ts - grid.point(n)).abs() <= TOLERANCE_SECS {
            values[n] = value;
        }
    }
    NormalizedSeries {
        labels: series.labels.clone(),
        values,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn series(samples: &[(f64, f64)]) -> FetchedSeries {
        let mut labels = BTreeMap::new();
        labels.insert("__name__".to_string(), "m".to_string());
        FetchedSeries {
            labels,
            samples: samples.to_vec(),
        }
    }

    #[test]
    fn grid_point_n_is_exactly_start_plus_n_times_step() {
        let g = Grid::new(1000.0, 1150.0, 30.0).expect("valid grid");
        assert_eq!(g.len, 6, "1000..=1150 at 30s is six points");
        for n in 0..g.len {
            assert_eq!(g.point(n), 1000.0 + n as f64 * 30.0);
        }
    }

    #[test]
    fn a_range_that_is_not_a_whole_multiple_stops_short_of_end() {
        // 1000..=1145 at 30s: points at 1000,1030,1060,1090,1120 — 1150 > end.
        let g = Grid::new(1000.0, 1145.0, 30.0).expect("valid grid");
        assert_eq!(g.len, 5);
        assert_eq!(g.point(g.len - 1), 1120.0);
    }

    #[test]
    fn float_error_does_not_lose_the_last_grid_point() {
        // (end-start)/step lands just under 6 in binary floating point.
        let g = Grid::new(0.0, 0.3, 0.05).expect("valid grid");
        assert_eq!(g.len, 7, "0.0..=0.3 at 0.05 is seven points");
    }

    #[test]
    fn rejects_non_positive_step_and_inverted_range() {
        assert!(Grid::new(0.0, 10.0, 0.0).is_none());
        assert!(Grid::new(0.0, 10.0, -1.0).is_none());
        assert!(Grid::new(10.0, 0.0, 1.0).is_none());
        assert!(Grid::new(f64::NAN, 10.0, 1.0).is_none());
    }

    #[test]
    fn a_complete_series_resamples_to_its_own_values() {
        let g = Grid::new(100.0, 130.0, 10.0).expect("valid grid");
        let s = series(&[(100.0, 1.0), (110.0, 2.0), (120.0, 3.0), (130.0, 4.0)]);
        let n = normalize(&s, g);
        assert_eq!(n.values, vec![1.0, 2.0, 3.0, 4.0]);
        assert_eq!(n.gap_count(), 0);
    }

    #[test]
    fn a_missing_sample_becomes_nan_and_does_not_shift_its_neighbours() {
        // This is the whole reason gaps are NaN. The value after the hole must
        // stay at its own grid point rather than sliding into the hole.
        let g = Grid::new(100.0, 130.0, 10.0).expect("valid grid");
        let s = series(&[(100.0, 1.0), (120.0, 3.0), (130.0, 4.0)]);
        let n = normalize(&s, g);
        assert_eq!(n.values.len(), 4);
        assert_eq!(n.values[0], 1.0);
        assert!(n.values[1].is_nan(), "the hole is NaN, not a shifted value");
        assert_eq!(n.values[2], 3.0, "3.0 stays at its own grid point");
        assert_eq!(n.values[3], 4.0);
        assert_eq!(n.gap_count(), 1);
    }

    #[test]
    fn a_series_with_no_samples_at_all_is_all_gap() {
        let g = Grid::new(100.0, 130.0, 10.0).expect("valid grid");
        let n = normalize(&series(&[]), g);
        assert_eq!(n.values.len(), 4);
        assert_eq!(n.gap_count(), 4);
    }

    #[test]
    fn resampling_never_fabricates_a_value_between_two_samples() {
        // The reviewer's named attack: interpolation would put something at
        // 110 and 120. Nothing may appear where the TSDB reported nothing.
        let g = Grid::new(100.0, 130.0, 10.0).expect("valid grid");
        let s = series(&[(100.0, 0.0), (130.0, 300.0)]);
        let n = normalize(&s, g);
        assert!(n.values[1].is_nan());
        assert!(n.values[2].is_nan());
        assert_eq!(n.values[0], 0.0);
        assert_eq!(n.values[3], 300.0);
    }

    #[test]
    fn samples_outside_the_window_are_dropped_not_folded_in() {
        let g = Grid::new(100.0, 120.0, 10.0).expect("valid grid");
        let s = series(&[(80.0, 9.0), (100.0, 1.0), (140.0, 9.0)]);
        let n = normalize(&s, g);
        assert_eq!(n.values.len(), 3);
        assert_eq!(n.values[0], 1.0);
        assert!(n.values[1].is_nan());
        assert!(n.values[2].is_nan());
    }

    #[test]
    fn sub_millisecond_jitter_still_lands_on_its_grid_point() {
        let g = Grid::new(100.0, 120.0, 10.0).expect("valid grid");
        let s = series(&[(100.0002, 1.0), (109.9997, 2.0)]);
        let n = normalize(&s, g);
        assert_eq!(n.values[0], 1.0);
        assert_eq!(n.values[1], 2.0);
    }

    #[test]
    fn a_sample_far_off_grid_is_not_snapped_onto_it() {
        // 105 is nearest to grid point 100 but four seconds away: it is a
        // different instant and must not overwrite that point.
        let g = Grid::new(100.0, 120.0, 10.0).expect("valid grid");
        let s = series(&[(104.0, 7.0)]);
        let n = normalize(&s, g);
        assert!(n.values.iter().all(|v| v.is_nan()), "nothing was snapped");
    }

    #[test]
    fn counter_reset_is_carried_through_verbatim() {
        // Replay is exact: a reset is data, not an anomaly to smooth.
        let g = Grid::new(0.0, 30.0, 10.0).expect("valid grid");
        let s = series(&[(0.0, 98.0), (10.0, 99.0), (20.0, 0.0), (30.0, 1.0)]);
        let n = normalize(&s, g);
        assert_eq!(n.values, vec![98.0, 99.0, 0.0, 1.0]);
    }
}
