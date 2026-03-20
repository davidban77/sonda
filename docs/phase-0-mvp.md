# Phase 0 — MVP Implementation Plan

**Goal:** A working CLI that generates valid Prometheus text metrics to stdout at a controlled rate,
with gap support and multiple value patterns.

**Final exit criteria:** `sonda metrics --name up --rate 1000 --duration 10s --gap-every 2m --gap-for 20s --value-mode sine --amplitude 5 --period-secs 30 --offset 10 --label hostname=t0-a1 --label zone=eu1` produces correct, valid Prometheus exposition text at the target rate.

---

## Slice 0.0 — Workspace Verification & CI

### Input state
- Workspace skeleton exists (Cargo.toml, crate stubs).
- `sonda-core` has `Constant` generator with one test.

### Specification

**Files to create:**
- `.github/workflows/ci.yml` — GitHub Actions workflow: build, test, clippy, fmt on push/PR.

**Verification steps:**
```bash
cargo build --workspace
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all -- --check
```

### Output files
| File | Status |
|------|--------|
| `.github/workflows/ci.yml` | new |

### Test criteria
- `cargo build --workspace` succeeds with zero warnings.
- `cargo test --workspace` passes (1 existing test).
- CI workflow file is valid YAML, triggers on push and PR.

### Review criteria
- CI runs build, test, clippy, fmt — in that order.
- Clippy uses `-D warnings` (deny, not warn).
- No unnecessary CI steps.

### UAT criteria
- Push to GitHub → CI runs → all steps green.

---

## Slice 0.1 — Telemetry Model

### Input state
- Slice 0.0 passes all gates.
- `sonda-core/src/model/metric.rs` exists with basic `Labels` and `MetricEvent`.

### Specification

**Files to modify:**
- `sonda-core/src/model/metric.rs`:

  **`Labels`** — add:
  - `pub fn from_pairs(pairs: &[(&str, &str)]) -> Result<Self, SondaError>` — validates label keys
    match `[a-zA-Z_][a-zA-Z0-9_]*`, rejects invalid keys with `SondaError::Config`.
  - `pub fn len(&self) -> usize`
  - `pub fn is_empty(&self) -> bool` (already exists, verify)

  **`MetricEvent`** — add:
  - `pub fn new(name: String, value: f64, labels: Labels) -> Result<Self, SondaError>` — validates
    metric name matches `[a-zA-Z_:][a-zA-Z0-9_:]*`.
  - `pub fn with_timestamp(name: String, value: f64, labels: Labels, timestamp: SystemTime) -> Result<Self, SondaError>` — for deterministic testing.

  **Validation** — implement without regex (manual char checks for zero external deps):
  - `fn is_valid_label_key(s: &str) -> bool`
  - `fn is_valid_metric_name(s: &str) -> bool`

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/model/metric.rs` | modified |

### Test criteria
- Valid `Labels::from_pairs` with multiple k/v pairs → `Ok(Labels)` with correct entries.
- Invalid label key (starts with digit, contains hyphen) → `Err(SondaError::Config(...))`.
- Valid metric names: `"up"`, `"http_requests_total"`, `"__internal"`.
- Invalid metric names: `"123bad"`, `"has-dash"`, `""` (empty).
- `MetricEvent::with_timestamp` produces event with exact timestamp provided.
- `Labels` entries are sorted by key (BTreeMap guarantee).

### Review criteria
- Validation uses manual char checks, not regex (zero-dep constraint).
- `SondaError::Config` variants have descriptive messages including the invalid input.
- No `unwrap()` anywhere.
- `///` doc comments on all public items.

### UAT criteria
- N/A (no binary behavior yet — this slice is library-only).

---

## Slice 0.2 — Value Generators

### Input state
- Slice 0.1 passes all gates.
- `Labels` and `MetricEvent` have working validation.

### Specification

**Files to create:**
- `sonda-core/src/generator/uniform.rs`:
  ```rust
  pub struct UniformRandom { min: f64, max: f64, seed: u64 }
  ```
  - `value(tick)` uses a hash-based approach: `seed XOR tick` → deterministic f64 in `[min, max]`.
  - This keeps `ValueGenerator` stateless (`&self` only).

