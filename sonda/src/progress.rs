//! Live progress display for running scenarios.
//!
//! Polls [`ScenarioStats`] via the shared [`RwLock`] at regular intervals and
//! renders updating status lines to stderr. All output goes to stderr so that
//! stdout remains clean for data when the sink is stdout.
//!
//! **TTY mode** (stderr is a terminal): Uses ANSI escape sequences to overwrite
//! progress lines in place, producing a compact, live-updating display.
//!
//! **Non-TTY mode** (stderr is piped or redirected): Emits a static progress
//! line every [`NON_TTY_INTERVAL`] to avoid flooding logs while still showing
//! that the tool is alive.
//!
//! The display is driven by a dedicated monitoring thread spawned via
//! [`ProgressDisplay::start`]. The thread is joined cleanly by
//! [`ProgressDisplay::stop`], which also erases the progress lines in TTY mode
//! so that stop banners print without visual artifacts.

use std::collections::HashSet;
use std::io::{self, IsTerminal, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use owo_colors::OwoColorize;
use owo_colors::Stream::Stderr;

use sonda_core::schedule::stats::ScenarioStats;

/// How often to poll stats and redraw in TTY mode.
const TTY_POLL_INTERVAL: Duration = Duration::from_millis(200);

/// How often to emit a status line in non-TTY mode.
///
/// Kept high to avoid spamming redirected logs or CI output. Each line is
/// self-contained (no ANSI cursor control).
const NON_TTY_INTERVAL: Duration = Duration::from_secs(5);

/// Descriptor for a single scenario being monitored.
///
/// Holds the scenario name, its shared stats arc, and the target rate so the
/// progress display can compute and render meaningful indicators.
struct MonitoredScenario {
    /// Human-readable scenario name (from the config).
    name: String,
    /// Shared stats updated by the runner thread on each tick.
    stats: Arc<RwLock<ScenarioStats>>,
    /// Configured target rate (events per second).
    target_rate: f64,
    /// Lock-free liveness flag flipped to `false` when the runner thread exits.
    alive: Arc<AtomicBool>,
}

/// A live progress display for one or more running scenarios.
///
/// Created via [`ProgressDisplay::start`], which spawns a background monitoring
/// thread. Call [`ProgressDisplay::stop`] to join the thread and clean up
/// terminal state before printing stop banners.
///
/// The monitoring thread only reads stats via `RwLock::read()`, so it does not
/// contend with the writer (the scenario runner thread). The polling interval
/// is 200ms in TTY mode, which is fast enough for visual feedback but
/// negligible overhead compared to event generation.
pub struct ProgressDisplay {
    /// Thread running the progress polling loop.
    thread: Option<JoinHandle<()>>,
    /// Flag to signal the monitoring thread to stop.
    stop_flag: Arc<AtomicBool>,
}

impl ProgressDisplay {
    /// Start the progress display for the given scenarios.
    ///
    /// Spawns a background thread that polls stats and renders progress lines
    /// to stderr. Returns a [`ProgressDisplay`] handle that must be stopped
    /// before printing stop banners.
    ///
    /// Each tuple contains `(name, stats_arc, target_rate, alive_flag)`.
    ///
    /// # Panics
    ///
    /// Panics if the monitoring thread cannot be spawned (system resource
    /// exhaustion).
    #[allow(clippy::type_complexity)]
    pub fn start(
        scenarios: Vec<(String, Arc<RwLock<ScenarioStats>>, f64, Arc<AtomicBool>)>,
    ) -> Self {
        let stop_flag = Arc::new(AtomicBool::new(false));
        let flag_clone = Arc::clone(&stop_flag);

        let monitored: Vec<MonitoredScenario> = scenarios
            .into_iter()
            .map(|(name, stats, target_rate, alive)| MonitoredScenario {
                name,
                stats,
                target_rate,
                alive,
            })
            .collect();

        let is_tty = io::stderr().is_terminal();

        let thread = thread::Builder::new()
            .name("sonda-progress".to_string())
            .spawn(move || {
                if is_tty {
                    run_tty_loop(&monitored, &flag_clone);
                } else {
                    run_non_tty_loop(&monitored, &flag_clone);
                }
            })
            .expect("failed to spawn progress monitoring thread");

        ProgressDisplay {
            thread: Some(thread),
            stop_flag,
        }
    }

    /// Stop the progress display and clean up terminal state.
    ///
    /// Signals the monitoring thread to exit, joins it, and in TTY mode erases
    /// the progress lines so that subsequent output (stop banners) starts on a
    /// clean line.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            // The thread checks the flag every poll interval, so this join
            // completes within one interval plus the time to erase lines.
            let _ = thread.join();
        }
    }
}

