//! Materializing an extension against its base.
//!
//! [`materialize`] is a pure function of (extension, base) that returns a
//! [`MetricPackDef`] indistinguishable from a hand-written one: expansion,
//! label composition and sub-signal registration never learn that packs can
//! extend. Chain walking and base lookup live in
//! [`crate::compiler::expand`], which is where the resolver is.
//!
//! Three properties the shape holds, in the vocabulary of the MIB/YANG model
//! it is drawn from:
//!
//! - **The base is never edited.** It is an input; the result is new.
//! - **Adding and changing are different operations.** `metrics:` is purely
//!   additive and refuses a selector the base already declares;
//!   [`Deviation`] is the only way to change one.
//! - **The join key is explicit.** A deviation names its target with the
//!   same selector `overrides:` uses, and a selector matching nothing is an
//!   error rather than a silent no-op.

use std::collections::{BTreeSet, HashMap};

use super::{Deviation, MetricPackDef, MetricSpec};

/// Why an extension cannot be materialized against its base.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum ExtendError {
    /// A deviation gave both `replace:` and `not_supported:`, or neither.
    ///
    /// Not a last-wins or a default: which one was meant is unknowable, and
    /// the two do opposite things.
    #[error(
        "pack '{pack_name}': deviation on '{selector}' must give exactly one of \
         `replace:` or `not_supported: true`, not {given}"
    )]
    DeviationNeedsExactlyOneAction {
        /// The extension being materialized.
        pack_name: String,
        /// The deviation's `metric:` selector as written.
        selector: String,
        /// Which way it was wrong — `"both"` or `"neither"`.
        given: &'static str,
    },

    /// Two deviations in one pack address the same spec.
    #[error("pack '{pack_name}': two deviations address metric '{selector}'")]
    RepeatedDeviationSelector {
        /// The extension being materialized.
        pack_name: String,
        /// The canonical selector both deviations resolve to.
        selector: String,
    },

    /// A deviation's selector does not address a spec in the base.
    ///
    /// A no-op deviation is almost always a typo or a base that moved on, so
    /// it fails rather than passing silently — which is also what stops the
    /// corpus gate covering selectors vacuously.
    #[error(
        "pack '{pack_name}': deviation selector '{selector}' matches no metric in base \
         '{base_name}'; available: {available}"
    )]
    DeviationSelectorNoMatch {
        /// The extension being materialized.
        pack_name: String,
        /// The deviation's `metric:` selector as written.
        selector: String,
        /// The base it was resolved against.
        base_name: String,
        /// Every selector the base does answer to.
        available: String,
    },

    /// An added metric's selector is one the base already declares.
    #[error(
        "pack '{pack_name}': metric '{selector}' is already declared by base '{base_name}'; \
         `metrics:` only adds — use a deviation to change it"
    )]
    AddedMetricCollidesWithBase {
        /// The extension being materialized.
        pack_name: String,
        /// The colliding selector.
        selector: String,
        /// The base that already declares it.
        base_name: String,
    },
}

