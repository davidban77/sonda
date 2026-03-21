//! File sink — writes encoded telemetry to a file on disk.

use std::fs::{self, File};
use std::io::{BufWriter, Write};
use std::path::Path;

use super::Sink;
use crate::SondaError;

/// A sink that writes encoded event data to a file using a buffered writer.
///
/// Parent directories are created automatically if they do not exist.
/// Wraps the underlying [`File`] in a [`BufWriter`] to batch syscalls and
/// reduce per-event overhead.
pub struct FileSink {
    writer: BufWriter<File>,
}

impl FileSink {
    /// Create a new `FileSink` writing to the given path.
    ///
    /// Creates any missing parent directories before opening the file.
    ///
    /// # Errors
    ///
    /// Returns `SondaError::Sink` if the parent directories cannot be created
    /// or the file cannot be opened for writing.
    pub fn new(path: &Path) -> Result<Self, SondaError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent).map_err(|e| {
                    std::io::Error::new(
                        e.kind(),
                        format!(
                            "failed to create parent directories for {}: {}",
                            path.display(),
                            e
                        ),
                    )
                })?;
            }
        }

        let file = File::create(path).map_err(|e| {
            std::io::Error::new(
                e.kind(),
                format!("failed to open {} for writing: {}", path.display(), e),
            )
        })?;

        Ok(Self {
            writer: BufWriter::new(file),
        })
    }
}

impl Sink for FileSink {
    /// Write `data` to the buffered file writer.
    fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer.write_all(data)?;
        Ok(())
    }

    /// Flush any buffered bytes to the file.
    fn flush(&mut self) -> Result<(), SondaError> {
        self.writer.flush()?;
        Ok(())
    }
}
