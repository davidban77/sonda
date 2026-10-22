//! The pack catalog compiled into the binary.
//!
//! Sonda ships as a single static binary, so the builtin packs are embedded
//! with [`include_str!`] rather than discovered on disk at run time. The
//! source files live in the repo-root `packs/` directory and are the same
//! ones `docker-compose.yml` mounts at `/packs`.
//!
//! They are reached through the `sonda-core/packs` symlink, because
//! `cargo publish` packages only what is under the crate root: an
//! `include_str!` leaving the crate builds from a checkout and fails the
//! release. `docker_build_context.rs` resolves the symlink, so the escape
//! stays visible to the check that guards the Docker build context.

use std::path::PathBuf;

use super::{header_entry, CatalogEntry, EntryOrigin};
use crate::compiler::expand::{PackResolveError, PackResolveOrigin, PackResolver};
use crate::packs::MetricPackDef;

/// One pack embedded at compile time.
#[derive(Debug, Clone, Copy)]
pub struct BuiltinPack {
    /// Catalog name — the string a scenario writes under `pack:`. Must equal
    /// the pack's own `name:` field; [`PACK_COUNT`]'s test asserts it does.
    pub name: &'static str,
    /// Source file under `packs/`, carried for provenance in `sonda list`.
    pub file: &'static str,
    /// The pack YAML, verbatim.
    pub yaml: &'static str,
}

/// The number of packs [`PACKS`] must contain.
///
/// Declared here beside the list so a half-finished edit — a new
/// `include_str!` without its entry, or an entry deleted from the middle —
/// fails the count gate instead of quietly shipping a shorter catalog.
pub const PACK_COUNT: usize = 6;

/// Every pack embedded in this binary, sorted by name.
pub static PACKS: &[BuiltinPack] = &[
    BuiltinPack {
        name: "http_server_red",
        file: "http-server-red.yaml",
        yaml: include_str!("../../packs/http-server-red.yaml"),
    },
    BuiltinPack {
        name: "kube_state_metrics",
        file: "kube-state-metrics.yaml",
        yaml: include_str!("../../packs/kube-state-metrics.yaml"),
    },
    BuiltinPack {
        name: "node_exporter_cpu",
        file: "node-exporter-cpu.yaml",
        yaml: include_str!("../../packs/node-exporter-cpu.yaml"),
    },
    BuiltinPack {
        name: "node_exporter_memory",
        file: "node-exporter-memory.yaml",
        yaml: include_str!("../../packs/node-exporter-memory.yaml"),
    },
    BuiltinPack {
        name: "openconfig_interface",
        file: "openconfig-interface.yaml",
        yaml: include_str!("../../packs/openconfig-interface.yaml"),
    },
    BuiltinPack {
        name: "telegraf_snmp_interface",
        file: "telegraf-snmp-interface.yaml",
        yaml: include_str!("../../packs/telegraf-snmp-interface.yaml"),
    },
];

/// The source path reported for an embedded pack.
///
/// Not a path anything can open: the YAML is in the binary, and `<builtin>`
/// is not a legal directory name a user could have typed. `sonda show` reads
/// [`BuiltinPack::yaml`], never this.
pub fn source_path(pack: &BuiltinPack) -> PathBuf {
    PathBuf::from("<builtin>").join(pack.file)
}

/// Look up an embedded pack by catalog name.
pub fn find(name: &str) -> Option<&'static BuiltinPack> {
    PACKS.iter().find(|p| p.name == name)
}

/// Parse an embedded pack into its definition.
pub fn parse_pack(pack: &BuiltinPack) -> Result<MetricPackDef, serde_yaml_ng::Error> {
    serde_yaml_ng::from_str::<MetricPackDef>(pack.yaml)
}

/// The embedded packs as catalog entries, for listing.
///
/// Shares [`header_entry`] with the on-disk walk, so a builtin and a user
/// file with the same header produce the same row.
pub fn entries() -> Vec<CatalogEntry> {
    PACKS
        .iter()
        .filter_map(|pack| {
            header_entry(
                pack.yaml,
                pack.name,
                source_path(pack),
                EntryOrigin::Builtin,
            )
            .ok()
        })
        .collect()
}

/// [`PackResolver`] over the embedded catalog.
///
/// Resolves names only. File-path references are not this resolver's job —
/// [`super::CatalogPackResolver`] handles those before consulting builtins.
#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltinPackResolver;

impl BuiltinPackResolver {
    /// Create a resolver over [`PACKS`].
    pub fn new() -> Self {
        Self
    }
}