/// Fold `extension` onto `base`, returning the effective pack.
///
/// `base` must already be addressable — [`super::validate_pack`] passed —
/// because deviation selectors are resolved against it. The caller
/// validates each link as it walks the chain.
///
/// Identity (`name`, `description`, `category`) comes from `extension`: the
/// result is that pack, resolved. `extends:` and `deviations:` are consumed,
/// so the return value carries neither and cannot be materialized twice.
///
/// # Errors
///
/// The four [`ExtendError`] variants. Note what is *not* checked here: the
/// result may still be unaddressable — two added metrics can share a name —
/// which [`super::validate_pack`] catches on the materialized pack rather
/// than being re-derived in this function.
pub fn materialize(
    extension: &MetricPackDef,
    base: &MetricPackDef,
) -> Result<MetricPackDef, ExtendError> {
    let mut metrics = base.metrics.clone();

    // Resolve every deviation before applying any, so a pack with one bad
    // deviation fails whole rather than half-applied.
    let mut targets: Vec<(usize, &Deviation)> = Vec::with_capacity(extension.deviations.len());
    let mut seen: BTreeSet<usize> = BTreeSet::new();
    for deviation in &extension.deviations {
        // A `replace:` that names no field says nothing, so it counts as no
        // action rather than as one. Without this the rule stops at the
        // deviation and lets the same typo class through one level down:
        // `replace: { generatr: … }` drops the unknown key — no pack type
        // can deny unknown keys, since pack files carry `kind:` and
        // `version:` — and would otherwise be accepted as a change that
        // changes nothing.
        let replace_says_nothing = deviation
            .replace
            .as_ref()
            .is_some_and(|r| r.generator.is_none() && r.labels.is_none());
        let given = match (&deviation.replace, deviation.not_supported) {
            (Some(_), true) => Some("both"),
            (Some(_), false) if replace_says_nothing => Some("neither"),
            (Some(_), false) | (None, true) => None,
            (None, false) => Some("neither"),
        };
        if let Some(given) = given {
            return Err(ExtendError::DeviationNeedsExactlyOneAction {
                pack_name: extension.name.clone(),
                selector: deviation.metric.clone(),
                given,
            });
        }

        let index = super::resolve_selector(base, &deviation.metric).map_err(|_| {
            // The base passed `validate_pack`, so a selector can only fail
            // by matching nothing — never by ambiguity.
            ExtendError::DeviationSelectorNoMatch {
                pack_name: extension.name.clone(),
                selector: deviation.metric.clone(),
                base_name: base.name.clone(),
                available: super::available_selectors(base),
            }
        })?;

        if !seen.insert(index) {
            return Err(ExtendError::RepeatedDeviationSelector {
                pack_name: extension.name.clone(),
                selector: base.metrics[index].selector(),
            });
        }
        targets.push((index, deviation));
    }

    // Apply in reverse index order so removals do not shift a later target.
    targets.sort_by_key(|(index, _)| std::cmp::Reverse(*index));
    for (index, deviation) in targets {
        match &deviation.replace {
            Some(replace) => {
                let spec = &mut metrics[index];
                if let Some(generator) = &replace.generator {
                    spec.generator = Some(generator.clone());
                }
                if let Some(labels) = &replace.labels {
                    spec.labels = Some(labels.clone());
                }
            }
            None => {
                metrics.remove(index);
            }
        }
    }

    // Additive metrics are checked against the base as it *arrived*, not as
    // deviations left it: `not_supported` then re-adding the same selector
    // would be a change dressed as an addition, and `replace:` says it
    // directly.
    let base_selectors: BTreeSet<String> = base.metrics.iter().map(MetricSpec::selector).collect();
    for spec in &extension.metrics {
        let selector = spec.selector();
        if base_selectors.contains(&selector) {
            return Err(ExtendError::AddedMetricCollidesWithBase {
                pack_name: extension.name.clone(),
                selector,
                base_name: base.name.clone(),
            });
        }
        metrics.push(spec.clone());
    }

    Ok(MetricPackDef {
        name: extension.name.clone(),
        description: extension.description.clone(),
        category: extension.category.clone(),
        extends: None,
        shared_labels: merge_shared_labels(base, extension),
        metrics,
        deviations: Vec::new(),
    })
}