impl Drop for ProgressDisplay {
    fn drop(&mut self) {
        // Safety net: if stop() was not called, signal the thread to exit.
        // We cannot join in Drop (would need &mut self and the thread is Option),
        // but at least we signal it so it does not run forever.
        self.stop_flag.store(true, Ordering::SeqCst);
    }
}

/// Read a stats snapshot from the shared lock.
///
/// Returns a default `ScenarioStats` if the lock is poisoned.
fn read_stats(stats: &Arc<RwLock<ScenarioStats>>) -> ScenarioStats {
    match stats.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => poisoned.into_inner().clone(),
    }
}

/// Run the TTY progress loop.
///
/// Renders progress lines using ANSI cursor control so they update in place.
/// Stopped scenarios receive a one-shot "STOPPED" banner and drop out of the
/// live-line set so the cursor maths stays consistent.
fn run_tty_loop(scenarios: &[MonitoredScenario], stop_flag: &AtomicBool) {
    let mut first_render = true;
    let start = Instant::now();
    let mut banner_emitted: HashSet<String> = HashSet::new();
    let mut live_lines_last_render: usize = 0;

    while !stop_flag.load(Ordering::SeqCst) {
        thread::sleep(TTY_POLL_INTERVAL);

        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        let elapsed = start.elapsed();
        let mut stderr = io::stderr().lock();

        if !first_render && live_lines_last_render > 0 {
            for _ in 0..live_lines_last_render {
                let _ = write!(stderr, "\x1b[A");
            }
        }

        let mut new_banners: Vec<String> = Vec::new();
        let mut live_count: usize = 0;

        for scenario in scenarios {
            if banner_emitted.contains(&scenario.name) {
                continue;
            }
            let stats = read_stats(&scenario.stats);
            if !scenario.alive.load(Ordering::SeqCst) {
                let banner = format_stopped_line_tty(&scenario.name, &stats, elapsed);
                new_banners.push(banner);
                banner_emitted.insert(scenario.name.clone());
                continue;
            }
            let line = format_tty_line(&scenario.name, &stats, scenario.target_rate, elapsed);
            let _ = write!(stderr, "\x1b[2K{line}\r\n");
            live_count += 1;
        }

        for banner in new_banners {
            let _ = write!(stderr, "\x1b[2K{banner}\r\n");
        }

        let _ = stderr.flush();
        first_render = false;
        live_lines_last_render = live_count;
    }

    // Erase remaining live progress lines on exit so stop banners print
    // cleanly. Banners that were already permanent stay where they are.
    let mut stderr = io::stderr().lock();
    if !first_render && live_lines_last_render > 0 {
        for _ in 0..live_lines_last_render {
            let _ = write!(stderr, "\x1b[A\x1b[2K");
        }
    }

    // Drain any dead scenarios that were not banner'd before stop was
    // signaled — when a scenario dies fast (e.g. CLI fail-mode), the
    // shutdown signal can race ahead of the next polling iteration and
    // skip the banner pass.
    let final_elapsed = start.elapsed();
    drain_stopped_scenarios(scenarios, &banner_emitted, |name, stats| {
        let banner = format_stopped_line_tty(name, stats, final_elapsed);
        let _ = write!(stderr, "\x1b[2K{banner}\r\n");
    });
    let _ = stderr.flush();
}

