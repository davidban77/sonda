# Skill: Add an Encoder

Use this skill when implementing a new output encoder for sonda-core.

## Steps

1. **Create the source file**: `sonda-core/src/encoder/<format>.rs`

2. **Define the struct**:
   ```rust
   /// Encodes metric events into <Format> format.
   pub struct <Format>Encoder {
       // Pre-built invariant content goes here (label prefixes, header bytes, etc.).
       // Build everything possible at construction time, not per-event.
   }

   impl <Format>Encoder {
       /// Creates a new <Format>Encoder.
       pub fn new(/* config params */) -> Self {
           // Pre-compute label serialization, metric name validation, etc.
           Self { /* ... */ }
       }
   }
   ```

3. **Implement the trait**:
   ```rust
   impl Encoder for <Format>Encoder {
       fn encode_metric(&self, event: &MetricEvent, buf: &mut Vec<u8>) -> Result<(), SondaError> {
           // Write into the caller-provided buffer. NEVER allocate a new buffer.
           // Use write! or buf.extend_from_slice() for zero-copy appends.
           Ok(())
       }
   }
   ```

4. **Register in factory** (`sonda-core/src/encoder/mod.rs`):
   - Add `mod <format>;` declaration.
   - Add `pub use <format>::<Format>Encoder;` re-export.
   - Add a variant to `EncoderConfig` enum.
   - Add a match arm in `create_encoder()`.

## Performance Rules

- **Zero allocation in encode_metric().** Write into the provided `buf`.
- **Pre-build label strings.** Labels are static per scenario — serialize them once in `new()`.
- **Use `write!` macro** for formatted output into `Vec<u8>` (via `std::io::Write` impl).
- **Avoid `format!` or `String` allocation** in the hot path.

## Quality Checklist

- [ ] `encode_metric()` writes into caller-provided `&mut Vec<u8>`.
- [ ] No per-event allocations (no `format!`, `String::new()`, `Vec::new()` in encode).
- [ ] Invariant content pre-built in `new()`.
- [ ] `///` doc comments on struct, `new()`, and trait methods.
- [ ] No `unwrap()` in library code.
- [ ] Registered in factory.

## Test Criteria (for the tester agent)

- Known MetricEvent → exact expected byte output (regression anchor).
- Empty labels → correct output (no trailing comma or empty braces).
- Special characters in label values → properly escaped.
- Multiple events → buffer accumulates correctly.
- Hardcoded expected strings — not computed from the same logic being tested.