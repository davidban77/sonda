//! Time-series pattern detection. Classifies a `Vec<f64>` into one of:
//! `Steady`, `Spike`, `Climb`, `Sawtooth`, `Flap`, `Step`. Pure statistics;
//! no I/O or CLI concerns. Consumed by `sonda new --from <csv>`.

use std::fmt;

/// The detected dominant pattern for a time series.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// Low variance oscillation around a center value.
    Steady {
        /// Center (mean) of the oscillation.
        center: f64,
        /// Half the peak-to-peak swing (amplitude of the sine).
        amplitude: f64,
    },
    /// Periodic spikes above a stable baseline.
    Spike {
        /// Normal baseline value.
        baseline: f64,
        /// Height of spikes above baseline.
        spike_height: f64,
        /// Approximate duration of each spike in data points.
        spike_duration_points: usize,
        /// Approximate interval between spike starts in data points.
        spike_interval_points: usize,
    },
    /// Monotonic upward trend (resource leak / saturation).
    Climb {
        /// Value at the start of the ramp.
        baseline: f64,
        /// Value at the end of the ramp (ceiling).
        ceiling: f64,
    },
    /// Repeating climb-reset cycles.
    Sawtooth {
        /// Minimum value in the cycle.
        min: f64,
        /// Maximum value in the cycle.
        max: f64,
        /// Approximate period in data points.
        period_points: usize,
    },
    /// Binary up/down toggle (bimodal distribution).
    Flap {
        /// Value in the "up" state.
        up_value: f64,
        /// Value in the "down" state.
        down_value: f64,
        /// Approximate duration of the up state in data points.
        up_duration_points: usize,
        /// Approximate duration of the down state in data points.
        down_duration_points: usize,
    },
    /// Discrete level changes with plateaus.
    Step {
        /// Starting value.
        start: f64,
        /// Increment per step.
        step_size: f64,
    },
}

impl Pattern {
    /// Returns the short, lowercase name of the pattern variant.
    ///
    /// This is the human-readable pattern identifier used in styled CLI output
    /// (e.g., `"steady"`, `"spike"`, `"flap"`). It does NOT include the
    /// parameter details that [`Display`] provides.
    pub fn name(&self) -> &'static str {
        match self {
            Pattern::Steady { .. } => "steady",
            Pattern::Spike { .. } => "spike",
            Pattern::Climb { .. } => "climb",
            Pattern::Sawtooth { .. } => "sawtooth",
            Pattern::Flap { .. } => "flap",
            Pattern::Step { .. } => "step",
        }
    }
}

impl fmt::Display for Pattern {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Pattern::Steady { center, amplitude } => {
                write!(f, "steady (center={center:.2}, amplitude={amplitude:.2})")
            }
            Pattern::Spike {
                baseline,
                spike_height,
                spike_duration_points,
                spike_interval_points,
            } => {
                write!(
                    f,
                    "spike (baseline={baseline:.2}, height={spike_height:.2}, \
                     duration={spike_duration_points}pts, interval={spike_interval_points}pts)"
                )
            }
            Pattern::Climb { baseline, ceiling } => {
                write!(f, "climb (baseline={baseline:.2}, ceiling={ceiling:.2})")
            }
            Pattern::Sawtooth {
                min,
                max,
                period_points,
            } => {
                write!(
                    f,
                    "sawtooth (min={min:.2}, max={max:.2}, period={period_points}pts)"
                )
            }
            Pattern::Flap {
                up_value,
                down_value,
                up_duration_points,
                down_duration_points,
            } => {
                write!(
                    f,
                    "flap (up={up_value:.2}, down={down_value:.2}, \
                     up_dur={up_duration_points}pts, down_dur={down_duration_points}pts)"
                )
            }
            Pattern::Step { start, step_size } => {
                write!(f, "step (start={start:.2}, step_size={step_size:.4})")
            }
        }
    }
}

