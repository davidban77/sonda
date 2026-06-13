//! File sink — writes encoded telemetry to a file on disk.

use std::path::Path;

use async_trait::async_trait;
use tokio::fs::{self, File};
use tokio::io::{AsyncWriteExt, BufWriter};

use super::Sink;
use crate::SondaError;

/// A sink that writes encoded event data to a file using a buffered writer.
pub struct FileSink {
    writer: BufWriter<File>,
}

impl FileSink {
    /// Open `path` for writing, creating any missing parent directories.
    pub async fn new(path: &Path) -> Result<Self, SondaError> {
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                fs::create_dir_all(parent)
                    .await
                    .map_err(|e| {
                        std::io::Error::new(
                            e.kind(),
                            format!(
                                "failed to create parent directories for {}: {}",
                                path.display(),
                                e
                            ),
                        )
                    })
                    .map_err(SondaError::Sink)?;
            }
        }

        let file = File::create(path)
            .await
            .map_err(|e| {
                std::io::Error::new(
                    e.kind(),
                    format!("failed to open {} for writing: {}", path.display(), e),
                )
            })
            .map_err(SondaError::Sink)?;

        Ok(Self {
            writer: BufWriter::new(file),
        })
    }
}

#[async_trait]
impl Sink for FileSink {
    async fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
        self.writer
            .write_all(data)
            .await
            .map_err(SondaError::Sink)?;
        Ok(())
    }

    async fn flush(&mut self) -> Result<(), SondaError> {
        self.writer.flush().await.map_err(SondaError::Sink)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[tokio::test]
    async fn write_to_temp_file_and_read_back_matches() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("out.txt");

        let mut sink = FileSink::new(&path).await.expect("should open file");
        sink.write(b"hello, sonda\n")
            .await
            .expect("write should succeed");
        sink.flush().await.expect("flush should succeed");
        drop(sink);

        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"hello, sonda\n");
    }

    #[tokio::test]
    async fn multiple_writes_accumulate_in_file() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("multi.txt");

        let mut sink = FileSink::new(&path).await.expect("should open file");
        sink.write(b"line1\n").await.expect("write 1");
        sink.write(b"line2\n").await.expect("write 2");
        sink.write(b"line3\n").await.expect("write 3");
        sink.flush().await.expect("flush");
        drop(sink);

        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"line1\nline2\nline3\n");
    }

    #[tokio::test]
    async fn write_empty_slice_succeeds_and_file_is_empty() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("empty.txt");

        let mut sink = FileSink::new(&path).await.expect("should open file");
        sink.write(b"").await.expect("empty write should succeed");
        sink.flush().await.expect("flush should succeed");
        drop(sink);

        let content = fs::read(&path).expect("should read file back");
        assert!(
            content.is_empty(),
            "file should be empty after writing empty slice"
        );
    }

    #[tokio::test]
    async fn parent_dirs_created_automatically_for_nested_path() {
        let base = tempfile::tempdir().expect("create tempdir");
        let path = base.path().join("a").join("b").join("c").join("out.txt");

        let mut sink = FileSink::new(&path)
            .await
            .expect("should create parent dirs and open file");
        sink.write(b"nested\n").await.expect("write should succeed");
        sink.flush().await.expect("flush should succeed");
        drop(sink);

        assert!(path.exists(), "file should exist after write");
        let content = fs::read(&path).expect("should read file back");
        assert_eq!(content, b"nested\n");
    }

    #[tokio::test]
    async fn parent_dir_creation_matches_spec_path_pattern() {
        let base = tempfile::tempdir().expect("create tempdir");
        let path = base.path().join("subdir").join("out.txt");

        let mut sink = FileSink::new(&path)
            .await
            .expect("should create parent dirs");
        sink.write(b"spec path\n").await.expect("write");
        sink.flush().await.expect("flush");
        drop(sink);

        assert!(path.exists(), "file must exist at spec-style path");
    }

    #[tokio::test]
    async fn flush_then_drop_persists_data() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("drop.txt");

        {
            let mut sink = FileSink::new(&path).await.expect("should open file");
            sink.write(b"buffered data\n")
                .await
                .expect("write should succeed");
            sink.flush().await.expect("flush before drop");
        }

        let content = fs::read(&path).expect("file must be readable after drop");
        assert_eq!(content, b"buffered data\n");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn write_to_readonly_dir_returns_sink_error_with_path_in_message() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().expect("create tempdir");
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o555)).unwrap();

        let path = dir.path().join("denied.txt");
        let result = FileSink::new(&path).await;
        assert!(result.is_err(), "should fail on read-only dir");
        let err = result.err().unwrap();

        assert!(
            matches!(err, SondaError::Sink(_)),
            "expected SondaError::Sink, got: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("denied.txt") || msg.contains(dir.path().to_str().unwrap()),
            "error message should contain the path, got: {msg}"
        );

        let _ = fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755));
    }

    #[tokio::test]
    async fn write_to_path_under_nonexistent_root_with_no_create_perm_returns_err() {
        let dir = tempfile::tempdir().expect("create tempdir");
        let blocker = dir.path().join("file.txt");
        fs::write(&blocker, b"I am a file").unwrap();
        let path = blocker.join("child.txt");

        let result = FileSink::new(&path).await;
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

    #[test]
    fn file_sink_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<FileSink>();
    }

    #[tokio::test]
    async fn create_sink_file_config_creates_file_at_path() {
        use crate::sink::create_sink;
        use crate::sink::SinkConfig;

        let dir = tempfile::tempdir().expect("create tempdir");
        let path = dir.path().join("factory.txt");

        let config = SinkConfig::File {
            path: path.to_str().unwrap().to_string(),
        };
        let mut sink = create_sink(&config, None)
            .await
            .expect("factory should create FileSink");
        sink.write(b"via factory\n")
            .await
            .expect("write should succeed");
        sink.flush().await.expect("flush should succeed");
        drop(sink);

        let content = fs::read(&path).expect("file should exist");
        assert_eq!(content, b"via factory\n");
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_file_deserializes_from_yaml() {
        use crate::sink::SinkConfig;

        let yaml = "type: file\npath: /tmp/sonda-test.txt";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize");
        match config {
            SinkConfig::File { path } => {
                assert_eq!(path, "/tmp/sonda-test.txt");
            }
            other => panic!("expected SinkConfig::File, got {other:?}"),
        }
    }

    #[cfg(feature = "config")]
    #[test]
    fn sink_config_file_deserializes_from_inline_yaml() {
        use crate::sink::SinkConfig;

        let yaml = "{type: file, path: /tmp/inline.txt}";
        let config: SinkConfig = serde_yaml_ng::from_str(yaml).expect("should deserialize inline");
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