- `sonda-core/src/generator/sine.rs`:
  ```rust
  pub struct Sine { amplitude: f64, period_ticks: f64, offset: f64 }
  ```
  - `value(tick)` = `offset + amplitude * sin(2π * tick / period_ticks)`.
  - Constructor takes `period_secs` and `rate` and pre-computes `period_ticks = period_secs * rate`.

- `sonda-core/src/generator/sawtooth.rs`:
  ```rust
  pub struct Sawtooth { min: f64, max: f64, period_ticks: f64 }
  ```
  - `value(tick)` = linear ramp from min to max, resets at period boundary.

**Files to modify:**
- `sonda-core/src/generator/mod.rs`:
  - Uncomment module declarations.
  - Add `GeneratorConfig` enum (serde-tagged):
    ```rust
    #[derive(Debug, Clone, Deserialize)]
    #[serde(tag = "type")]
    pub enum GeneratorConfig {
        #[serde(rename = "constant")]  Constant { value: f64 },
        #[serde(rename = "uniform")]   Uniform { min: f64, max: f64, seed: Option<u64> },
        #[serde(rename = "sine")]      Sine { amplitude: f64, period_secs: f64, offset: f64 },
        #[serde(rename = "sawtooth")]  Sawtooth { min: f64, max: f64, period_secs: f64 },
    }
    ```
  - Add factory: `pub fn create_generator(config: &GeneratorConfig, rate: f64) -> Box<dyn ValueGenerator>`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/generator/uniform.rs` | new |
| `sonda-core/src/generator/sine.rs` | new |
| `sonda-core/src/generator/sawtooth.rs` | new |
| `sonda-core/src/generator/mod.rs` | modified |

### Test criteria
- **Constant**: value(0) == value(1_000_000) == configured value.
- **Uniform**: same seed + same tick → same value (determinism). All values within [min, max] for 10,000 ticks. Different seeds → different sequences.
- **Sine**: value(0) == offset. value at quarter-period ≈ offset + amplitude. value at half-period ≈ offset. Symmetry: value(t) + value(t + half_period) ≈ 2 * offset.
- **Sawtooth**: value(0) == min. value(period_ticks - 1) approaches max. value(period_ticks) == min (reset).
- **Factory**: `create_generator(Constant{value: 1.0}, _)` returns a generator where `value(0) == 1.0`. Each config variant produces the correct concrete type.
- **Config deserialization**: `serde_yaml::from_str` with `type: sine` YAML → correct `GeneratorConfig::Sine`.

### Review criteria
- `ValueGenerator` trait is `&self` only (stateless, no `&mut self`).
- UniformRandom uses hash-based determinism, not stateful RNG.
- Sine/Sawtooth pre-compute `period_ticks` at construction, not per-tick.
- All structs implement `Send + Sync`.
- Factory returns `Box<dyn ValueGenerator>`.

### UAT criteria
- N/A (no binary behavior yet — library only).

---

## Slice 0.3 — Prometheus Encoder

### Input state
- Slice 0.2 passes all gates.
- `MetricEvent` and `Labels` are validated and working.

### Specification

**Files to create:**
- `sonda-core/src/encoder/prometheus.rs`:
  ```rust
  pub struct PrometheusText;
  ```
  - `encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError>`:
    - Format: `metric_name{label1="val1",label2="val2"} value timestamp\n`
    - Timestamp in milliseconds since epoch.
    - Label value escaping: `\` → `\\`, `"` → `\"`, newline → `\n`.
    - No labels → omit `{}` entirely: `metric_name value timestamp\n`.
    - Use `write!` macro into buf, or manual `extend_from_slice` for performance.