/// Detect the dominant pattern in a time series.
///
/// The detection proceeds through a priority chain:
/// 1. Check for bimodal (flap) distribution.
/// 2. Check for discrete step changes.
/// 3. Check for spike pattern (high kurtosis with periodic outliers).
/// 4. Check for sawtooth (repeating climb-reset cycles).
/// 5. Check for monotonic climb (strong positive trend).
/// 6. Default to steady.
///
/// Returns the detected [`Pattern`] with extracted parameters. If the data
/// has fewer than 2 points, returns a steady pattern with the mean as center.
pub fn detect_pattern(values: &[f64]) -> Pattern {
    if values.is_empty() {
        return Pattern::Steady {
            center: 0.0,
            amplitude: 0.0,
        };
    }

    if values.len() < 2 {
        return Pattern::Steady {
            center: values[0],
            amplitude: 0.0,
        };
    }

    let stats = BasicStats::compute(values);

    // Priority chain: check most distinctive patterns first.
    // 1. Step: very distinctive (constant diffs, high R-squared).
    if let Some(step) = detect_step(values, &stats) {
        return step;
    }

    // 2. Flap: strict bimodal check (must be genuinely binary).
    if let Some(flap) = detect_flap(values, &stats) {
        return flap;
    }

    // 3. Spike: periodic outliers above a stable baseline. Checked before
    //    sawtooth because spike data can trigger false sawtooth detection
    //    (the drop from spike back to baseline looks like a sawtooth reset).
    if let Some(spike) = detect_spike(values, &stats) {
        return spike;
    }

    // 4. Climb: monotonic upward trend. Checked before sawtooth because
    //    a single ramp could trigger false sawtooth detection.
    if let Some(climb) = detect_climb(values, &stats) {
        return climb;
    }

    // 5. Sawtooth: repeating climb-reset cycles.
    if let Some(saw) = detect_sawtooth(values, &stats) {
        return saw;
    }

    // Default: steady.
    let amplitude = (stats.max - stats.min) / 2.0;
    Pattern::Steady {
        center: stats.mean,
        amplitude: if amplitude < 1e-9 { 0.0 } else { amplitude },
    }
}

/// Basic descriptive statistics for a numeric series.
#[derive(Debug)]
struct BasicStats {
    mean: f64,
    min: f64,
    max: f64,
    range: f64,
    /// Slope from linear regression (value per point index).
    slope: f64,
    /// R-squared from linear regression.
    r_squared: f64,
    count: usize,
}

impl BasicStats {
    fn compute(values: &[f64]) -> Self {
        let n = values.len() as f64;
        let mean = values.iter().sum::<f64>() / n;
        let min = values.iter().cloned().fold(f64::INFINITY, f64::min);
        let max = values.iter().cloned().fold(f64::NEG_INFINITY, f64::max);
        let range = max - min;

        // Linear regression: y = slope * x + intercept
        let (slope, r_squared) = linear_regression(values);

        BasicStats {
            mean,
            min,
            max,
            range,
            slope,
            r_squared,
            count: values.len(),
        }
    }
}

/// Compute slope and R-squared from linear regression against index.
fn linear_regression(values: &[f64]) -> (f64, f64) {
    let n = values.len() as f64;
    let mut sum_x = 0.0_f64;
    let mut sum_y = 0.0_f64;
    let mut sum_xy = 0.0_f64;
    let mut sum_x2 = 0.0_f64;
    let mut sum_y2 = 0.0_f64;

    for (i, &y) in values.iter().enumerate() {
        let x = i as f64;
        sum_x += x;
        sum_y += y;
        sum_xy += x * y;
        sum_x2 += x * x;
        sum_y2 += y * y;
    }

    let denom = n * sum_x2 - sum_x * sum_x;
    if denom.abs() < 1e-12 {
        return (0.0, 0.0);
    }

    let slope = (n * sum_xy - sum_x * sum_y) / denom;

    // R-squared
    let ss_res = values
        .iter()
        .enumerate()
        .map(|(i, &y)| {
            let predicted = slope * i as f64 + (sum_y - slope * sum_x) / n;
            (y - predicted).powi(2)
        })
        .sum::<f64>();
    let ss_tot = sum_y2 - sum_y * sum_y / n;
    let r_squared = if ss_tot.abs() < 1e-12 {
        0.0
    } else {
        1.0 - ss_res / ss_tot
    };

    (slope, r_squared)
}

