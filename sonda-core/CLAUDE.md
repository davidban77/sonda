# sonda-core — The Engine

This is the library crate. It owns **all** domain logic. If it generates signals, schedules events,
encodes data, or delivers output — it lives here.

## Module Layout

```
src/
├── lib.rs              ← public API surface, re-exports
├── model/
│   ├── mod.rs          ← module declarations
│   ├── metric.rs       ← MetricEvent, Labels
│   └── log.rs          ← LogEvent (post-MVP)
├── generator/
│   ├── mod.rs          ← ValueGenerator trait + factory
│   ├── constant.rs
│   ├── uniform.rs
│   ├── sine.rs
│   ├── sawtooth.rs
│   ├── counter.rs
│   ├── gauge.rs        ← random-walk gauge style
│   └── microburst.rs
├── schedule/
│   ├── mod.rs          ← Scheduler, GapWindow, BurstWindow
│   └── runner.rs       ← the main event loop
├── encoder/
│   ├── mod.rs          ← Encoder trait + factory
│   ├── prometheus.rs   ← Prometheus text exposition format
│   ├── influx.rs       ← Influx Line Protocol (post-MVP)
│   └── json.rs         ← JSON Lines (post-MVP)
├── sink/
│   ├── mod.rs          ← Sink trait + factory
│   ├── stdout.rs       ← BufWriter<Stdout>
│   ├── file.rs         ← BufWriter<File>
│   ├── tcp.rs          ← TCP socket (BufWriter<TcpStream>)
│   ├── udp.rs          ← UDP socket (UdpSocket)
│   └── http.rs         ← HTTP push sink (ureq)
└── config/
    ├── mod.rs          ← ScenarioConfig, deserialization
    └── validate.rs     ← config validation logic
```

## How to Add a New Generator

1. Create `src/generator/your_name.rs` with a struct that implements `ValueGenerator`.
2. The struct must be `Send + Sync`. Store configuration in the struct fields.
3. Implement `fn value(&self, tick: u64) -> f64`. This is a pure function — no side effects.
4. Register it in `src/generator/mod.rs`:
   - Add a variant to the `GeneratorConfig` enum (serde-tagged).
   - Add a match arm in `create_generator()` that returns `Box::new(YourGenerator::new(...))`.
5. Add unit tests in the same file: test determinism, edge cases, boundary values.
6. Update the YAML config schema doc if it exists.

## How to Add a New Encoder

Same pattern as generators:
1. Create `src/encoder/your_format.rs` implementing the `Encoder` trait.
2. The `encode_metric` method writes into a caller-provided `&mut Vec<u8>`. Never allocate a new
   buffer — reuse what is given.
3. Pre-build any invariant content (label prefix, metric name validation) in `new()`. The encode
   method should do as little work as possible per event.
4. Register in `src/encoder/mod.rs` factory.
5. Test with known inputs → expected byte output. Use `assert_eq!(String::from_utf8(buf).unwrap(), ...)`.

## How to Add a New Sink

1. Create `src/sink/your_sink.rs` implementing the `Sink` trait.
2. `write(&mut self, data: &[u8])` delivers one encoded event. `flush(&mut self)` forces delivery.
3. Sinks own their I/O resources (file handles, sockets, etc.).
4. Register in `src/sink/mod.rs` factory.
5. Test with a mock or in-memory buffer sink.

## Performance Guidelines

- **No per-event allocations.** The hot path is: generate value → build MetricEvent → encode into
  buffer → write to sink. Each step should write into pre-allocated or caller-provided memory.
- **Pre-build label strings.** Labels don't change between events for a given scenario. Build the
  serialized label prefix once at construction time.
- **Use `BufWriter`.** Never write individual lines to stdout or files without buffering.
- **Benchmark before optimizing.** Use `cargo bench` with criterion if you suspect a bottleneck.
  Don't optimize speculatively.

## Error Handling

- Define errors in `src/lib.rs` or a dedicated `error.rs` using `thiserror`.
- Every public function returns `Result<T, SondaError>`.
- Never `unwrap()` in this crate. Use `?` propagation or explicit error mapping.
- I/O errors from sinks should be wrapped in `SondaError::Sink(...)` with context.

## Testing

- Every generator: test at tick=0, tick=1, tick at period boundary, tick at large values.
- Every encoder: test with known MetricEvent → assert exact byte output.
- Schedule logic: test gap window math, burst window transitions, rate calculation.
- Use `#[cfg(test)] mod tests` at the bottom of each file.
- Seed all RNG-based generators for deterministic tests.
