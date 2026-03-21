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

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;

    /// Return a unique temporary directory path for the given test name.
    /// The directory is created by the caller so sub-paths can be used freely.
    fn tmp_path(test_name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("sonda-filesink-tests-{test_name}"));
        // Best-effort cleanup of any previous run; ignore errors.
        let _ = fs::remove_dir_all(&dir);
        dir
    }

    // ---- Happy path: write → read back ----------------------------------------

    #[test]
    fn write_to_temp_file_and_read_back_matches() {
        let dir = tmp_path("write_read_back");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("out.txt");

        let mut sink = FileSink::new(&path).expect("should open file");
        sink.write(b"hello, sonda\n").expect("write should succeed");
        sink.flush().expect("flush should succeed");

        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"hello, sonda\n");
    }

    #[test]
    fn multiple_writes_accumulate_in_file() {
        let dir = tmp_path("multiple_writes");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("multi.txt");

        let mut sink = FileSink::new(&path).expect("should open file");
        sink.write(b"line1\n").expect("write 1");
        sink.write(b"line2\n").expect("write 2");
        sink.write(b"line3\n").expect("write 3");
        sink.flush().expect("flush");

        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"line1\nline2\nline3\n");
    }

    #[test]
    fn write_empty_slice_succeeds_and_file_is_empty() {
        let dir = tmp_path("empty_write");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("empty.txt");

        let mut sink = FileSink::new(&path).expect("should open file");
        sink.write(b"").expect("empty write should succeed");
        sink.flush().expect("flush should succeed");

        let content = fs::read(&path).expect("should read file back");
        assert!(
            content.is_empty(),
            "file should be empty after writing empty slice"
        );
    }

    // ---- Parent directory creation --------------------------------------------

    #[test]
    fn parent_dirs_created_automatically_for_nested_path() {
        let base = tmp_path("parent_dirs");
        // Three levels of directories that do not yet exist.
        let path = base.join("a").join("b").join("c").join("out.txt");

        let mut sink = FileSink::new(&path).expect("should create parent dirs and open file");
        sink.write(b"nested\n").expect("write should succeed");
        sink.flush().expect("flush should succeed");

        assert!(path.exists(), "file should exist after write");
        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"nested\n");
    }

    #[test]
    fn parent_dir_creation_matches_spec_path_pattern() {
        // Spec criterion: write to /tmp/sonda-test/subdir/out.txt → dirs created.
        // We use a unique suffix to avoid collisions with parallel test runs.
        let path = std::env::temp_dir()
            .join("sonda-test-slice13")
            .join("subdir")
            .join("out.txt");
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());

        let mut sink = FileSink::new(&path).expect("should create parent dirs");
        sink.write(b"spec path\n").expect("write");
        sink.flush().expect("flush");

        assert!(path.exists(), "file must exist at spec-style path");
    }

    // ---- Flush on drop: data appears in file after sink is dropped -----------

    #[test]
    fn flush_on_drop_data_visible_after_sink_dropped() {
        let dir = tmp_path("flush_on_drop");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("drop.txt");

        {
            let mut sink = FileSink::new(&path).expect("should open file");
            // Write but do NOT call flush() explicitly.
            sink.write(b"buffered data\n")
                .expect("write should succeed");
            // sink is dropped here — BufWriter::drop calls flush automatically.
        }

        let content = fs::read(&path).expect("file must be readable after drop");
        assert_eq!(
            content, b"buffered data\n",
            "BufWriter must flush on drop — data must appear in file"
        );
    }

    // ---- Error path: permission / invalid path → SondaError::Sink -----------

    #[cfg(unix)]
    #[test]
    fn write_to_readonly_dir_returns_sink_error_with_path_in_message() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tmp_path("readonly_dir");
        fs::create_dir_all(&dir).unwrap();
        // Make the directory read-only so we cannot create files inside it.
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o555)).unwrap();

        let path = dir.join("denied.txt");
        let result = FileSink::new(&path);
        assert!(result.is_err(), "should fail on read-only dir");
        let err = result.err().unwrap();

        // Must be a Sink variant.
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
        // Error message must mention the file path.
        let msg = err.to_string();
        assert!(
            msg.contains("denied.txt") || msg.contains(dir.to_str().unwrap()),
            "error message should contain the path, got: {msg}"
        );

        // Clean up: restore permissions so tmp cleanup works.
        let _ = fs::set_permissions(&dir, fs::Permissions::from_mode(0o755));
    }

    #[test]
    fn write_to_path_under_nonexistent_root_with_no_create_perm_returns_err() {
        // On most systems we cannot write under /proc or similar read-only roots.
        // Use a clearly invalid path: a file whose "parent" is itself a file.
        let dir = tmp_path("parent_is_file");
        fs::create_dir_all(&dir).unwrap();
        // Create a regular file.
        let blocker = dir.join("file.txt");
        fs::write(&blocker, b"I am a file").unwrap();
        // Try to use that file as a directory.
        let path = blocker.join("child.txt");

        let result = FileSink::new(&path);
        assert!(
            result.is_err(),
            "opening a path whose parent is a regular file must fail"
        );
        let err = result.err().unwrap();
        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
    }

    // ---- Trait contract: Send + Sync ----------------------------------------

    #[test]
    fn file_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FileSink>();
    }

    // ---- Factory wiring: SinkConfig::File → FileSink ------------------------

    #[test]
    fn create_sink_file_config_creates_file_at_path() {
        use crate::sink::create_sink;
        use crate::sink::SinkConfig;

        let dir = tmp_path("factory_wiring");
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("factory.txt");

        let config = SinkConfig::File {
            path: path.to_str().unwrap().to_string(),
        };
        let mut sink = create_sink(&config).expect("factory should create FileSink");
        sink.write(b"via factory\n").expect("write should succeed");
        sink.flush().expect("flush should succeed");

        let content = fs::read(&path).expect("file should exist");
        assert_eq!(content, b"via factory\n");
    }

    #[test]
    fn sink_config_file_deserializes_from_yaml() {
        use crate::sink::SinkConfig;

        // serde_yaml 0.9.x uses YAML tag syntax for externally-tagged enum
        // variants with struct fields. The unit variant "stdout" stays a plain
        // string, but "File { path }" requires the YAML tag form:
        //   !file
        //   path: /tmp/sonda-test.txt
        let yaml = "!file\npath: /tmp/sonda-test.txt";
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda-test.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_file_deserializes_from_inline_yaml_tag() {
        use crate::sink::SinkConfig;

        // Inline YAML tag form also accepted by serde_yaml 0.9.
        let yaml = "!file {path: /tmp/inline.txt}";
        let config: SinkConfig = serde_yaml::from_str(yaml).expect("should deserialize inline");
        match config {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/inline.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
    }

    #[test]
    fn sink_config_file_is_cloneable_and_debuggable() {
        use crate::sink::SinkConfig;

        let config = SinkConfig::File {
            path: "/tmp/test.txt".to_string(),
        };
        let cloned = config.clone();
        let debug_str = format!("{cloned:?}");
        assert!(debug_str.contains("File"));
        assert!(debug_str.contains("/tmp/test.txt"));
    }
}