/// Detect a flap (bimodal) pattern.
///
/// Checks if values cluster around exactly two distinct levels. A strict
/// bimodal check: each data point must be within a tight tolerance of one
/// of two cluster centers, and the gap between clusters must dwarf the
/// within-cluster spread.
fn detect_flap(values: &[f64], stats: &BasicStats) -> Option<Pattern> {
    if stats.count < 4 {
        return None;
    }

    // Need meaningful range to be bimodal.
    if stats.range < 1e-9 {
        return None;
    }

    // Simple k-means with k=2: split at the midpoint, iterate a few times.
    let midpoint = (stats.min + stats.max) / 2.0;

    let (center_a, center_b) = refine_two_clusters(values, midpoint);

    // Check separation: the gap between clusters should be large relative
    // to within-cluster spread.
    let (cluster_a, cluster_b): (Vec<f64>, Vec<f64>) = values
        .iter()
        .partition(|&&v| (v - center_a).abs() <= (v - center_b).abs());

    if cluster_a.is_empty() || cluster_b.is_empty() {
        return None;
    }

    let var_a = cluster_a
        .iter()
        .map(|v| (v - center_a).powi(2))
        .sum::<f64>()
        / cluster_a.len() as f64;
    let var_b = cluster_b
        .iter()
        .map(|v| (v - center_b).powi(2))
        .sum::<f64>()
        / cluster_b.len() as f64;
    let max_within_std = var_a.sqrt().max(var_b.sqrt());
    let gap = (center_a - center_b).abs();

    // Strict criterion 1: the gap between clusters must be at least 8x
    // the max within-cluster stddev. This prevents continuous distributions
    // (linear ramps, sine waves) from being classified as flap.
    if max_within_std > 0.0 && gap < 8.0 * max_within_std {
        return None;
    }

    // Strict criterion 2: within-cluster spread must be tiny relative to
    // the total range. Each cluster should have CV (stddev/|center|) < 10%
    // relative to the gap. This means values are genuinely clustered around
    // discrete levels.
    if max_within_std > gap * 0.10 {
        return None;
    }

    // Both clusters should have substantial representation (at least 25%).
    // A spike pattern has a small minority at the high level (5-40%),
    // so this threshold distinguishes flap (balanced) from spike (skewed).
    let min_fraction = 0.25;
    let a_frac = cluster_a.len() as f64 / stats.count as f64;
    let b_frac = cluster_b.len() as f64 / stats.count as f64;
    if a_frac < min_fraction || b_frac < min_fraction {
        return None;
    }

    // Determine up/down: the higher value is "up".
    let (up_value, down_value) = if center_a > center_b {
        (center_a, center_b)
    } else {
        (center_b, center_a)
    };

    // Estimate durations by counting consecutive runs.
    let (up_dur, down_dur) = estimate_run_lengths(values, (up_value + down_value) / 2.0);

    Some(Pattern::Flap {
        up_value,
        down_value,
        up_duration_points: up_dur.max(1),
        down_duration_points: down_dur.max(1),
    })
}

/// Refine two cluster centers using a few iterations of k-means.
fn refine_two_clusters(values: &[f64], initial_midpoint: f64) -> (f64, f64) {
    let mut center_a = initial_midpoint + 1.0;
    let mut center_b = initial_midpoint - 1.0;

    // Initialize from actual data points above/below midpoint.
    let above: Vec<f64> = values
        .iter()
        .filter(|&&v| v >= initial_midpoint)
        .cloned()
        .collect();
    let below: Vec<f64> = values
        .iter()
        .filter(|&&v| v < initial_midpoint)
        .cloned()
        .collect();

    if !above.is_empty() {
        center_a = above.iter().sum::<f64>() / above.len() as f64;
    }
    if !below.is_empty() {
        center_b = below.iter().sum::<f64>() / below.len() as f64;
    }

    // Iterate k-means a few times.
    for _ in 0..10 {
        let mut sum_a = 0.0;
        let mut count_a = 0_usize;
        let mut sum_b = 0.0;
        let mut count_b = 0_usize;

        for &v in values {
            if (v - center_a).abs() <= (v - center_b).abs() {
                sum_a += v;
                count_a += 1;
            } else {
                sum_b += v;
                count_b += 1;
            }
        }

        let new_a = if count_a > 0 {
            sum_a / count_a as f64
        } else {
            center_a
        };
        let new_b = if count_b > 0 {
            sum_b / count_b as f64
        } else {
            center_b
        };

        if (new_a - center_a).abs() < 1e-9 && (new_b - center_b).abs() < 1e-9 {
            break;
        }
        center_a = new_a;
        center_b = new_b;
    }

    (center_a, center_b)
}