impl PackResolver for BuiltinPackResolver {
    fn resolve(&self, reference: &str) -> Result<MetricPackDef, PackResolveError> {
        let pack = find(reference).ok_or_else(|| {
            PackResolveError::new(
                format!(
                    "unknown pack {reference:?}; builtin packs: {}",
                    names().join(", ")
                ),
                PackResolveOrigin::Name,
            )
        })?;
        parse_pack(pack).map_err(|e| {
            PackResolveError::new(
                format!("cannot parse builtin pack {}: {e}", pack.file),
                PackResolveOrigin::Name,
            )
        })
    }
}

/// Every embedded pack name, in [`PACKS`] order.
pub fn names() -> Vec<&'static str> {
    PACKS.iter().map(|p| p.name).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::catalog::{EntryKind, SkipReason};

    /// The count gate. An embedded set that lost a pack — or gained one
    /// without the constant moving — fails here.
    #[test]
    fn the_embedded_set_matches_the_declared_count() {
        assert_eq!(
            PACKS.len(),
            PACK_COUNT,
            "PACKS has {} entries but PACK_COUNT says {PACK_COUNT}; \
             update both together",
            PACKS.len()
        );
        const { assert!(PACK_COUNT > 0, "an empty builtin catalog is never correct") };
    }

    /// The Rust-side `name` is a second copy of the pack's own `name:`. This
    /// is what keeps the two from drifting.
    #[test]
    fn every_declared_name_matches_the_packs_own_name_field() {
        assert_eq!(PACKS.len(), PACK_COUNT, "count gate must pass first");
        for pack in PACKS {
            let def = parse_pack(pack)
                .unwrap_or_else(|e| panic!("builtin pack {} must parse: {e}", pack.file));
            assert_eq!(
                def.name, pack.name,
                "{}: declared name {:?} but the YAML says {:?}",
                pack.file, pack.name, def.name
            );
        }
    }

    #[test]
    fn packs_are_sorted_by_name_and_unique() {
        let names = names();
        let mut sorted = names.clone();
        sorted.sort_unstable();
        sorted.dedup();
        assert_eq!(names, sorted, "PACKS must be sorted by name and unique");
    }

    #[test]
    fn every_pack_lists_as_a_composable_entry_carrying_a_category() {
        let entries = entries();
        assert_eq!(
            entries.len(),
            PACK_COUNT,
            "a pack whose header does not classify would be dropped from the listing"
        );
        for entry in &entries {
            assert_eq!(entry.kind, EntryKind::Composable, "{}", entry.name);
            assert_eq!(entry.origin, EntryOrigin::Builtin, "{}", entry.name);
            assert!(
                entry.category.is_some(),
                "{} must declare a category: `sonda list` groups by it",
                entry.name
            );
            assert!(!entry.description.is_empty(), "{}", entry.name);
        }
    }

    #[test]
    fn resolver_returns_a_builtin_by_name() {
        let resolver = BuiltinPackResolver::new();
        let pack = resolver
            .resolve("node_exporter_cpu")
            .expect("builtin must resolve with no catalog dir");
        assert_eq!(pack.name, "node_exporter_cpu");
        assert!(!pack.metrics.is_empty());
    }

    #[test]
    fn resolver_error_names_the_available_builtins() {
        let resolver = BuiltinPackResolver::new();
        let err = resolver.resolve("no_such_pack").expect_err("must error");
        let msg = format!("{err}");
        assert!(msg.contains("no_such_pack"), "got: {msg}");
        assert!(msg.contains("node_exporter_cpu"), "got: {msg}");
    }

    /// `SkipReason` is imported for the shared-classifier signature; assert
    /// the builtins never hit it.
    #[test]
    fn no_builtin_is_skipped_by_the_shared_classifier() {
        for pack in PACKS {
            let outcome = header_entry(
                pack.yaml,
                pack.name,
                source_path(pack),
                EntryOrigin::Builtin,
            );
            match outcome {
                Ok(entry) => assert_eq!(entry.name, pack.name),
                Err(reason) => panic!(
                    "{} was skipped by the catalog classifier: {}",
                    pack.file,
                    SkipReason::describe(&reason)
                ),
            }
        }
    }

    /// The OpenConfig leaf of every metric `openconfig_interface` declares.
    const OPENCONFIG_LEAVES: [(&str, &str); 11] = [
        ("in_octets", "state/counters/in-octets"),
        ("out_octets", "state/counters/out-octets"),
        ("in_pkts", "state/counters/in-pkts"),
        ("out_pkts", "state/counters/out-pkts"),
        ("in_errors", "state/counters/in-errors"),
        ("out_errors", "state/counters/out-errors"),
        ("in_discards", "state/counters/in-discards"),
        ("out_discards", "state/counters/out-discards"),
        ("carrier_transitions", "state/counters/carrier-transitions"),
        ("oper_status", "state/oper-status"),
        ("admin_status", "state/admin-status"),
    ];

    fn openconfig_pack() -> MetricPackDef {
        let pack = find("openconfig_interface").expect("openconfig_interface is embedded");
        parse_pack(pack).expect("openconfig_interface parses")
    }

    /// Runs with the `gnmi` feature off too: the hints parse and the pack
    /// validates without it.
    #[test]
    fn the_openconfig_pack_loads_and_every_metric_carries_its_hint() {
        use crate::encoder::gnmi::GnmiValueType;
        let pack = openconfig_pack();
        assert_eq!(crate::packs::validate_pack(&pack), Ok(()));
        let hints: Vec<(&str, &str, &GnmiValueType)> = pack
            .metrics
            .iter()
            .map(|spec| {
                let hint = spec
                    .gnmi
                    .as_ref()
                    .unwrap_or_else(|| panic!("{} carries no gnmi hint", spec.name));
                (spec.name.as_str(), hint.path.as_str(), &hint.value)
            })
            .collect();
        assert_eq!(hints.len(), OPENCONFIG_LEAVES.len());
        let status = GnmiValueType::Enum(std::collections::BTreeMap::from([
            (1, "UP".to_string()),
            (2, "DOWN".to_string()),
            (3, "TESTING".to_string()),
        ]));
        for ((name, path, value), (want_name, leaf)) in hints.into_iter().zip(OPENCONFIG_LEAVES) {
            assert_eq!(name, want_name);
            assert_eq!(
                path,
                format!("/interfaces/interface[name={{ifName}}]/{leaf}")
            );
            let want = if name.ends_with("_status") {
                &status
            } else {
                &GnmiValueType::Uint
            };
            assert_eq!(value, want, "{name}");
        }
    }

    #[cfg(feature = "gnmi")]
    #[test]
    fn every_openconfig_hint_path_parses_as_a_template() {
        let pack = openconfig_pack();
        assert_eq!(pack.metrics.len(), OPENCONFIG_LEAVES.len());
        for spec in &pack.metrics {
            let hint = spec.gnmi.as_ref().expect("a hint");
            crate::encoder::gnmi::path::PathTemplate::parse(&hint.path)
                .unwrap_or_else(|e| panic!("{}: {e}", spec.name));
        }
    }

    /// Two interfaces of the pack under a gnmi encoder that names no paths
    /// or values pass the whole-file validation, leaf check included. The
    /// entries share one address (not port 0, where each entry has a target
    /// of its own and no leaf is shared); validation binds nothing.
    #[cfg(feature = "gnmi")]
    #[test]
    fn the_openconfig_pack_under_a_bare_gnmi_encoder_passes_validate_file() {
        use crate::encoder::EncoderConfig;
        use crate::ScenarioEntry;
        let yaml = "version: 2
kind: runnable
defaults:
  rate: 1
  encoder: { type: gnmi, origin: openconfig-interfaces, target_label: device }
  sink: { type: gnmi_target, listen: 127.0.0.1:57400 }
scenarios:
  - signal_type: metrics
    pack: openconfig_interface
    labels: { device: rtr-1, ifName: Ethernet1 }
  - signal_type: metrics
    pack: openconfig_interface
    labels: { device: rtr-1, ifName: Ethernet2 }
";
        let entries = crate::compile_scenario_file(yaml, &BuiltinPackResolver::new())
            .expect("the scenario compiles");
        assert_eq!(entries.len(), 2 * OPENCONFIG_LEAVES.len());
        for entry in &entries {
            let ScenarioEntry::Metrics(config) = entry else {
                panic!("a metrics entry");
            };
            let EncoderConfig::Gnmi(cfg) = &config.encoder else {
                panic!("a gnmi encoder");
            };
            assert_eq!(
                cfg.paths.len(),
                OPENCONFIG_LEAVES.len(),
                "every hint folded"
            );
            assert_eq!(
                cfg.values.len(),
                OPENCONFIG_LEAVES.len(),
                "every hint folded"
            );
        }
        let indexed: Vec<(usize, &ScenarioEntry)> = entries.iter().enumerate().collect();
        crate::schedule::launch::validate_file(&indexed).expect("the file validates");
    }
}