/// The base's shared labels with the extension's merged over them.
///
/// This is where the extension's slot in the label precedence chain lives:
/// the materialized pack carries one map in which the extension has already
/// won, so `compose_pack_metric_labels` still sees a single
/// `shared_labels` layer and needs no knowledge of extension.
fn merge_shared_labels(
    base: &MetricPackDef,
    extension: &MetricPackDef,
) -> Option<HashMap<String, String>> {
    match (&base.shared_labels, &extension.shared_labels) {
        (None, None) => None,
        (Some(only), None) | (None, Some(only)) => Some(only.clone()),
        (Some(base_labels), Some(extension_labels)) => {
            let mut merged = base_labels.clone();
            merged.extend(extension_labels.iter().map(|(k, v)| (k.clone(), v.clone())));
            Some(merged)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generator::GeneratorConfig;
    use crate::packs::DeviationReplace;

    fn spec(name: &str, id: Option<&str>, value: f64) -> MetricSpec {
        MetricSpec {
            name: name.to_string(),
            id: id.map(str::to_string),
            labels: None,
            generator: Some(GeneratorConfig::Constant { value }),
        }
    }

    fn pack(name: &str, metrics: Vec<MetricSpec>) -> MetricPackDef {
        MetricPackDef {
            name: name.to_string(),
            description: "test".to_string(),
            category: "network".to_string(),
            extends: None,
            shared_labels: None,
            metrics,
            deviations: Vec::new(),
        }
    }

    fn replacing(selector: &str, value: f64) -> Deviation {
        Deviation {
            metric: selector.to_string(),
            replace: Some(DeviationReplace {
                generator: Some(GeneratorConfig::Constant { value }),
                labels: None,
            }),
            not_supported: false,
        }
    }

    fn removing(selector: &str) -> Deviation {
        Deviation {
            metric: selector.to_string(),
            replace: None,
            not_supported: true,
        }
    }

    fn constant_of(spec: &MetricSpec) -> f64 {
        match spec.generator {
            Some(GeneratorConfig::Constant { value }) => value,
            ref other => panic!("expected a constant, got {other:?}"),
        }
    }

    #[test]
    fn an_extension_adds_its_own_metrics_after_the_bases() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![spec("b", None, 2.0)]);
        ext.extends = Some("base".to_string());

        let out = materialize(&ext, &base).expect("must materialize");
        assert_eq!(out.name, "ext", "identity is the extension's");
        assert_eq!(
            out.metrics.iter().map(|m| m.selector()).collect::<Vec<_>>(),
            vec!["a", "b"]
        );
        assert!(out.extends.is_none() && out.deviations.is_empty());
    }

    #[test]
    fn replace_swaps_the_named_field_and_inherits_the_rest() {
        let mut base_spec = spec("a", None, 1.0);
        let mut labels = HashMap::new();
        labels.insert("kept".to_string(), "yes".to_string());
        base_spec.labels = Some(labels);
        let base = pack("base", vec![base_spec]);

        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![replacing("a", 99.0)];

        let out = materialize(&ext, &base).expect("must materialize");
        assert_eq!(out.metrics.len(), 1);
        assert_eq!(constant_of(&out.metrics[0]), 99.0);
        assert_eq!(
            out.metrics[0]
                .labels
                .as_ref()
                .and_then(|l| l.get("kept"))
                .map(String::as_str),
            Some("yes"),
            "a field the deviation did not name is inherited"
        );
    }

    #[test]
    fn not_supported_removes_the_spec() {
        let base = pack("base", vec![spec("a", None, 1.0), spec("b", None, 2.0)]);
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![removing("a")];

        let out = materialize(&ext, &base).expect("must materialize");
        assert_eq!(
            out.metrics.iter().map(|m| m.selector()).collect::<Vec<_>>(),
            vec!["b"]
        );
    }

    /// Removals shift indices, so a pack that removes one spec and replaces
    /// a later one must still hit the spec it named.
    #[test]
    fn a_removal_does_not_misaim_a_later_replacement() {
        let base = pack(
            "base",
            vec![
                spec("a", None, 1.0),
                spec("b", None, 2.0),
                spec("c", None, 3.0),
            ],
        );
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![removing("a"), replacing("c", 99.0)];

        let out = materialize(&ext, &base).expect("must materialize");
        assert_eq!(
            out.metrics.iter().map(|m| m.selector()).collect::<Vec<_>>(),
            vec!["b", "c"]
        );
        assert_eq!(constant_of(&out.metrics[0]), 2.0, "b is untouched");
        assert_eq!(constant_of(&out.metrics[1]), 99.0, "c took the replacement");
    }

    #[test]
    fn a_deviation_addresses_a_repeated_name_by_id() {
        let base = pack(
            "base",
            vec![
                spec("cpu", Some("user"), 1.0),
                spec("cpu", Some("idle"), 2.0),
            ],
        );
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![replacing("cpu.idle", 99.0)];

        let out = materialize(&ext, &base).expect("must materialize");
        assert_eq!(constant_of(&out.metrics[0]), 1.0, "user untouched");
        assert_eq!(constant_of(&out.metrics[1]), 99.0, "idle deviated");
    }

    #[test]
    fn the_extensions_shared_labels_win_over_the_bases() {
        let mut base = pack("base", vec![spec("a", None, 1.0)]);
        let mut base_labels = HashMap::new();
        base_labels.insert("job".to_string(), "snmp".to_string());
        base_labels.insert("kept".to_string(), "base".to_string());
        base.shared_labels = Some(base_labels);

        let mut ext = pack("ext", vec![]);
        let mut ext_labels = HashMap::new();
        ext_labels.insert("job".to_string(), "iosxe".to_string());
        ext.shared_labels = Some(ext_labels);

        let out = materialize(&ext, &base).expect("must materialize");
        let merged = out.shared_labels.expect("both maps present");
        assert_eq!(merged.get("job").map(String::as_str), Some("iosxe"));
        assert_eq!(merged.get("kept").map(String::as_str), Some("base"));
    }

    #[test]
    fn a_deviation_selector_matching_nothing_is_an_error_not_a_no_op() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![replacing("nope", 1.0)];

        match materialize(&ext, &base).expect_err("must refuse") {
            ExtendError::DeviationSelectorNoMatch {
                selector,
                available,
                ..
            } => {
                assert_eq!(selector, "nope");
                assert!(available.contains('a'), "must name what the base has");
            }
            other => panic!("expected DeviationSelectorNoMatch, got {other:?}"),
        }
    }

    #[test]
    fn two_deviations_on_one_spec_are_refused_rather_than_last_wins() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![replacing("a", 2.0), replacing("a", 3.0)];

        assert!(matches!(
            materialize(&ext, &base).expect_err("must refuse"),
            ExtendError::RepeatedDeviationSelector { .. }
        ));
    }

    /// The same spec reached two ways — bare name and `name.id` — is still
    /// two deviations on one spec, which is why the check keys on the
    /// resolved index rather than on the string.
    #[test]
    fn two_deviations_reaching_one_spec_by_different_selectors_are_refused() {
        let base = pack("base", vec![spec("a", Some("x"), 1.0)]);
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![replacing("a", 2.0), replacing("a.x", 3.0)];

        match materialize(&ext, &base).expect_err("must refuse") {
            ExtendError::RepeatedDeviationSelector { selector, .. } => {
                assert_eq!(selector, "a.x", "names the spec canonically")
            }
            other => panic!("expected RepeatedDeviationSelector, got {other:?}"),
        }
    }

    /// The same typo class one nesting level down. `replace: { generatr: … }`
    /// drops the unknown key for the same reason, leaving a `replace:` that
    /// names no field — a change that changes nothing. It is refused as
    /// "neither", because a `replace` saying nothing is not an action.
    #[test]
    fn a_replace_naming_no_field_is_refused_like_naming_no_action() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![]);
        // `replace: {}` written out, and what `replace: { generatr: … }`
        // deserializes to, are the same value — one case covers both.
        ext.deviations = vec![Deviation {
            metric: "a".to_string(),
            replace: Some(DeviationReplace {
                generator: None,
                labels: None,
            }),
            not_supported: false,
        }];

        match materialize(&ext, &base).expect_err("a replace saying nothing must refuse") {
            ExtendError::DeviationNeedsExactlyOneAction { given, .. } => {
                assert_eq!(given, "neither")
            }
            other => panic!("expected DeviationNeedsExactlyOneAction, got {other:?}"),
        }
    }

    /// And the guard does not overreach: naming one field is an action, and
    /// the field left out is still inherited.
    #[test]
    fn a_replace_naming_only_labels_is_still_an_action() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![]);
        ext.deviations = vec![Deviation {
            metric: "a".to_string(),
            replace: Some(DeviationReplace {
                generator: None,
                labels: Some([("k".to_string(), "v".to_string())].into_iter().collect()),
            }),
            not_supported: false,
        }];

        let out = materialize(&ext, &base).expect("naming one field is an action");
        assert_eq!(constant_of(&out.metrics[0]), 1.0, "generator inherited");
        assert_eq!(
            out.metrics[0]
                .labels
                .as_ref()
                .and_then(|l| l.get("k"))
                .map(String::as_str),
            Some("v")
        );
    }

    /// No pack type denies unknown YAML keys — pack files carry `kind:` and
    /// `version:`, which are not `MetricPackDef` fields — so a misspelled
    /// `not_supported:` deserializes to `false` and is silently dropped.
    /// The exactly-one-action rule is what turns that into a loud error:
    /// the deviation then names no action at all.
    #[test]
    fn a_misspelled_action_key_surfaces_as_naming_no_action() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![]);
        // What `not_suported: true` deserializes to.
        ext.deviations = vec![Deviation {
            metric: "a".to_string(),
            replace: None,
            not_supported: false,
        }];

        match materialize(&ext, &base).expect_err("must refuse") {
            ExtendError::DeviationNeedsExactlyOneAction { given, .. } => {
                assert_eq!(given, "neither")
            }
            other => panic!("expected DeviationNeedsExactlyOneAction, got {other:?}"),
        }
    }

    #[test]
    fn a_deviation_must_give_exactly_one_action() {
        let base = pack("base", vec![spec("a", None, 1.0)]);

        let mut both = pack("ext", vec![]);
        both.deviations = vec![Deviation {
            metric: "a".to_string(),
            replace: Some(DeviationReplace {
                generator: None,
                labels: None,
            }),
            not_supported: true,
        }];
        match materialize(&both, &base).expect_err("both must refuse") {
            ExtendError::DeviationNeedsExactlyOneAction { given, .. } => assert_eq!(given, "both"),
            other => panic!("expected DeviationNeedsExactlyOneAction, got {other:?}"),
        }

        let mut neither = pack("ext", vec![]);
        neither.deviations = vec![Deviation {
            metric: "a".to_string(),
            replace: None,
            not_supported: false,
        }];
        match materialize(&neither, &base).expect_err("neither must refuse") {
            ExtendError::DeviationNeedsExactlyOneAction { given, .. } => {
                assert_eq!(given, "neither")
            }
            other => panic!("expected DeviationNeedsExactlyOneAction, got {other:?}"),
        }
    }

    #[test]
    fn adding_a_metric_the_base_declares_is_an_error() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![spec("a", None, 9.0)]);
        ext.extends = Some("base".to_string());

        match materialize(&ext, &base).expect_err("must refuse") {
            ExtendError::AddedMetricCollidesWithBase { selector, .. } => assert_eq!(selector, "a"),
            other => panic!("expected AddedMetricCollidesWithBase, got {other:?}"),
        }
    }

    /// Removing a spec and adding one back under the same selector is a
    /// change written as an addition. `replace:` says it directly, so the
    /// collision check reads the base as it arrived.
    #[test]
    fn not_supported_does_not_free_the_selector_for_re_adding() {
        let base = pack("base", vec![spec("a", None, 1.0)]);
        let mut ext = pack("ext", vec![spec("a", None, 9.0)]);
        ext.deviations = vec![removing("a")];

        assert!(matches!(
            materialize(&ext, &base).expect_err("must refuse"),
            ExtendError::AddedMetricCollidesWithBase { .. }
        ));
    }

    /// The base is an input. Materializing must not touch it, or a chain
    /// resolved twice would give different answers.
    #[test]
    fn the_base_is_never_edited() {
        let base = pack("base", vec![spec("a", None, 1.0), spec("b", None, 2.0)]);
        let mut ext = pack("ext", vec![spec("c", None, 3.0)]);
        ext.deviations = vec![removing("a"), replacing("b", 99.0)];

        let before: Vec<String> = base.metrics.iter().map(|m| m.selector()).collect();
        let before_b = constant_of(&base.metrics[1]);
        let _ = materialize(&ext, &base).expect("must materialize");

        assert_eq!(
            base.metrics
                .iter()
                .map(|m| m.selector())
                .collect::<Vec<_>>(),
            before
        );
        assert_eq!(constant_of(&base.metrics[1]), before_b);
    }
}
