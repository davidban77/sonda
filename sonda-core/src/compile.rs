//! One-shot v2 scenario compilation from YAML to the runtime's input shape.
//!
//! This module composes the v2 compilation phases — `env_interpolate`,
//! `parse`, `normalize`, `expand`, `compile_after`, and `prepare` — behind a
//! single callable so that library consumers can go from YAML text to
//! `Vec<ScenarioEntry>` in one step. `env_interpolate` runs first so every
//! caller (CLI file load, HTTP body POST, programmatic) gets the same
//! `${VAR}` / `${VAR:-default}` substitution semantics.
//!
//! Every caller (CLI, server, tests) goes through this entry point — the
//! runtime accepts `Vec<ScenarioEntry>` directly and there is no v1 fallback.
//!
//! # Phase boundaries
//!
//! Callers who need to inspect an intermediate representation (e.g. a
//! [`NormalizedFile`][crate::compiler::normalize::NormalizedFile]) should
//! invoke the phase functions individually. [`compile_scenario_file`] is a
//! convenience wrapper; every error variant it returns is the same error the
//! underlying phase would have produced — see [`CompileError`].

use crate::compiler::compile_after::{compile_after, CompileAfterError, CompiledFile};
use crate::compiler::env_interpolate::{interpolate, InterpolateError};
use crate::compiler::expand::{expand, ExpandError, PackResolver};
use crate::compiler::normalize::{normalize, NormalizeError};
use crate::compiler::parse::{parse, ParseError};
use crate::compiler::prepare::{prepare, PrepareError};
use crate::config::ScenarioEntry;

