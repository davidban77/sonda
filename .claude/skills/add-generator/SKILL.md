# Skill: Add a Value Generator

Use this skill when implementing a new value generator for sonda-core.

## Steps

1. **Create the source file**: `sonda-core/src/generator/<name>.rs`

2. **Define the struct**:
   ```rust
   /// Brief description of what this generator produces.
   pub struct <Name>Generator {
       // Configuration fields stored at construction time.
       // Must be Send + Sync — no interior mutability without synchronization.
   }

   impl <Name>Generator {
       /// Creates a new <Name>Generator.
       pub fn new(/* config params */) -> Self {
           Self { /* ... */ }
       }
   }
   ```

3. **Implement the trait**:
   ```rust
   impl ValueGenerator for <Name>Generator {
       fn value(&self, tick: u64) -> f64 {
           // Pure function. No side effects. No allocations.
           // tick is the monotonically increasing event counter.
       }
   }
   ```

4. **Register in factory** (`sonda-core/src/generator/mod.rs`):
   - Add `mod <name>;` declaration.
   - Add `pub use <name>::<Name>Generator;` re-export.
   - Add a variant to `GeneratorConfig` enum (serde-tagged).
   - Add a match arm in `create_generator()` returning `Box::new(<Name>Generator::new(...))`.

5. **Update config schema**: Add the new variant to the YAML config docs if they exist.

## Quality Checklist

- [ ] Struct is `Send + Sync` (no `Rc`, `Cell`, or non-threadsafe types).
- [ ] `value()` is a pure function — same tick always returns same value.
- [ ] `///` doc comment on the struct and `new()`.
- [ ] No `unwrap()` — use `expect()` only with justification.
- [ ] Registered in factory with matching serde variant.
- [ ] `cargo build --workspace` passes.
- [ ] `cargo clippy --workspace -- -D warnings` passes.

## Test Criteria (for the tester agent)

- tick=0 returns expected initial value.
- tick=1 returns expected second value.
- Large tick values don't panic or overflow.
- Determinism: two calls with same tick return identical values.
- If RNG-based: seeded generator produces identical sequence across runs.
- Edge cases: zero-config values, negative parameters (should error at construction).