/// Run the non-TTY progress loop.
///
/// Emits a self-contained status line every [`NON_TTY_INTERVAL`]. No ANSI
/// escape sequences are used. Each scenario receives a one-shot STOPPED
/// banner the first iteration after its runner thread exits.
fn run_non_tty_loop(scenarios: &[MonitoredScenario], stop_flag: &AtomicBool) {
    let start = Instant::now();
    let mut last_emit = Instant::now();
    let check_interval = Duration::from_millis(200);
    let mut banner_emitted: HashSet<String> = HashSet::new();

    while !stop_flag.load(Ordering::SeqCst) {
        thread::sleep(check_interval);

        if stop_flag.load(Ordering::SeqCst) {
            break;
        }

        // Detect newly-stopped scenarios immediately and emit a stopped
        // banner without waiting for the next NON_TTY_INTERVAL tick.
        for scenario in scenarios {
            if banner_emitted.contains(&scenario.name) {
                continue;
            }
            if !scenario.alive.load(Ordering::SeqCst) {
                let stats = read_stats(&scenario.stats);
                let banner = format_stopped_line_plain(&scenario.name, &stats, start.elapsed());
                eprintln!("{banner}");
                banner_emitted.insert(scenario.name.clone());
            }
        }

        if last_emit.elapsed() < NON_TTY_INTERVAL {
            continue;
        }

        last_emit = Instant::now();
        let elapsed = start.elapsed();

        for scenario in scenarios {
            if banner_emitted.contains(&scenario.name) {
                continue;
            }
            let stats = read_stats(&scenario.stats);
            let line = format_non_tty_line(&scenario.name, &stats, scenario.target_rate, elapsed);
            eprintln!("{line}");
        }
    }

    // Drain any dead scenarios that were not banner'd before stop was
    // signaled — when a scenario dies fast (e.g. CLI fail-mode), the
    // shutdown signal can race ahead of the next polling iteration and
    // skip the banner pass.
    let final_elapsed = start.elapsed();
    drain_stopped_scenarios(scenarios, &banner_emitted, |name, stats| {
        let banner = format_stopped_line_plain(name, stats, final_elapsed);
        eprintln!("{banner}");
    });
}

/// Invoke `emit` for every scenario whose `alive` flag is false and whose
/// banner has not yet been recorded in `banner_emitted`.
///
/// Used at loop exit so that a fast-dying scenario whose stop raced the
/// polling cadence still gets its STOPPED banner.
fn drain_stopped_scenarios<F>(
    scenarios: &[MonitoredScenario],
    banner_emitted: &HashSet<String>,
    mut emit: F,
) where
    F: FnMut(&str, &ScenarioStats),
{
    for scenario in scenarios {
        if banner_emitted.contains(&scenario.name) {
            continue;
        }
        if !scenario.alive.load(Ordering::SeqCst) {
            let stats = read_stats(&scenario.stats);
            emit(&scenario.name, &stats);
        }
    }
}

/// Format a one-shot STOPPED banner for non-TTY output.
fn format_stopped_line_plain(name: &str, stats: &ScenarioStats, elapsed: Duration) -> String {
    let error_clause = match stats.last_sink_error.as_ref() {
        Some(e) => format!(" (sink: {e})"),
        None => String::new(),
    };
    format!(
        "[progress] {name}  STOPPED{error_clause} | events: {events} | bytes: {bytes} | elapsed: {elapsed_str}",
        events = stats.total_events,
        bytes = format_bytes(stats.bytes_emitted),
        elapsed_str = format_elapsed_plain(elapsed),
    )
}