/// Estimate average run lengths for values above and below a threshold.
///
/// A "run" is a consecutive sequence of values on the same side of the threshold.
fn estimate_run_lengths(values: &[f64], threshold: f64) -> (usize, usize) {
    let mut up_runs: Vec<usize> = Vec::new();
    let mut down_runs: Vec<usize> = Vec::new();
    let mut current_run = 1_usize;
    let mut is_up = values[0] >= threshold;

    for &v in values.iter().skip(1) {
        let this_up = v >= threshold;
        if this_up == is_up {
            current_run += 1;
        } else {
            if is_up {
                up_runs.push(current_run);
            } else {
                down_runs.push(current_run);
            }
            current_run = 1;
            is_up = this_up;
        }
    }
    // Push the last run.
    if is_up {
        up_runs.push(current_run);
    } else {
        down_runs.push(current_run);
    }

    let avg_up = if up_runs.is_empty() {
        1
    } else {
        (up_runs.iter().sum::<usize>() as f64 / up_runs.len() as f64).round() as usize
    };
    let avg_down = if down_runs.is_empty() {
        1
    } else {
        (down_runs.iter().sum::<usize>() as f64 / down_runs.len() as f64).round() as usize
    };

    (avg_up, avg_down)
}

/// Detect a step pattern (monotonic counter increments).
///
/// Looks for data that increases by a nearly constant step size with very high
/// R-squared in the linear regression.
fn detect_step(values: &[f64], stats: &BasicStats) -> Option<Pattern> {
    if stats.count < 4 {
        return None;
    }

    // Need a strong linear trend.
    if stats.r_squared < 0.95 {
        return None;
    }

    // The slope should be positive (step counters go up).
    if stats.slope <= 0.0 {
        return None;
    }

    // Check that the differences between consecutive values are nearly constant.
    let diffs: Vec<f64> = values.windows(2).map(|w| w[1] - w[0]).collect();
    let diff_mean = diffs.iter().sum::<f64>() / diffs.len() as f64;

    if diff_mean.abs() < 1e-12 {
        return None;
    }

    let diff_var = diffs.iter().map(|d| (d - diff_mean).powi(2)).sum::<f64>() / diffs.len() as f64;
    let diff_cv = diff_var.sqrt() / diff_mean.abs();

    // Low coefficient of variation in the diffs means constant step size.
    if diff_cv > 0.15 {
        return None;
    }

    Some(Pattern::Step {
        start: values[0],
        step_size: diff_mean,
    })
}

/// Detect a spike pattern.
///
/// Checks for data where most values are near a baseline and periodic outliers
/// jump well above it. Uses either IQR-based outlier detection or a
/// range-fraction threshold when the IQR is too small (e.g., when most
/// values are identical).
fn detect_spike(values: &[f64], stats: &BasicStats) -> Option<Pattern> {
    if stats.count < 10 {
        return None;
    }

    // Need meaningful range.
    if stats.range < 1e-9 {
        return None;
    }

    // Use percentiles to identify baseline vs spikes.
    let mut sorted = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));

    let p25 = percentile(&sorted, 0.25);
    let p75 = percentile(&sorted, 0.75);
    let iqr = p75 - p25;
    let median = percentile(&sorted, 0.50);

    // Determine spike threshold. When IQR is very small (e.g., most values
    // are identical), fall back to median + fraction of range.
    let spike_threshold = if iqr > stats.range * 0.01 {
        let threshold = p75 + 1.5 * iqr;
        // Need the threshold to be meaningfully above the median.
        if threshold <= median + stats.range * 0.1 {
            return None;
        }
        threshold
    } else {
        // IQR is tiny: use median + 30% of range as the spike boundary.
        median + stats.range * 0.30
    };

    // Count spike points.
    let spike_points: Vec<usize> = values
        .iter()
        .enumerate()
        .filter(|(_, &v)| v > spike_threshold)
        .map(|(i, _)| i)
        .collect();

    let spike_fraction = spike_points.len() as f64 / stats.count as f64;

    // Spikes should be a minority of points (3% - 40%).
    if !(0.03..=0.40).contains(&spike_fraction) {
        return None;
    }

    // Calculate baseline from non-spike points.
    let baseline_values: Vec<f64> = values
        .iter()
        .filter(|&&v| v <= spike_threshold)
        .cloned()
        .collect();
    if baseline_values.is_empty() {
        return None;
    }
    let baseline = baseline_values.iter().sum::<f64>() / baseline_values.len() as f64;

    // Calculate spike height.
    let spike_values: Vec<f64> = values
        .iter()
        .filter(|&&v| v > spike_threshold)
        .cloned()
        .collect();
    if spike_values.is_empty() {
        return None;
    }
    let spike_mean = spike_values.iter().sum::<f64>() / spike_values.len() as f64;
    let spike_height = spike_mean - baseline;

    // Estimate spike duration and interval from the spike point indices.
    let (spike_dur, spike_interval) = estimate_spike_timing(&spike_points, stats.count);

    Some(Pattern::Spike {
        baseline,
        spike_height,
        spike_duration_points: spike_dur.max(1),
        spike_interval_points: spike_interval.max(1),
    })
}

