//! Shared helpers for integration tests.
//!
//! Cargo treats `tests/common/mod.rs` as a non-binary test module — the
//! file is compiled once per integration test that declares `mod common;`
//! at its root, so it never produces a standalone `no tests` harness run.
//!
//! This module consolidates the fixture-loading, pack-loading, and
//! compilation-chaining helpers that were previously duplicated across
//! `v2_fixture_examples.rs`, `v2_expand_fixtures.rs`,
//! `v2_compile_after_fixtures.rs`, `v2_story_parity.rs`, and
//! `v2_pack_parity.rs`.
//!
//! Snapshot assertions are handled by [`insta`] directly — this module only
//! produces the value that the caller feeds into `insta::assert_json_snapshot!`.
//!
//! Keep the surface area here deliberately small: every helper either loads
//! a fixture from disk or runs a deterministic compile step. Nothing in
//! this module decides *what* a test expects — that still lives in the caller.

#![cfg(feature = "config")]
#![allow(dead_code)]

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use sonda_core::compiler::compile_after::{compile_after, CompiledFile};
use sonda_core::compiler::expand::{expand, ExpandedFile, InMemoryPackResolver};
use sonda_core::compiler::normalize::normalize;
use sonda_core::compiler::parse::parse;
use sonda_core::packs::MetricPackDef;
use sonda_core::prepare_entries;
use sonda_core::schedule::histogram_runner;
use sonda_core::schedule::log_runner;
use sonda_core::schedule::runner;
use sonda_core::schedule::summary_runner;
use sonda_core::sink::Sink;
use sonda_core::{ScenarioEntry, SondaError};

// -----------------------------------------------------------------------------
// Paths
// -----------------------------------------------------------------------------

/// Return the absolute path to the crate's `tests/fixtures/` directory.
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures")
}

/// Return the absolute path to the repository root (the workspace dir).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate has a parent directory")
        .to_path_buf()
}

// -----------------------------------------------------------------------------
// Fixture loaders
// -----------------------------------------------------------------------------

/// Read a scenario fixture from `tests/fixtures/v2-examples/`.
///
/// Panics with a clear message if the file cannot be read; that is the
/// right behavior for tests — a missing fixture is always a bug.
pub fn example_fixture(name: &str) -> String {
    let path = fixtures_dir().join("v2-examples").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {}", path.display(), e))
}

/// Read a scenario fixture from `tests/fixtures/v2-parity/`.
pub fn parity_fixture(name: &str) -> String {
    let path = fixtures_dir().join("v2-parity").join(name);
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read fixture {}: {}", path.display(), e))
}

/// Load and parse a pack YAML from the repo-root `packs/` directory.
pub fn load_repo_pack(file_name: &str) -> MetricPackDef {
    let path = repo_root().join("packs").join(file_name);
    let yaml = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("cannot read pack {}: {}", path.display(), e));
    serde_yaml_ng::from_str::<MetricPackDef>(&yaml)
        .unwrap_or_else(|e| panic!("cannot parse pack {}: {}", path.display(), e))
}

// -----------------------------------------------------------------------------
// Resolvers
// -----------------------------------------------------------------------------

/// Build an [`InMemoryPackResolver`] preloaded with the three built-in
/// packs (telegraf_snmp_interface, node_exporter_cpu, node_exporter_memory),
/// keyed by both the canonical pack name and the `./packs/<file>` form.
///
/// The path-form keys are defensive: no current integration fixture uses
/// file-path pack references (the `valid-expand-pack-file-path` fixture
/// was deleted as redundant during the test-infra consolidation — its
/// distinction is covered by the in-tree `classify_pack_reference` and
/// `pack_by_file_path_is_resolved_through_trait` unit tests in
/// `compiler::expand::tests`). Keep the dual registration so a future
/// fixture that exercises path-style references can rely on this
/// resolver without expanding the helper.
pub fn builtin_pack_resolver() -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    for (file, pack_name) in [
        ("telegraf-snmp-interface.yaml", "telegraf_snmp_interface"),
        ("node-exporter-cpu.yaml", "node_exporter_cpu"),
        ("node-exporter-memory.yaml", "node_exporter_memory"),
    ] {
        let pack = load_repo_pack(file);
        r.insert(pack_name, pack.clone());
        r.insert(format!("./packs/{file}"), pack);
    }
    r
}