/// Format a one-shot STOPPED banner for TTY output.
fn format_stopped_line_tty(name: &str, stats: &ScenarioStats, elapsed: Duration) -> String {
    let bold_name = format!("{}", name.if_supports_color(Stderr, |t| t.bold()));
    let label = format!("{}", "STOPPED".if_supports_color(Stderr, |t| t.red()));
    let pipe = format!("{}", "|".if_supports_color(Stderr, |t| t.dimmed()));
    let error_clause = match stats.last_sink_error.as_ref() {
        Some(e) => format!(" (sink: {e})"),
        None => String::new(),
    };
    let events_label = format!("{}", "events:".if_supports_color(Stderr, |t| t.dimmed()));
    let events_value = format_count(stats.total_events);
    let bytes_label = format!("{}", "bytes:".if_supports_color(Stderr, |t| t.dimmed()));
    let bytes_value = format!(
        "{}",
        format_bytes(stats.bytes_emitted).if_supports_color(Stderr, |t| t.cyan())
    );
    let elapsed_label = format!("{}", "elapsed:".if_supports_color(Stderr, |t| t.dimmed()));
    let elapsed_value = format_elapsed(elapsed);
    format!(
        "  {label} {bold_name}{error_clause}  {events_label} {events_value} {pipe} {bytes_label} {bytes_value} {pipe} {elapsed_label} {elapsed_value}"
    )
}

/// Format a TTY progress line for a single scenario.
///
/// Example: `  ~ cpu_usage  events: 1,234 | rate: 98.5/s | bytes: 12.3 KB | elapsed: 5.2s`
fn format_tty_line(
    name: &str,
    stats: &ScenarioStats,
    target_rate: f64,
    elapsed: Duration,
) -> String {
    let indicator = format_indicator(stats);
    let bold_name = format!("{}", name.if_supports_color(Stderr, |t| t.bold()));
    let pipe = format!("{}", "|".if_supports_color(Stderr, |t| t.dimmed()));

    let events_label = format!("{}", "events:".if_supports_color(Stderr, |t| t.dimmed()));
    let events_value = format_count(stats.total_events);

    let rate_label = format!("{}", "rate:".if_supports_color(Stderr, |t| t.dimmed()));
    let rate_value = format_rate_with_target(stats.current_rate, target_rate);

    let bytes_label = format!("{}", "bytes:".if_supports_color(Stderr, |t| t.dimmed()));
    let bytes_value = format!(
        "{}",
        format_bytes(stats.bytes_emitted).if_supports_color(Stderr, |t| t.cyan())
    );

    let elapsed_label = format!("{}", "elapsed:".if_supports_color(Stderr, |t| t.dimmed()));
    let elapsed_value = format_elapsed(elapsed);

    let window_tag = format_window_tag(stats);

    format!(
        "  {indicator} {bold_name}  {events_label} {events_value} {pipe} {rate_label} {rate_value} {pipe} {bytes_label} {bytes_value} {pipe} {elapsed_label} {elapsed_value}{window_tag}"
    )
}

/// Format a non-TTY progress line for a single scenario.
///
/// Example: `[progress] cpu_usage  events: 1234 | rate: 98.5/s | bytes: 12.3 KB | elapsed: 5.2s`
fn format_non_tty_line(
    name: &str,
    stats: &ScenarioStats,
    target_rate: f64,
    elapsed: Duration,
) -> String {
    let events = stats.total_events;
    let rate = format_rate_plain(stats.current_rate, target_rate);
    let bytes = format_bytes(stats.bytes_emitted);
    let elapsed_str = format_elapsed_plain(elapsed);
    let window = format_window_tag_plain(stats);

    format!(
        "[progress] {name}  events: {events} | rate: {rate} | bytes: {bytes} | elapsed: {elapsed_str}{window}"
    )
}

/// Format the spinning/status indicator character.
///
/// Shows a colored `~` (tilde) as a subtle activity indicator. The color
/// reflects the scenario state: green for normal operation, yellow for
/// gap windows, magenta for burst windows.
fn format_indicator(stats: &ScenarioStats) -> String {
    if stats.in_gap {
        format!("{}", "~".if_supports_color(Stderr, |t| t.yellow()))
    } else if stats.in_burst {
        format!("{}", "~".if_supports_color(Stderr, |t| t.magenta()))
    } else {
        format!("{}", "~".if_supports_color(Stderr, |t| t.green()))
    }
}