/// Estimate spike duration and interval from indices of spike points.
fn estimate_spike_timing(spike_indices: &[usize], total_points: usize) -> (usize, usize) {
    if spike_indices.is_empty() {
        return (1, total_points);
    }

    // Group consecutive spike indices into bursts.
    let mut bursts: Vec<(usize, usize)> = Vec::new(); // (start, length)
    let mut burst_start = spike_indices[0];
    let mut burst_len = 1_usize;

    for &idx in spike_indices.iter().skip(1) {
        if idx == burst_start + burst_len {
            burst_len += 1;
        } else {
            bursts.push((burst_start, burst_len));
            burst_start = idx;
            burst_len = 1;
        }
    }
    bursts.push((burst_start, burst_len));

    // Average burst length is the spike duration.
    let avg_duration = (bursts.iter().map(|(_, l)| l).sum::<usize>() as f64 / bursts.len() as f64)
        .round() as usize;

    // Average interval between burst starts.
    let avg_interval = if bursts.len() < 2 {
        total_points
    } else {
        let intervals: Vec<usize> = bursts.windows(2).map(|w| w[1].0 - w[0].0).collect();
        (intervals.iter().sum::<usize>() as f64 / intervals.len() as f64).round() as usize
    };

    (avg_duration, avg_interval)
}

/// Detect a sawtooth pattern (repeating climb-reset cycles).
///
/// Looks for data with periodic sharp drops (resets) after gradual climbs.
fn detect_sawtooth(values: &[f64], stats: &BasicStats) -> Option<Pattern> {
    if stats.count < 10 {
        return None;
    }

    if stats.range < 1e-9 {
        return None;
    }

    // Look for sharp drops: points where the value drops by more than
    // 50% of the total range in a single step.
    let drop_threshold = stats.range * 0.4;
    let mut drop_indices: Vec<usize> = Vec::new();

    for i in 1..values.len() {
        let diff = values[i - 1] - values[i];
        if diff > drop_threshold {
            drop_indices.push(i);
        }
    }

    // Need at least 2 drops to establish a repeating pattern.
    if drop_indices.len() < 2 {
        return None;
    }

    // The segments between drops should show upward trends.
    let mut segments_climbing = 0_usize;
    let mut prev_start = 0_usize;
    for &drop_idx in &drop_indices {
        if drop_idx > prev_start + 2 {
            let segment = &values[prev_start..drop_idx];
            let (seg_slope, _) = linear_regression(segment);
            if seg_slope > 0.0 {
                segments_climbing += 1;
            }
        }
        prev_start = drop_idx;
    }

    // At least half the segments should be climbing.
    if segments_climbing < drop_indices.len() / 2 {
        return None;
    }

    // Estimate period from intervals between drops.
    let intervals: Vec<usize> = drop_indices.windows(2).map(|w| w[1] - w[0]).collect();
    let avg_period = if intervals.is_empty() {
        stats.count
    } else {
        (intervals.iter().sum::<usize>() as f64 / intervals.len() as f64).round() as usize
    };

    Some(Pattern::Sawtooth {
        min: stats.min,
        max: stats.max,
        period_points: avg_period,
    })
}

/// Detect a monotonic climb (leak/saturation without reset).
///
/// Strong positive trend with high R-squared and non-constant diffs
/// (distinguishing from step counters which are handled earlier).
fn detect_climb(values: &[f64], stats: &BasicStats) -> Option<Pattern> {
    if stats.count < 4 {
        return None;
    }

    // Need a strong positive trend with decent R-squared.
    if stats.slope <= 0.0 || stats.r_squared < 0.7 {
        return None;
    }

    // The trend should explain a meaningful portion of the range.
    let trend_range = stats.slope * (stats.count as f64 - 1.0);
    if trend_range < stats.range * 0.5 {
        return None;
    }

    Some(Pattern::Climb {
        baseline: values[0],
        ceiling: *values.last().expect("checked non-empty"),
    })
}