/// Build an [`InMemoryPackResolver`] containing exactly one pack registered
/// under the given lookup name.
pub fn resolver_with(name: &str, pack: MetricPackDef) -> InMemoryPackResolver {
    let mut r = InMemoryPackResolver::new();
    r.insert(name, pack);
    r
}

// -----------------------------------------------------------------------------
// Compile chain helpers
// -----------------------------------------------------------------------------

/// Run `parse → normalize → expand` on a fixture YAML, panicking on any
/// step's failure. Use this when the fixture is known to expand cleanly.
pub fn compile_to_expanded(yaml: &str, resolver: &InMemoryPackResolver) -> ExpandedFile {
    let parsed = parse(yaml).expect("fixture must parse");
    let normalized = normalize(parsed).expect("fixture must normalize");
    expand(normalized, resolver).expect("fixture must expand")
}

/// Run the full v2 compile pipeline (`parse → normalize → expand →
/// compile_after`), panicking on any step's failure.
pub fn compile_to_compiled(yaml: &str, resolver: &InMemoryPackResolver) -> CompiledFile {
    let expanded = compile_to_expanded(yaml, resolver);
    compile_after(expanded).expect("fixture must compile after")
}

// -----------------------------------------------------------------------------
// Snapshot settings
// -----------------------------------------------------------------------------

// -----------------------------------------------------------------------------
// Runtime parity harness
// -----------------------------------------------------------------------------

/// An in-memory [`Sink`] that appends every byte written to a shared buffer.
///
/// Duplicated (tiny) from `sonda_core::sink::memory::MemorySink` so the
/// parity harness can push an `Arc<Mutex<Vec<u8>>>` through the closure
/// boundary and drain the captured bytes after the runner thread joins —
/// `MemorySink` owns its buffer directly and does not expose the shared
/// ownership the harness needs.
struct CapturingSink {
    buffer: Arc<Mutex<Vec<u8>>>,
}

impl Sink for CapturingSink {
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        let mut guard = self
            .buffer
            .lock()
            .expect("parity harness buffer lock poisoned");
        guard.extend_from_slice(data);
        Ok(())
    }

    fn flush(&mut self) -> Result<(), SondaError> {
        Ok(())
    }
}