**Files to modify:**
- `sonda-core/src/encoder/mod.rs`:
  - Uncomment `pub mod prometheus`.
  - Add `EncoderConfig` enum:
    ```rust
    #[derive(Debug, Clone, Deserialize)]
    pub enum EncoderConfig {
        #[serde(rename = "prometheus_text")]
        PrometheusText,
    }
    ```
  - Add factory: `pub fn create_encoder(config: &EncoderConfig) -> Box<dyn Encoder>`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/encoder/prometheus.rs` | new |
| `sonda-core/src/encoder/mod.rs` | modified |

### Test criteria
- Metric with no labels → `metric_name value timestamp\n` (no `{}`).
- Metric with two labels → labels sorted by key, comma-separated, quoted values.
- Label value with `"` → escaped to `\"`.
- Label value with `\` → escaped to `\\`.
- Label value with newline → escaped to `\n`.
- Timestamp is milliseconds since epoch (integer, not float).
- Regression anchor: hardcoded `MetricEvent` → exact expected byte string.
- Writing into a pre-allocated `Vec<u8>` does not reallocate (pre-size the buffer).

### Review criteria
- No per-event allocation — writes into caller-provided `Vec<u8>`.
- Escaping is correct per Prometheus exposition format spec.
- `write!` or `extend_from_slice` — not `format!` returning a String.
- `///` doc comments on the struct and method.

### UAT criteria
- N/A (no binary yet).

---

## Slice 0.4 — Stdout Sink

### Input state
- Slice 0.3 passes all gates.

### Specification

**Files to create:**
- `sonda-core/src/sink/stdout.rs`:
  ```rust
  pub struct StdoutSink { writer: BufWriter<Stdout> }
  ```
  - `StdoutSink::new() -> Self` — wraps `std::io::stdout()` in `BufWriter`.
  - `write(&mut self, data: &[u8])` → `self.writer.write_all(data)`.
  - `flush(&mut self)` → `self.writer.flush()`.

- `sonda-core/src/sink/memory.rs` (test utility):
  ```rust
  pub struct MemorySink { pub buffer: Vec<u8> }
  ```
  - Implements `Sink` by appending to `buffer`. Used in tests across the project.

**Files to modify:**
- `sonda-core/src/sink/mod.rs`:
  - Uncomment `pub mod stdout`.
  - Add `pub mod memory`.
  - Add `SinkConfig` enum:
    ```rust
    #[derive(Debug, Clone, Deserialize)]
    pub enum SinkConfig {
        #[serde(rename = "stdout")]
        Stdout,
    }
    ```
  - Add factory: `pub fn create_sink(config: &SinkConfig) -> Result<Box<dyn Sink>, SondaError>`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/sink/stdout.rs` | new |
| `sonda-core/src/sink/memory.rs` | new |
| `sonda-core/src/sink/mod.rs` | modified |

### Test criteria
- `MemorySink`: write data → buffer contains exact bytes. Flush is a no-op (returns Ok).
- `StdoutSink`: constructable (does not panic). Write + flush does not error.
- Factory: `create_sink(SinkConfig::Stdout)` returns `Ok(...)`.

### Review criteria
- Uses `BufWriter` for stdout (not raw write).
- `MemorySink` is simple, no unnecessary complexity.
- Factory pattern matches encoder factory style.

### UAT criteria
- N/A (no binary yet).

---

## Slice 0.5 — Scenario Config & YAML Loading

### Input state
- Slices 0.1–0.4 pass all gates.
- Generators, encoder, and sink factories exist.

### Specification

**Files to create:**
- `sonda-core/src/config/validate.rs`:
  - `pub fn parse_duration(s: &str) -> Result<Duration, SondaError>` — parse "30s", "5m", "1h", "100ms".
  - `pub fn validate_config(config: &ScenarioConfig) -> Result<(), SondaError>`:
    - `rate > 0.0`
    - `duration` parseable (if provided).
    - If gaps: `gap.for < gap.every`.
    - Metric name is valid.

**Files to modify:**
- `sonda-core/src/config/mod.rs`:
  ```rust
  pub mod validate;

  #[derive(Debug, Clone, Deserialize)]
  pub struct ScenarioConfig {
      pub name: String,
      pub rate: f64,
      #[serde(default)]
      pub duration: Option<String>,
      pub generator: GeneratorConfig,
      #[serde(default)]
      pub gaps: Option<GapConfig>,
      #[serde(default)]
      pub labels: Option<HashMap<String, String>>,
      #[serde(default = "default_encoder")]
      pub encoder: EncoderConfig,
      #[serde(default = "default_sink")]
      pub sink: SinkConfig,
  }

  #[derive(Debug, Clone, Deserialize)]
  pub struct GapConfig {
      pub every: String,
      pub r#for: String,
  }
  ```
  - Default encoder: `PrometheusText`. Default sink: `Stdout`.

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/config/mod.rs` | modified |
| `sonda-core/src/config/validate.rs` | new |

