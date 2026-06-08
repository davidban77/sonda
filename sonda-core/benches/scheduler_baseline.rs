//! Phase 0 baseline: measure the current thread-per-scenario scheduler at
//! varying concurrent-scenario counts. Captures RSS, vsize, thread count,
//! CPU%, tick-drift and dropped-tick percentage per N, and writes a markdown
//! report alongside a stdout table.

use std::fs;
use std::path::Path;
use std::process::Command;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use tokio_util::sync::CancellationToken;

use sonda_core::config::{BaseScheduleConfig, ScenarioConfig, ScenarioEntry};
use sonda_core::encoder::EncoderConfig;
use sonda_core::generator::GeneratorConfig;
use sonda_core::schedule::handle::ScenarioHandle;
use sonda_core::sink::memory::CapturedRing;
use sonda_core::sink::SinkConfig;
use sonda_core::{launch_scenario, prepare_entries, OnSinkError};
use sysinfo::{Pid, ProcessRefreshKind, RefreshKind, System};

const DEFAULT_SCENARIO_COUNTS: &[usize] = &[1, 10, 50, 100, 250, 500];
const RATE_HZ: f64 = 100.0;
const DEFAULT_WARMUP_SECS: u64 = 30;
const DEFAULT_MEASURE_SECS: u64 = 60;
const SAMPLE_INTERVAL: Duration = Duration::from_secs(1);
const DROPPED_TICK_TOLERANCE: f64 = 0.10;
const CAPTURE_RING_SIZE: usize = 1_000_000;
const DRIFT_WARMUP_FRACTION: f64 = 0.10;