/// Compute the p-th percentile from a sorted slice.
fn percentile(sorted: &[f64], p: f64) -> f64 {
    let idx = (p * (sorted.len() - 1) as f64).round() as usize;
    sorted[idx.min(sorted.len() - 1)]
}

#[cfg(test)]
mod tests {
    use super::*;

    // Pattern::name() — short variant identifiers

    #[test]
    fn name_returns_steady_for_steady_variant() {
        let p = Pattern::Steady {
            center: 50.0,
            amplitude: 5.0,
        };
        assert_eq!(p.name(), "steady");
    }

    #[test]
    fn name_returns_spike_for_spike_variant() {
        let p = Pattern::Spike {
            baseline: 0.0,
            spike_height: 100.0,
            spike_duration_points: 3,
            spike_interval_points: 10,
        };
        assert_eq!(p.name(), "spike");
    }

    #[test]
    fn name_returns_climb_for_climb_variant() {
        let p = Pattern::Climb {
            baseline: 0.0,
            ceiling: 100.0,
        };
        assert_eq!(p.name(), "climb");
    }

    #[test]
    fn name_returns_sawtooth_for_sawtooth_variant() {
        let p = Pattern::Sawtooth {
            min: 0.0,
            max: 100.0,
            period_points: 20,
        };
        assert_eq!(p.name(), "sawtooth");
    }

    #[test]
    fn name_returns_flap_for_flap_variant() {
        let p = Pattern::Flap {
            up_value: 1.0,
            down_value: 0.0,
            up_duration_points: 5,
            down_duration_points: 5,
        };
        assert_eq!(p.name(), "flap");
    }

    #[test]
    fn name_returns_step_for_step_variant() {
        let p = Pattern::Step {
            start: 0.0,
            step_size: 10.0,
        };
        assert_eq!(p.name(), "step");
    }

    // Steady pattern detection