### Test criteria
- **Duration parsing**: "30s" → 30 sec, "5m" → 300 sec, "1h" → 3600 sec, "100ms" → 100ms.
- **Duration parsing errors**: "abc" → Err, "" → Err, "-5s" → Err.
- **YAML deserialization**: the example YAML from `docs/architecture.md` Section 6 → valid `ScenarioConfig`.
- **Validation**: rate=0 → Err. rate=-1 → Err. gap_for > gap_every → Err.
- **Defaults**: YAML with only name/rate/generator → encoder defaults to prometheus_text, sink to stdout.
- **Round-trip**: deserialize → validate → `create_generator()` + `create_encoder()` + `create_sink()` all succeed.

### Review criteria
- Duration parser handles all units: ms, s, m, h.
- Defaults are sensible (prometheus_text + stdout).
- Validation errors include the field name and invalid value in the message.
- Serde `#[serde(default)]` used correctly for optional fields.

### UAT criteria
- N/A (no binary yet).

---

## Slice 0.6 — Scheduler & Runner

### Input state
- Slice 0.5 passes all gates.
- Config, generators, encoder, and sink factories all work.

### Specification

**Files to create:**
- `sonda-core/src/schedule/runner.rs`:
  ```rust
  pub fn run(config: &ScenarioConfig) -> Result<(), SondaError>
  ```
  The main event loop:
  1. Parse config: build `Schedule`, create generator, encoder, sink from config.
  2. Build `Labels` from config labels.
  3. Pre-allocate encode buffer: `let mut buf = Vec::with_capacity(256)`.
  4. Main loop:
     - Calculate elapsed time since start.
     - Check duration → break if exceeded.
     - Check gap window → if in gap, sleep until gap ends, continue.
     - Generate value: `generator.value(tick)`.
     - Build `MetricEvent` with name, value, labels, current timestamp.
     - Encode: `encoder.encode_metric(&event, &mut buf)`.
     - Write to sink: `sink.write(&buf)`.
     - Clear buffer: `buf.clear()`.
     - Rate control: sleep for `1.0 / rate` seconds minus elapsed iteration time.
     - Increment tick.
  5. Flush sink on exit.

- `sonda-core/src/schedule/mod.rs` — update:
  - Uncomment `pub mod runner`.
  - Add gap logic function:
    ```rust
    pub fn is_in_gap(elapsed: Duration, gap: &GapWindow) -> bool
    ```
    - `cycle_pos = elapsed.as_secs_f64() % gap.every.as_secs_f64()`
    - `in_gap = cycle_pos >= (gap.every - gap.duration).as_secs_f64()`

  - Add time-until-gap-end function:
    ```rust
    pub fn time_until_gap_end(elapsed: Duration, gap: &GapWindow) -> Duration
    ```

### Output files
| File | Status |
|------|--------|
| `sonda-core/src/schedule/runner.rs` | new |
| `sonda-core/src/schedule/mod.rs` | modified |

### Test criteria
- **`is_in_gap`**: at 0s with gap_every=10s, gap_for=2s → false. At 8.5s → true (gap from 8-10s). At 10s → false (new cycle). At 18.5s → true.
- **`time_until_gap_end`**: during gap at 9s → 1s remaining.
- **Rate math**: interval for rate=1000 → 1ms. For rate=1 → 1s. For rate=0.5 → 2s.
- **Integration test** (using MemorySink): run config with rate=100, duration=1s → ~100 encoded events in the sink buffer. Parse buffer to count newlines.
- **Gap integration test**: run config with rate=100, duration=5s, gap_every=3s, gap_for=1s → fewer events than 500 (gap suppresses ~100 events).

### Review criteria
- Buffer is pre-allocated and reused (`.clear()`, not reallocated).
- Rate control uses `Instant::now()` elapsed subtraction, not just `thread::sleep(interval)`.
- Gap detection sleeps through gaps (not busy-wait).
- Sink is flushed on exit (including on error paths).
- No `unwrap()` — all errors propagated with `?`.

### UAT criteria
- N/A yet (CLI not wired — but integration tests via MemorySink serve as UAT proxy).

---

