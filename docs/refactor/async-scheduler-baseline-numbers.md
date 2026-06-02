# Async-Scheduler Baseline Numbers (BEFORE — thread-per-scenario)

**Captured**: 2026-06-02T10:14:33Z
**Host**: macos/aarch64, Apple M3 Pro, 11 cores, 36.0 GB RAM
**Sonda commit**: d5c8f5f8b43a234269c15bdf1a5ab6e7977ae9b6
**Harness**: sonda-core/benches/scheduler_baseline.rs

## Methodology

Each row is N concurrent scenarios, each emitting at 100 events/sec via the Prometheus text encoder to a `file:/dev/null` sink (no real I/O — measures the scheduler, not the sink). 30s warm-up + 60s measurement window. RSS / VSize / thread count / CPU% sampled every ~1s via the `sysinfo` crate; tick drift and dropped-tick rate computed from per-scenario `ScenarioStats::total_events` deltas between consecutive 1s samples (the production sinks do not expose per-event timestamps to the harness without a production-code change, so per-sample event-rate deviation is used as the scheduler-fidelity proxy). A bucket is counted as `dropped` when the observed events in the 1s window deviate from the expected count (`rate * dt`) by more than ±10%.

## Results

| N scenarios | RSS (MB) | VSize (MB) | Threads | CPU % | Tick drift mean (ms) | Tick drift p99 (ms) | Dropped-tick % |
|---|---|---|---|---|---|---|---|
| 1 | 8.1 | 431847.5 | 2 | 0.7 | 17.80 | 30.00 | 0.00 |
| 10 | 8.7 | 431867.2 | 11 | 3.9 | 22.19 | 40.00 | 0.00 |
| 50 | 10.0 | 431954.8 | 51 | 13.2 | 26.18 | 40.00 | 0.00 |
| 100 | 11.3 | 432064.4 | 101 | 21.0 | 22.53 | 40.00 | 0.00 |
| 250 | 15.7 | 432393.0 | 251 | 46.3 | 26.36 | 40.00 | 0.00 |
| 500 | 23.2 | 432944.8 | 501 | 87.2 | 24.08 | 40.00 | 0.00 |

## Inflection point analysis

On a beefy host (11-core Apple M3 Pro, 36 GB RAM, macOS aarch64), the thread-per-scenario scheduler is keeping up at every N tested. Tick drift stays flat at ~22-26 ms mean (with p99 ≤40 ms), and the dropped-tick rate is 0% from N=1 through N=500. The scheduler is not the runtime bottleneck on this hardware.

The **system-level inflection point on this host is CPU saturation**: CPU% climbs roughly linearly with N (0.7% at N=1 → 87.2% at N=500). At N=500 the process is consuming ~9 cores of an 11-core box, leaving little headroom for burst load or for sink I/O that takes longer than the tick interval.

The **architectural inflection point is thread count**, which is what motivates the rewrite. Threads grow 1:1 with N (peak observed: 501 threads at N=500). On macOS this is invisible — Darwin user-thread stacks are lazily paged and the RSS only climbs from 8 MB to 23 MB across the full range. Linux is the harder story: with default `ulimit -u` ≈ 4096 on most distros and 8 MB stack reservations per thread, N=500 consumes ~4 GB of *virtual* address space just for thread stacks, and N=4000 hits the thread limit hard. Production hosts in containers commonly have tighter caps (often ≤2048).

So the bench numbers understate the architectural problem on a generous host, and the rewrite's "AFTER" measurement on the same macOS host should show:

- **Thread count**: bounded by `--workers` (not N) — the headline improvement.
- **CPU%**: comparable for the same workload (the work itself is not the bottleneck; how it's scheduled is).
- **Tick drift**: comparable or better, with less context-switching overhead at high N.
- **RSS**: not meaningfully different on macOS; dramatically lower at the same N on Linux.
- **Same harness, same host**: enables a clean before/after numeric story.

The Phase 5 validation target ("1000 concurrent scenarios on a 4-core machine, ≤4 GB RSS, 16 worker threads, tick drift ≤5%") is best validated on a Linux host where the thread-count ceiling matters; on this macOS host it should be trivial.

## Notes

- VSize is huge (~432 GB) because the Rust allocator (mimalloc/jemalloc-style) reserves a large virtual address space up front. Not a useful comparative signal — RSS is the meaningful number.
- Context-switches/sec is Linux-only via `/proc/<pid>/status`; not collected on macOS.
- Sink: `file:/dev/null` (kernel-discarded writes, minimal overhead) — the goal is to measure the scheduler, not sink I/O. The `MemorySink` / `ChannelSink` types in `sonda-core` are not exposed via `SinkConfig`; surfacing them is a small follow-up captured as a candidate Phase 2.5 add if Phase 5 needs per-event timestamps for tighter drift measurement.