    #[test]
    fn detect_constant_values_as_steady() {
        let values: Vec<f64> = vec![50.0; 100];
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Steady { center, amplitude } => {
                assert!((center - 50.0).abs() < 0.1);
                assert!(amplitude < 0.1);
            }
            other => panic!("expected Steady, got {other}"),
        }
    }

    #[test]
    fn detect_low_variance_as_steady() {
        // Generate a gentle sine-like wave.
        let values: Vec<f64> = (0..100)
            .map(|i| 50.0 + 5.0 * (i as f64 * std::f64::consts::PI / 50.0).sin())
            .collect();
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Steady { center, amplitude } => {
                assert!((center - 50.0).abs() < 1.0);
                assert!(amplitude > 0.0);
            }
            other => panic!("expected Steady, got {other}"),
        }
    }

    // Spike pattern detection

    #[test]
    fn detect_periodic_spikes() {
        // Baseline of 10 with spikes to 100 every 20 points, lasting 3 points.
        let mut values: Vec<f64> = vec![10.0; 100];
        for start in (0..100).step_by(20) {
            for offset in 0..3 {
                if start + offset < 100 {
                    values[start + offset] = 100.0;
                }
            }
        }
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Spike {
                baseline,
                spike_height,
                ..
            } => {
                assert!((baseline - 10.0).abs() < 5.0, "baseline={baseline}");
                assert!(spike_height > 50.0, "spike_height={spike_height}");
            }
            other => panic!("expected Spike, got {other}"),
        }
    }

    // Climb pattern detection

    #[test]
    fn detect_linear_climb() {
        // Linear ramp from 0 to 100.
        let values: Vec<f64> = (0..100).map(|i| i as f64).collect();
        let pattern = detect_pattern(&values);
        match pattern {
            // Could be Step or Climb depending on heuristics. Step takes priority
            // for constant-diff series.
            Pattern::Step { start, step_size } => {
                assert!((start - 0.0).abs() < 0.1);
                assert!((step_size - 1.0).abs() < 0.1);
            }
            Pattern::Climb { baseline, ceiling } => {
                assert!(baseline < 5.0, "baseline={baseline}");
                assert!(ceiling > 95.0, "ceiling={ceiling}");
            }
            other => panic!("expected Climb or Step, got {other}"),
        }
    }

    #[test]
    fn detect_noisy_climb() {
        // Noisy ramp: linear + noise (noise prevents step detection).
        let values: Vec<f64> = (0..100)
            .map(|i| {
                let base = i as f64;
                let noise = ((i * 7 + 3) % 11) as f64 - 5.0; // deterministic noise
                base + noise * 0.5
            })
            .collect();
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Climb { baseline, ceiling } => {
                assert!(baseline < 10.0, "baseline={baseline}");
                assert!(ceiling > 90.0, "ceiling={ceiling}");
            }
            other => panic!("expected Climb, got {other}"),
        }
    }

    // Sawtooth pattern detection

    #[test]
    fn detect_sawtooth_pattern() {
        // Three cycles of ramp-and-reset, each 30 points.
        let values: Vec<f64> = (0..90)
            .map(|i| {
                let phase = i % 30;
                phase as f64 * (100.0 / 29.0)
            })
            .collect();
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Sawtooth {
                min,
                max,
                period_points,
            } => {
                assert!(min < 5.0, "min={min}");
                assert!(max > 95.0, "max={max}");
                assert!(
                    (period_points as i64 - 30).unsigned_abs() <= 3,
                    "period={period_points}"
                );
            }
            other => panic!("expected Sawtooth, got {other}"),
        }
    }

    // Flap pattern detection

    #[test]
    fn detect_flap_pattern() {
        // Alternating between 1.0 and 0.0, 10 points each.
        let mut values: Vec<f64> = Vec::new();
        for _ in 0..5 {
            values.extend(std::iter::repeat_n(1.0, 10));
            values.extend(std::iter::repeat_n(0.0, 10));
        }
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Flap {
                up_value,
                down_value,
                up_duration_points,
                down_duration_points,
            } => {
                assert!((up_value - 1.0).abs() < 0.1, "up={up_value}");
                assert!((down_value - 0.0).abs() < 0.1, "down={down_value}");
                assert!(
                    (up_duration_points as i64 - 10).unsigned_abs() <= 2,
                    "up_dur={up_duration_points}"
                );
                assert!(
                    (down_duration_points as i64 - 10).unsigned_abs() <= 2,
                    "down_dur={down_duration_points}"
                );
            }
            other => panic!("expected Flap, got {other}"),
        }
    }

    // Step pattern detection

    #[test]
    fn detect_monotonic_counter_as_step() {
        // Monotonic counter: 0, 5, 10, 15, ...
        let values: Vec<f64> = (0..50).map(|i| i as f64 * 5.0).collect();
        let pattern = detect_pattern(&values);
        match pattern {
            Pattern::Step { start, step_size } => {
                assert!((start - 0.0).abs() < 0.1);
                assert!((step_size - 5.0).abs() < 0.1, "step_size={step_size}");
            }
            other => panic!("expected Step, got {other}"),
        }
    }

    // Edge cases

    #[test]
    fn empty_values_returns_steady_zero() {
        let pattern = detect_pattern(&[]);
        match pattern {
            Pattern::Steady { center, amplitude } => {
                assert_eq!(center, 0.0);
                assert_eq!(amplitude, 0.0);
            }
            other => panic!("expected Steady, got {other}"),
        }
    }

    #[test]
    fn single_value_returns_steady() {
        let pattern = detect_pattern(&[42.0]);
        match pattern {
            Pattern::Steady { center, amplitude } => {
                assert_eq!(center, 42.0);
                assert_eq!(amplitude, 0.0);
            }
            other => panic!("expected Steady, got {other}"),
        }
    }

    #[test]
    fn two_values_returns_steady() {
        let pattern = detect_pattern(&[10.0, 11.0]);
        match pattern {
            Pattern::Steady { center, .. } => {
                assert!((center - 10.5).abs() < 0.1);
            }
            other => panic!("expected Steady, got {other}"),
        }
    }

    // Display implementation

    #[test]
    fn pattern_display_includes_parameters() {
        let p = Pattern::Steady {
            center: 50.0,
            amplitude: 10.0,
        };
        let s = format!("{p}");
        assert!(s.contains("steady"));
        assert!(s.contains("50.00"));
    }

    // Determinism: same input produces same output

    #[test]
    fn pattern_detection_is_deterministic() {
        let values: Vec<f64> = (0..100)
            .map(|i| 50.0 + 10.0 * (i as f64 * 0.1).sin())
            .collect();
        let a = detect_pattern(&values);
        let b = detect_pattern(&values);
        assert_eq!(a, b);
    }
}