## Slice 0.7 — CLI

### Input state
- Slice 0.6 passes all gates.
- The scenario runner works end-to-end in tests with MemorySink.

### Specification

**Files to create:**
- `sonda/src/cli.rs`:
  ```rust
  #[derive(Parser)]
  #[command(name = "sonda", version, about = "Synthetic telemetry generator")]
  pub struct Cli {
      #[command(subcommand)]
      pub command: Commands,
  }

  #[derive(Subcommand)]
  pub enum Commands {
      /// Generate synthetic metrics
      Metrics(MetricsArgs),
  }

  #[derive(Args)]
  pub struct MetricsArgs {
      /// Path to YAML scenario file
      #[arg(long)]
      pub scenario: Option<PathBuf>,
      /// Metric name (overrides scenario file)
      #[arg(long)]
      pub name: Option<String>,
      /// Events per second (overrides scenario file)
      #[arg(long)]
      pub rate: Option<f64>,
      /// Duration (e.g., "30s", "5m") (overrides scenario file)
      #[arg(long)]
      pub duration: Option<String>,
      /// Value mode (overrides scenario file)
      #[arg(long)]
      pub value_mode: Option<String>,
      /// Sine amplitude
      #[arg(long)]
      pub amplitude: Option<f64>,
      /// Sine/sawtooth period in seconds
      #[arg(long)]
      pub period_secs: Option<f64>,
      /// Sine offset / constant value
      #[arg(long)]
      pub offset: Option<f64>,
      /// Uniform min
      #[arg(long)]
      pub min: Option<f64>,
      /// Uniform max
      #[arg(long)]
      pub max: Option<f64>,
      /// RNG seed for deterministic output
      #[arg(long)]
      pub seed: Option<u64>,
      /// Gap interval (e.g., "2m")
      #[arg(long)]
      pub gap_every: Option<String>,
      /// Gap duration (e.g., "20s")
      #[arg(long)]
      pub gap_for: Option<String>,
      /// Labels (repeatable: --label k=v)
      #[arg(long = "label", value_parser = parse_label)]
      pub labels: Vec<(String, String)>,
      /// Encoder format
      #[arg(long, default_value = "prometheus_text")]
      pub encoder: String,
  }
  ```
  - `fn parse_label(s: &str) -> Result<(String, String), String>` — split on first `=`.

- `sonda/src/config.rs`:
  - `pub fn load_config(args: &MetricsArgs) -> Result<ScenarioConfig>`:
    - If `--scenario` provided: read file, deserialize YAML.
    - Overlay CLI flags (any non-None value overrides the YAML).
    - If no scenario file: build config entirely from flags (require `--name` and `--rate`).
    - Build `GeneratorConfig` from `--value-mode` + associated flags.

- `sonda/src/main.rs` — rewrite:
  - Parse `Cli::parse()`.
  - Match `Commands::Metrics(args)`.
  - Call `load_config(&args)?`.
  - Call `sonda_core::config::validate::validate_config(&config)?`.
  - Call `sonda_core::schedule::runner::run(&config)?`.
  - Handle Ctrl+C: use `ctrlc` crate to set an `AtomicBool`.
  - On error: print to stderr, exit code 1.

**Files to modify:**
- `sonda/Cargo.toml` — add `ctrlc = "3"` dependency.

### Output files
| File | Status |
|------|--------|
| `sonda/src/cli.rs` | new |
| `sonda/src/config.rs` | new |
| `sonda/src/main.rs` | rewritten |
| `sonda/Cargo.toml` | modified |

### Test criteria
- **Config from flags only**: `--name up --rate 10 --duration 5s --value-mode constant --offset 1.0` → valid `ScenarioConfig`.
- **Config from YAML**: load a test YAML file → valid config.
- **Config merge**: YAML has rate=100, CLI has --rate 500 → effective rate is 500.
- **Missing required**: no --name and no --scenario → error mentioning "name is required".
- **Label parsing**: `"hostname=t0-a1"` → `("hostname", "t0-a1")`. `"bad"` → error.
- **Value mode mapping**: `--value-mode sine --amplitude 5 --period-secs 30 --offset 10` → `GeneratorConfig::Sine { amplitude: 5.0, period_secs: 30.0, offset: 10.0 }`.