/// Errors produced by [`compile_scenario_file`].
///
/// Each variant wraps the corresponding phase's error so callers can
/// programmatically discriminate where compilation failed without string
/// matching. The `#[from]` conversions let each phase's fallible call site
/// bubble up naturally via `?`.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CompileError {
    /// **Phase 0** (env_interpolate): `${VAR}` substitution against the
    /// process environment failed (unset required variable, malformed
    /// reference, or invalid variable name).
    #[error("env interpolation error")]
    EnvInterpolate(#[from] InterpolateError),

    /// **Phase 1** (parse): YAML parsing or schema validation failed.
    #[error("parse error")]
    Parse(#[from] ParseError),

    /// **Phase 2** (normalize): defaults resolution failed (e.g. an entry
    /// was missing a required field with no default available).
    #[error("normalize error")]
    Normalize(#[from] NormalizeError),

    /// **Phase 3** (expand): pack expansion failed (unknown pack, unknown
    /// override key, duplicate id, or resolver I/O error).
    #[error("expand error")]
    Expand(#[from] ExpandError),

    /// **Phase 4+5** (compile_after): `after:` resolution, dependency
    /// graph, or clock-group assignment failed.
    #[error("compile_after error")]
    CompileAfter(#[from] CompileAfterError),

    /// **Phase 6** (prepare): translation to the runtime input shape
    /// failed. Shape invariants not visible to earlier phases surface
    /// here — e.g. an unknown `signal_type` on a programmatically-
    /// constructed [`CompiledFile`][crate::compiler::compile_after::CompiledFile].
    ///
    /// Note: the [`PrepareError::UnknownSignalType`],
    /// [`PrepareError::MissingGenerator`],
    /// [`PrepareError::MissingLogGenerator`], and
    /// [`PrepareError::MissingDistribution`] cases are effectively
    /// unreachable when the input comes through
    /// [`compile_scenario_file`] — earlier phases gate those shapes at
    /// YAML-level. They remain reachable for programmatic callers that
    /// build a [`CompiledFile`][crate::compiler::compile_after::CompiledFile]
    /// in code and feed it directly to
    /// [`prepare`][crate::compiler::prepare::prepare].
    #[error("prepare error")]
    Prepare(#[from] PrepareError),

    /// The YAML uses `while:` or `delay:` clauses, which require the
    /// gated runtime. [`compile_scenario_file`] returns
    /// `Vec<ScenarioEntry>`, a shape that has no fields for these
    /// clauses, so silently accepting such input would drop the gate
    /// semantics and run the downstream as ungated. Call
    /// [`compile_scenario_file_compiled`] instead and feed the
    /// resulting [`CompiledFile`] to
    /// [`run_multi_compiled`][crate::schedule::multi_runner::run_multi_compiled].
    #[error(
        "scenario `{id}` uses {clause} (continuous coupling); call \
         `compile_scenario_file_compiled` and feed the result to \
         `run_multi_compiled` to preserve gate semantics"
    )]
    GatedClauseRequiresCompiledPath {
        /// The id of the entry that carries the gated clause.
        id: String,
        /// Which clause kind tripped the check (`"while:"` or `"delay:"`).
        clause: &'static str,
    },
}

/// Compile a v2 scenario YAML into the runtime's `Vec<ScenarioEntry>` input
/// shape.
///
/// The returned entries are ready to hand to
/// [`prepare_entries`][crate::schedule::launch::prepare_entries] (which
/// handles phase-offset parsing, csv_replay expansion, and validation) and
/// subsequently [`launch_scenario`][crate::schedule::launch::launch_scenario]
/// or [`run_multi`][crate::schedule::multi_runner::run_multi].
///
/// # Parameters
///
/// * `yaml` — raw v2 scenario YAML source. Version 2 is mandatory; v1
///   scenario shapes (flat single-entry, `pack:` shorthand, top-level
///   `scenarios:` list without `version: 2`) are rejected by
///   [`parse`][crate::compiler::parse::parse] with a clear error.
/// * `resolver` — pack-reference resolver used by
///   [`expand`][crate::compiler::expand::expand]. Pass an
///   [`InMemoryPackResolver`][crate::compiler::expand::InMemoryPackResolver]
///   seeded with the packs your scenario references, or a filesystem-backed
///   implementation for CLI-style usage.
///
/// # Errors
///
/// Returns a [`CompileError`] variant corresponding to the phase that
/// rejected the input; no partial output is produced.
pub fn compile_scenario_file(
    yaml: &str,
    resolver: &dyn PackResolver,
) -> Result<Vec<ScenarioEntry>, CompileError> {
    let compiled = compile_scenario_file_compiled(yaml, resolver)?;
    for (idx, entry) in compiled.entries.iter().enumerate() {
        let entry_label = || entry.id.clone().unwrap_or_else(|| format!("entry[{idx}]"));
        if entry.while_clause.is_some() {
            return Err(CompileError::GatedClauseRequiresCompiledPath {
                id: entry_label(),
                clause: "while:",
            });
        }
        if entry.delay_clause.is_some() {
            return Err(CompileError::GatedClauseRequiresCompiledPath {
                id: entry_label(),
                clause: "delay:",
            });
        }
    }
    Ok(prepare(compiled)?)
}

/// Compile a v2 scenario YAML to a [`CompiledFile`], preserving `while:` /
/// `delay:` clauses for the gated multi-runner.
///
/// Use this entry point when the runtime needs to wire `while:` gates
/// across scenarios. [`compile_scenario_file`] discards `while_clause` /
/// `delay_clause` because [`ScenarioEntry`] has no fields for them — the
/// gated multi-runner subscribes downstreams to upstream
/// [`GateBus`][crate::schedule::gate_bus::GateBus]es via
/// [`run_multi_compiled`][crate::schedule::multi_runner::run_multi_compiled],
/// which consumes a [`CompiledFile`].
pub fn compile_scenario_file_compiled(
    yaml: &str,
    resolver: &dyn PackResolver,
) -> Result<CompiledFile, CompileError> {
    // `expand` uses a `Sized` generic bound, so wrap the trait object in a
    // local `Sized` adapter that forwards each call. This keeps the public
    // signature `&dyn PackResolver` (object-safe, no monomorphization blow-up
    // for callers that cross module boundaries) without modifying `expand`'s
    // API.
    let wrapped = DynPackResolver(resolver);
    let interpolated = interpolate(yaml)?;
    let parsed = parse(&interpolated)?;
    let normalized = normalize(parsed)?;
    let expanded = expand(normalized, &wrapped)?;
    Ok(compile_after(expanded)?)
}

/// Adapter that implements the `Sized` bound `expand` requires while
/// delegating to an underlying `&dyn PackResolver`.
struct DynPackResolver<'a>(&'a dyn PackResolver);

impl<'a> PackResolver for DynPackResolver<'a> {
    fn resolve(
        &self,
        reference: &str,
    ) -> Result<crate::packs::MetricPackDef, crate::compiler::expand::PackResolveError> {
        self.0.resolve(reference)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::expand::InMemoryPackResolver;

    fn empty_resolver() -> InMemoryPackResolver {
        InMemoryPackResolver::new()
    }

    /// Happy path: a minimal inline v2 YAML compiles cleanly and produces
    /// one [`ScenarioEntry`].
    #[test]
    fn one_shot_compiles_minimal_inline_scenario() {
        let yaml = r#"
version: 2

defaults:
  rate: 10
  duration: 500ms

scenarios:
  - id: cpu
    signal_type: metrics
    name: cpu_usage
    generator:
      type: constant
      value: 1.0
"#;
        let resolver = empty_resolver();
        let entries = compile_scenario_file(yaml, &resolver).expect("one-shot must succeed");
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].base().name, "cpu_usage");
        assert_eq!(entries[0].base().rate, 10.0);
    }

    /// `parse` failures surface as `CompileError::Parse`.
    #[test]
    fn parse_failure_surfaces_as_parse_variant() {
        let yaml = "version: 1\nscenarios: []\n";
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver).expect_err("v1 yaml must fail");
        assert!(
            matches!(err, CompileError::Parse(_)),
            "v1 version must surface as Parse, got {err:?}"
        );
    }

    #[test]
    fn yaml_with_while_clause_rejected_with_compiled_path_hint() {
        let yaml = r#"
version: 2

defaults:
  rate: 1
  duration: 30s

scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: flap
      up_duration: 5s
      down_duration: 5s

  - id: downstream
    signal_type: metrics
    name: downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: "<"
      value: 1
"#;
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver)
            .expect_err("while: must reject through the lossy entry point");
        match err {
            CompileError::GatedClauseRequiresCompiledPath { id, clause } => {
                assert_eq!(id, "downstream");
                assert_eq!(clause, "while:");
            }
            other => panic!("expected GatedClauseRequiresCompiledPath, got {other:?}"),
        }
    }

    #[test]
    fn yaml_with_delay_clause_rejected_with_compiled_path_hint() {
        let yaml = r#"
version: 2

defaults:
  rate: 1
  duration: 30s

scenarios:
  - id: upstream
    signal_type: metrics
    name: upstream
    generator:
      type: flap
      up_duration: 5s
      down_duration: 5s

  - id: downstream
    signal_type: metrics
    name: downstream
    generator:
      type: constant
      value: 1.0
    while:
      ref: upstream
      op: "<"
      value: 1
    delay:
      open: 2s
      close: 0s
"#;
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver)
            .expect_err("delay: must reject through the lossy entry point");
        match err {
            CompileError::GatedClauseRequiresCompiledPath { id, clause } => {
                assert_eq!(id, "downstream");
                assert!(
                    clause == "while:" || clause == "delay:",
                    "expected while: or delay:, got {clause}"
                );
            }
            other => panic!("expected GatedClauseRequiresCompiledPath, got {other:?}"),
        }
    }

    /// `normalize` failures surface as `CompileError::Normalize`.
    /// A metrics entry without `rate` (and no default) fails at Phase 2.
    #[test]
    fn normalize_failure_surfaces_as_normalize_variant() {
        let yaml = r#"
version: 2

scenarios:
  - id: no_rate
    signal_type: metrics
    name: no_rate
    generator:
      type: constant
      value: 1.0
"#;
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver).expect_err("missing rate must fail");
        assert!(
            matches!(err, CompileError::Normalize(_)),
            "missing rate must surface as Normalize, got {err:?}"
        );
    }

    /// `expand` failures surface as `CompileError::Expand`.
    /// An unresolvable pack name produces ResolveFailed.
    #[test]
    fn expand_failure_surfaces_as_expand_variant() {
        let yaml = r#"
version: 2

defaults:
  rate: 1

scenarios:
  - signal_type: metrics
    pack: unknown_pack_xyz
"#;
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver).expect_err("unknown pack must fail");
        assert!(
            matches!(err, CompileError::Expand(_)),
            "unresolvable pack must surface as Expand, got {err:?}"
        );
    }

    /// `compile_after` failures surface as `CompileError::CompileAfter`.
    /// A self-reference fires `SelfReference`.
    #[test]
    fn compile_after_failure_surfaces_as_compile_after_variant() {
        let yaml = r#"
version: 2

defaults:
  rate: 1

scenarios:
  - id: loopy
    signal_type: metrics
    name: loopy
    generator:
      type: flap
      up_duration: 60s
      down_duration: 30s
    after:
      ref: loopy
      op: "<"
      value: 1
"#;
        let resolver = empty_resolver();
        let err = compile_scenario_file(yaml, &resolver).expect_err("self-ref must fail");
        assert!(
            matches!(err, CompileError::CompileAfter(_)),
            "self-reference must surface as CompileAfter, got {err:?}"
        );
    }

    /// Error types satisfy Send + Sync so they can cross thread boundaries.
    #[test]
    fn compile_error_is_send_and_sync() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<CompileError>();
    }
}
