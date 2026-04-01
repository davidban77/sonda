//! Replay log generator — re-emits lines from a file, cycling indefinitely.
//!
//! Lines are loaded once at construction time. Each call to `generate()` returns
//! the line at `tick % lines.len()`, cycling back to the start when the file is
//! exhausted.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::log::{LogEvent, Severity};
use crate::model::metric::Labels;
use crate::SondaError;

use super::LogGenerator;

/// A log generator that replays lines from a pre-loaded file.
///
/// The file is read once at construction time. Each tick emits one line,
/// wrapping around when the end of the file is reached.
pub struct LogReplayGenerator {
    lines: Vec<String>,
}

impl LogReplayGenerator {
    /// Load a `LogReplayGenerator` from a file at the given path.
    ///
    /// The file is read line-by-line. Blank lines (empty or whitespace-only)
    /// are filtered out. Returns a [`SondaError::Config`] error if the file
    /// contains no non-blank lines.
    ///
    /// # Errors
    /// - Returns [`SondaError::Generator`] if the file cannot be read.
    /// - Returns [`SondaError::Config`] if the file contains no non-blank lines.
    pub fn from_file(path: &Path) -> Result<Self, SondaError> {
        let content = std::fs::read_to_string(path).map_err(|e| {
            SondaError::Generator(format!("cannot read replay file {:?}: {}", path, e))
        })?;
        let lines: Vec<String> = content
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.trim().is_empty())
            .collect();

        if lines.is_empty() {
            return Err(SondaError::Config(format!(
                "replay file {:?} contains no lines",
                path
            )));
        }

        Ok(Self { lines })
    }

    /// Construct a `LogReplayGenerator` from an in-memory list of lines.
    ///
    /// Returns [`SondaError::Config`] if `lines` is empty.
    ///
    /// This constructor is primarily useful for testing without requiring a file
    /// on disk.
    pub fn from_lines(lines: Vec<String>) -> Result<Self, SondaError> {
        if lines.is_empty() {
            return Err(SondaError::Config(
                "replay generator requires at least one line".into(),
            ));
        }
        Ok(Self { lines })
    }
}

