//! The pack catalog compiled into the binary.
//!
//! Sonda ships as a single static binary, so the builtin packs are embedded
//! with [`include_str!`] rather than discovered on disk at run time. The
//! source files live in the repo-root `packs/` directory and are the same
//! ones `docker-compose.yml` mounts at `/packs`.

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
pub const PACK_COUNT: usize = 3;

/// Every pack embedded in this binary, sorted by name.
pub static PACKS: &[BuiltinPack] = &[
    BuiltinPack {
        name: "node_exporter_cpu",
        file: "node-exporter-cpu.yaml",
        yaml: include_str!("../../../packs/node-exporter-cpu.yaml"),
    },
    BuiltinPack {
        name: "node_exporter_memory",
        file: "node-exporter-memory.yaml",
        yaml: include_str!("../../../packs/node-exporter-memory.yaml"),
    },
    BuiltinPack {
        name: "telegraf_snmp_interface",
        file: "telegraf-snmp-interface.yaml",
        yaml: include_str!("../../../packs/telegraf-snmp-interface.yaml"),
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
}
