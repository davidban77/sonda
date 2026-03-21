//! Replay log generator — re-emits lines from a file, cycling indefinitely.
//!
//! Lines are loaded once at construction time. Each call to `generate()` returns
//! the line at `tick % lines.len()`, cycling back to the start when the file is
//! exhausted.

use std::collections::BTreeMap;
use std::path::Path;

use crate::model::log::{LogEvent, Severity};
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
    /// The file is read line-by-line; empty lines are preserved. Blank files
    /// (containing only whitespace or no content) return a
    /// [`SondaError::Config`] error.
    ///
    /// # Errors
    /// - Returns [`SondaError::Sink`] (wrapping `std::io::Error`) if the file
    ///   cannot be read.
    /// - Returns [`SondaError::Config`] if the file contains no non-empty lines.
    pub fn from_file(path: &Path) -> Result<Self, SondaError> {
        let content = std::fs::read_to_string(path).map_err(SondaError::Sink)?;
        let lines: Vec<String> = content
            .lines()
            .map(|l| l.to_string())
            .filter(|l| !l.is_empty())
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
        LogEvent::new(Severity::Info, line.clone(), BTreeMap::new())
    }
}