/// Format the window state tag (gap, burst, spike) for TTY output.
///
/// Returns an empty string when no special window is active.
fn format_window_tag(stats: &ScenarioStats) -> String {
    let mut tags = Vec::new();
    if stats.in_gap {
        tags.push(format!(
            "{}",
            "[gap]".if_supports_color(Stderr, |t| t.yellow())
        ));
    }
    if stats.in_burst {
        tags.push(format!(
            "{}",
            "[burst]".if_supports_color(Stderr, |t| t.magenta())
        ));
    }
    if stats.in_cardinality_spike {
        tags.push(format!(
            "{}",
            "[spike]".if_supports_color(Stderr, |t| t.red())
        ));
    }
    if tags.is_empty() {
        String::new()
    } else {
        format!(" {}", tags.join(" "))
    }
}

/// Format window state tags for non-TTY output (no ANSI codes).
fn format_window_tag_plain(stats: &ScenarioStats) -> String {
    let mut tags = Vec::new();
    if stats.in_gap {
        tags.push("[gap]");
    }
    if stats.in_burst {
        tags.push("[burst]");
    }
    if stats.in_cardinality_spike {
        tags.push("[spike]");
    }
    if tags.is_empty() {
        String::new()
    } else {
        format!(" {}", tags.join(" "))
    }
}

/// Format the current rate with color feedback relative to the target.
///
/// Green when within 80-120% of target, yellow otherwise.
fn format_rate_with_target(current: f64, target: f64) -> String {
    let rate_str = format!("{:.1}/s", current);
    let ratio = if target > 0.0 { current / target } else { 1.0 };

    if (0.8..=1.2).contains(&ratio) {
        format!("{}", rate_str.if_supports_color(Stderr, |t| t.green()))
    } else {
        format!("{}", rate_str.if_supports_color(Stderr, |t| t.yellow()))
    }
}

/// Format a rate value for non-TTY output (no ANSI codes).
fn format_rate_plain(current: f64, _target: f64) -> String {
    format!("{:.1}/s", current)
}

/// Format an event count with thousand separators for readability.
fn format_count(n: u64) -> String {
    if n < 1_000 {
        return format!("{}", n.if_supports_color(Stderr, |t| t.green()));
    }

    // Format with commas: 1,234,567
    let s = n.to_string();
    let mut result = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            result.push(',');
        }
        result.push(ch);
    }
    let formatted: String = result.chars().rev().collect();
    format!("{}", formatted.if_supports_color(Stderr, |t| t.green()))
}

/// Format elapsed time as a human-readable string.
fn format_elapsed(elapsed: Duration) -> String {
    let secs = elapsed.as_secs_f64();
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else if secs < 3600.0 {
        let mins = (secs / 60.0).floor() as u64;
        let remaining = secs - (mins as f64 * 60.0);
        format!("{mins}m{remaining:.0}s")
    } else {
        let hours = (secs / 3600.0).floor() as u64;
        let remaining_mins = ((secs - hours as f64 * 3600.0) / 60.0).floor() as u64;
        format!("{hours}h{remaining_mins}m")
    }
}

/// Format elapsed time for non-TTY output (plain text).
fn format_elapsed_plain(elapsed: Duration) -> String {
    format_elapsed(elapsed)
}

