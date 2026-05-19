# Skill: Add a Sink

Use this skill when implementing a new output sink for sonda-core.

## Steps

1. **Create the source file**: `sonda-core/src/sink/<name>.rs`

2. **Define the struct**:
   ```rust
   /// Delivers encoded telemetry data to <destination>.
   pub struct <Name>Sink {
       // Owns I/O resources: file handles, sockets, buffers, etc.
       // Use BufWriter to avoid per-write syscalls.
   }

   impl <Name>Sink {
       /// Creates a new <Name>Sink.
       ///
       /// # Errors
       /// Returns `SondaError::Sink` if the destination cannot be opened.
       pub fn new(/* config params */) -> Result<Self, SondaError> {
           // Open file, connect socket, etc.
           Ok(Self { /* ... */ })
       }
   }
   ```

3. **Implement the trait**:
   ```rust
   impl Sink for <Name>Sink {
       fn write(&mut self, data: &[u8]) -> Result<(), SondaError> {
           // Write one encoded event. data is already formatted by the encoder.
           // Use self.writer.write_all(data)? for reliable delivery.
           Ok(())
       }

       fn flush(&mut self) -> Result<(), SondaError> {
           // Force delivery of buffered data.
           // Called at shutdown and periodically during operation.
           Ok(())
       }
   }
   ```

4. **Register in factory** (`sonda-core/src/sink/mod.rs`):
   - Add `mod <name>;` declaration.
   - Add `pub use <name>::<Name>Sink;` re-export.
   - Add a variant to `SinkConfig` enum.
   - Add a match arm in `create_sink()`.

## Design Rules

- **Sinks own their I/O resources.** File handles, TCP connections, etc. live in the struct.
- **Use `BufWriter`.** Wrap all I/O with `std::io::BufWriter` to batch syscalls.
- **`new()` is fallible.** Return `Result` — connection failures, permission errors, etc. happen here.
- **`write()` gets pre-encoded data.** Don't re-encode or transform. Just deliver.
- **`flush()` must be idempotent.** Safe to call multiple times, including after errors.

## Per-event context: `write_log_event`

The `Sink` trait carries a second log-write method, `write_log_event(&mut self, event:
&LogEvent, encoded: &[u8])`. The default impl ignores the event and forwards the encoded
bytes to `write()`. **Override it whenever your sink consumes any per-event field (labels,
severity, timestamp) for delivery routing** — stream selection, partition keys, structured
envelopes. The Loki sink (`sink/loki.rs`) is the in-tree example: it overrides to promote
`LogEvent.labels` into the Loki stream label set so each unique label combination becomes
its own stream. Inheriting the default when you actually need the event context silently
drops that context at runtime and produces wrong output.

## Quality Checklist

- [ ] `new()` returns `Result` and handles I/O failures.
- [ ] Uses `BufWriter` for buffered I/O.
- [ ] `write()` delivers pre-encoded `&[u8]` without transformation.
- [ ] `flush()` is idempotent.
- [ ] `///` doc comments on struct and all public methods.
- [ ] I/O errors wrapped in `SondaError::Sink`.
- [ ] No `unwrap()`.
- [ ] Registered in factory.

## Test Criteria (for the tester agent)

- Write known bytes → verify they arrive at destination.
- Flush → verify buffered data is delivered.
- For file sinks: verify file contents match written data.
- For network sinks: use a mock server or in-memory buffer.
- Error path: write to closed/invalid destination → returns `Err`, doesn't panic.