impl LogGenerator for LogReplayGenerator {
    /// Return the log event for the given tick.
    ///
    /// Wraps around when `tick >= lines.len()`. The severity is always `Info`
    /// and `fields` is empty — the full log context is in the message.
    fn generate(&self, tick: u64) -> LogEvent {
        let line = &self.lines[(tick as usize) % self.lines.len()];
        LogEvent::new(
            Severity::Info,
            line.clone(),
            Labels::default(),
            BTreeMap::new(),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn five_line_generator() -> LogReplayGenerator {
        let lines: Vec<String> = (0..5).map(|i| format!("line-{i}")).collect();
        LogReplayGenerator::from_lines(lines).expect("five lines should succeed")
    }

    // ---------------------------------------------------------------------------
    // Basic cycling behaviour
    // ---------------------------------------------------------------------------

    #[test]
    fn tick_zero_returns_first_line() {
        let gen = five_line_generator();
        assert_eq!(gen.generate(0).message, "line-0");
    }

    #[test]
    fn tick_four_returns_fifth_line() {
        let gen = five_line_generator();
        assert_eq!(gen.generate(4).message, "line-4");
    }

    #[test]
    fn tick_five_wraps_to_line_zero() {
        let gen = five_line_generator();
        assert_eq!(gen.generate(5).message, "line-0");
    }

    #[test]
    fn tick_six_wraps_to_line_one() {
        let gen = five_line_generator();
        assert_eq!(gen.generate(6).message, "line-1");
    }

    #[test]
    fn tick_ten_wraps_to_line_zero_again() {
        let gen = five_line_generator();
        assert_eq!(gen.generate(10).message, "line-0");
    }

    // ---------------------------------------------------------------------------
    // Severity and fields are fixed
    // ---------------------------------------------------------------------------

    #[test]
    fn severity_is_always_info() {
        let gen = five_line_generator();
        for tick in 0..15 {
            assert_eq!(
                gen.generate(tick).severity,
                Severity::Info,
                "severity at tick {tick} must be Info"
            );
        }
    }

    #[test]
    fn fields_are_always_empty() {
        let gen = five_line_generator();
        for tick in 0..15 {
            assert!(
                gen.generate(tick).fields.is_empty(),
                "fields at tick {tick} must be empty"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Empty input error
    // ---------------------------------------------------------------------------

    #[test]
    fn from_lines_empty_returns_error() {
        let result = LogReplayGenerator::from_lines(vec![]);
        assert!(result.is_err(), "from_lines([]) must return Err, not Ok");
    }

    #[test]
    fn from_file_truly_empty_file_returns_error() {
        // A file with no content at all (zero bytes) must error.
        let tmp = NamedTempFile::new().expect("create temp file");
        // Do not write anything — file is empty.
        let result = LogReplayGenerator::from_file(tmp.path());
        assert!(
            result.is_err(),
            "from_file with zero-byte file must return Err"
        );
    }

    #[test]
    fn from_file_only_empty_lines_returns_error() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        // Write lines that are truly empty (just newlines), which get filtered.
        writeln!(tmp, "").expect("write empty line");
        writeln!(tmp, "").expect("write empty line");
        let result = LogReplayGenerator::from_file(tmp.path());
        assert!(
            result.is_err(),
            "from_file with only empty lines must return Err"
        );
    }

    #[test]
    fn from_file_missing_file_returns_generator_error() {
        let result = LogReplayGenerator::from_file(std::path::Path::new(
            "/nonexistent/path/that/does/not/exist.log",
        ));
        match result {
            Err(ref err) => {
                assert!(
                    matches!(err, SondaError::Generator(_)),
                    "missing replay file must produce Generator variant, not Sink; got: {err:?}"
                );
                let msg = format!("{err}");
                assert!(
                    msg.contains("cannot read replay file"),
                    "error message should mention 'cannot read replay file', got: {msg}"
                );
            }
            Ok(_) => panic!("missing file must return Err"),
        }
    }

    // ---------------------------------------------------------------------------
    // File-based construction
    // ---------------------------------------------------------------------------

    #[test]
    fn from_file_five_lines_cycles_correctly() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        for i in 0..5 {
            writeln!(tmp, "file-line-{i}").expect("write line");
        }
        let gen = LogReplayGenerator::from_file(tmp.path()).expect("five-line file should succeed");
        for tick in 0..5u64 {
            assert_eq!(
                gen.generate(tick).message,
                format!("file-line-{tick}"),
                "tick {tick} should return file-line-{tick}"
            );
        }
        assert_eq!(
            gen.generate(5).message,
            "file-line-0",
            "tick 5 should wrap to file-line-0"
        );
    }

    #[test]
    fn from_file_skips_blank_lines() {
        let mut tmp = NamedTempFile::new().expect("create temp file");
        // Write 3 content lines with blank lines interspersed.
        writeln!(tmp, "alpha").expect("write");
        writeln!(tmp).expect("write blank");
        writeln!(tmp, "beta").expect("write");
        writeln!(tmp).expect("write blank");
        writeln!(tmp, "gamma").expect("write");
        let gen = LogReplayGenerator::from_file(tmp.path()).expect("non-empty file");
        assert_eq!(gen.generate(0).message, "alpha");
        assert_eq!(gen.generate(1).message, "beta");
        assert_eq!(gen.generate(2).message, "gamma");
        // Wrap at 3
        assert_eq!(gen.generate(3).message, "alpha");
    }

    // ---------------------------------------------------------------------------
    // Large tick values
    // ---------------------------------------------------------------------------

    #[test]
    fn large_tick_does_not_panic() {
        let gen = five_line_generator();
        let _ = gen.generate(u64::MAX);
        let _ = gen.generate(u64::MAX - 1);
    }

    // ---------------------------------------------------------------------------
    // Determinism
    // ---------------------------------------------------------------------------

    #[test]
    fn same_tick_always_returns_same_message() {
        let gen = five_line_generator();
        for tick in 0..20 {
            assert_eq!(
                gen.generate(tick).message,
                gen.generate(tick).message,
                "generate(tick) must be deterministic"
            );
        }
    }

    // ---------------------------------------------------------------------------
    // Send + Sync contract
    // ---------------------------------------------------------------------------

    fn assert_send_sync<T: Send + Sync>() {}

    #[test]
    fn log_replay_generator_is_send_and_sync() {
        assert_send_sync::<LogReplayGenerator>();
    }
}