/// Run every entry in `entries` to completion against an in-memory sink,
/// returning the raw concatenated stdout-equivalent bytes.
///
/// The harness mirrors a trimmed-down version of `launch_scenario`:
///
/// 1. `prepare_entries` expands csv_replay, desugars aliases, validates
///    every entry, and resolves each entry's `phase_offset` into a
///    `start_delay: Option<Duration>` — exactly the same preparation the
///    production launcher does.
/// 2. Each prepared entry runs on its own OS thread with a
///    [`CapturingSink`] substituted for the user-configured sink. The
///    shared `Arc<Mutex<Vec<u8>>>` is cloned into the thread so the parent
///    can drain bytes after the thread joins.
/// 3. Each thread honors its `start_delay` via `thread::sleep` (no shared
///    shutdown signal — the scenario's own `duration:` field bounds the
///    run, which must be set on every entry this harness sees).
///
/// The returned `Vec<u8>` is the raw byte stream a real stdout sink would
/// have produced. Callers choose `assert_eq!` or a line-multiset comparison
/// depending on whether order is deterministic for their scenario.
///
/// # Panics
///
/// Panics if `prepare_entries` fails, if a runner thread panics, or if a
/// runner returns an error. For parity tests these are all bugs, not
/// legitimate test outcomes.
///
/// # Determinism
///
/// All seeds, jitter seeds, and `seed:` fields must be pinned by the
/// caller's configuration. The harness does not inject any randomness.
/// Multi-entry output order is **not** deterministic — concurrent threads
/// interleave writes at byte granularity. For multi-signal parity tests,
/// compare via [`assert_line_multisets_equal`].
pub fn run_and_capture_stdout(entries: Vec<ScenarioEntry>) -> Vec<u8> {
    let prepared =
        prepare_entries(entries).expect("run_and_capture_stdout: prepare_entries must succeed");

    let mut handles = Vec::with_capacity(prepared.len());
    for (idx, prepared_entry) in prepared.into_iter().enumerate() {
        let buffer = Arc::new(Mutex::new(Vec::<u8>::with_capacity(4096)));
        let buffer_for_thread = Arc::clone(&buffer);
        let start_delay = prepared_entry.start_delay;
        let entry = prepared_entry.entry;

        // Each runner needs a `'static` closure, so move ownership into the thread.
        let handle = thread::Builder::new()
            .name(format!("parity-{idx}"))
            .spawn(move || -> Result<(), SondaError> {
                if let Some(delay) = start_delay {
                    let deadline = Instant::now() + delay;
                    while Instant::now() < deadline {
                        let remaining = deadline.saturating_duration_since(Instant::now());
                        let chunk = remaining.min(Duration::from_millis(25));
                        if chunk > Duration::ZERO {
                            thread::sleep(chunk);
                        }
                    }
                }

                let mut sink = CapturingSink {
                    buffer: buffer_for_thread,
                };
                run_entry_with_sink(&entry, &mut sink)
            })
            .expect("failed to spawn parity harness thread");

        handles.push((handle, buffer));
    }

    // Join in order. Each thread's scenario-level `duration:` bounds the
    // run, so joining sequentially is fine — every thread exits naturally.
    let mut result = Vec::new();
    for (handle, buffer) in handles {
        handle
            .join()
            .expect("parity harness thread panicked")
            .expect("parity harness runner returned an error");
        let mut guard = buffer.lock().expect("buffer lock poisoned");
        result.extend_from_slice(&guard);
        guard.clear();
    }
    result
}