### Review criteria
- CLI crate has zero business logic — all it does is parse, load, validate, call core.
- `anyhow` for errors, not `thiserror`.
- Error messages are user-friendly (no internal paths, no Debug formatting of structs).
- `--help` text is complete and accurate.
- Ctrl+C handler is registered.

### UAT criteria
- `cargo run -p sonda -- metrics --name up --rate 10 --duration 5s` → prints ~50 valid Prometheus lines to stdout and exits.
- `cargo run -p sonda -- metrics --name up --rate 10 --duration 3s --value-mode sine --amplitude 5 --period-secs 10 --offset 10 --label hostname=t0-a1` → values oscillate around 10, labels present in output.
- `cargo run -p sonda -- metrics` (no flags) → clear error message, exit code 1.
- `cargo run -p sonda -- metrics --name up --rate -1` → "rate must be positive" error.
- `cargo run -p sonda -- --help` → shows subcommands and description.
- `cargo run -p sonda -- metrics --help` → shows all flags with descriptions.
- Pipe to `wc -l`: `... --rate 100 --duration 5s | wc -l` → approximately 500.
- Ctrl+C during a long run → exits within 1 second, no panic.

---

## Slice 0.8 — Static Binary & Final Validation

### Input state
- Slice 0.7 passes all gates.
- CLI works end to end.

### Specification

**Files to create:**
- `examples/basic-metrics.yaml`:
  ```yaml
  name: interface_oper_state
  rate: 1000
  duration: 30s
  generator:
    type: sine
    amplitude: 5.0
    period_secs: 30
    offset: 10.0
  gaps:
    every: 2m
    for: 20s
  labels:
    hostname: t0-a1
    zone: eu1
  encoder: prometheus_text
  sink: stdout
  ```

- `examples/simple-constant.yaml`:
  ```yaml
  name: up
  rate: 10
  duration: 10s
  generator:
    type: constant
    value: 1.0
  encoder: prometheus_text
  sink: stdout
  ```

- `README.md` — project README with: what Sonda is, installation, quick start, CLI reference, example output.

**Verification:**
- `cargo build --release --target x86_64-unknown-linux-musl -p sonda` succeeds.
- `file target/x86_64-unknown-linux-musl/release/sonda` → "statically linked".
- `cargo run -p sonda -- metrics --scenario examples/basic-metrics.yaml` works.

### Output files
| File | Status |
|------|--------|
| `examples/basic-metrics.yaml` | new |
| `examples/simple-constant.yaml` | new |
| `README.md` | new |

### Test criteria
- Both example YAML files deserialize and validate correctly.
- Static binary compiles for musl target.

### Review criteria
- README is complete: description, install, usage, examples, CLI reference.
- Example YAMLs use realistic metric names and labels.
- No TODO items remaining in any source file.
- All public items in sonda-core have `///` doc comments.

### UAT criteria
- **Static binary test**: build musl binary, run it standalone, verify output.
- **Example scenario test**: `sonda metrics --scenario examples/basic-metrics.yaml` → valid output at ~1000 events/sec.
- **Rate accuracy**: `sonda metrics --name up --rate 1000 --duration 10s | wc -l` → between 9500 and 10500 (within 5%).
- **Binary size**: note and report (expected: < 5MB for musl release build).
- **Memory usage**: run at 10,000 events/sec for 30s, check RSS stays under 20MB.
- **Full CLI reference**: every flag in `--help` works as documented.
- **Error UX**: all error cases produce human-readable messages, never panics or stack traces.

---

## Dependency Graph

```
Slice 0.0 (CI)
  ↓
Slice 0.1 (telemetry model)
  ↓
  ├── Slice 0.2 (generators)
  ├── Slice 0.3 (encoder)       ← parallel with 0.2
  └── Slice 0.4 (sink)          ← parallel with 0.2, 0.3
       ↓
Slice 0.5 (config & YAML)       ← needs 0.2 + 0.3 + 0.4
  ↓
Slice 0.6 (scheduler & runner)
  ↓
Slice 0.7 (CLI)
  ↓
Slice 0.8 (static binary & validation)
```

Slices 0.2, 0.3, and 0.4 can be developed in parallel after 0.1. Everything converges at 0.5.