/// Format a byte count as a human-readable string with appropriate units.
fn format_bytes(bytes: u64) -> String {
    const KB: u64 = 1024;
    const MB: u64 = 1024 * 1024;
    const GB: u64 = 1024 * 1024 * 1024;

    if bytes < KB {
        format!("{bytes} B")
    } else if bytes < MB {
        format!("{:.1} KB", bytes as f64 / KB as f64)
    } else if bytes < GB {
        format!("{:.1} MB", bytes as f64 / MB as f64)
    } else {
        format!("{:.1} GB", bytes as f64 / GB as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // -----------------------------------------------------------------------
    // format_bytes: all unit thresholds
    // -----------------------------------------------------------------------

    #[test]
    fn format_bytes_zero() {
        assert_eq!(format_bytes(0), "0 B");
    }

    #[test]
    fn format_bytes_below_kb() {
        assert_eq!(format_bytes(500), "500 B");
    }

    #[test]
    fn format_bytes_one_kb() {
        assert_eq!(format_bytes(1024), "1.0 KB");
    }

    #[test]
    fn format_bytes_one_mb() {
        assert_eq!(format_bytes(1_048_576), "1.0 MB");
    }

    #[test]
    fn format_bytes_one_gb() {
        assert_eq!(format_bytes(1_073_741_824), "1.0 GB");
    }

    // -----------------------------------------------------------------------
    // format_elapsed: time formatting
    // -----------------------------------------------------------------------

    #[test]
    fn format_elapsed_seconds_only() {
        let d = Duration::from_secs_f64(5.3);
        assert_eq!(format_elapsed(d), "5.3s");
    }

    #[test]
    fn format_elapsed_minutes_and_seconds() {
        let d = Duration::from_secs(90);
        assert_eq!(format_elapsed(d), "1m30s");
    }

    #[test]
    fn format_elapsed_hours_and_minutes() {
        let d = Duration::from_secs(3661);
        assert_eq!(format_elapsed(d), "1h1m");
    }

    #[test]
    fn format_elapsed_zero() {
        let d = Duration::ZERO;
        assert_eq!(format_elapsed(d), "0.0s");
    }

    #[test]
    fn format_elapsed_exactly_one_minute() {
        let d = Duration::from_secs(60);
        assert_eq!(format_elapsed(d), "1m0s");
    }

    #[test]
    fn format_elapsed_exactly_one_hour() {
        let d = Duration::from_secs(3600);
        assert_eq!(format_elapsed(d), "1h0m");
    }

    // -----------------------------------------------------------------------
    // format_count: thousand separators
    // -----------------------------------------------------------------------

    /// Helper that strips ANSI escape codes for testing formatted counts.
    fn strip_ansi(s: &str) -> String {
        let mut result = String::new();
        let mut in_escape = false;
        for ch in s.chars() {
            if ch == '\x1b' {
                in_escape = true;
            } else if in_escape {
                if ch.is_ascii_alphabetic() {
                    in_escape = false;
                }
            } else {
                result.push(ch);
            }
        }
        result
    }

    #[test]
    fn format_count_small_number() {
        let s = strip_ansi(&format_count(42));
        assert_eq!(s, "42");
    }

    #[test]
    fn format_count_thousands() {
        let s = strip_ansi(&format_count(1234));
        assert_eq!(s, "1,234");
    }

    #[test]
    fn format_count_millions() {
        let s = strip_ansi(&format_count(1_234_567));
        assert_eq!(s, "1,234,567");
    }

    #[test]
    fn format_count_zero() {
        let s = strip_ansi(&format_count(0));
        assert_eq!(s, "0");
    }

    #[test]
    fn format_count_exactly_one_thousand() {
        let s = strip_ansi(&format_count(1000));
        assert_eq!(s, "1,000");
    }

    // -----------------------------------------------------------------------
    // format_window_tag_plain: window state tags
    // -----------------------------------------------------------------------

    #[test]
    fn window_tag_plain_no_windows_active() {
        let stats = ScenarioStats::default();
        assert_eq!(format_window_tag_plain(&stats), "");
    }

    #[test]
    fn window_tag_plain_gap_active() {
        // `ScenarioStats` is `#[non_exhaustive]` across the crate boundary,
        // so struct-literal construction is forbidden here. Start from
        // `Default::default()` and set the fields the test cares about.
        let mut stats = ScenarioStats::default();
        stats.in_gap = true;
        assert_eq!(format_window_tag_plain(&stats), " [gap]");
    }

    #[test]
    fn window_tag_plain_burst_active() {
        let mut stats = ScenarioStats::default();
        stats.in_burst = true;
        assert_eq!(format_window_tag_plain(&stats), " [burst]");
    }

    #[test]
    fn window_tag_plain_spike_active() {
        let mut stats = ScenarioStats::default();
        stats.in_cardinality_spike = true;
        assert_eq!(format_window_tag_plain(&stats), " [spike]");
    }

    #[test]
    fn window_tag_plain_multiple_windows_active() {
        let mut stats = ScenarioStats::default();
        stats.in_burst = true;
        stats.in_cardinality_spike = true;
        assert_eq!(format_window_tag_plain(&stats), " [burst] [spike]");
    }

    // -----------------------------------------------------------------------
    // format_rate_plain: rate formatting
    // -----------------------------------------------------------------------

    #[test]
    fn format_rate_plain_normal_rate() {
        assert_eq!(format_rate_plain(99.5, 100.0), "99.5/s");
    }

    #[test]
    fn format_rate_plain_zero_rate() {
        assert_eq!(format_rate_plain(0.0, 100.0), "0.0/s");
    }

    // -----------------------------------------------------------------------
    // format_non_tty_line: complete line formatting
    // -----------------------------------------------------------------------

    #[test]
    fn non_tty_line_contains_scenario_name() {
        let mut stats = ScenarioStats::default();
        stats.total_events = 42;
        stats.bytes_emitted = 1024;
        stats.current_rate = 10.0;
        let line = format_non_tty_line("cpu_usage", &stats, 10.0, Duration::from_secs(5));
        assert!(
            line.contains("cpu_usage"),
            "line must contain scenario name"
        );
        assert!(
            line.contains("[progress]"),
            "line must contain [progress] prefix"
        );
        assert!(line.contains("42"), "line must contain event count");
    }

    #[test]
    fn non_tty_line_shows_window_state() {
        let mut stats = ScenarioStats::default();
        stats.in_burst = true;
        let line = format_non_tty_line("test", &stats, 10.0, Duration::from_secs(1));
        assert!(
            line.contains("[burst]"),
            "line must show burst window state"
        );
    }

    // -----------------------------------------------------------------------
    // ProgressDisplay: lifecycle
    // -----------------------------------------------------------------------

    fn alive_flag() -> Arc<AtomicBool> {
        Arc::new(AtomicBool::new(true))
    }

    #[test]
    fn progress_display_starts_and_stops_cleanly() {
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let display = ProgressDisplay::start(vec![("test".to_string(), stats, 10.0, alive_flag())]);
        // Give the thread a moment to start.
        thread::sleep(Duration::from_millis(50));
        display.stop();
        // If we get here without hanging, the lifecycle works.
    }

    #[test]
    fn progress_display_handles_multiple_scenarios() {
        let stats1 = Arc::new(RwLock::new(ScenarioStats::default()));
        let stats2 = Arc::new(RwLock::new(ScenarioStats::default()));
        let display = ProgressDisplay::start(vec![
            ("scenario-1".to_string(), stats1, 10.0, alive_flag()),
            ("scenario-2".to_string(), stats2, 20.0, alive_flag()),
        ]);
        thread::sleep(Duration::from_millis(50));
        display.stop();
    }

    #[test]
    fn progress_display_drop_signals_stop_without_panic() {
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let display =
            ProgressDisplay::start(vec![("drop-test".to_string(), stats, 10.0, alive_flag())]);
        // Drop without calling stop() — should not panic.
        drop(display);
        // Give the thread a moment to notice the flag.
        thread::sleep(Duration::from_millis(300));
    }

    #[test]
    fn progress_display_reads_updated_stats() {
        let stats = Arc::new(RwLock::new(ScenarioStats::default()));
        let stats_writer = Arc::clone(&stats);
        let display =
            ProgressDisplay::start(vec![("live-test".to_string(), stats, 100.0, alive_flag())]);

        // Simulate the runner updating stats.
        {
            let mut s = stats_writer.write().expect("lock must not be poisoned");
            s.total_events = 500;
            s.bytes_emitted = 4096;
            s.current_rate = 95.0;
        }

        // Give the display a moment to poll.
        thread::sleep(Duration::from_millis(300));
        display.stop();
        // The test passes if no panics occurred during read.
    }

    #[test]
    fn format_stopped_line_plain_includes_stopped_label() {
        let mut stats = ScenarioStats::default();
        stats.total_events = 100;
        stats.bytes_emitted = 1024;
        let line = format_stopped_line_plain("svc", &stats, Duration::from_secs(5));
        assert!(line.contains("STOPPED"), "missing STOPPED in: {line}");
        assert!(line.contains("svc"));
        assert!(line.contains("events: 100"));
    }

    #[test]
    fn format_stopped_line_plain_with_error_includes_sink_clause() {
        let mut stats = ScenarioStats::default();
        stats.last_sink_error = Some("connection refused".to_string());
        let line = format_stopped_line_plain("svc", &stats, Duration::from_secs(1));
        assert!(line.contains("(sink: connection refused)"), "got: {line}");
    }

    #[test]
    fn format_stopped_line_plain_clean_shutdown_has_no_parenthetical() {
        let stats = ScenarioStats::default();
        let line = format_stopped_line_plain("svc", &stats, Duration::from_secs(1));
        assert!(
            !line.contains("(sink:"),
            "must not include sink clause for clean shutdown: {line}"
        );
    }

    #[test]
    fn format_stopped_line_tty_includes_stopped_label() {
        let stats = ScenarioStats::default();
        let line = format_stopped_line_tty("svc", &stats, Duration::from_secs(1));
        // Strip ANSI for content check
        assert!(strip_ansi(&line).contains("STOPPED"));
        assert!(line.contains("svc"));
    }

    // -----------------------------------------------------------------------
    // Contract: ProgressDisplay is Send
    // -----------------------------------------------------------------------

    #[test]
    fn progress_display_is_send() {
        fn assert_send<T: Send>() {}
        assert_send::<ProgressDisplay>();
    }

    // -----------------------------------------------------------------------
    // drain_stopped_scenarios: post-loop cleanup of dead scenarios
    // -----------------------------------------------------------------------

    fn dead_scenario(name: &str) -> MonitoredScenario {
        MonitoredScenario {
            name: name.to_string(),
            stats: Arc::new(RwLock::new(ScenarioStats::default())),
            target_rate: 10.0,
            alive: Arc::new(AtomicBool::new(false)),
        }
    }

    fn live_scenario(name: &str) -> MonitoredScenario {
        MonitoredScenario {
            name: name.to_string(),
            stats: Arc::new(RwLock::new(ScenarioStats::default())),
            target_rate: 10.0,
            alive: Arc::new(AtomicBool::new(true)),
        }
    }

    #[test]
    fn drain_stopped_scenarios_emits_for_dead_unbannered() {
        let scenarios = vec![dead_scenario("died_fast")];
        let emitted = HashSet::new();
        let mut names: Vec<String> = Vec::new();
        drain_stopped_scenarios(&scenarios, &emitted, |name, _stats| {
            names.push(name.to_string());
        });
        assert_eq!(names, vec!["died_fast".to_string()]);
    }

    #[test]
    fn drain_stopped_scenarios_skips_already_bannered() {
        let scenarios = vec![dead_scenario("died_first"), dead_scenario("died_second")];
        let mut emitted = HashSet::new();
        emitted.insert("died_first".to_string());
        let mut names: Vec<String> = Vec::new();
        drain_stopped_scenarios(&scenarios, &emitted, |name, _stats| {
            names.push(name.to_string());
        });
        assert_eq!(names, vec!["died_second".to_string()]);
    }

    #[test]
    fn drain_stopped_scenarios_skips_live_scenarios() {
        let scenarios = vec![live_scenario("still_running")];
        let emitted = HashSet::new();
        let mut names: Vec<String> = Vec::new();
        drain_stopped_scenarios(&scenarios, &emitted, |name, _stats| {
            names.push(name.to_string());
        });
        assert!(
            names.is_empty(),
            "live scenarios must not receive a final banner"
        );
    }

    #[test]
    fn drain_stopped_scenarios_passes_stats_snapshot() {
        let scenario = dead_scenario("with_error");
        {
            let mut s = scenario.stats.write().expect("lock");
            s.last_sink_error = Some("boom".to_string());
            s.total_events = 7;
        }
        let scenarios = vec![scenario];
        let emitted = HashSet::new();
        let mut captured: Option<ScenarioStats> = None;
        drain_stopped_scenarios(&scenarios, &emitted, |_name, stats| {
            captured = Some(stats.clone());
        });
        let snap = captured.expect("must emit once");
        assert_eq!(snap.total_events, 7);
        assert_eq!(snap.last_sink_error.as_deref(), Some("boom"));
    }
}