/// Normalize timestamps embedded in encoded metric / log output so that
/// two runs can be compared byte-for-byte.
///
/// Every encoder emits a wall-clock timestamp: Prometheus text encodes
/// it as an integer `ms-since-epoch` trailing each sample line, while JSON
/// Lines embeds an RFC 3339 `"timestamp":"..."` field. This helper rewrites
/// all such tokens to fixed sentinels so parity tests do not fail on
/// clock-drift between v1 and v2 runs.
///
/// The replacement is intentionally aggressive — it normalizes:
///
/// - any run of 11–19 digits immediately followed by `\n` (Prometheus text
///   millisecond epoch timestamps) to `___TS___\n`,
/// - any `"timestamp":"...Z"` JSON field to `"timestamp":"___TS___"`.
///
/// Non-timestamp substrings that match these shapes do not appear in
/// sonda's encoder output; see the unit tests for the encoders in
/// `src/encoder/` for the exact byte patterns.
pub fn normalize_timestamps(bytes: &[u8]) -> Vec<u8> {
    // Phase 1: replace Prometheus ` <ts_ms>\n` trailers.
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let rest = &bytes[i..];
        // Look for ` <digits>\n` pattern (Prometheus trailing timestamp).
        if rest[0] == b' ' {
            let after_space = &rest[1..];
            let mut digit_end = 0;
            while digit_end < after_space.len() && after_space[digit_end].is_ascii_digit() {
                digit_end += 1;
            }
            if digit_end >= 11 && digit_end <= 19 && after_space.get(digit_end) == Some(&b'\n') {
                out.extend_from_slice(b" ___TS___\n");
                i += 1 + digit_end + 1;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }

    // Phase 2: normalize JSON `"timestamp":"...Z"` fields.
    let bytes = out;
    let mut out = Vec::with_capacity(bytes.len());
    let needle = b"\"timestamp\":\"";
    let mut i = 0;
    while i < bytes.len() {
        if bytes.len() - i >= needle.len() && &bytes[i..i + needle.len()] == needle {
            // Emit the needle, then scan ahead for the closing quote.
            out.extend_from_slice(b"\"timestamp\":\"___TS___\"");
            let scan_start = i + needle.len();
            let mut j = scan_start;
            while j < bytes.len() && bytes[j] != b'"' {
                j += 1;
            }
            // Skip past the closing `"`.
            i = j.saturating_add(1);
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    out
}

/// Dispatch a single `ScenarioEntry` to the runner matching its variant.
///
/// The runner is driven with `shutdown: None` (the scenario's own
/// `duration:` field bounds the run) and `stats: None` (parity tests do
/// not inspect stats).
fn run_entry_with_sink(entry: &ScenarioEntry, sink: &mut dyn Sink) -> Result<(), SondaError> {
    // All four runners take the same shape: &Config, &mut dyn Sink,
    // Option<&AtomicBool>, Option<Arc<RwLock<ScenarioStats>>>.
    const NONE_ATOMIC: Option<&AtomicBool> = None;
    match entry {
        ScenarioEntry::Metrics(config) => runner::run_with_sink(config, sink, NONE_ATOMIC, None),
        ScenarioEntry::Logs(config) => {
            log_runner::run_logs_with_sink(config, sink, NONE_ATOMIC, None)
        }
        ScenarioEntry::Histogram(config) => {
            histogram_runner::run_with_sink(config, sink, NONE_ATOMIC, None)
        }
        ScenarioEntry::Summary(config) => {
            summary_runner::run_with_sink(config, sink, NONE_ATOMIC, None)
        }
    }
}

/// Assert that two byte streams contain the same set of newline-delimited
/// lines, ignoring order.
///
/// Used for multi-signal parity tests where runner threads interleave
/// output nondeterministically. Both sides must produce the same *set* of
/// lines — duplicates are preserved (comparison is multiset, not set).
///
/// # Panics
///
/// Panics with a detailed diff-like report if the line multisets differ.
pub fn assert_line_multisets_equal(label: &str, expected: &[u8], actual: &[u8]) {
    let expected_lines: Vec<&[u8]> = split_lines_preserve_empty(expected);
    let actual_lines: Vec<&[u8]> = split_lines_preserve_empty(actual);

    let mut expected_sorted: Vec<Vec<u8>> = expected_lines.iter().map(|l| l.to_vec()).collect();
    let mut actual_sorted: Vec<Vec<u8>> = actual_lines.iter().map(|l| l.to_vec()).collect();
    expected_sorted.sort();
    actual_sorted.sort();

    if expected_sorted != actual_sorted {
        // Build human-readable diagnostics.
        let expected_set: BTreeSet<&[u8]> = expected_sorted.iter().map(Vec::as_slice).collect();
        let actual_set: BTreeSet<&[u8]> = actual_sorted.iter().map(Vec::as_slice).collect();
        let only_in_expected: Vec<String> = expected_set
            .difference(&actual_set)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        let only_in_actual: Vec<String> = actual_set
            .difference(&expected_set)
            .map(|b| String::from_utf8_lossy(b).into_owned())
            .collect();
        panic!(
            "{label}: line multisets differ\n\
             expected {} lines, got {} lines\n\
             only in expected:\n  {}\n\
             only in actual:\n  {}",
            expected_sorted.len(),
            actual_sorted.len(),
            only_in_expected.join("\n  "),
            only_in_actual.join("\n  ")
        );
    }
}

/// Split a byte slice on `\n`, preserving empty leading/trailing lines so
/// the multiset comparison is exact.
fn split_lines_preserve_empty(bytes: &[u8]) -> Vec<&[u8]> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let mut lines: Vec<&[u8]> = bytes.split(|&b| b == b'\n').collect();
    // `split` yields a trailing empty slice when the input ends with `\n`;
    // drop it so the line count reflects actual emitted lines.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }
    lines
}

// -----------------------------------------------------------------------------
// Snapshot settings
// -----------------------------------------------------------------------------

/// Return an [`insta::Settings`] pre-configured for compiler snapshots.
///
/// Every snapshot in the v2 suite wants `sort_maps = true` so that output is
/// stable regardless of `HashMap` iteration order on the producer side. This
/// helper centralizes that default; call
/// `snapshot_settings().bind(|| insta::assert_json_snapshot!(value))` instead
/// of duplicating a `with_settings!` block in every test.
pub fn snapshot_settings() -> insta::Settings {
    let mut s = insta::Settings::clone_current();
    s.set_sort_maps(true);
    s
}