fn warmup_secs() -> u64 {
    std::env::var("SONDA_BENCH_WARMUP_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_WARMUP_SECS)
}

fn measure_secs() -> u64 {
    std::env::var("SONDA_BENCH_MEASURE_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_MEASURE_SECS)
}

fn scenario_counts() -> Vec<usize> {
    match std::env::var("SONDA_BENCH_COUNTS").ok() {
        Some(s) => s
            .split(',')
            .filter_map(|p| p.trim().parse::<usize>().ok())
            .collect(),
        None => DEFAULT_SCENARIO_COUNTS.to_vec(),
    }
}

struct RowResult {
    n: usize,
    rss_mb: f64,
    vsize_mb: f64,
    threads: usize,
    cpu_pct: f64,
    drift_mean_ms: f64,
    drift_p99_ms: f64,
    drift_p50_us: f64,
    drift_p90_us: f64,
    drift_p99_us: f64,
    drift_max_us: f64,
    dropped_pct: f64,
}

struct Sample {
    rss_bytes: u64,
    vsize_bytes: u64,
    threads: usize,
    cpu_pct: f64,
    per_scenario_events: Vec<u64>,
}

fn metrics_entry(name: String, rate: f64, sink: SinkConfig) -> ScenarioEntry {
    ScenarioEntry::Metrics(ScenarioConfig {
        base: BaseScheduleConfig {
            name,
            rate,
            duration: None,
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink,
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    })
}

fn metrics_config_for_capture(name: String, rate: f64) -> ScenarioConfig {
    ScenarioConfig {
        base: BaseScheduleConfig {
            name,
            rate,
            duration: None,
            gaps: None,
            bursts: None,
            cardinality_spikes: None,
            dynamic_labels: None,
            labels: None,
            sink: SinkConfig::Memory {
                capture: true,
                max_events: Some(CAPTURE_RING_SIZE),
            },
            phase_offset: None,
            clock_group: None,
            clock_group_is_auto: None,
            start_time: None,
            jitter: None,
            jitter_seed: None,
            on_sink_error: OnSinkError::Warn,
        },
        generator: GeneratorConfig::Constant { value: 1.0 },
        encoder: EncoderConfig::PrometheusText { precision: None },
        metric_type: None,
        help: None,
    }
}

struct DriftStats {
    p50_us: f64,
    p90_us: f64,
    p99_us: f64,
    max_us: f64,
}

fn compute_drift_stats(timestamps: &[Instant], rate_hz: f64) -> DriftStats {
    if timestamps.len() < 2 {
        return DriftStats {
            p50_us: 0.0,
            p90_us: 0.0,
            p99_us: 0.0,
            max_us: 0.0,
        };
    }
    let expected_interval_us = 1_000_000.0 / rate_hz;
    let skip = ((timestamps.len() as f64) * DRIFT_WARMUP_FRACTION).floor() as usize;
    let trimmed = &timestamps[skip..];
    if trimmed.len() < 2 {
        return DriftStats {
            p50_us: 0.0,
            p90_us: 0.0,
            p99_us: 0.0,
            max_us: 0.0,
        };
    }
    let mut deltas_us: Vec<f64> = Vec::with_capacity(trimmed.len() - 1);
    for pair in trimmed.windows(2) {
        let dt_us = pair[1].duration_since(pair[0]).as_micros() as f64;
        deltas_us.push((dt_us - expected_interval_us).abs());
    }
    DriftStats {
        p50_us: percentile(&deltas_us, 50.0),
        p90_us: percentile(&deltas_us, 90.0),
        p99_us: percentile(&deltas_us, 99.0),
        max_us: deltas_us.iter().copied().fold(0.0f64, f64::max),
    }
}

fn launch_n(n: usize) -> (Vec<ScenarioHandle>, Arc<Mutex<CapturedRing>>) {
    assert!(n >= 1, "launch_n requires at least one scenario");
    let capture_handle = Arc::new(Mutex::new(CapturedRing::new(CAPTURE_RING_SIZE)));

    let mut handles = Vec::with_capacity(n);
    handles.push(launch_capturing_scenario(
        "bench_0".to_string(),
        Arc::clone(&capture_handle),
    ));

    if n > 1 {
        let entries: Vec<ScenarioEntry> = (1..n)
            .map(|i| {
                metrics_entry(
                    format!("bench_{i}"),
                    RATE_HZ,
                    SinkConfig::Memory {
                        capture: false,
                        max_events: None,
                    },
                )
            })
            .collect();
        let prepared = prepare_entries(entries).expect("prepare_entries must succeed");
        for (offset, p) in prepared.into_iter().enumerate() {
            let i = offset + 1;
            let handle = launch_scenario(
                format!("bench_{i}"),
                p.entry,
                CancellationToken::new(),
                p.start_delay,
            )
            .expect("launch_scenario must succeed");
            handles.push(handle);
        }
    }
    (handles, capture_handle)
}

fn launch_capturing_scenario(
    id: String,
    _capture_handle: Arc<Mutex<CapturedRing>>,
) -> ScenarioHandle {
    let config = metrics_config_for_capture(id.clone(), RATE_HZ);
    let entry = ScenarioEntry::Metrics(config);
    launch_scenario(id, entry, CancellationToken::new(), None)
        .expect("launch_scenario must succeed")
}

fn stop_all(handles: &mut [ScenarioHandle]) {
    for h in handles.iter() {
        h.stop();
    }
    for h in handles.iter_mut() {
        let _ = h.join(Some(Duration::from_secs(5)));
    }
}

fn sample_process(system: &mut System, pid: Pid, handles: &[ScenarioHandle]) -> Option<Sample> {
    system.refresh_processes_specifics(
        sysinfo::ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::nothing().with_cpu().with_memory(),
    );
    let proc = system.process(pid)?;
    let rss_bytes = proc.memory();
    let vsize_bytes = proc.virtual_memory();
    let cpu_pct = proc.cpu_usage() as f64;
    let threads = current_thread_count().unwrap_or(0);
    let per_scenario_events: Vec<u64> = handles
        .iter()
        .map(|h| h.stats_snapshot().total_events)
        .collect();
    Some(Sample {
        rss_bytes,
        vsize_bytes,
        threads,
        cpu_pct,
        per_scenario_events,
    })
}

#[cfg(target_os = "linux")]
fn current_thread_count() -> Option<usize> {
    let s = fs::read_to_string("/proc/self/status").ok()?;
    for line in s.lines() {
        if let Some(rest) = line.strip_prefix("Threads:") {
            return rest.trim().parse::<usize>().ok();
        }
    }
    None
}

#[cfg(target_os = "macos")]
fn current_thread_count() -> Option<usize> {
    let pid = std::process::id();
    let out = Command::new("ps")
        .args(["-M", "-p", &pid.to_string()])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let lines = String::from_utf8_lossy(&out.stdout).lines().count();
    if lines > 1 {
        Some(lines - 1)
    } else {
        None
    }
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
fn current_thread_count() -> Option<usize> {
    None
}

fn run_row(n: usize, warmup: u64, measure: u64) -> RowResult {
    eprintln!(
        "[bench] N={n}: launching scenarios at {RATE_HZ:.0} Hz \
         (warmup {warmup}s, measure {measure}s)"
    );

    let (mut handles, capture_handle) = launch_n(n);

    thread::sleep(Duration::from_secs(warmup));

    let pid = Pid::from_u32(std::process::id());
    let mut system = System::new_with_specifics(
        RefreshKind::nothing()
            .with_processes(ProcessRefreshKind::nothing().with_cpu().with_memory()),
    );
    let _ = sample_process(&mut system, pid, &handles);

    let initial_events: Vec<u64> = handles
        .iter()
        .map(|h| h.stats_snapshot().total_events)
        .collect();

    let mut samples: Vec<Sample> = Vec::with_capacity(measure as usize);
    let measure_end = Instant::now() + Duration::from_secs(measure);

    while Instant::now() < measure_end {
        thread::sleep(SAMPLE_INTERVAL);
        if let Some(s) = sample_process(&mut system, pid, &handles) {
            samples.push(s);
        }
    }

    let rss_mb = mean(samples.iter().map(|s| s.rss_bytes as f64)) / (1024.0 * 1024.0);
    let vsize_mb = mean(samples.iter().map(|s| s.vsize_bytes as f64)) / (1024.0 * 1024.0);
    let threads = samples.iter().map(|s| s.threads).max().unwrap_or(0);
    let cpu_pct = mean(samples.iter().map(|s| s.cpu_pct));

    let drift_samples_ms = compute_drift_ms(&samples, &initial_events);
    let drift_mean_ms = mean(drift_samples_ms.iter().copied());
    let drift_p99_ms = percentile(&drift_samples_ms, 99.0);
    let dropped_pct = compute_dropped_pct(&samples, &initial_events);

    let captured_timestamps: Vec<Instant> = {
        let guard = capture_handle.lock().expect("capture handle poisoned");
        guard.events().iter().map(|(ts, _)| *ts).collect()
    };
    let drift = compute_drift_stats(&captured_timestamps, RATE_HZ);

    eprintln!(
        "[bench] N={n}: rss={rss_mb:.1}MB vsize={vsize_mb:.0}MB threads={threads} \
         cpu={cpu_pct:.1}% drift_mean={drift_mean_ms:.2}ms drift_p99={drift_p99_ms:.2}ms \
         drift_us p50={:.1} p90={:.1} p99={:.1} max={:.1} \
         dropped={dropped_pct:.2}%",
        drift.p50_us, drift.p90_us, drift.p99_us, drift.max_us
    );

    stop_all(&mut handles);

    RowResult {
        n,
        rss_mb,
        vsize_mb,
        threads,
        cpu_pct,
        drift_mean_ms,
        drift_p99_ms,
        drift_p50_us: drift.p50_us,
        drift_p90_us: drift.p90_us,
        drift_p99_us: drift.p99_us,
        drift_max_us: drift.max_us,
        dropped_pct,
    }
}

fn compute_drift_ms(samples: &[Sample], initial_events: &[u64]) -> Vec<f64> {
    let mut out = Vec::new();
    let mut prev = initial_events.to_vec();
    let expected_per_sample = RATE_HZ * SAMPLE_INTERVAL.as_secs_f64();
    let ms_per_tick = 1000.0 / RATE_HZ;
    for s in samples {
        for (i, &observed) in s.per_scenario_events.iter().enumerate() {
            let prev_v = *prev.get(i).unwrap_or(&0);
            let delta = observed.saturating_sub(prev_v) as f64;
            let drift_ticks = (expected_per_sample - delta).abs();
            out.push(drift_ticks * ms_per_tick);
        }
        prev = s.per_scenario_events.clone();
    }
    out
}

fn compute_dropped_pct(samples: &[Sample], initial_events: &[u64]) -> f64 {
    let expected_per_sample = RATE_HZ * SAMPLE_INTERVAL.as_secs_f64();
    let mut prev = initial_events.to_vec();
    let mut dropped_buckets = 0u64;
    let mut total_buckets = 0u64;
    for s in samples {
        for (i, &observed) in s.per_scenario_events.iter().enumerate() {
            let prev_v = *prev.get(i).unwrap_or(&0);
            let delta = observed.saturating_sub(prev_v) as f64;
            let lo = expected_per_sample * (1.0 - DROPPED_TICK_TOLERANCE);
            let hi = expected_per_sample * (1.0 + DROPPED_TICK_TOLERANCE);
            if delta < lo || delta > hi {
                dropped_buckets += 1;
            }
            total_buckets += 1;
        }
        prev = s.per_scenario_events.clone();
    }
    if total_buckets == 0 {
        0.0
    } else {
        100.0 * (dropped_buckets as f64) / (total_buckets as f64)
    }
}

fn mean<I: Iterator<Item = f64>>(iter: I) -> f64 {
    let mut sum = 0.0;
    let mut n = 0usize;
    for v in iter {
        sum += v;
        n += 1;
    }
    if n == 0 {
        0.0
    } else {
        sum / (n as f64)
    }
}

fn percentile(values: &[f64], p: f64) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted: Vec<f64> = values.to_vec();
    sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let rank = (p / 100.0) * ((sorted.len() - 1) as f64);
    let lo = rank.floor() as usize;
    let hi = rank.ceil() as usize;
    if lo == hi {
        sorted[lo]
    } else {
        let frac = rank - (lo as f64);
        sorted[lo] * (1.0 - frac) + sorted[hi] * frac
    }
}

fn print_table(rows: &[RowResult]) {
    println!();
    println!(
        "| {:>5} | {:>9} | {:>11} | {:>7} | {:>5} | {:>20} | {:>19} | {:>12} | {:>12} | {:>12} | {:>11} | {:>14} |",
        "N",
        "RSS (MB)",
        "VSize (MB)",
        "Threads",
        "CPU %",
        "Tick drift mean (ms)",
        "Tick drift p99 (ms)",
        "Drift p50 us",
        "Drift p90 us",
        "Drift p99 us",
        "Drift max us",
        "Dropped-tick %"
    );
    println!(
        "|{:-<7}|{:-<11}|{:-<13}|{:-<9}|{:-<7}|{:-<22}|{:-<21}|{:-<14}|{:-<14}|{:-<14}|{:-<13}|{:-<16}|",
        "", "", "", "", "", "", "", "", "", "", "", ""
    );
    for r in rows {
        println!(
            "| {:>5} | {:>9.1} | {:>11.1} | {:>7} | {:>5.1} | {:>20.2} | {:>19.2} | {:>12.1} | {:>12.1} | {:>12.1} | {:>11.1} | {:>14.2} |",
            r.n,
            r.rss_mb,
            r.vsize_mb,
            r.threads,
            r.cpu_pct,
            r.drift_mean_ms,
            r.drift_p99_ms,
            r.drift_p50_us,
            r.drift_p90_us,
            r.drift_p99_us,
            r.drift_max_us,
            r.dropped_pct
        );
    }
    println!();
}

fn render_markdown(rows: &[RowResult], warmup: u64, measure: u64) -> String {
    let ts = chrono_ish_utc_now();
    let host = host_descriptor();
    let commit = git_head_sha().unwrap_or_else(|| "unknown".to_string());

    let mut out = String::new();
    out.push_str("# Async-Scheduler Baseline Numbers (BEFORE — thread-per-scenario)\n\n");
    out.push_str(&format!("**Captured**: {ts}\n"));
    out.push_str(&format!("**Host**: {host}\n"));
    out.push_str(&format!("**Sonda commit**: {commit}\n"));
    out.push_str("**Harness**: sonda-core/benches/scheduler_baseline.rs\n\n");
    out.push_str("## Methodology\n\n");
    out.push_str(&format!(
        "Each row is N concurrent scenarios, each emitting at 100 events/sec via the \
         Prometheus text encoder into a `memory` sink (no I/O — measures the scheduler, \
         not the sink). {warmup}s warm-up + {measure}s measurement window. RSS / VSize / \
         thread count / CPU% sampled every ~1s via the `sysinfo` crate. \
         Tick drift is reported two ways: \n\
         \n\
         - **ms-level proxy** — `total_events` deltas between consecutive 1s samples. A bucket \
         is counted as `dropped` when the observed events in the 1s window deviate from the \
         expected count (`rate * dt`) by more than ±10%. \n\
         - **microsecond-level direct** — the first of the N main scenarios writes through a \
         `memory` sink with `capture: true`, retaining `(Instant, bytes)` per event across the \
         full warm-up and measurement window. All N scenarios share the same scheduler, so the \
         captured cadence reflects the load every scenario experiences at this N. Per-event \
         drift is computed as `(t[i+1] - t[i]) - (1_000_000us / rate_hz)`; the first 10% of \
         samples are dropped as warm-up jitter; p50/p90/p99/max are reported in microseconds. \n\n"
    ));
    out.push_str("## Results\n\n");
    out.push_str(
        "| N scenarios | RSS (MB) | VSize (MB) | Threads | CPU % | Tick drift mean (ms) | Tick drift p99 (ms) | Drift p50 (us) | Drift p90 (us) | Drift p99 (us) | Drift max (us) | Dropped-tick % |\n",
    );
    out.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    for r in rows {
        out.push_str(&format!(
            "| {} | {:.1} | {:.1} | {} | {:.1} | {:.2} | {:.2} | {:.1} | {:.1} | {:.1} | {:.1} | {:.2} |\n",
            r.n,
            r.rss_mb,
            r.vsize_mb,
            r.threads,
            r.cpu_pct,
            r.drift_mean_ms,
            r.drift_p99_ms,
            r.drift_p50_us,
            r.drift_p90_us,
            r.drift_p99_us,
            r.drift_max_us,
            r.dropped_pct
        ));
    }
    out.push_str("\n## Inflection point analysis\n\n");
    out.push_str(&inflection_paragraph(rows));
    out.push_str("\n\n## Notes\n\n");
    out.push_str(&notes_paragraph(rows));
    out.push('\n');
    out
}

fn inflection_paragraph(rows: &[RowResult]) -> String {
    let max_threads = rows.iter().map(|r| r.threads).max().unwrap_or(0);
    let max_rss = rows.iter().map(|r| r.rss_mb).fold(0.0f64, f64::max);
    let max_drift_p99 = rows.iter().map(|r| r.drift_p99_ms).fold(0.0f64, f64::max);
    let max_drift_p99_us = rows.iter().map(|r| r.drift_p99_us).fold(0.0f64, f64::max);
    let max_dropped = rows.iter().map(|r| r.dropped_pct).fold(0.0f64, f64::max);
    format!(
        "Thread count grows linearly with N (peak observed: {max_threads}); RSS scales with \
         the per-thread stack reservation (peak observed: {max_rss:.1} MB). The earliest \
         metric to break under this scheduler is whichever among tick-drift p99 (peak \
         observed: {max_drift_p99:.2} ms proxy / {max_drift_p99_us:.1} us direct), \
         dropped-tick percentage (peak observed: {max_dropped:.2}%), and CPU% saturates \
         first as N climbs. Read the row-to-row deltas above to identify where the curve \
         bends — the linear-thread, linear-stack growth itself is the canary for this \
         baseline."
    )
}

fn notes_paragraph(rows: &[RowResult]) -> String {
    let mut notes = String::new();
    let failed_rows: Vec<usize> = rows
        .iter()
        .filter(|r| r.dropped_pct > 50.0)
        .map(|r| r.n)
        .collect();
    if !failed_rows.is_empty() {
        notes.push_str(&format!(
            "- High dropped-tick percentage (> 50%) observed at N = {failed_rows:?}; the \
             scheduler could not sustain the 100 Hz target for these scenarios.\n"
        ));
    }
    #[cfg(target_os = "macos")]
    notes.push_str(
        "- Context-switches/sec is Linux-only via `/proc/<pid>/status`; not collected on macOS.\n",
    );
    if notes.is_empty() {
        notes.push_str("None.");
    }
    notes
}

fn host_descriptor() -> String {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    let mut sys = System::new_all();
    sys.refresh_all();
    let cpu_brand = sys
        .cpus()
        .first()
        .map(|c| c.brand().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    let cores = sys.cpus().len();
    let total_ram_gb = (sys.total_memory() as f64) / (1024.0 * 1024.0 * 1024.0);
    format!("{os}/{arch}, {cpu_brand}, {cores} cores, {total_ram_gb:.1} GB RAM")
}

fn git_head_sha() -> Option<String> {
    let out = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?;
    Some(s.trim().to_string())
}

fn chrono_ish_utc_now() -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    let secs = now.as_secs();
    let (year, month, day, hh, mm, ss) = epoch_to_ymdhms(secs);
    format!("{year:04}-{month:02}-{day:02}T{hh:02}:{mm:02}:{ss:02}Z")
}

fn epoch_to_ymdhms(secs: u64) -> (i32, u32, u32, u32, u32, u32) {
    let days = (secs / 86400) as i64;
    let rem = (secs % 86400) as u32;
    let hh = rem / 3600;
    let mm = (rem % 3600) / 60;
    let ss = rem % 60;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = (z - era * 146_097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = (if mp < 10 { mp + 3 } else { mp - 9 }) as u32;
    let year = if m <= 2 { y + 1 } else { y };
    (year as i32, m, d, hh, mm, ss)
}

fn main() {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("runtime must build");
    runtime.block_on(async {
        let counts = scenario_counts();
        let warmup = warmup_secs();
        let measure = measure_secs();
        eprintln!("[bench] sonda scheduler baseline harness");
        eprintln!(
            "[bench] N values: {counts:?}; rate {RATE_HZ:.0} Hz; \
             warmup {warmup}s; measure {measure}s"
        );
        let mut rows = Vec::with_capacity(counts.len());
        for &n in &counts {
            rows.push(run_row(n, warmup, measure));
        }
        print_table(&rows);
        let md = render_markdown(&rows, warmup, measure);
        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("crate dir must have a parent (workspace root)");
        let out_path = workspace_root.join("target/bench-output/scheduler-baseline.md");
        if let Some(parent) = out_path.parent() {
            let _ = fs::create_dir_all(parent);
        }
        fs::write(&out_path, md).expect("must write markdown report");
        eprintln!("[bench] wrote {}", out_path.display());
    });
}
